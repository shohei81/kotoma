use crate::app::DevicePicker;
use crate::msg::{DraftState, TranslatorStatus};
use crate::transcribe::TranscriptLine;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap};
use std::cell::Cell;

pub struct UiState<'a> {
    pub lines: &'a [TranscriptLine],
    pub level: f32,
    pub language: &'a str,
    pub recording: bool,
    pub input_name: &'a str,
    pub model_name: &'a str,
    pub saved_note: Option<&'a str>,
    pub translator_status: TranslatorStatus,
    pub picker: Option<&'a DevicePicker>,
    pub draft: DraftState,
    /// Rows scrolled up from the tail. 0 = follow latest.
    pub scroll_up: u16,
    /// Out-param: set during draw to the largest usable `scroll_up` across
    /// both transcript columns, so the caller can clamp user input.
    pub scroll_max: &'a Cell<u16>,
}

pub fn draw(f: &mut Frame, state: &UiState) {
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

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[2]);

    state.scroll_max.set(0);
    render_transcript_column(
        f,
        cols[0],
        state.lines,
        "en",
        " English ",
        state.scroll_up,
        state.scroll_max,
    );
    render_transcript_column(
        f,
        cols[1],
        state.lines,
        "ja",
        " 日本語 ",
        state.scroll_up,
        state.scroll_max,
    );

    let nav = if state.scroll_up == 0 {
        " ↑/PgUp scroll ".to_string()
    } else {
        format!(" ↑↓/PgUp/PgDn · End follow (−{}) ", state.scroll_up)
    };
    let help = match state.saved_note {
        Some(note) => format!(
            " {} · q quit&save · s save · l lang · d device · m sys-audio · space pause ·{}",
            note, nav
        ),
        None => format!(
            " q quit&save · s save · l lang · d device · m sys-audio · space pause ·{}",
            nav
        ),
    };
    let help_p = Paragraph::new(help).style(Style::default().fg(Color::DarkGray));
    f.render_widget(help_p, chunks[3]);

    if let Some(pk) = state.picker {
        render_device_picker(f, pk);
    }
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

fn render_transcript_column(
    f: &mut Frame,
    area: Rect,
    lines: &[TranscriptLine],
    col_lang: &str,
    title: &str,
    scroll_up: u16,
    scroll_max: &Cell<u16>,
) {
    let block = Block::default().borders(Borders::ALL).title(title.to_string());
    let inner = block.inner(area);
    let wrapped: Vec<Line> = lines.iter().map(|l| render_line(l, col_lang)).collect();

    let para = Paragraph::new(wrapped).wrap(Wrap { trim: false });
    let total = para.line_count(inner.width) as u16;
    let tail = total.saturating_sub(inner.height);
    let col_max = tail;
    if col_max > scroll_max.get() {
        scroll_max.set(col_max);
    }
    let scroll = tail.saturating_sub(scroll_up.min(col_max));
    let para = para.scroll((scroll, 0)).block(block);
    f.render_widget(para, area);
}

fn render_line<'a>(line: &'a TranscriptLine, col_lang: &str) -> Line<'a> {
    let ts = line.started_at.format("%H:%M:%S").to_string();
    let is_source = line.src_lang == col_lang;
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
