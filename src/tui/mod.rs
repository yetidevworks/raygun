use std::{
    collections::HashSet,
    io::{self, Stdout},
    net::SocketAddr,
    time::{Duration, Instant},
};

use crate::ui::detail::{self, DetailSegment, DetailViewModel, SegmentStyle};
use color_eyre::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyEvent,
        MouseEvent,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap},
};
use tokio::{sync::mpsc, task};
use tracing::{debug, error};
use uuid::Uuid;

static RAYGUN_BANNER: &[&str] = &[
    "██████╗  █████╗ ██╗   ██╗ ██████╗ ██╗   ██╗███╗   ██╗",
    "██╔══██╗██╔══██╗╚██╗ ██╔╝██╔════╝ ██║   ██║████╗  ██║",
    "██████╔╝███████║ ╚████╔╝ ██║  ███╗██║   ██║██╔██╗ ██║",
    "██╔══██╗██╔══██║  ╚██╔╝  ██║   ██║██║   ██║██║╚██╗██║",
    "██║  ██║██║  ██║   ██║   ╚██████╔╝╚██████╔╝██║ ╚████║",
    "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝    ╚═════╝  ╚═════╝ ╚═╝  ╚═══╝",
];

#[derive(Debug)]
pub enum Event {
    Input(KeyEvent),
    Tick,
    Resize(u16, u16),
    Mouse(MouseEvent),
}

#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub id: Uuid,
    pub kind: String,
    pub summary: String,
    pub age: String,
    pub color: Option<String>,
    pub label: Option<String>,
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
    pub layout: LayoutConfig,
    pub detail_state: Option<DetailStateView>,
    pub active_color_filter: Option<String>,
    pub available_colors: Vec<String>,
    pub show_help: bool,
    pub debug_json: Option<String>,
    pub debug_scroll: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutConfig {
    pub timeline_percent: u16,
    pub detail_percent: u16,
}

#[derive(Debug, Clone)]
pub struct DetailStateView {
    pub cursor: usize,
    pub collapsed: HashSet<usize>,
}

pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.hide_cursor()?;

        Ok(Self { terminal })
    }

    pub fn draw<F, R>(&mut self, f: F) -> Result<R>
    where
        F: FnOnce(&mut Frame<'_>) -> R,
    {
        let mut result = None;
        self.terminal.draw(|frame| {
            result = Some(f(frame));
        })?;
        Ok(result.expect("terminal draw closure did not produce a result"))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Err(err) = disable_raw_mode() {
            error!(?err, "failed to disable raw mode");
        }

        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen) {
            error!(?err, "failed to leave alternate screen");
        }

        if let Err(err) = self.terminal.show_cursor() {
            error!(?err, "failed to show cursor");
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AppRenderMetadata {
    pub timeline_inner: Rect,
    pub detail_inner: Rect,
    pub overlay: Option<OverlayArea>,
}

#[derive(Debug, Clone, Copy)]
pub enum OverlayArea {
    Help(Rect),
    Debug(Rect),
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
                    Ok(CrosstermEvent::Mouse(mouse)) => {
                        if tx.send(Event::Mouse(mouse)).is_err() {
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

pub fn render_app(frame: &mut Frame<'_>, view_model: &AppViewModel) -> AppRenderMetadata {
    let frame_rect = frame.size();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Percentage(view_model.layout.timeline_percent),
            Constraint::Percentage(view_model.layout.detail_percent),
            Constraint::Length(2),
        ])
        .split(frame_rect);

    render_header(frame, layout[0], view_model);
    render_timeline(frame, layout[1], view_model);
    render_detail(frame, layout[2], view_model);
    render_footer(frame, layout[3]);

    let mut overlay = None;
    if view_model.show_help {
        let area = centered_rect(80, 70, frame_rect);
        render_help_overlay(frame, view_model, area);
        overlay = Some(OverlayArea::Help(area));
    } else if let Some(json) = view_model.debug_json.as_deref() {
        let area = centered_rect(90, 80, frame_rect);
        render_debug_overlay(frame, json, view_model.debug_scroll, area);
        overlay = Some(OverlayArea::Debug(area));
    }

    AppRenderMetadata {
        timeline_inner: inner(layout[1]),
        detail_inner: inner(layout[2]),
        overlay,
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, view_model: &AppViewModel) {
    let mut title = format!(
        "Raygun — waiting for payloads ({} total) @ {}",
        view_model.total_events, view_model.bind_addr
    );

    if let Some(color) = &view_model.active_color_filter {
        title.push_str(&format!(" | color filter: {}", color));
    }

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .title(title)
        .style(Style::default().fg(Color::Cyan));

    frame.render_widget(block, area);
}

fn render_timeline(frame: &mut Frame<'_>, area: Rect, view_model: &AppViewModel) {
    let mut title = "Timeline".to_string();
    if let Some(filter) = &view_model.active_color_filter {
        title = format!("Timeline (color = {})", filter);
    }

    let block = Block::default()
        .title(title)
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

    let inner_area = inner(area);
    if inner_area.height == 0 {
        return;
    }

    if view_model.timeline.is_empty() {
        EmptyTimelineMessage::new(view_model).render(frame, inner_area);
        return;
    }

    let view_height = inner_area.height as usize;
    let selected = view_model.selected.unwrap_or(0);
    let total = view_model.timeline.len();
    let max_start = total.saturating_sub(view_height);
    let start = selected
        .saturating_sub(view_height.saturating_sub(1))
        .min(max_start);

    let mut items = Vec::new();
    for idx in start..(start + view_height).min(total) {
        if let Some(entry) = view_model.timeline.get(idx) {
            let is_selected = Some(idx) == view_model.selected;
            let highlight_style = if is_selected {
                Some(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                None
            };

            let bullet_color = entry
                .color
                .as_deref()
                .and_then(color_from_name)
                .unwrap_or(Color::DarkGray);

            let mut bullet_style = Style::default()
                .fg(bullet_color)
                .add_modifier(Modifier::BOLD);
            let mut text_style = Style::default().fg(Color::Gray);
            if let Some(style) = highlight_style {
                bullet_style = bullet_style.patch(style);
                text_style = text_style.patch(style);
            }

            let mut spans = Vec::new();
            spans.push(Span::styled("⬤", bullet_style));
            spans.push(Span::raw(" "));

            let mut bracket_style = text_style;
            let mut kind_style = Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD);
            if let Some(style) = highlight_style {
                bracket_style = bracket_style.patch(style);
                kind_style = kind_style.patch(style);
            }

            spans.push(Span::styled("[", bracket_style));
            spans.push(Span::styled(entry.kind.clone(), kind_style));
            spans.push(Span::styled("] ", bracket_style));

            spans.push(Span::styled(entry.summary.clone(), text_style));

            let mut separator_style = text_style;
            if let Some(style) = highlight_style {
                separator_style = separator_style.patch(style);
            }
            spans.push(Span::styled(" · ", separator_style));

            let mut age_style = Style::default().fg(Color::DarkGray);
            if let Some(style) = highlight_style {
                age_style = age_style.patch(style);
            }
            spans.push(Span::styled(entry.age.clone(), age_style));

            if let Some(label) = entry.label.as_deref() {
                let mut label_style = Style::default().fg(Color::DarkGray);
                if let Some(style) = highlight_style {
                    label_style = label_style.patch(style);
                }
                spans.push(Span::raw(" "));
                spans.push(Span::styled(format!("({})", label), label_style));
            }

            items.push(ListItem::new(Line::from(spans)));
        }
    }

    let list = List::new(items).block(Block::default());
    frame.render_widget(list, inner_area);
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
        let state_view = view_model.detail_state.as_ref();
        let (visible_indices, has_children) =
            detail::visible_indices_with_children(detail, state_view.map(|state| &state.collapsed));

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

        let highlight_target = state_view
            .filter(|_| view_model.focus_detail)
            .map(|state| state.cursor.min(visible_indices.len().saturating_sub(1)));

        for (position, &line_index) in visible_indices.iter().enumerate() {
            let detail_line = &detail.lines[line_index];
            let mut spans = Vec::new();

            let is_selected = highlight_target == Some(position);

            let highlight_style = if is_selected {
                Some(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                None
            };

            let collapsed_here = state_view
                .map(|state| state.collapsed.contains(&line_index))
                .unwrap_or(false);

            let icon = if has_children[line_index] {
                if collapsed_here { "+ " } else { "- " }
            } else {
                "  "
            };

            let mut indent_style = Style::default().fg(Color::DarkGray);
            if let Some(style) = highlight_style {
                indent_style = indent_style.patch(style);
            }

            if detail_line.indent > 0 {
                spans.push(Span::styled("  ".repeat(detail_line.indent), indent_style));
            }

            spans.push(Span::styled(icon.to_string(), indent_style));

            for segment in &detail_line.segments {
                let mut style = style_for_segment(segment);
                if let Some(highlight) = highlight_style {
                    style = style.patch(highlight);
                }
                spans.push(Span::styled(segment.text.clone(), style));
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

struct EmptyTimelineMessage<'a> {
    view_model: &'a AppViewModel,
}

impl<'a> EmptyTimelineMessage<'a> {
    fn new(view_model: &'a AppViewModel) -> Self {
        Self { view_model }
    }

    fn render(self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines: Vec<Line> = std::iter::once(Line::default())
            .chain(RAYGUN_BANNER.iter().map(|line| {
                Line::styled(
                    *line,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            }))
            .collect();

        lines.push(Line::default());

        if let Some(filter) = &self.view_model.active_color_filter {
            lines.push(Line::from(vec![Span::styled(
                format!("No payloads match color filter `{}`.", filter),
                Style::default().fg(Color::Gray),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "Press `f` to clear the filter or send a payload.",
                Style::default().fg(Color::DarkGray),
            )]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                format!("Listening on {}", self.view_model.bind_addr),
                Style::default().fg(Color::Gray),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "Use the `ray()` helper to send data here.",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "Press `q` to exit.",
                Style::default().fg(Color::DarkGray),
            )]));
        }

        let paragraph = Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, area);
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .title("Keymap")
        .style(Style::default().fg(Color::DarkGray));

    let content = Paragraph::new("? help · f cycle color · ctrl+l cycle layout · ctrl+k clear timeline · ctrl+d raw payload · Tab focus detail · ↑/↓ navigate · PgUp/PgDn jump · Enter/→ expand · ← collapse · Space toggle · q quit · ctrl+c force quit")
    .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(block, area);

    if area.height > 1 {
        let content_area = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(1),
        };
        frame.render_widget(content, content_area);
    }
}

fn inner(area: Rect) -> Rect {
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

fn render_help_overlay(frame: &mut Frame<'_>, view_model: &AppViewModel, area: Rect) {
    frame.render_widget(Clear, area);

    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        "Keymap & Controls",
        Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "Navigation: ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("↑/↓, j/k move · PgUp/PgDn jump · Home/End to bounds · Tab switches focus"),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Details: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("Enter/→ expand · ← collapse · Space toggle · Ctrl+L cycle layout"),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Global: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(
            "f cycle color filter · ctrl+k clear timeline · ctrl+d raw payload · Esc closes overlays · ? close help · q quit · Ctrl+C force quit",
        ),
    ]));

    if !view_model.available_colors.is_empty() {
        lines.push(Line::raw(""));
        let mut spans = Vec::new();
        spans.push(Span::styled(
            "Available colors: ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        for color in &view_model.available_colors {
            let block_style = color_from_name(color)
                .map(|color| Style::default().bg(color).fg(Color::Black))
                .unwrap_or_else(|| Style::default().bg(Color::DarkGray).fg(Color::Black));
            spans.push(Span::styled("  ", block_style));
            spans.push(Span::raw(format!(" {}  ", color)));
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(
        "Tips: use `f` repeatedly to cycle colors; when no color matches the filter, the timeline shows a hint.",
    ));

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("About: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("Raygun CLI · https://github.com/yetidevworks/raygun · MIT License"),
    ]));

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Help")
            .padding(Padding::uniform(1))
            .border_style(Style::default().fg(Color::Cyan)),
    );

    frame.render_widget(paragraph, area);
}

fn render_debug_overlay(frame: &mut Frame<'_>, json: &str, scroll: usize, area: Rect) {
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Raw Payload (Ctrl+D or Esc to close)")
        .padding(Padding::uniform(1))
        .border_style(Style::default().fg(Color::Magenta));

    let paragraph = Paragraph::new(json.to_string())
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::Gray))
        .scroll((scroll.min(u16::MAX as usize) as u16, 0))
        .block(block);

    frame.render_widget(paragraph, area);
}

