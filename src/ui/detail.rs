use std::time::{SystemTime, UNIX_EPOCH};

use html_escape::decode_html_entities;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

use crate::protocol::{Payload, PayloadKind};

#[derive(Debug, Clone)]
pub struct DetailViewModel {
    pub header: String,
    pub footer: String,
    pub lines: Vec<DetailLine>,
}

impl DetailViewModel {
    pub fn total_line_count(&self) -> usize {
        let mut len = self.lines.len();
        if !self.header.is_empty() {
            len += 2;
        }
        if !self.footer.is_empty() {
            len += 2;
        }
        len
    }
}

#[derive(Debug, Clone)]
pub struct DetailLine {
    pub indent: usize,
    pub segments: Vec<DetailSegment>,
}

#[derive(Debug, Clone)]
pub struct DetailSegment {
    pub text: String,
    pub style: SegmentStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentStyle {
    Plain,
    Key,
    Type,
    String,
    Number,
    Boolean,
    Null,
}

pub fn build_detail_view(payload: &Payload, received_at: SystemTime) -> DetailViewModel {
    let header = format!(
        "{} • {}",
        payload_label(&payload.kind),
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

    let lines = match &payload.kind {
        PayloadKind::Log => render_log(payload),
        PayloadKind::Text => render_text(payload),
        PayloadKind::DecodedJson | PayloadKind::JsonString => render_json(payload),
        _ => fallback_lines(payload),
    };

    DetailViewModel {
        header,
        footer,
        lines,
    }
}

fn payload_label(kind: &PayloadKind) -> &'static str {
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

fn render_log(payload: &Payload) -> Vec<DetailLine> {
    if let Some(clipboard) = payload
        .content_object()
        .and_then(|map| map.get("meta"))
        .and_then(|meta| meta.as_array())
        .and_then(|meta| meta.first())
        .and_then(|entry| entry.get("clipboard_data"))
        .and_then(|value| value.as_str())
    {
        return parse_sf_dump(clipboard);
    }

    fallback_lines(payload)
}

fn render_text(payload: &Payload) -> Vec<DetailLine> {
    payload
        .content_string("content")
        .map(|text| text.lines().map(parse_plain_line).collect())
        .unwrap_or_else(|| fallback_lines(payload))
}

fn render_json(payload: &Payload) -> Vec<DetailLine> {
    let value = payload
        .content_object()
        .and_then(|map| map.get("content"))
        .cloned()
        .or_else(|| payload.content_object().cloned().map(Value::Object));

    value
        .map(|value| serde_json::to_string_pretty(&value).unwrap_or_default())
        .map(|json| json.lines().map(parse_plain_line).collect())
        .unwrap_or_else(|| fallback_lines(payload))
}

fn fallback_lines(payload: &Payload) -> Vec<DetailLine> {
    let content = payload.content_object().cloned().unwrap_or_default();
    serde_json::to_string_pretty(&Value::Object(content))
        .unwrap_or_default()
        .lines()
        .map(parse_plain_line)
        .collect()
}

fn parse_plain_line(line: &str) -> DetailLine {
    DetailLine {
        indent: count_indent(line),
        segments: vec![DetailSegment {
            text: line.trim_start().to_string(),
            style: SegmentStyle::Plain,
        }],
    }
}

fn parse_sf_dump(dump: &str) -> Vec<DetailLine> {
    let sanitized = sanitize_sf_dump(dump);

    sanitized
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_highlighted_line)
        .collect()
}

fn parse_highlighted_line(line: &str) -> DetailLine {
    let indent = count_indent(line);
    let trimmed = line.trim_start();
    let mut segments = Vec::new();
    let mut cursor = 0;

    while cursor < trimmed.len() {
        let rest = &trimmed[cursor..];

        if let Some(mat) = KEY_RE.find(rest) {
            if mat.start() == 0 {
                segments.push(DetailSegment {
                    text: mat.as_str().to_string(),
                    style: SegmentStyle::Key,
                });
                cursor += mat.end();
                continue;
            }
        }

        if rest.starts_with('"') || rest.starts_with('\'') {
            if let Some((token, len)) = extract_string(rest) {
                segments.push(DetailSegment {
                    text: token,
                    style: SegmentStyle::String,
                });
                cursor += len;
                continue;
            }
        }

        if let Some(mat) = TYPE_RE.find(rest) {
            if mat.start() == 0 {
                segments.push(DetailSegment {
                    text: mat.as_str().to_string(),
                    style: SegmentStyle::Type,
                });
                cursor += mat.end();
                continue;
            }
        }

        if let Some(mat) = BOOL_RE.find(rest) {
            if mat.start() == 0 {
                segments.push(DetailSegment {
                    text: mat.as_str().to_string(),
                    style: SegmentStyle::Boolean,
                });
                cursor += mat.end();
                continue;
            }
        }

        if let Some(mat) = NULL_RE.find(rest) {
            if mat.start() == 0 {
                segments.push(DetailSegment {
                    text: mat.as_str().to_string(),
                    style: SegmentStyle::Null,
                });
                cursor += mat.end();
                continue;
            }
        }

        if let Some(mat) = NUMBER_RE.find(rest) {
            if mat.start() == 0 {
                segments.push(DetailSegment {
                    text: mat.as_str().to_string(),
                    style: SegmentStyle::Number,
                });
                cursor += mat.end();
                continue;
            }
        }

        let ch_len = rest.chars().next().map(|ch| ch.len_utf8()).unwrap_or(1);
        push_plain(&trimmed[cursor..cursor + ch_len], &mut segments);
        cursor += ch_len;
    }

    DetailLine { indent, segments }
}

fn push_plain(text: &str, segments: &mut Vec<DetailSegment>) {
    if text.is_empty() {
        return;
    }

    if let Some(last) = segments.last_mut() {
        if last.style == SegmentStyle::Plain {
            last.text.push_str(text);
            return;
        }
    }

    segments.push(DetailSegment {
        text: text.to_string(),
        style: SegmentStyle::Plain,
    });
}

fn extract_string(input: &str) -> Option<(String, usize)> {
    let mut chars = input.chars();
    let quote = chars.next()?;
    let mut escaped = false;
    let mut collected = String::new();
    collected.push(quote);
    let mut len = quote.len_utf8();

    for ch in chars {
        len += ch.len_utf8();
        collected.push(ch);
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return Some((collected, len));
        }
    }

    None
}

fn sanitize_sf_dump(input: &str) -> String {
    let upto_script = input
        .find("<script")
        .map(|idx| &input[..idx])
        .unwrap_or(input);

    let mut sanitized = upto_script
        .replace("<br>", "\n")
        .replace("<br />", "\n")
        .replace("\r", "")
        .replace("&nbsp;", " ");

    sanitized = TAG_RE.replace_all(&sanitized, "").into_owned();

    decode_html_entities(&sanitized).into_owned()
}

fn count_indent(line: &str) -> usize {
    let spaces = line.chars().take_while(|ch| ch.is_whitespace()).count();
    spaces / 2
}

fn humanize_timestamp(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("{}s", duration.as_secs()),
        Err(_) => "now".to_string(),
    }
}

static TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]+>").unwrap());
static KEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^(\+?\[[^\]]+\]|\+["'][^"']+["'])"#).unwrap());
static TYPE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(?:stdClass#\d+|array:\d+|object\([^)]*\))").unwrap());
static BOOL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(true|false)\b").unwrap());
static NULL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^null\b").unwrap());
static NUMBER_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^-?\d+(?:\.\d+)?").unwrap());
