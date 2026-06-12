use crate::app::DevicePicker;
use crate::msg::{DraftState, TranslatorStatus};
use crate::transcribe::TranscriptLine;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap};
use std::cell::Cell;

/// Sentinel `col_lang` for the single-column (no-translation) view: every line
/// renders its own transcribed text, whatever language it was detected as.
const SOURCE_COL: &str = "*";

/// Per-column cache of wrapped row counts, so a frame only re-wraps lines
/// that are new (or whose translation just arrived) instead of the whole
/// transcript. Without this, rendering cost grows with session length and
/// long sessions freeze during scroll.
pub struct ColumnCache {
    width: u16,
    rows: Vec<u16>,
}

impl ColumnCache {
    fn new() -> Self {
        Self {
            width: 0,
            rows: Vec::new(),
        }
    }

    fn invalidate_from(&mut self, idx: usize) {
        self.rows.truncate(idx);
    }

    fn sync(&mut self, lines: &[TranscriptLine], col_lang: &str, width: u16) {
        if width != self.width {
            self.width = width;
            self.rows.clear();
        }
        if width == 0 {
            self.rows.clear();
            return;
        }
        while self.rows.len() < lines.len() {
            let i = self.rows.len();
            let para = Paragraph::new(vec![render_line(&lines[i], col_lang)])
                .wrap(Wrap { trim: false });
            self.rows
                .push(para.line_count(width).min(u16::MAX as usize) as u16);
        }
    }
}

pub struct TranscriptLayout {
    en: ColumnCache,
    ja: ColumnCache,
}

impl TranscriptLayout {
    pub fn new() -> Self {
        Self {
            en: ColumnCache::new(),
            ja: ColumnCache::new(),
        }
    }

    /// Call when an existing line changed (e.g. its translation arrived).
    pub fn invalidate_line(&mut self, idx: usize) {
        self.en.invalidate_from(idx);
        self.ja.invalidate_from(idx);
    }
}

pub struct UiState<'a> {
    pub lines: &'a [TranscriptLine],
    pub level: f32,
    pub language: &'a str,
    pub recording: bool,
    pub input_name: &'a str,
    pub model_name: &'a str,
    pub saved_note: Option<&'a str>,
    pub translator_status: TranslatorStatus,
    /// When true, translation is off: render one full-width column showing
    /// each line's transcribed (source) text instead of the EN ↔ JA split.
    pub single_column: bool,
    pub picker: Option<&'a DevicePicker>,
    pub show_help: bool,
    pub draft: DraftState,
    /// Rows scrolled up from the tail. 0 = follow latest.
    pub scroll_up: u16,
    /// Out-param: set during draw to the largest usable `scroll_up` across
    /// both transcript columns, so the caller can clamp user input.
    pub scroll_max: &'a Cell<u16>,
}

pub fn draw(f: &mut Frame, state: &UiState, layout: &mut TranscriptLayout) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(f.area());

    let level_pct = ((state.level * 400.0).clamp(0.0, 100.0)) as u16;
    let status_label = if state.recording { "REC" } else { "PAUSED" };
    let tr_label = match state.translator_status {
        TranslatorStatus::Loading => "tr=loading",
        TranslatorStatus::Ready => "tr=ready",
        TranslatorStatus::Failed => "tr=off",
    };
    let title = format!(
        " kotoma · {} · lang={} · in={} · model={} · {} ",
        status_label, state.language, state.input_name, state.model_name, tr_label
    );
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(title))
        .gauge_style(Style::default().fg(if state.recording {
            Color::Green
        } else {
            Color::DarkGray
        }))
        .percent(level_pct);
    f.render_widget(gauge, chunks[0]);

    let draft_text = if state.draft.active {
        format!(
            " ● capturing · {:.1}s speech · {:.1}s elapsed",
            state.draft.speech_ms as f32 / 1000.0,
            state.draft.elapsed_ms as f32 / 1000.0
        )
    } else {
        " (no speech)".to_string()
    };
    let draft_style = if state.draft.active {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let draft_para = Paragraph::new(draft_text).style(draft_style);
    f.render_widget(draft_para, chunks[1]);

    state.scroll_max.set(0);
    if state.single_column {
        let title = match state.language {
            "en" => " English ",
            "ja" => " 日本語 ",
            _ => " Transcript ",
        };
        render_transcript_column(
            f,
            chunks[2],
            state.lines,
            SOURCE_COL,
            title,
            state.scroll_up,
            state.scroll_max,
            &mut layout.en,
        );
    } else {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[2]);

        render_transcript_column(
            f,
            cols[0],
            state.lines,
            "en",
            " English ",
            state.scroll_up,
            state.scroll_max,
            &mut layout.en,
        );
        render_transcript_column(
            f,
            cols[1],
            state.lines,
            "ja",
            " 日本語 ",
            state.scroll_up,
            state.scroll_max,
            &mut layout.ja,
        );
    }

    let nav = if state.scroll_up == 0 {
        " ↑/PgUp scroll ".to_string()
    } else {
        format!(" ↑↓/PgUp/PgDn · End follow (−{}) ", state.scroll_up)
    };
    let help = match state.saved_note {
        Some(note) => format!(
            " {} · q quit&save · s save · ? help ·{}",
            note, nav
        ),
        None => format!(
            " q quit&save · s save · l lang · d device · m sys-audio · space pause · ? help ·{}",
            nav
        ),
    };
    let help_p = Paragraph::new(help).style(Style::default().fg(Color::DarkGray));
    f.render_widget(help_p, chunks[3]);

    if let Some(pk) = state.picker {
        render_device_picker(f, pk);
    }
    if state.show_help {
        render_help_overlay(f);
    }
}

