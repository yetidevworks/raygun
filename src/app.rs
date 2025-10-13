use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io::ErrorKind,
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use color_eyre::{
    Result,
    eyre::{Report, eyre},
};
use crossterm::event::{KeyCode, KeyModifiers};
use html_escape::decode_html_entities;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Number, Value};
use tokio::{select, sync::mpsc};
use tracing::{debug, info, warn};

use crate::{
    config::Config,
    protocol::{Origin, Payload, PayloadKind},
    server,
    state::{AppState, PayloadLogger, TimelineEvent},
    tui::{self, AppViewModel, DetailStateView, Event, LayoutConfig, TerminalGuard, TimelineEntry},
    ui::detail::{self, build_detail_view},
};
use uuid::Uuid;

pub struct RaygunApp {
    tick_rate: Duration,
    state: Arc<AppState>,
    server: Option<server::ServerHandle>,
    server_addr: SocketAddr,
    selected: Option<usize>,
    focus: Focus,
    detail_scroll: usize,
    layout: LayoutPreset,
    detail_states: HashMap<Uuid, DetailState>,
    visible_events: Vec<Uuid>,
    color_filter: Option<String>,
    available_colors: Vec<String>,
    show_help: bool,
    show_debug: bool,
    debug_scroll: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Timeline,
    Detail,
}

const TIMELINE_VIEW_LIMIT: usize = 200;

impl RaygunApp {
    pub async fn bootstrap(config: Config) -> Result<Self> {
        let payload_logger = config
            .debug_dump
            .as_ref()
            .map(|path| PayloadLogger::new(path.clone()));
        let state = Arc::new(AppState::with_logger(payload_logger));
        let bind_addr = config.bind_addr;
        let server = server::spawn(Arc::clone(&state), server::ServerConfig { bind_addr })
            .await
            .map_err(|err| match err {
                server::ServerError::Io(io_err) if io_err.kind() == ErrorKind::AddrInUse => eyre!("Port {} is already in use. Pass --bind <addr:port> to choose a different address.", bind_addr),
                other => Report::from(other),
            })?;
        let server_addr = server.addr();

        info!(addr = %server_addr, "HTTP server ready");

        Ok(Self {
            tick_rate: Duration::from_millis(250),
            state,
            server: Some(server),
            server_addr,
            selected: None,
            focus: Focus::Timeline,
            detail_scroll: 0,
            layout: LayoutPreset::DetailFocus,
            detail_states: HashMap::new(),
            visible_events: Vec::new(),
            color_filter: None,
            available_colors: Vec::new(),
            show_help: false,
            show_debug: false,
            debug_scroll: 0,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        info!("starting Raygun placeholder UI");

        let mut terminal = TerminalGuard::new()?;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let event_handle = tui::spawn_event_loop(tx, self.tick_rate);

        loop {
            let view_model = self.build_view_model().await;
            let timeline_len = view_model.timeline.len();

            let detail_context = DetailContext::new(
                view_model.detail.as_ref(),
                view_model
                    .detail_state
                    .as_ref()
                    .map(|state| &state.collapsed),
            );

            terminal.draw(|frame| tui::render_app(frame, &view_model))?;

            let exit_requested = select! {
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => self.handle_event(event, timeline_len, &detail_context),
                        None => true,
                    }
                }
                ctrl_c = tokio::signal::ctrl_c() => {
                    if let Err(err) = ctrl_c {
                        warn!(?err, "failed to listen for ctrl+c");
                    } else {
                        info!("received ctrl+c");
                    }
                    true
                }
            };

            if exit_requested {
                break;
            }
        }

        drop(terminal);
        drop(rx);

        if let Err(err) = event_handle.await {
            warn!(?err, "terminal event loop task ended unexpectedly");
        }

        if let Some(server) = self.server.take() {
            server.shutdown().await?;
        }

        info!("Raygun shutting down");
        Ok(())
    }

