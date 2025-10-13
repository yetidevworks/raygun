use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::SystemTime,
};

use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    sync::{RwLock, mpsc},
};
use tracing::warn;
use uuid::Uuid;

use crate::protocol::{PayloadKind, RayRequest};

const DEFAULT_RETENTION: usize = 1_024;

#[derive(Debug, Clone)]
pub struct TimelineEvent {
    pub id: Uuid,
    pub received_at: SystemTime,
    pub request: Arc<RayRequest>,
    pub screen: Option<String>,
    pub color: Option<String>,
    pub label: Option<String>,
}

impl TimelineEvent {
    pub fn new(request: RayRequest, screen: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            received_at: SystemTime::now(),
            request: Arc::new(request),
            screen,
            color: None,
            label: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LockRecord {
    pub hostname: Option<String>,
    pub project_name: Option<String>,
}

impl LockRecord {
    fn new(hostname: Option<String>, project_name: Option<String>) -> Self {
        Self {
            hostname,
            project_name,
        }
    }
}

#[derive(Debug)]
pub struct AppState {
    retention: usize,
    inner: RwLock<StateInner>,
    debug_logger: Option<Arc<PayloadLogger>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::with_logger(None)
    }
}

impl AppState {
    #[cfg(test)]
    pub fn new(retention: usize) -> Self {
        Self::with_debug_logger(retention, None)
    }

    pub fn with_logger(debug_logger: Option<Arc<PayloadLogger>>) -> Self {
        Self::with_debug_logger(DEFAULT_RETENTION, debug_logger)
    }

    pub fn with_debug_logger(retention: usize, debug_logger: Option<Arc<PayloadLogger>>) -> Self {
        Self {
            retention,
            inner: RwLock::new(StateInner::default()),
            debug_logger,
        }
    }

    pub async fn record_request(&self, request: RayRequest) -> Option<TimelineEvent> {
        let screen_hint = extract_screen_from_meta(&request.meta);
        let mut event = TimelineEvent::new(request, screen_hint);

        let mut inner = self.inner.write().await;
        let outcome = inner.apply_payloads(&mut event);

        if matches!(outcome, ApplyOutcome::Record) {
            inner.merge_previous_log_into_context(&mut event);
        }

        if matches!(outcome, ApplyOutcome::Skip) {
            return None;
        }

        if event.screen.is_none() {
            event.screen = inner.current_screen.clone();
        }

        let stored_event = event.clone();
        inner.timeline.push_back(stored_event.clone());
        if inner.timeline.len() > self.retention {
            inner.timeline.pop_front();
        }

        let logger = self.debug_logger.clone();
        let log_request = stored_event.request.clone();

        drop(inner);

        if let Some(logger) = logger {
            logger.log(log_request);
        }

        Some(stored_event)
    }

    pub async fn timeline_snapshot(&self) -> Vec<TimelineEvent> {
        let inner = self.inner.read().await;
        inner.timeline.iter().cloned().collect()
    }

    pub async fn timeline_len(&self) -> usize {
        let inner = self.inner.read().await;
        inner.timeline.len()
    }

    pub async fn lock_exists(
        &self,
        name: &str,
        hostname: Option<&str>,
        project: Option<&str>,
    ) -> bool {
        let inner = self.inner.read().await;
        inner
            .locks
            .get(name)
            .map(|record| {
                hostname.map_or(true, |expected| {
                    record.hostname.as_deref() == Some(expected)
                }) && project.map_or(true, |expected| {
                    record.project_name.as_deref() == Some(expected)
                })
            })
            .unwrap_or(false)
    }

    #[allow(dead_code)]
    pub async fn clear_lock(&self, name: &str) {
        let mut inner = self.inner.write().await;
        inner.locks.remove(name);
    }

    pub async fn clear_timeline(&self) {
        let mut inner = self.inner.write().await;
        inner.timeline.clear();
        inner.current_screen = None;
    }
}

#[derive(Debug, Default)]
struct StateInner {
    timeline: VecDeque<TimelineEvent>,
    locks: HashMap<String, LockRecord>,
    current_screen: Option<String>,
}

#[derive(Debug)]
pub struct PayloadLogger {
    sender: mpsc::UnboundedSender<Arc<RayRequest>>,
}

impl PayloadLogger {
    pub fn new(path: PathBuf) -> Arc<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let logger = Arc::new(Self { sender: tx });
        let task_logger = Arc::clone(&logger);

