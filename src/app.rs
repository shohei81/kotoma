use crate::{
    audio::AudioCapture,
    config::Config,
    markdown,
    msg::{DraftState, TranslatorStatus, UiMsg},
    transcribe::{Segment, TranscribeRunner, TranscriptLine},
    translate::{self, TranslatorConfig},
    ui::{draw, TranscriptLayout, UiState},
    vad::VadRunner,
};
use anyhow::Result;
use crossbeam_channel::{bounded, unbounded, Receiver};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use std::cell::Cell;
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use tracing::info;

pub struct DevicePicker {
    pub mic: Vec<String>,
    pub sys: Vec<String>,
    pub mic_sel: usize,
    pub sys_sel: usize,
    /// 0 = mic section, 1 = system-audio section.
    pub focus: u8,
}

/// Restore the terminal before the default panic output. Without this a
/// panic leaves the terminal in raw mode + alternate screen and the user's
/// shell appears broken.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        original(info);
    }));
}

pub fn run(cfg: Config, existing_content: Option<String>) -> Result<()> {
    install_panic_hook();
    let (audio_tx, audio_rx) = bounded::<Vec<f32>>(64);
    let (seg_tx, seg_rx) = bounded::<Segment>(16);
    let (line_tx, line_rx) = bounded::<TranscriptLine>(32);
    let (ui_tx, ui_rx) = unbounded::<UiMsg>();
    let (level_tx, level_rx) = bounded::<f32>(16);
    let (draft_tx, draft_rx) = bounded::<DraftState>(8);

    let language = Arc::new(RwLock::new(cfg.language.clone()));

    let mut capture = AudioCapture::start_dual(
        &cfg.input_device,
        cfg.system_audio_device.as_deref(),
        audio_tx.clone(),
    )?;
    let mut mic_device = cfg.input_device.clone();
    let mut sys_device = cfg
        .system_audio_device
        .clone()
        .unwrap_or_else(|| "(none)".to_string());
    let model_name = cfg
        .model_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());

    let paused = Arc::new(AtomicBool::new(false));
    let paused_vad = paused.clone();
    let vad_cfg = cfg.vad.clone();
    thread::spawn(move || {
        let vad = VadRunner::new(
            vad_cfg.aggressiveness,
            vad_cfg.min_speech_ms,
            vad_cfg.silence_ms,
            vad_cfg.max_segment_ms,
        );
        if let Err(e) = vad.run(audio_rx, seg_tx, level_tx, draft_tx, paused_vad) {
            tracing::error!("vad thread ended: {}", e);
        }
    });

    let model_path = cfg.model_path.clone();
    let threads_n = cfg.threads;
    let lang_for_worker = language.clone();
    thread::spawn(move || match TranscribeRunner::new(&model_path, threads_n, lang_for_worker) {
        Ok(runner) => runner.run(seg_rx, line_tx),
        Err(e) => tracing::error!("whisper init failed: {}", e),
    });

    // Translator fanout: mirror transcript lines to UI immediately so the source
    // text always renders even if translation is slow or disabled.
    let translator_cfg = cfg
        .translator
        .clone()
        .filter(|t| {
            if !t.model_path.exists() {
                tracing::warn!(
                    path = %t.model_path.display(),
                    "translator model not found — running without translation"
                );
                return false;
            }
            if let Ok(meta) = std::fs::metadata(&t.model_path) {
                if meta.len() < 10_000_000 {
                    tracing::error!(
                        path = %t.model_path.display(),
                        size = meta.len(),
                        "translator model file is too small — likely a broken download"
                    );
                    return false;
                }
            }
            true
        });

    let translator_status_init = match translator_cfg {
        Some(tcfg) => {
            let (trans_in_tx, trans_in_rx) = bounded::<TranscriptLine>(32);
            let ui_tx_fanout = ui_tx.clone();
            thread::spawn(move || {
                while let Ok(line) = line_rx.recv() {
                    let _ = ui_tx_fanout.send(UiMsg::NewLine(line.clone()));
                    let _ = trans_in_tx.send(line);
                }
            });
            translate::spawn(
                TranslatorConfig {
                    binary: tcfg.binary,
                    model_path: tcfg.model_path,
                    port: tcfg.port,
                    n_ctx: tcfg.n_ctx,
                    max_new_tokens: tcfg.max_new_tokens,
                    startup_timeout_secs: tcfg.startup_timeout_secs,
                    log_dir: cfg.log_dir.clone(),
                },
                trans_in_rx,
                ui_tx.clone(),
            );
            TranslatorStatus::Loading
        }
        None => {
            let ui_tx_fwd = ui_tx.clone();
            thread::spawn(move || {
                while let Ok(line) = line_rx.recv() {
                    let _ = ui_tx_fwd.send(UiMsg::NewLine(line));
                }
            });
            TranslatorStatus::Failed
        }
    };

    let mut terminal = setup_terminal()?;
    let res = run_loop(
        &mut terminal,
        &cfg,
        &language,
        &paused,
        &mut capture,
        audio_tx,
        &model_name,
        ui_rx,
        level_rx,
        draft_rx,
        translator_status_init,
        existing_content.as_deref(),
        &mut mic_device,
        &mut sys_device,
    );
    restore_terminal(&mut terminal)?;
    drop(capture);
    res
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    cfg: &Config,
    language: &Arc<RwLock<String>>,
    paused: &Arc<AtomicBool>,
    capture: &mut AudioCapture,
    audio_tx: crossbeam_channel::Sender<Vec<f32>>,
    model_name: &str,
    ui_rx: Receiver<UiMsg>,
    level_rx: Receiver<f32>,
    draft_rx: Receiver<DraftState>,
    initial_translator_status: TranslatorStatus,
    existing_content: Option<&str>,
    mic_device: &mut String,
    sys_device: &mut String,
) -> Result<()> {
    let mut lines: Vec<TranscriptLine> = Vec::new();
    let mut level = 0.0f32;
    let mut level_smooth = 0.0f32;
    let mut saved_note: Option<String> = None;
    let mut translator_status = initial_translator_status;
    let mut input_name = capture.input_name.clone();
    let mut picker: Option<DevicePicker> = None;
    let mut draft = DraftState::default();
    let mut show_help = false;
    let mut scroll_up: u16 = 0;
    let scroll_max_cell: Cell<u16> = Cell::new(0);
    let mut layout = TranscriptLayout::new();
    // Redraw only when something visible changed — a constant 20 fps repaint
    // of an idle screen wastes CPU and battery.
    //
    // The audio gauge animates continuously, so naively redrawing on every
    // level change repaints the whole TUI at the 20 Hz poll rate and keeps the
    // terminal + window compositor busy (and the machine warm) the entire
    // session — even ambient mic noise flickers the level. So: quantise the
    // gauge (deadband) so silence is genuinely idle, and throttle gauge-only
    // repaints to ~10 Hz. Discrete changes (new line, keypress, …) redraw at once.
    let mut needs_redraw = true;
    let mut last_gauge_bucket = u16::MAX;
    let mut last_draw = std::time::Instant::now();
    const GAUGE_MIN_INTERVAL: Duration = Duration::from_millis(100);

    loop {
        while let Ok(msg) = ui_rx.try_recv() {
            needs_redraw = true;
            match msg {
                UiMsg::NewLine(line) => lines.push(line),
                UiMsg::TranslationReady { id, translated } => {
                    if let Some(idx) = lines.iter().position(|l| l.id == id) {
                        lines[idx].translated = Some(translated);
                        layout.invalidate_line(idx);
                    }
                }
                UiMsg::TranslatorStatus(s) => translator_status = s,
            }
        }
        while let Ok(l) = level_rx.try_recv() {
            level = l;
        }
        level_smooth = level_smooth * 0.7 + level * 0.3;
        while let Ok(d) = draft_rx.try_recv() {
            draft = d;
            needs_redraw = true;
        }
        // Deadband: sub-1% levels (ambient noise) read as zero, then bucket to
        // 4% steps so tiny fluctuations don't each trigger a repaint.
        let gauge_pct = if level_smooth < 0.0025 {
            0
        } else {
            (level_smooth * 400.0).clamp(0.0, 100.0) as u16
        };
        let gauge_bucket = gauge_pct / 4;
        let gauge_due =
            gauge_bucket != last_gauge_bucket && last_draw.elapsed() >= GAUGE_MIN_INTERVAL;

        if needs_redraw || gauge_due {
            needs_redraw = false;
            last_gauge_bucket = gauge_bucket;
            last_draw = std::time::Instant::now();
            let lang = language.read().map(|g| g.clone()).unwrap_or_default();
            let is_recording = !paused.load(Ordering::Relaxed);
            terminal.draw(|f| {
                draw(
                    f,
                    &UiState {
                        lines: &lines,
                        level: level_smooth,
                        language: &lang,
                        recording: is_recording,
                        input_name: &input_name,
                        model_name,
                        saved_note: saved_note.as_deref(),
                        translator_status,
                        single_column: cfg.translator.is_none(),
                        picker: picker.as_ref(),
                        show_help,
                        draft,
                        scroll_up,
                        scroll_max: &scroll_max_cell,
                    },
                    &mut layout,
                );
            })?;
            // Clamp any key-driven over-scroll to what the columns can actually show.
            scroll_up = scroll_up.min(scroll_max_cell.get());
        }
        let page = terminal.size().map(|r| r.height / 2).unwrap_or(5).max(1);

        if event::poll(Duration::from_millis(50))? {
            let ev = event::read()?;
            needs_redraw = true;
            if let Event::Mouse(MouseEvent { kind, .. }) = ev {
                match kind {
                    MouseEventKind::ScrollUp => {
                        scroll_up = scroll_up.saturating_add(3).min(scroll_max_cell.get());
                    }
                    MouseEventKind::ScrollDown => {
                        scroll_up = scroll_up.saturating_sub(3);
                    }
                    _ => {}
                }
            } else if let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = ev
            {
                if show_help {
                    match code {
                        KeyCode::Char('?')
                        | KeyCode::Esc
                        | KeyCode::Char('q')
                        | KeyCode::Enter => show_help = false,
                        _ => {}
                    }
                } else if let Some(pk) = picker.as_mut() {
                    match code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            picker = None;
                        }
                        KeyCode::Tab | KeyCode::Right | KeyCode::Left => {
                            pk.focus ^= 1;
                        }
                        KeyCode::Up => {
                            let sel = if pk.focus == 0 { &mut pk.mic_sel } else { &mut pk.sys_sel };
                            if *sel > 0 {
                                *sel -= 1;
                            }
                        }
                        KeyCode::Down => {
                            let (sel, len) = if pk.focus == 0 {
                                (&mut pk.mic_sel, pk.mic.len())
                            } else {
                                (&mut pk.sys_sel, pk.sys.len())
                            };
                            if *sel + 1 < len {
                                *sel += 1;
                            }
                        }
                        KeyCode::Enter => {
                            let mic_choice = pk.mic[pk.mic_sel].clone();
                            let sys_choice = pk.sys[pk.sys_sel].clone();
                            let sys_arg = if sys_choice == "(none)" {
                                None
                            } else {
                                Some(sys_choice.as_str())
                            };
                            match AudioCapture::start_dual(
                                &mic_choice,
                                sys_arg,
                                audio_tx.clone(),
                            ) {
                                Ok(new_cap) => {
                                    input_name = new_cap.input_name.clone();
                                    *capture = new_cap;
                                    *mic_device = mic_choice.clone();
                                    *sys_device = sys_choice.clone();
                                    info!(
                                        mic = %mic_choice,
                                        sys = %sys_choice,
                                        "switched input devices"
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        mic = %mic_choice,
                                        sys = %sys_choice,
                                        error = %e,
                                        "device switch failed"
                                    );
                                }
                            }
                            picker = None;
                        }
                        _ => {}
                    }
                } else {
                    match (code, modifiers) {
                        (KeyCode::Char('q'), _)
                        | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            markdown::write(
                                &cfg.output_path,
                                &lines,
                                existing_content,
                            )?;
                            info!(path = %cfg.output_path.display(), "saved on quit");
                            break;
                        }
                        (KeyCode::Char('s'), _) => {
                            markdown::write(&cfg.output_path, &lines, existing_content)?;
                            saved_note = Some(format!("saved → {}", cfg.output_path.display()));
                        }
                        (KeyCode::Char('l'), _) => {
                            if let Ok(mut g) = language.write() {
                                *g = match g.as_str() {
                                    "en" => "ja".into(),
                                    "ja" => "auto".into(),
                                    _ => "en".into(),
                                };
                            }
                        }
                        (KeyCode::Char(' '), _) => {
                            let now_paused = !paused.load(Ordering::Relaxed);
                            paused.store(now_paused, Ordering::Relaxed);
                            info!(paused = now_paused, "toggle recording");
                        }
                        (KeyCode::Char('d'), _) => {
                            let (mic, sys) = crate::audio::list_devices_split();
                            let mic_sel = mic
                                .iter()
                                .position(|d| d == &*mic_device)
                                .unwrap_or(0);
                            let sys_sel = sys
                                .iter()
                                .position(|d| d == &*sys_device)
                                .unwrap_or_else(|| sys.iter().position(|d| d == "(auto)").unwrap_or(0));
                            picker = Some(DevicePicker {
                                mic,
                                sys,
                                mic_sel,
                                sys_sel,
                                focus: 0,
                            });
                        }
                        (KeyCode::Char('m'), _) => {
                            // Quick toggle: mic only ↔ mic + auto-detected system audio.
                            let next_sys = if sys_device.as_str() == "(none)" {
                                "(auto)".to_string()
                            } else {
                                "(none)".to_string()
                            };
                            let sys_arg = if next_sys == "(none)" {
                                None
                            } else {
                                Some(next_sys.as_str())
                            };
                            match AudioCapture::start_dual(
                                mic_device.as_str(),
                                sys_arg,
                                audio_tx.clone(),
                            ) {
                                Ok(new_cap) => {
                                    input_name = new_cap.input_name.clone();
                                    *capture = new_cap;
                                    *sys_device = next_sys.clone();
                                    saved_note = Some(format!("system audio: {}", input_name));
                                    info!(sys = %next_sys, "toggled system audio");
                                }
                                Err(e) => {
                                    saved_note = Some(format!("system audio: {}", e));
                                    tracing::error!(error = %e, "system audio toggle failed");
                                }
                            }
                        }
                        (KeyCode::Char('?'), _) => {
                            show_help = true;
                        }
                        (KeyCode::Up, _) => {
                            scroll_up = scroll_up.saturating_add(1).min(scroll_max_cell.get());
                        }
                        (KeyCode::Down, _) => {
                            scroll_up = scroll_up.saturating_sub(1);
                        }
                        (KeyCode::PageUp, _) => {
                            scroll_up = scroll_up.saturating_add(page).min(scroll_max_cell.get());
                        }
                        (KeyCode::PageDown, _) => {
                            scroll_up = scroll_up.saturating_sub(page);
                        }
                        (KeyCode::Home, _) => {
                            scroll_up = scroll_max_cell.get();
                        }
                        (KeyCode::End, _) => {
                            scroll_up = 0;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}