fn color_from_name(name: &str) -> Option<Color> {
    let normalized = name.trim().to_lowercase();
    match normalized.as_str() {
        "red" => Some(Color::Rgb(255, 82, 82)),
        "green" => Some(Color::Rgb(48, 209, 88)),
        "blue" => Some(Color::Rgb(64, 156, 255)),
        "yellow" => Some(Color::Rgb(255, 214, 10)),
        "orange" => Some(Color::Rgb(255, 159, 10)),
        "purple" | "magenta" => Some(Color::Rgb(191, 90, 242)),
        "pink" => Some(Color::Rgb(255, 55, 95)),
        "gray" | "grey" => Some(Color::Rgb(138, 141, 165)),
        "white" => Some(Color::White),
        "black" => Some(Color::Black),
        "cyan" => Some(Color::Rgb(100, 210, 255)),
        "teal" => Some(Color::Rgb(64, 200, 224)),
        "lightblue" => Some(Color::Rgb(173, 216, 230)),
        "lightgreen" => Some(Color::Rgb(144, 238, 144)),
        "brown" => Some(Color::Rgb(141, 110, 99)),
        _ => {
            let hex = normalized.strip_prefix('#').unwrap_or(&normalized);
            if hex.len() == 6 && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
                if let (Ok(r), Ok(g), Ok(b)) = (
                    u8::from_str_radix(&hex[0..2], 16),
                    u8::from_str_radix(&hex[2..4], 16),
                    u8::from_str_radix(&hex[4..6], 16),
                ) {
                    return Some(Color::Rgb(r, g, b));
                }
            }
            None
        }
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