        tokio::spawn(async move {
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
            {
                Ok(mut file) => {
                    while let Some(request) = rx.recv().await {
                        let dump = format!("{:#?}\n", request);
                        if let Err(err) = file.write_all(dump.as_bytes()).await {
                            warn!(?err, "failed to write payload dump");
                            break;
                        }
                    }
                }
                Err(err) => {
                    warn!(?err, "failed to open payload dump file");
                    while rx.recv().await.is_some() {}
                }
            }

            drop(task_logger);
        });

        logger
    }

    pub fn log(&self, request: Arc<RayRequest>) {
        let _ = self.sender.send(request);
    }
}

impl StateInner {
    fn apply_payloads(&mut self, event: &mut TimelineEvent) -> ApplyOutcome {
        let mut displayable = false;
        let mut outcome = ApplyOutcome::Record;
        let mut pending_color: Option<String> = None;
        let mut pending_label: Option<String> = None;

        for payload in &event.request.payloads {
            match &payload.kind {
                PayloadKind::CreateLock => {
                    if let Some(name) = payload.content_string("name") {
                        let hostname = event
                            .request
                            .meta
                            .get("hostname")
                            .and_then(|value| value.as_str())
                            .map(ToOwned::to_owned);
                        let project = event
                            .request
                            .meta
                            .get("project_name")
                            .and_then(|value| value.as_str())
                            .map(ToOwned::to_owned);
                        self.locks
                            .insert(name.to_owned(), LockRecord::new(hostname, project));
                    }
                }
                PayloadKind::ClearAll => {
                    self.timeline.clear();
                    self.locks.clear();
                    self.current_screen = None;
                    outcome = ApplyOutcome::Skip;
                }
                PayloadKind::Remove => {
                    if let Some(name) = payload.content_string("name") {
                        self.locks.remove(name);
                    }
                    self.timeline.pop_back();
                    outcome = ApplyOutcome::Skip;
                }
                PayloadKind::Hide => {
                    self.timeline.pop_back();
                    outcome = ApplyOutcome::Skip;
                }
                PayloadKind::NewScreen => {
                    if let Some(name) = payload.content_string("name") {
                        let sanitized = sanitize_screen_name(name);
                        self.current_screen = Some(sanitized.clone());
                        event.screen = Some(sanitized);
                    }
                    displayable = true;
                }
                PayloadKind::Color => {
                    if let Some(value) = payload.content_string("color") {
                        let color_value = value.to_owned();
                        event.color = Some(color_value.clone());
                        pending_color = Some(color_value);
                    }
                }
                PayloadKind::Label => {
                    if let Some(value) = payload.content_string("label") {
                        let label_value = value.to_owned();
                        event.label = Some(label_value.clone());
                        pending_label = Some(label_value);
                    }
                }
                _ => {}
            }

            if matches!(
                payload.kind,
                PayloadKind::Log
                    | PayloadKind::Custom
                    | PayloadKind::Text
                    | PayloadKind::Notify
                    | PayloadKind::Exception
                    | PayloadKind::Trace
                    | PayloadKind::Table
                    | PayloadKind::Image
                    | PayloadKind::JsonString
                    | PayloadKind::DecodedJson
                    | PayloadKind::Separator
                    | PayloadKind::Measure
                    | PayloadKind::PhpInfo
                    | PayloadKind::Size
                    | PayloadKind::Caller
                    | PayloadKind::ShowBrowser
                    | PayloadKind::ShowApp
                    | PayloadKind::HideApp
                    | PayloadKind::Ban
                    | PayloadKind::Charles
                    | PayloadKind::NewScreen
            ) {
                displayable = true;
            }
        }

        if !displayable {
            if let Some(color_value) = pending_color {
                if let Some(last) = self.timeline.back_mut() {
                    last.color = Some(color_value);
                }
            }
            if let Some(label_value) = pending_label {
                if let Some(last) = self.timeline.back_mut() {
                    last.label = Some(label_value);
                }
            }
            outcome = ApplyOutcome::Skip;
        }

        if event.screen.is_none() {
            event.screen = self.current_screen.clone();
        }

        outcome
    }

