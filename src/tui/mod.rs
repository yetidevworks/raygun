use std::{
    io::{self, Stdout},
    net::SocketAddr,
    time::{Duration, Instant},
};

use crate::ui::detail::{DetailSegment, DetailViewModel, SegmentStyle};
use color_eyre::Result;
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyEvent},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use tokio::{sync::mpsc, task};
use tracing::{debug, error};

#[derive(Debug)]
pub enum Event {
    Input(KeyEvent),
    Tick,
    Resize(u16, u16),
}

#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub kind: String,
    pub summary: String,
    pub age: String,
}

#[derive(Debug, Clone)]
pub struct AppViewModel {
    pub total_events: usize,
    pub bind_addr: SocketAddr,
    pub timeline: Vec<TimelineEntry>,
    pub selected: Option<usize>,
    pub detail: Option<DetailViewModel>,
    pub focus_detail: bool,
    pub detail_scroll: usize,
}

pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.hide_cursor()?;

        Ok(Self { terminal })
    }

    pub fn draw<F>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.terminal.draw(f)?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Err(err) = disable_raw_mode() {
            error!(?err, "failed to disable raw mode");
        }

        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, LeaveAlternateScreen) {
            error!(?err, "failed to leave alternate screen");
        }

        if let Err(err) = self.terminal.show_cursor() {
            error!(?err, "failed to show cursor");
        }
    }
}

pub fn spawn_event_loop(
    tx: mpsc::UnboundedSender<Event>,
    tick_rate: Duration,
) -> task::JoinHandle<()> {
    task::spawn_blocking(move || {
        let mut last_tick = Instant::now();

        loop {
            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            match event::poll(timeout) {
                Ok(true) => match event::read() {
                    Ok(CrosstermEvent::Key(key)) => {
                        if tx.send(Event::Input(key)).is_err() {
                            break;
                        }
                    }
                    Ok(CrosstermEvent::Resize(w, h)) => {
                        if tx.send(Event::Resize(w, h)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        error!(?err, "failed to read terminal event");
                        break;
                    }
                },
                Ok(false) => {}
                Err(err) => {
                    error!(?err, "failed to poll terminal events");
                    break;
                }
            }

            if last_tick.elapsed() >= tick_rate {
                if tx.send(Event::Tick).is_err() {
                    break;
                }
                last_tick = Instant::now();
            }
        }

        debug!("terminal event loop terminated");
    })
}

pub fn render_app(frame: &mut Frame<'_>, view_model: &AppViewModel) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Percentage(50),
            Constraint::Percentage(50),
            Constraint::Length(1),
        ])
        .split(frame.size());

    render_header(frame, layout[0], view_model);
    render_timeline(frame, layout[1], view_model);
    render_detail(frame, layout[2], view_model);
    render_footer(frame, layout[3]);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, view_model: &AppViewModel) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .title(format!(
            "Raygun — waiting for payloads ({} total) @ {}",
            view_model.total_events, view_model.bind_addr
        ))
        .style(Style::default().fg(Color::Cyan));

    frame.render_widget(block, area);
}

fn render_timeline(frame: &mut Frame<'_>, area: Rect, view_model: &AppViewModel) {
    let block = Block::default()
        .title("Timeline")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if view_model.focus_detail {
            Color::DarkGray
        } else {
            Color::Cyan
        }))
        .title_style(
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(block, area);

    if view_model.timeline.is_empty() {
        let content = Paragraph::new(format!(
            "Waiting for Ray payloads…\n\nUse the PHP `ray()` helper to send data here.\nListening on {}.\nPress `q` to exit.",
            view_model.bind_addr
        ))
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::Gray));

        frame.render_widget(content, inner(area));
        return;
    }

    let items: Vec<ListItem> = view_model
        .timeline
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let text = format!("[{}] {} · {}", entry.kind, entry.summary, entry.age);
            let base = Style::default().fg(Color::Gray);
            let style = if Some(idx) == view_model.selected {
                base.add_modifier(Modifier::BOLD).bg(Color::DarkGray)
            } else {
                base
            };
            ListItem::new(text).style(style)
        })
        .collect();

    let list = List::new(items).block(Block::default());
    frame.render_widget(list, inner(area));
}

fn render_detail(frame: &mut Frame<'_>, area: Rect, view_model: &AppViewModel) {
    let block = Block::default()
        .title("Details")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if view_model.focus_detail {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title_style(
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(block, area);

    let inner_area = inner(area);

    if let Some(detail) = &view_model.detail {
        let mut lines: Vec<Line> = Vec::new();

        if !detail.header.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                detail.header.clone(),
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::default());
        }

        for detail_line in &detail.lines {
            let mut spans = Vec::new();
            if detail_line.indent > 0 {
                spans.push(Span::raw("  ".repeat(detail_line.indent)));
            }
            for segment in &detail_line.segments {
                spans.push(Span::styled(
                    segment.text.clone(),
                    style_for_segment(segment),
                ));
            }
            lines.push(Line::from(spans));
        }

        if !detail.footer.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(vec![Span::styled(
                detail.footer.clone(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )]));
        }

        let scroll = view_model.detail_scroll.min(u16::MAX as usize) as u16;
        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        frame.render_widget(paragraph, inner_area);
    } else {
        let paragraph =
            Paragraph::new("No event selected").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(paragraph, inner_area);
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .title("Keymap")
        .style(Style::default().fg(Color::DarkGray));

    let content = Paragraph::new(
        "q/esc quit · ctrl+c quit · Tab focus detail · ↑/↓ navigate · PgUp/PgDn jump · coming soon: expand/collapse",
    )
    .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(block, area);
    frame.render_widget(content, inner(area));
}

fn inner(area: Rect) -> Rect {
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn style_for_segment(segment: &DetailSegment) -> Style {
    match segment.style {
        SegmentStyle::Plain => Style::default().fg(Color::Gray),
        SegmentStyle::Key => Style::default().fg(Color::Cyan),
        SegmentStyle::Type => Style::default().fg(Color::Yellow),
        SegmentStyle::String => Style::default().fg(Color::Green),
        SegmentStyle::Number => Style::default().fg(Color::LightMagenta),
        SegmentStyle::Boolean => Style::default().fg(Color::LightBlue),
        SegmentStyle::Null => Style::default().fg(Color::DarkGray),
    }
}
