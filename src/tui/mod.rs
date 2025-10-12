use std::{
    io::{self, Stdout},
    net::SocketAddr,
    time::{Duration, Instant},
};

use crate::ui::detail::DetailViewModel;
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
        .border_style(Style::default().fg(Color::DarkGray))
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
        .border_style(Style::default().fg(Color::DarkGray))
        .title_style(
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(block, area);

    let inner_area = inner(area);

    if let Some(detail) = &view_model.detail {
        let mut content = String::new();

        if !detail.header.is_empty() {
            content.push_str(&detail.header);
            content.push_str("\n\n");
        }

        for line in &detail.body {
            content.push_str(line);
            content.push('\n');
        }

        if !detail.footer.is_empty() {
            content.push_str("\n");
            content.push_str(&detail.footer);
        }

        let paragraph = Paragraph::new(content).wrap(Wrap { trim: false });
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
        "q/esc quit · ctrl+c quit · ↑/↓ navigate · PgUp/PgDn jump · coming soon: filter, palette, search",
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
