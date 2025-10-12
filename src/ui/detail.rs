use std::time::{SystemTime, UNIX_EPOCH};

use html_escape::decode_html_entities;
use serde_json::Value;

use crate::protocol::{Payload, PayloadKind};

#[derive(Debug, Clone)]
pub struct DetailViewModel {
    pub header: String,
    pub footer: String,
    pub body: Vec<String>,
}

pub fn build_detail_view(payload: &Payload, received_at: SystemTime) -> DetailViewModel {
    let header = format!(
        "{} • {}",
        payload_kind_label(&payload.kind),
        humanize_timestamp(received_at)
    );

    let footer = payload
        .origin
        .as_ref()
        .and_then(|origin| origin.file.as_ref().map(|file| (file, origin.line_number)))
        .map(|(file, line)| match line {
            Some(line) => format!("{}:{}", file, line),
            None => file.to_string(),
        })
        .unwrap_or_default();

    let body = match &payload.kind {
        PayloadKind::Log => render_log(payload),
        PayloadKind::Text => render_text(payload),
        PayloadKind::DecodedJson | PayloadKind::JsonString => render_json(payload),
        _ => fallback_lines(payload),
    };

    DetailViewModel {
        header,
        footer,
        body,
    }
}

fn payload_kind_label(kind: &PayloadKind) -> &'static str {
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
        PayloadKind::Unknown(_) => "unknown",
    }
}

fn render_log(payload: &Payload) -> Vec<String> {
    if let Some(clipboard) = payload
        .content_object()
        .and_then(|map| map.get("meta"))
        .and_then(|meta| meta.as_array())
        .and_then(|meta| meta.first())
        .and_then(|entry| entry.get("clipboard_data"))
        .and_then(|value| value.as_str())
    {
        return render_sf_dump(clipboard);
    }

    fallback_lines(payload)
}

fn render_text(payload: &Payload) -> Vec<String> {
    payload
        .content_string("content")
        .map(|text| text.lines().map(|line| line.to_string()).collect())
        .unwrap_or_else(|| fallback_lines(payload))
}

fn render_json(payload: &Payload) -> Vec<String> {
    let value = payload
        .content_object()
        .and_then(|map| map.get("content"))
        .cloned()
        .or_else(|| payload.content_object().cloned().map(Value::Object));

    value
        .map(|value| serde_json::to_string_pretty(&value).unwrap_or_default())
        .map(|json| json.lines().map(|line| line.to_string()).collect())
        .unwrap_or_else(|| fallback_lines(payload))
}

fn fallback_lines(payload: &Payload) -> Vec<String> {
    let content = payload.content_object().cloned().unwrap_or_default();
    serde_json::to_string_pretty(&Value::Object(content))
        .unwrap_or_default()
        .lines()
        .map(|line| line.to_string())
        .collect()
}

fn render_sf_dump(dump: &str) -> Vec<String> {
    let decoded = decode_html_entities(dump);
    decoded
        .split(['\r', '\n'])
        .map(|line| collapse_whitespace(line).to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn collapse_whitespace(input: &str) -> &str {
    input.trim()
}

fn humanize_timestamp(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("{}s", duration.as_secs()),
        Err(_) => "now".to_string(),
    }
}