    fn merge_previous_log_into_context(&mut self, event: &mut TimelineEvent) {
        if !event
            .request
            .payloads
            .iter()
            .any(|payload| matches!(payload.kind, PayloadKind::Trace | PayloadKind::Caller))
        {
            return;
        }

        let last_message = self.timeline.back().and_then(extract_single_log_message);

        if let Some(message) = last_message {
            self.timeline.pop_back();
            if event.label.is_none() {
                event.label = Some(message);
            }
        }
    }
}

fn extract_single_log_message(event: &TimelineEvent) -> Option<String> {
    if event.request.payloads.len() != 1 {
        return None;
    }

    let payload = event.request.payloads.first()?;
    if !matches!(payload.kind, PayloadKind::Log) {
        return None;
    }

    let clipboard = payload
        .content_object()
        .and_then(|map| map.get("meta"))
        .and_then(|meta| meta.as_array())
        .and_then(|items| {
            items.iter().find_map(|meta| {
                meta.as_object().and_then(|object| {
                    object
                        .get("clipboard_data")
                        .and_then(|value| value.as_str())
                        .map(|text| text.trim())
                        .filter(|text| !text.is_empty())
                        .map(|text| text.to_string())
                })
            })
        });

    if clipboard.is_some() {
        return clipboard;
    }

    payload
        .content_object()
        .and_then(|map| map.get("values"))
        .and_then(|value| value.as_array())
        .and_then(|values| {
            values.iter().find_map(|value| {
                value
                    .as_str()
                    .map(|text| text.trim())
                    .filter(|text| !text.is_empty())
                    .map(|text| text.to_string())
            })
        })
}

fn sanitize_screen_name(raw: &str) -> String {
    let name = raw.trim();
    if name.is_empty() {
        "Screen".to_string()
    } else {
        name.to_string()
    }
}

