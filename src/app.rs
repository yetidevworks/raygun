use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use html_escape::decode_html_entities;
use serde_json::Value;
use tokio::{select, sync::mpsc};
use tracing::{debug, info, warn};

use crate::{
    config::Config,
    protocol::{Payload, PayloadKind},
    server,
    state::{AppState, TimelineEvent},
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Timeline,
    Detail,
}

const TIMELINE_VIEW_LIMIT: usize = 200;

impl RaygunApp {
    pub async fn bootstrap(config: Config) -> Result<Self> {
        let state = Arc::new(AppState::default());
        let bind_addr = config.bind_addr;
        let server = server::spawn(Arc::clone(&state), server::ServerConfig { bind_addr }).await?;
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
            .and_then(|event| {
                event
                    .request
                    .payloads
                    .first()
                    .map(|payload| (payload, event))
            })
            .map(|(payload, event)| build_detail_view(payload, event.received_at));

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
        }
    }

    fn handle_event(
        &mut self,
        event: Event,
        timeline_len: usize,
        detail_ctx: &DetailContext,
    ) -> bool {
        match event {
            Event::Input(key) => match key.code {
                KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
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
                KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => {
                    if self.focus == Focus::Detail {
                        self.expand_current_node(detail_ctx);
                    }
                    false
                }
                KeyCode::Left => {
                    if self.focus == Focus::Detail {
                        self.collapse_current_node(detail_ctx);
                    }
                    false
                }
                _ => false,
            },
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

    fn expand_current_node(&mut self, ctx: &DetailContext) {
        if ctx.visible_len() == 0 {
            return;
        }

        if let Some(state) = self.current_detail_state_mut() {
            if ctx.visible_len() == 0 {
                return;
            }

            let cursor = state.cursor.min(ctx.visible_len().saturating_sub(1));
            if let Some(&line_index) = ctx.visible_indices.get(cursor) {
                if ctx.has_children.get(line_index).copied().unwrap_or(false) {
                    state.collapsed.remove(&line_index);
                }
                state.scroll = state.cursor.min(ctx.visible_len().saturating_sub(1));
                self.detail_scroll = state.scroll;
            }
        }
    }

    fn collapse_current_node(&mut self, ctx: &DetailContext) {
        if ctx.visible_len() == 0 {
            return;
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
                        if indent > 0 {
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
                                return;
                            }
                        }
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
                        return;
                    }
                }

                state.scroll = state.cursor.min(ctx.visible_len().saturating_sub(1));
                self.detail_scroll = state.scroll;
            }
        }
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

    if let Some(payload) = event.request.payloads.first() {
        let kind = payload_kind_label(&payload.kind);
        let mut summary = payload_summary(payload);

        if let Some(screen) = event.screen.as_deref() {
            summary = format!("{} | {}", screen, summary);
        }

        TimelineEntry {
            id: event.id,
            kind,
            summary,
            age: format_elapsed(elapsed),
        }
    } else {
        let mut summary = "Request without payloads".to_string();
        if let Some(screen) = event.screen.as_deref() {
            summary = format!("{} | {}", screen, summary);
        }
        TimelineEntry {
            id: event.id,
            kind: "empty".to_string(),
            summary,
            age: format_elapsed(elapsed),
        }
    }
}

fn payload_kind_label(kind: &PayloadKind) -> String {
    match kind {
        PayloadKind::Log => "log",
        PayloadKind::Custom => "custom",
        PayloadKind::CreateLock => "create_lock",
        PayloadKind::ClearAll => "clear_all",
        PayloadKind::Hide => "hide",
        PayloadKind::ShowApp => "show_app",
        PayloadKind::ShowBrowser => "show_browser",
        PayloadKind::Notify => "notify",
        PayloadKind::Separator => "separator",
        PayloadKind::Exception => "exception",
        PayloadKind::Table => "table",
        PayloadKind::Text => "text",
        PayloadKind::Image => "image",
        PayloadKind::JsonString => "json_string",
        PayloadKind::DecodedJson => "decoded_json",
        PayloadKind::Boolean => "boolean",
        PayloadKind::Size => "size",
        PayloadKind::Color => "color",
        PayloadKind::Trace => "trace",
        PayloadKind::Caller => "caller",
        PayloadKind::Measure => "measure",
        PayloadKind::PhpInfo => "phpinfo",
        PayloadKind::NewScreen => "new_screen",
        PayloadKind::Remove => "remove",
        PayloadKind::HideApp => "hide_app",
        PayloadKind::Ban => "ban",
        PayloadKind::Charles => "charles",
        PayloadKind::Unknown(value) => value.as_str(),
    }
    .to_string()
}

fn payload_summary(payload: &Payload) -> String {
    match &payload.kind {
        PayloadKind::Log => summarize_log(payload).unwrap_or_else(|| "log payload".to_string()),
        PayloadKind::Custom | PayloadKind::Boolean => {
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
        PayloadKind::Table => "table".to_string(),
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
        .and_then(|values| values.first())
        .map(value_preview)
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