fn render_help_overlay(f: &mut Frame) {
    const ROWS: &[(&str, &str)] = &[
        ("q / Ctrl+C", "Save & quit (+ 要約/Summary)"),
        ("s", "Save now"),
        ("l", "Cycle language (en → ja → auto)"),
        ("space", "Pause / resume capture"),
        ("m", "Toggle system-audio mix"),
        ("d", "Audio source picker"),
        ("↑ / ↓", "Scroll one line"),
        ("PgUp / PgDn", "Scroll half a page"),
        ("Home / End", "Oldest line / follow live"),
        ("mouse wheel", "Scroll transcript"),
        ("?", "Toggle this help"),
    ];

    // Launch flags can't be toggled at runtime, but listing them here saves a
    // trip to `--help` for the most common ones.
    const FLAGS: &[(&str, &str)] = &[
        ("--no-translate", "Transcribe only (no translation)"),
        ("-l <en|ja|auto>", "Set starting language"),
        ("-r / --resume", "Append to an existing file"),
    ];

    let area = centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help  (? or esc to close) ");
    let row = |key: &str, desc: &str| {
        Line::from(vec![
            Span::styled(
                format!(" {key:<16}"),
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::raw(desc.to_string()),
        ])
    };
    let mut lines: Vec<Line> = ROWS.iter().map(|(k, d)| row(k, d)).collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Launch flags",
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    )));
    lines.extend(FLAGS.iter().map(|(k, d)| row(k, d)));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_device_picker(f: &mut Frame, pk: &DevicePicker) {
    let area = centered_rect(70, 70, f.area());
    f.render_widget(Clear, area);

    let outer = Block::default().borders(Borders::ALL).title(
        " Audio sources  (↑↓ select · tab switch · enter apply · esc cancel) ",
    );
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    render_picker_column(f, cols[0], "Microphone", &pk.mic, pk.mic_sel, pk.focus == 0);
    render_picker_column(f, cols[1], "System audio", &pk.sys, pk.sys_sel, pk.focus == 1);
}

fn render_picker_column(
    f: &mut Frame,
    area: Rect,
    title: &str,
    devices: &[String],
    selected: usize,
    focused: bool,
) {
    let items: Vec<ListItem> = devices
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let is_sel = i == selected;
            let marker = if is_sel && focused {
                "▶ "
            } else if is_sel {
                "● "
            } else {
                "  "
            };
            let label = match d.strip_prefix(crate::audio::LOOPBACK_PREFIX) {
                Some(name) => format!("[loopback] {}", name),
                None => d.clone(),
            };
            let style = if is_sel && focused {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else if is_sel {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            ListItem::new(Span::styled(format!("{}{}", marker, label), style))
        })
        .collect();

    let border_style = if focused {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(format!(" {} ", title)),
    );
    f.render_widget(list, area);
}

fn centered_rect(pct_x: u16, pct_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[allow(clippy::too_many_arguments)]
fn render_transcript_column(
    f: &mut Frame,
    area: Rect,
    lines: &[TranscriptLine],
    col_lang: &str,
    title: &str,
    scroll_up: u16,
    scroll_max: &Cell<u16>,
    cache: &mut ColumnCache,
) {
    let block = Block::default().borders(Borders::ALL).title(title.to_string());
    let inner = block.inner(area);
    cache.sync(lines, col_lang, inner.width);

    let total: u32 = cache.rows.iter().map(|&r| r as u32).sum();
    let height = inner.height as u32;
    let tail = total.saturating_sub(height);
    let col_max = tail.min(u16::MAX as u32) as u16;
    if col_max > scroll_max.get() {
        scroll_max.set(col_max);
    }
    let top = tail.saturating_sub(scroll_up.min(col_max) as u32);

    // First visible line and the row offset inside it.
    let mut cum = 0u32;
    let mut start = lines.len();
    let mut offset = 0u16;
    for (i, &r) in cache.rows.iter().enumerate() {
        let next = cum + r as u32;
        if next > top {
            start = i;
            offset = (top - cum) as u16;
            break;
        }
        cum = next;
    }

    // Only the lines that can appear in the viewport are wrapped and drawn.
    let mut wrapped: Vec<Line> = Vec::new();
    let mut covered = 0u32;
    let mut end = start;
    while end < lines.len() && covered < height + offset as u32 {
        wrapped.push(render_line(&lines[end], col_lang));
        covered += cache.rows[end] as u32;
        end += 1;
    }

    let para = Paragraph::new(wrapped)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0))
        .block(block);
    f.render_widget(para, area);
}

fn render_line<'a>(line: &'a TranscriptLine, col_lang: &str) -> Line<'a> {
    let ts = line.started_at.format("%H:%M:%S").to_string();
    let is_source = col_lang == SOURCE_COL || line.src_lang == col_lang;
    let (text, style) = if is_source {
        (line.text.as_str(), Style::default())
    } else {
        match line.translated.as_deref() {
            Some(t) => (t, Style::default().fg(Color::Gray)),
            None => ("…", Style::default().fg(Color::DarkGray)),
        }
    };
    let marker = if is_source { "▶ " } else { "  " };
    let head = format!("[{}] {}", ts, marker);

    Line::from(vec![
        Span::styled(head, Style::default().fg(Color::Cyan)),
        Span::styled(text.to_string(), style),
    ])
}