fn extract_screen_from_meta(meta: &BTreeMap<String, serde_json::Value>) -> Option<String> {
    const KEYS: &[&str] = &["screen", "screen_name", "screenName"];
    for key in KEYS {
        if let Some(value) = meta.get(*key).and_then(|value| value.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyOutcome {
    Record,
    Skip,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Payload, RayRequest};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn make_payload(value: serde_json::Value) -> Payload {
        serde_json::from_value(value).expect("payload should deserialize")
    }

    fn request_with_payload(payload: Payload) -> RayRequest {
        RayRequest {
            uuid: "test".into(),
            payloads: vec![payload],
            meta: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn records_timeline_with_retention() {
        let state = AppState::new(2);

        let payload = make_payload(json!({
            "type": "log",
            "content": { "values": ["a"], "meta": [] }
        }));

        assert!(
            state
                .record_request(request_with_payload(payload.clone()))
                .await
                .is_some()
        );
        assert!(
            state
                .record_request(request_with_payload(payload.clone()))
                .await
                .is_some()
        );
        assert!(
            state
                .record_request(request_with_payload(payload))
                .await
                .is_some()
        );

        let events = state.timeline_snapshot().await;
        assert_eq!(events.len(), 2, "timeline should enforce retention");
        assert_ne!(events[0].id, events[1].id);
        for event in events {
            assert!(event.received_at.elapsed().is_ok());
        }
    }

    #[tokio::test]
    async fn tracks_locks_from_payloads_without_recording_event() {
        let state = AppState::default();

        let payload = make_payload(json!({
            "type": "create_lock",
            "content": { "name": "pause-lock" }
        }));

        assert!(
            state
                .record_request(request_with_payload(payload))
                .await
                .is_none()
        );

        assert!(
            state.lock_exists("pause-lock", None, None).await,
            "lock should be registered"
        );

        state.clear_lock("pause-lock").await;
        assert!(
            !state.lock_exists("pause-lock", None, None).await,
            "lock should be removed after clear"
        );
    }

    #[tokio::test]
    async fn clear_all_purges_timeline() {
        let state = AppState::default();

        let log = make_payload(json!({
            "type": "log",
            "content": { "values": ["hello"], "meta": [] }
        }));

        state
            .record_request(request_with_payload(log))
            .await
            .expect("log should record");

        let clear = make_payload(json!({
            "type": "clear_all",
            "content": {}
        }));

        assert!(
            state
                .record_request(request_with_payload(clear))
                .await
                .is_none()
        );

        let events = state.timeline_snapshot().await;
        assert!(
            events.is_empty(),
            "timeline should be empty after clear_all"
        );
    }

    #[tokio::test]
    async fn new_screen_updates_current_screen() {
        let state = AppState::default();

        let screen = make_payload(json!({
            "type": "new_screen",
            "content": { "name": "Debug" }
        }));

        state
            .record_request(request_with_payload(screen))
            .await
            .expect("new screen should be recorded");

        let log = make_payload(json!({
            "type": "log",
            "content": { "values": ["data"], "meta": [] }
        }));

        state
            .record_request(request_with_payload(log))
            .await
            .expect("log should be recorded");

        let events = state.timeline_snapshot().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].screen.as_deref(), Some("Debug"));
        assert_eq!(events[1].screen.as_deref(), Some("Debug"));
    }

    #[tokio::test]
    async fn color_payload_sets_event_color() {
        let state = AppState::default();

        let color = make_payload(json!({
            "type": "color",
            "content": { "color": "blue" }
        }));

        let log = make_payload(json!({
            "type": "log",
            "content": { "values": ["hello"], "meta": [] }
        }));

        let request = RayRequest {
            uuid: "color-test".into(),
            payloads: vec![color, log],
            meta: BTreeMap::new(),
        };

        let event = state
            .record_request(request)
            .await
            .expect("request should record");

        assert_eq!(event.color.as_deref(), Some("blue"));
    }

    #[tokio::test]
    async fn color_only_payload_is_skipped() {
        let state = AppState::default();

        let color = make_payload(json!({
            "type": "color",
            "content": { "color": "green" }
        }));

        let request = RayRequest {
            uuid: "color-only".into(),
            payloads: vec![color],
            meta: BTreeMap::new(),
        };

        assert!(
            state.record_request(request).await.is_none(),
            "color-only payload should not appear in timeline"
        );
    }

    #[tokio::test]
    async fn color_payload_updates_previous_event() {
        let state = AppState::default();

        let log = make_payload(json!({
            "type": "log",
            "content": { "values": ["hello"], "meta": [] }
        }));

        let stored = state
            .record_request(request_with_payload(log))
            .await
            .expect("log should record");
        assert!(stored.color.is_none());

        let color = make_payload(json!({
            "type": "color",
            "content": { "color": "green" }
        }));

        let request = RayRequest {
            uuid: "color-followup".into(),
            payloads: vec![color],
            meta: BTreeMap::new(),
        };

        let outcome = state.record_request(request).await;
        assert!(
            outcome.is_none(),
            "color follow-up should not create a new event"
        );

        let events = state.timeline_snapshot().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].color.as_deref(), Some("green"));
    }

    #[tokio::test]
    async fn clear_timeline_method_resets_events() {
        let state = AppState::default();

        let log = make_payload(json!({
            "type": "log",
            "content": { "values": ["hello"], "meta": [] }
        }));

        state
            .record_request(request_with_payload(log))
            .await
            .expect("log should record");

        state.clear_timeline().await;

        let events = state.timeline_snapshot().await;
        assert!(
            events.is_empty(),
            "timeline should be empty after manual clear"
        );
    }

    #[tokio::test]
    async fn label_payload_updates_previous_event() {
        let state = AppState::default();

        let log_request = RayRequest {
            uuid: "test-log".into(),
            payloads: vec![make_payload(json!({
                "type": "log",
                "content": { "values": ["hello"], "meta": [] }
            }))],
            meta: BTreeMap::new(),
        };

        let event = state
            .record_request(log_request)
            .await
            .expect("log should record");
        assert!(event.label.is_none());

        let label_request = RayRequest {
            uuid: "test-log".into(),
            payloads: vec![make_payload(json!({
                "type": "label",
                "content": { "label": "example" }
            }))],
            meta: BTreeMap::new(),
        };

        assert!(state.record_request(label_request).await.is_none());

        let events = state.timeline_snapshot().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].label.as_deref(), Some("example"));
    }
}