    async fn build_view_model(&mut self) -> AppViewModel {
        let events = self.state.timeline_snapshot().await;
        let mut ordered_events: Vec<_> = events.into_iter().rev().collect();
        if ordered_events.len() > TIMELINE_VIEW_LIMIT {
            ordered_events.truncate(TIMELINE_VIEW_LIMIT);
        }

        let mut available_colors = BTreeSet::new();
        for event in &ordered_events {
            if let Some(color) = &event.color {
                available_colors.insert(color.clone());
            }
        }
        self.available_colors = available_colors.into_iter().collect();

        if let Some(filter) = &self.color_filter {
            if !self.available_colors.iter().any(|value| value == filter) {
                self.color_filter = None;
            }
        }

        if let Some(filter) = &self.color_filter {
            ordered_events.retain(|event| event.color.as_deref() == Some(filter.as_str()));
        }

        if ordered_events.is_empty() {
            self.show_debug = false;
            self.debug_scroll = 0;
        }

        let previous_selection = self.selected;

        if ordered_events.is_empty() {
            self.selected = None;
            self.detail_scroll = 0;
        } else {
            let max_index = ordered_events.len().saturating_sub(1);
            let clamped = self.selected.unwrap_or(0).min(max_index);
            self.selected = Some(clamped);
        }

        if self.selected != previous_selection {
            self.detail_scroll = 0;
        }

        let timeline = ordered_events
            .iter()
            .map(|event| summarize_event(event))
            .collect::<Vec<_>>();

        self.visible_events = timeline.iter().map(|entry| entry.id).collect();

        let detail = self
            .selected
            .and_then(|index| ordered_events.get(index))
            .map(build_detail_view_for_event);

        let debug_json = if self.show_debug {
            self.selected
                .and_then(|index| ordered_events.get(index))
                .map(|event| format!("{:#?}", event))
        } else {
            None
        };

        let mut detail_state_view = None;

        if let Some(event_id) = self.current_event_id() {
            let entry = self.detail_states.entry(event_id).or_default();
            if let Some(detail) = &detail {
                let (visible_indices, _) =
                    detail::visible_indices_with_children(detail, Some(&entry.collapsed));
                let visible_len = visible_indices.len();

                if visible_len == 0 {
                    entry.scroll = 0;
                    entry.cursor = 0;
                } else {
                    let max = visible_len.saturating_sub(1);
                    entry.scroll = entry.scroll.min(max);
                    entry.cursor = entry.cursor.min(max);
                }

                self.detail_scroll = entry.scroll;
            } else {
                entry.scroll = 0;
                entry.cursor = 0;
                self.detail_scroll = 0;
            }

            detail_state_view = Some(DetailStateView {
                cursor: entry.cursor,
                collapsed: entry.collapsed.clone(),
            });
        } else {
            self.detail_scroll = 0;
        }

        AppViewModel {
            total_events: self.state.timeline_len().await,
            bind_addr: self.server_addr,
            timeline,
            selected: self.selected,
            detail,
            focus_detail: matches!(self.focus, Focus::Detail),
            detail_scroll: self.detail_scroll,
            layout: self.layout.config(),
            detail_state: detail_state_view,
            active_color_filter: self.color_filter.clone(),
            available_colors: self.available_colors.clone(),
            show_help: self.show_help,
            debug_json,
            debug_scroll: self.debug_scroll,
        }
    }

    fn handle_event(
        &mut self,
        event: Event,
        timeline_len: usize,
        detail_ctx: &DetailContext,
    ) -> bool {
        match event {
            Event::Input(key) => {
                if self.show_help {
                    return match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
                        KeyCode::Char('q')
                        | KeyCode::Char('Q')
                        | KeyCode::Enter
                        | KeyCode::Char('?') => {
                            self.show_help = false;
                            false
                        }
                        _ => false,
                    };
                }

                if self.show_debug {
                    return match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.show_debug = false;
                            self.debug_scroll = 0;
                            false
                        }
                        KeyCode::Enter => {
                            self.show_debug = false;
                            self.debug_scroll = 0;
                            false
                        }
                        KeyCode::Up => {
                            self.debug_scroll = self.debug_scroll.saturating_sub(1);
                            false
                        }
                        KeyCode::Down => {
                            self.debug_scroll = self.debug_scroll.saturating_add(1);
                            false
                        }
                        KeyCode::PageUp => {
                            self.debug_scroll = self.debug_scroll.saturating_sub(10);
                            false
                        }
                        KeyCode::PageDown => {
                            self.debug_scroll = self.debug_scroll.saturating_add(10);
                            false
                        }
                        KeyCode::Home => {
                            self.debug_scroll = 0;
                            false
                        }
                        _ => false,
                    };
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => true,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
                    KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.clear_local_timeline();
                        false
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if self.show_debug {
                            self.show_debug = false;
                            self.debug_scroll = 0;
                        } else {
                            self.show_debug = true;
                            self.debug_scroll = 0;
                        }
                        false
                    }
                    KeyCode::Char('f') | KeyCode::Char('F') => {
                        if !key.modifiers.contains(KeyModifiers::CONTROL) {
                            self.store_detail_state(detail_ctx.visible_len());
                            self.cycle_color_filter();
                        }
                        false
                    }
                    KeyCode::Char('?') => {
                        self.show_help = true;
                        false
                    }
                    KeyCode::Tab => {
                        self.focus = match self.focus {
                            Focus::Timeline => Focus::Detail,
                            Focus::Detail => Focus::Timeline,
                        };
                        if let Some(state) = self.current_detail_state() {
                            self.detail_scroll =
                                state.scroll.min(detail_ctx.visible_len().saturating_sub(1));
                        } else {
                            self.detail_scroll = 0;
                        }
                        false
                    }
                    KeyCode::BackTab => {
                        self.focus = Focus::Timeline;
                        false
                    }
                    KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.layout = self.layout.next();
                        false
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if self.focus == Focus::Timeline {
                            self.store_detail_state(detail_ctx.visible_len());
                            if self.move_selection(1, timeline_len).is_some() {
                                if let Some(state) = self.current_detail_state() {
                                    self.detail_scroll = state.scroll;
                                } else {
                                    self.detail_scroll = 0;
                                }
                            }
                        } else {
                            self.advance_detail_cursor(1, detail_ctx);
                        }
                        false
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if self.focus == Focus::Timeline {
                            self.store_detail_state(detail_ctx.visible_len());
                            if self.move_selection(-1, timeline_len).is_some() {
                                if let Some(state) = self.current_detail_state() {
                                    self.detail_scroll = state.scroll;
                                } else {
                                    self.detail_scroll = 0;
                                }
                            }
                        } else {
                            self.advance_detail_cursor(-1, detail_ctx);
                        }
                        false
                    }
                    KeyCode::PageDown => {
                        if self.focus == Focus::Timeline {
                            self.store_detail_state(detail_ctx.visible_len());
                            if self.move_selection(10, timeline_len).is_some() {
                                if let Some(state) = self.current_detail_state() {
                                    self.detail_scroll = state.scroll;
                                } else {
                                    self.detail_scroll = 0;
                                }
                            }
                        } else {
                            self.advance_detail_cursor(10, detail_ctx);
                        }
                        false
                    }
                    KeyCode::PageUp => {
                        if self.focus == Focus::Timeline {
                            self.store_detail_state(detail_ctx.visible_len());
                            if self.move_selection(-10, timeline_len).is_some() {
                                if let Some(state) = self.current_detail_state() {
                                    self.detail_scroll = state.scroll;
                                } else {
                                    self.detail_scroll = 0;
                                }
                            }
                        } else {
                            self.advance_detail_cursor(-10, detail_ctx);
                        }
                        false
                    }
                    KeyCode::Home => {
                        if timeline_len > 0 && self.focus == Focus::Timeline {
                            self.store_detail_state(detail_ctx.visible_len());
                            self.selected = Some(0);
                            if let Some(state) = self.current_detail_state() {
                                self.detail_scroll = state.scroll;
                            } else {
                                self.detail_scroll = 0;
                            }
                        } else if self.focus == Focus::Detail {
                            if let Some(state) = self.current_detail_state_mut() {
                                state.cursor = 0;
                                state.scroll = 0;
                                self.detail_scroll = 0;
                            }
                        }
                        false
                    }
                    KeyCode::End => {
                        if timeline_len > 0 && self.focus == Focus::Timeline {
                            self.store_detail_state(detail_ctx.visible_len());
                            self.selected = Some(timeline_len.saturating_sub(1));
                            if let Some(state) = self.current_detail_state() {
                                self.detail_scroll = state.scroll;
                            } else {
                                self.detail_scroll = 0;
                            }
                        } else if self.focus == Focus::Detail {
                            if detail_ctx.visible_len() > 0 {
                                if let Some(state) = self.current_detail_state_mut() {
                                    let max = detail_ctx.visible_len().saturating_sub(1);
                                    state.cursor = max;
                                    state.scroll = max;
                                    self.detail_scroll = max;
                                }
                            }
                        }
                        false
                    }
                    KeyCode::Right | KeyCode::Enter => {
                        if self.focus == Focus::Detail {
                            if self.expand_current_node(detail_ctx) {
                                self.store_detail_state(detail_ctx.visible_len());
                            }
                        }
                        false
                    }
                    KeyCode::Left => {
                        if self.focus == Focus::Detail {
                            if self.collapse_current_node(detail_ctx) {
                                self.store_detail_state(detail_ctx.visible_len());
                            }
                        }
                        false
                    }
                    KeyCode::Char(' ') => {
                        if self.focus == Focus::Detail {
                            if self.toggle_current_node(detail_ctx) {
                                self.store_detail_state(detail_ctx.visible_len());
                            }
                        }
                        false
                    }
                    _ => false,
                }
            }
            Event::Tick => false,
            Event::Resize(width, height) => {
                debug!(%width, %height, "terminal resized");
                false
            }
        }
    }

    fn move_selection(&mut self, delta: i32, len: usize) -> Option<usize> {
        if len == 0 {
            self.selected = None;
            return None;
        }

        let current = self.selected.unwrap_or(0) as i32;
        let new_index = (current + delta).clamp(0, len.saturating_sub(1) as i32) as usize;
        let changed = self.selected != Some(new_index);
        self.selected = Some(new_index);
        if changed { Some(new_index) } else { None }
    }

    fn cycle_color_filter(&mut self) {
        if self.available_colors.is_empty() {
            self.color_filter = None;
            return;
        }

        let next = match &self.color_filter {
            None => Some(self.available_colors[0].clone()),
            Some(current) => {
                if let Some(position) = self
                    .available_colors
                    .iter()
                    .position(|value| value == current)
                {
                    if position + 1 < self.available_colors.len() {
                        Some(self.available_colors[position + 1].clone())
                    } else {
                        None
                    }
                } else {
                    Some(self.available_colors[0].clone())
                }
            }
        };

        self.color_filter = next;
        self.selected = Some(0);
        self.detail_scroll = 0;
    }

    fn clear_local_timeline(&mut self) {
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            state.clear_timeline().await;
        });
        self.selected = None;
        self.detail_scroll = 0;
        self.detail_states.clear();
        self.visible_events.clear();
        self.available_colors.clear();
        self.color_filter = None;
        self.show_help = false;
        self.show_debug = false;
        self.debug_scroll = 0;
    }

    fn advance_detail_cursor(&mut self, delta: i32, ctx: &DetailContext) {
        if ctx.visible_len() == 0 {
            self.detail_scroll = 0;
            if let Some(state) = self.current_detail_state_mut() {
                state.cursor = 0;
                state.scroll = 0;
            }
            return;
        }

        if let Some(state) = self.current_detail_state_mut() {
            let max = ctx.visible_len().saturating_sub(1) as i32;
            let new_cursor = (state.cursor as i32 + delta).clamp(0, max) as usize;
            state.cursor = new_cursor;
            state.scroll = new_cursor;
            self.detail_scroll = new_cursor;
        }
    }

    fn expand_current_node(&mut self, ctx: &DetailContext) -> bool {
        if ctx.visible_len() == 0 {
            return false;
        }

        if let Some(state) = self.current_detail_state_mut() {
            let cursor = state.cursor.min(ctx.visible_len().saturating_sub(1));
            if let Some(&line_index) = ctx.visible_indices.get(cursor) {
                if ctx.has_children.get(line_index).copied().unwrap_or(false) {
                    if state.collapsed.remove(&line_index) {
                        state.scroll = state.cursor.min(ctx.visible_len().saturating_sub(1));
                        self.detail_scroll = state.scroll;
                        return true;
                    }
                }

                state.scroll = state.cursor.min(ctx.visible_len().saturating_sub(1));
                self.detail_scroll = state.scroll;
            }
        }

        false
    }

    fn toggle_current_node(&mut self, ctx: &DetailContext) -> bool {
        if ctx.visible_len() == 0 {
            return false;
        }

        if let Some(state) = self.current_detail_state_mut() {
            let cursor = state.cursor.min(ctx.visible_len().saturating_sub(1));
            if let Some(&line_index) = ctx.visible_indices.get(cursor) {
                if ctx.has_children.get(line_index).copied().unwrap_or(false) {
                    if !state.collapsed.remove(&line_index) {
                        state.collapsed.insert(line_index);
                    }
                    state.scroll = state.cursor.min(ctx.visible_len().saturating_sub(1));
                    self.detail_scroll = state.scroll;
                    return true;
                }
            }
        }
        false
    }

    fn collapse_current_node(&mut self, ctx: &DetailContext) -> bool {
        if ctx.visible_len() == 0 {
            return false;
        }

        if let Some(state) = self.current_detail_state_mut() {
            let cursor = state.cursor.min(ctx.visible_len().saturating_sub(1));
            if let Some(&line_index) = ctx.visible_indices.get(cursor) {
                let indent = ctx
                    .detail
                    .map(|detail| detail.lines[line_index].indent)
                    .unwrap_or(0);

                if ctx.has_children.get(line_index).copied().unwrap_or(false) {
                    if !state.collapsed.insert(line_index) {
                        if let Some((pos, _)) =
                            ctx.visible_indices[..cursor].iter().enumerate().rev().find(
                                |(_, idx)| {
                                    ctx.detail
                                        .map(|detail| detail.lines[**idx].indent < indent)
                                        .unwrap_or(false)
                                },
                            )
                        {
                            state.cursor = pos;
                            state.scroll = pos;
                            self.detail_scroll = pos;
                            return true;
                        }
                    } else {
                        state.scroll = state.cursor.min(ctx.visible_len().saturating_sub(1));
                        self.detail_scroll = state.scroll;
                        return true;
                    }
                } else if indent > 0 {
                    if let Some((pos, _)) = ctx.visible_indices[..cursor]
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, idx)| {
                            ctx.detail
                                .map(|detail| detail.lines[**idx].indent < indent)
                                .unwrap_or(false)
                        })
                    {
                        state.cursor = pos;
                        state.scroll = pos;
                        self.detail_scroll = pos;
                        return true;
                    }
                }

                state.scroll = state.cursor.min(ctx.visible_len().saturating_sub(1));
                self.detail_scroll = state.scroll;
                return true;
            }
        }

        false
    }

    fn store_detail_state(&mut self, detail_len: usize) {
        let scroll = self.detail_scroll;
        if let Some(state) = self.current_detail_state_mut() {
            if detail_len == 0 {
                state.scroll = 0;
                state.cursor = 0;
                self.detail_scroll = 0;
            } else {
                let max = detail_len.saturating_sub(1);
                state.scroll = scroll.min(max);
                state.cursor = state.cursor.min(max);
                self.detail_scroll = state.scroll;
            }
        }
    }

    fn current_event_id(&self) -> Option<Uuid> {
        self.selected
            .and_then(|index| self.visible_events.get(index))
            .copied()
    }

    fn current_detail_state(&self) -> Option<&DetailState> {
        self.current_event_id()
            .and_then(|id| self.detail_states.get(&id))
    }

    fn current_detail_state_mut(&mut self) -> Option<&mut DetailState> {
        let id = self.current_event_id()?;
        Some(self.detail_states.entry(id).or_default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutPreset {
    TimelineFocus,
    Balanced,
    DetailFocus,
}

impl LayoutPreset {
    fn next(self) -> Self {
        match self {
            LayoutPreset::TimelineFocus => LayoutPreset::Balanced,
            LayoutPreset::Balanced => LayoutPreset::DetailFocus,
            LayoutPreset::DetailFocus => LayoutPreset::TimelineFocus,
        }
    }

    fn config(self) -> LayoutConfig {
        match self {
            LayoutPreset::TimelineFocus => LayoutConfig {
                timeline_percent: 65,
                detail_percent: 35,
            },
            LayoutPreset::Balanced => LayoutConfig {
                timeline_percent: 50,
                detail_percent: 50,
            },
            LayoutPreset::DetailFocus => LayoutConfig {
                timeline_percent: 33,
                detail_percent: 67,
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
struct DetailState {
    scroll: usize,
    cursor: usize,
    collapsed: HashSet<usize>,
}

struct DetailContext<'a> {
    detail: Option<&'a detail::DetailViewModel>,
    visible_indices: Vec<usize>,
    has_children: Vec<bool>,
}

impl<'a> DetailContext<'a> {
    fn new(
        detail: Option<&'a detail::DetailViewModel>,
        collapsed: Option<&HashSet<usize>>,
    ) -> Self {
        if let Some(detail_ref) = detail {
            let (visible_indices, has_children) =
                detail::visible_indices_with_children(detail_ref, collapsed);
            Self {
                detail,
                visible_indices,
                has_children,
            }
        } else {
            Self {
                detail,
                visible_indices: Vec::new(),
                has_children: Vec::new(),
            }
        }
    }

    fn visible_len(&self) -> usize {
        self.visible_indices.len()
    }
}

fn summarize_event(event: &TimelineEvent) -> TimelineEntry {
    let elapsed = event.received_at.elapsed().unwrap_or_default();

    let aggregated = aggregated_log_payload(event);
    let payload_ref = aggregated
        .as_ref()
        .map(|payload| payload as &Payload)
        .or_else(|| primary_payload(event));

    let mut timeline_label = aggregated
        .as_ref()
        .and_then(|payload| payload.content_string("label"))
        .map(|label| label.to_string())
        .or_else(|| event.label.clone());

    let (kind, mut summary) = if let Some(payload) = payload_ref {
        if timeline_label.is_none() {
            timeline_label = payload
                .content_string("label")
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty());
        }

        (payload_kind_label(payload), payload_summary(payload))
    } else {
        ("empty".to_string(), "Request without payloads".to_string())
    };

    if timeline_label
        .as_deref()
        .map(is_default_html_label)
        .unwrap_or(false)
    {
        timeline_label = None;
    }

    if let Some(screen) = event.screen.as_deref() {
        summary = format!("{} | {}", screen, summary);
    }

    TimelineEntry {
        id: event.id,
        kind,
        summary,
        age: format_elapsed(elapsed),
        color: event.color.clone(),
        label: timeline_label,
    }
}

fn primary_payload(event: &TimelineEvent) -> Option<&Payload> {
    event
        .request
        .payloads
        .iter()
        .find(|payload| is_primary_payload_kind(&payload.kind))
        .or_else(|| event.request.payloads.first())
}

fn build_detail_view_for_event(event: &TimelineEvent) -> detail::DetailViewModel {
    if let Some(merged) = aggregated_log_payload(event) {
        return build_detail_view(&merged, event.received_at);
    }

    if let Some(payload) = primary_payload(event) {
        return build_detail_view(payload, event.received_at);
    }

    detail::DetailViewModel {
        header: "no payloads".to_string(),
        footer: String::new(),
        lines: vec![detail::DetailLine {
            indent: 0,
            segments: vec![detail::DetailSegment {
                text: "Request contains no payloads".to_string(),
                style: detail::SegmentStyle::Plain,
            }],
        }],
    }
}

static HTML_TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]+>").unwrap());
static HTML_SCRIPT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap());
static HTML_IMG_SRC_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?is)<img[^>]*src\s*=\s*['"]([^'"]+)['"]"##).unwrap());

fn is_default_html_label(label: &str) -> bool {
    label.trim().eq_ignore_ascii_case("html")
}

fn looks_like_html_snippet(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with('<') && trimmed.contains('>')
}

fn looks_like_json_snippet(text: &str) -> bool {
    let trimmed = text.trim();
    (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
}

fn contains_image_tag(html: &str) -> bool {
    HTML_IMG_SRC_RE.is_match(html)
}

fn extract_image_src(html: &str) -> Option<&str> {
    HTML_IMG_SRC_RE
        .captures(html)
        .and_then(|capture| capture.get(1))
        .map(|m| m.as_str())
}

fn strip_html_tags(text: &str) -> String {
    let without_script = HTML_SCRIPT_RE.replace_all(text, "");
    let stripped = HTML_TAG_RE.replace_all(&without_script, " ").into_owned();
    flatten(stripped.trim())
}

fn contains_sf_dump(text: &str) -> bool {
    text.contains("sf-dump")
}

fn aggregated_log_payload(event: &TimelineEvent) -> Option<Payload> {
    use serde_json::Map;

    let mut values: Vec<Value> = Vec::new();
    let mut label: Option<String> = None;
    let mut meta_snapshot: Option<Value> = None;
    let mut origin_snapshot: Option<&Origin> = None;

    for payload in &event.request.payloads {
        match payload.kind {
            PayloadKind::Log => {
                if origin_snapshot.is_none() {
                    origin_snapshot = payload.origin.as_ref();
                }

                if let Some(object) = payload.content_object() {
                    if let Some(array) = object.get("values").and_then(|value| value.as_array()) {
                        values.extend(array.iter().cloned());
                    }

                    if label.is_none() {
                        if let Some(found) = object
                            .get("label")
                            .and_then(|value| value.as_str())
                            .map(|text| text.trim())
                            .filter(|text| !text.is_empty())
                        {
                            if !is_default_html_label(found) {
                                label = Some(found.to_string());
                            }
                        }
                    }

                    if meta_snapshot.is_none() {
                        if let Some(meta) = object.get("meta") {
                            meta_snapshot = Some(meta.clone());
                        }
                    }
                }
            }
            PayloadKind::Label => {
                if label.is_none() {
                    label = payload
                        .content_object()
                        .and_then(|map| map.get("label"))
                        .and_then(|value| value.as_str())
                        .map(|text| text.trim().to_string())
                        .filter(|text| !text.is_empty())
                        .filter(|text| !is_default_html_label(text));
                }
            }
            _ => {}
        }
    }

    if values.is_empty() && label.is_none() {
        return None;
    }

    if label.is_none() {
        label = event
            .request
            .payloads
            .iter()
            .find_map(|payload| payload.content_string("label"))
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .filter(|text| !is_default_html_label(text));
    }

    let mut content = Map::new();
    content.insert("values".to_string(), Value::Array(values));
    if let Some(label_value) = label.clone().filter(|label| !is_default_html_label(label)) {
        content.insert("label".to_string(), Value::String(label_value));
    }

    if let Some(meta) = meta_snapshot {
        content.insert("meta".to_string(), meta);
    }

    let mut root = Map::new();
    root.insert("type".to_string(), Value::String("log".to_string()));
    root.insert("content".to_string(), Value::Object(content));

    if let Some(origin) = origin_snapshot {
        let mut origin_map = Map::new();
        if let Some(file) = &origin.file {
            origin_map.insert("file".to_string(), Value::String(file.clone()));
        }
        if let Some(line) = origin.line_number {
            origin_map.insert("line_number".to_string(), Value::Number(Number::from(line)));
        }
        if let Some(host) = &origin.hostname {
            origin_map.insert("hostname".to_string(), Value::String(host.clone()));
        }
        root.insert("origin".to_string(), Value::Object(origin_map));
    }

    serde_json::from_value(Value::Object(root)).ok()
}

fn is_primary_payload_kind(kind: &PayloadKind) -> bool {
    !matches!(kind, PayloadKind::Color | PayloadKind::Label)
}

fn payload_kind_label(payload: &Payload) -> String {
    match &payload.kind {
        PayloadKind::Log => "log".to_string(),
        PayloadKind::Custom => custom_payload_type(payload).unwrap_or_else(|| "custom".to_string()),
        PayloadKind::CreateLock => "create_lock".to_string(),
        PayloadKind::ClearAll => "clear_all".to_string(),
        PayloadKind::Hide => "hide".to_string(),
        PayloadKind::ShowApp => "show_app".to_string(),
        PayloadKind::ShowBrowser => "show_browser".to_string(),
        PayloadKind::Notify => "notify".to_string(),
        PayloadKind::Separator => "separator".to_string(),
        PayloadKind::Exception => "exception".to_string(),
        PayloadKind::Table => "table".to_string(),
        PayloadKind::Text => "text".to_string(),
        PayloadKind::Image => "image".to_string(),
        PayloadKind::JsonString => "json_string".to_string(),
        PayloadKind::DecodedJson => "decoded_json".to_string(),
        PayloadKind::Boolean => "boolean".to_string(),
        PayloadKind::Size => "size".to_string(),
        PayloadKind::Color => "color".to_string(),
        PayloadKind::Label => "label".to_string(),
        PayloadKind::Trace => "trace".to_string(),
        PayloadKind::Caller => "caller".to_string(),
        PayloadKind::Measure => "measure".to_string(),
        PayloadKind::PhpInfo => "phpinfo".to_string(),
        PayloadKind::NewScreen => "new_screen".to_string(),
        PayloadKind::Remove => "remove".to_string(),
        PayloadKind::HideApp => "hide_app".to_string(),
        PayloadKind::Ban => "ban".to_string(),
        PayloadKind::Charles => "charles".to_string(),
        PayloadKind::Unknown(value) => value.as_str().to_string(),
    }
}

fn payload_summary(payload: &Payload) -> String {
    match &payload.kind {
        PayloadKind::Log => summarize_log(payload).unwrap_or_else(|| "log payload".to_string()),
        PayloadKind::Custom => summarize_custom(payload),
        PayloadKind::Boolean => {
            let label = payload.content_string("label");
            let body = payload
                .content_object()
                .and_then(|map| map.get("content"))
                .map(value_preview)
                .unwrap_or_else(|| "custom payload".to_string());

            match label {
                Some(label) if !label.is_empty() => clip(&format!("{}: {}", label, body), 80),
                _ => clip(&body, 80),
            }
        }
        PayloadKind::CreateLock => {
            let name = payload.content_string("name").unwrap_or("unknown");
            format!("create lock `{}`", name)
        }
        PayloadKind::ClearAll => "clear all".to_string(),
        PayloadKind::Hide => "hide payload".to_string(),
        PayloadKind::ShowApp => "show app".to_string(),
        PayloadKind::ShowBrowser => "show browser".to_string(),
        PayloadKind::Notify => payload
            .content_string("text")
            .map(|text| clip(text, 80))
            .unwrap_or_else(|| "notification".to_string()),
        PayloadKind::Separator => "separator".to_string(),
        PayloadKind::Exception => payload
            .content_object()
            .and_then(|map| map.get("message"))
            .map(value_preview)
            .unwrap_or_else(|| "exception".to_string()),
        PayloadKind::Table => payload
            .content_string("label")
            .map(|text| clip(text, 80))
            .unwrap_or_else(|| "table".to_string()),
        PayloadKind::Text => payload
            .content_string("content")
            .map(|text| clip(text, 80))
            .unwrap_or_else(|| "text".to_string()),
        PayloadKind::Image => "image".to_string(),
        PayloadKind::JsonString => "json string".to_string(),
        PayloadKind::DecodedJson => payload
            .content_object()
            .map(|map| {
                let json = Value::Object(map.clone()).to_string();
                clip(&flatten(&json), 80)
            })
            .unwrap_or_else(|| "json".to_string()),
        PayloadKind::Size => payload
            .content_string("size")
            .map(|value| format!("size {}", value))
            .unwrap_or_else(|| "size".to_string()),
        PayloadKind::Color => payload
            .content_string("color")
            .map(|value| format!("color {}", value))
            .unwrap_or_else(|| "color".to_string()),
        PayloadKind::Label => payload
            .content_string("label")
            .map(|value| format!("label {}", value))
            .unwrap_or_else(|| "label".to_string()),
        PayloadKind::Trace => "stack trace".to_string(),
        PayloadKind::Caller => "caller".to_string(),
        PayloadKind::Measure => payload
            .content_object()
            .and_then(|map| map.get("name"))
            .map(value_preview)
            .map(|name| format!("measure {}", name))
            .unwrap_or_else(|| "measure".to_string()),
        PayloadKind::PhpInfo => "phpinfo".to_string(),
        PayloadKind::NewScreen => payload
            .content_string("name")
            .map(|name| format!("new screen `{}`", name))
            .unwrap_or_else(|| "new screen".to_string()),
        PayloadKind::Remove => "remove".to_string(),
        PayloadKind::HideApp => "hide app".to_string(),
        PayloadKind::Ban => "ban".to_string(),
        PayloadKind::Charles => "charles".to_string(),
        PayloadKind::Unknown(name) => format!("{} payload", name),
    }
}

fn custom_payload_type(payload: &Payload) -> Option<String> {
    let raw_label = payload
        .content_string("label")
        .map(|label| label.trim())
        .filter(|label| !label.is_empty());

    if let Some(label) = raw_label {
        if label.eq_ignore_ascii_case("image") {
            return Some("image".to_string());
        }
        if label.eq_ignore_ascii_case("json") {
            return Some("json".to_string());
        }
        if is_default_html_label(label) {
            return Some("html".to_string());
        }
        return Some(label.to_string());
    }

    if let Some(content) = payload
        .content_object()
        .and_then(|map| map.get("content"))
        .and_then(|value| value.as_str())
    {
        if contains_image_tag(content) {
            return Some("image".to_string());
        }
        if contains_sf_dump(content) {
            return Some("json".to_string());
        }
        if looks_like_html_snippet(content) {
            return Some("html".to_string());
        }
        if looks_like_json_snippet(content) {
            return Some("json".to_string());
        }
    }

    None
}

fn summarize_custom(payload: &Payload) -> String {
    let type_hint = custom_payload_type(payload);

    let content_value = payload.content_object().and_then(|map| map.get("content"));

    if type_hint.as_deref() == Some("image") {
        let src = content_value
            .and_then(|value| value.as_str())
            .and_then(extract_image_src)
            .or_else(|| content_value.and_then(|value| value.as_str()))
            .unwrap_or("image payload");
        return clip(&format!("image: {}", src), 80);
    }

    if type_hint.as_deref() == Some("json") {
        return payload
            .content_string("label")
            .map(|label| clip(label, 80))
            .unwrap_or_else(|| "json payload".to_string());
    }

    let body = content_value
        .map(|value| match (value, type_hint.as_deref()) {
            (Value::String(text), Some("html")) => strip_html_tags(text),
            (other, _) => value_preview(other),
        })
        .unwrap_or_else(|| "custom payload".to_string());

    match type_hint.as_deref() {
        Some("html") => clip(&body, 80),
        Some(other) => clip(&format!("{}: {}", other, body), 80),
        None => clip(&body, 80),
    }
}

fn summarize_log(payload: &Payload) -> Option<String> {
    let meta_clipboard = payload
        .content_object()
        .and_then(|map| map.get("meta"))
        .and_then(|meta| meta.as_array())
        .and_then(|meta| meta.first())
        .and_then(|entry| entry.get("clipboard_data"))
        .and_then(|value| value.as_str())
        .map(flatten);

    if let Some(clipboard) = meta_clipboard {
        if !clipboard.is_empty() {
            return Some(clip(&clipboard, 80));
        }
    }

    payload
        .content_object()
        .and_then(|map| map.get("values"))
        .and_then(|values| values.as_array())
        .and_then(|values| {
            let mut previews: Vec<String> = values.iter().map(value_preview).collect();
            previews.retain(|value| !value.is_empty());
            if previews.is_empty() {
                None
            } else {
                let joined = previews.join(" | ");
                Some(clip(&joined, 80))
            }
        })
}

fn value_preview(value: &Value) -> String {
    match value {
        Value::String(text) => clip(&flatten(text), 80),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(_) | Value::Object(_) => clip(&flatten(&value.to_string()), 80),
    }
}

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", truncated)
}

fn flatten(text: &str) -> String {
    let decoded = decode_html_entities(text).into_owned();
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 1 {
        "<1s ago".to_string()
    } else if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3_600 {
        let minutes = secs / 60;
        let seconds = secs % 60;
        format!("{}m {:02}s ago", minutes, seconds)
    } else {
        let hours = secs / 3_600;
        let minutes = (secs % 3_600) / 60;
        format!("{}h {:02}m ago", hours, minutes)
    }
}
