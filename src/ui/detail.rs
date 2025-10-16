use std::time::{SystemTime, UNIX_EPOCH};

use html_escape::decode_html_entities;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};

use crate::protocol::{Payload, PayloadKind};

#[derive(Debug, Clone)]
pub struct DetailViewModel {
    pub header: String,
    pub footer: String,
    pub lines: Vec<DetailLine>,
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
        payload_label(payload),
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
        PayloadKind::Table => render_table(payload),
        PayloadKind::Custom => render_custom(payload),
        PayloadKind::Label => render_label(payload),
        PayloadKind::Trace => render_trace(payload),
        PayloadKind::Exception => render_exception(payload),
        PayloadKind::Measure => render_measure(payload),
        PayloadKind::Caller => render_caller(payload),
        PayloadKind::DecodedJson | PayloadKind::JsonString => render_json(payload),
        _ => fallback_lines(payload),
    };

    DetailViewModel {
        header,
        footer,
        lines,
    }
}

pub fn visible_indices_with_children(
    detail: &DetailViewModel,
    collapsed: Option<&HashSet<usize>>,
) -> (Vec<usize>, Vec<bool>) {
    let has_children = compute_has_children(&detail.lines);
    let mut visible = Vec::new();
    let mut hidden_indent: Option<usize> = None;

    for (index, line) in detail.lines.iter().enumerate() {
        if let Some(indent) = hidden_indent {
            if line.indent > indent {
                continue;
            }
            hidden_indent = None;
        }

        visible.push(index);

        let is_collapsed = collapsed.map(|set| set.contains(&index)).unwrap_or(false);

        if has_children[index] && is_collapsed {
            hidden_indent = Some(line.indent);
        }
    }

    (visible, has_children)
}

fn payload_label(payload: &Payload) -> String {
    match payload.kind {
        PayloadKind::Log => "log".to_string(),
        PayloadKind::Custom => {
            let content = payload
                .content_object()
                .and_then(|map| map.get("content"))
                .and_then(|value| value.as_str());

            let raw_label = payload
                .content_string("label")
                .map(|label| label.trim())
                .filter(|label| !label.is_empty());

            let has_image_label = raw_label
                .map(|label| label.eq_ignore_ascii_case("image"))
                .unwrap_or(false);
            let looks_image = content.map(contains_image_tag).unwrap_or(false);

            if has_image_label || looks_image {
                return "image".to_string();
            }

            let has_html_label = raw_label
                .map(|label| label.eq_ignore_ascii_case("html"))
                .unwrap_or(false)
                || content.map(looks_like_html).unwrap_or(false);

            if has_html_label {
                "html".to_string()
            } else if let Some(label) = raw_label {
                label.to_string()
            } else {
                "custom".to_string()
            }
        }
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
        PayloadKind::Unknown(_) => "unknown".to_string(),
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

    if let Some(values) = payload
        .content_object()
        .and_then(|map| map.get("values"))
        .and_then(|value| value.as_array())
    {
        let mut lines = Vec::new();

        if let Some(label) = payload
            .content_string("label")
            .map(|label| label.trim())
            .filter(|label| !label.is_empty())
        {
            lines.push(parse_plain_line(&format!("Label: {}", label)));
            lines.push(parse_plain_line(""));
        }

        for value in values {
            let text = value_to_plain(value);
            lines.push(parse_plain_line(&format!("- {}", text)));
        }

        if !lines.is_empty() {
            return lines;
        }
    }

    fallback_lines(payload)
}

fn render_text(payload: &Payload) -> Vec<DetailLine> {
    payload
        .content_string("content")
        .map(|text| text.lines().map(parse_plain_line).collect())
        .unwrap_or_else(|| fallback_lines(payload))
}

fn render_custom(payload: &Payload) -> Vec<DetailLine> {
    if let Some(object) = payload.content_object() {
        if let Some(content) = object.get("content").and_then(|value| value.as_str()) {
            let raw_label = object
                .get("label")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|label| !label.is_empty());

            if contains_sf_dump(content) {
                return render_sf_dump(content, raw_label);
            }

            let is_default_count_label = raw_label
                .map(|label| label.eq_ignore_ascii_case("count"))
                .unwrap_or(false);

            if is_default_count_label {
                return render_count(content, raw_label);
            }

            let is_default_image_label = raw_label
                .map(|label| label.eq_ignore_ascii_case("image"))
                .unwrap_or(false);

            if is_default_image_label || contains_image_tag(content) {
                let src = extract_image_src(content).unwrap_or_else(|| content.trim());
                return vec![parse_plain_line(src)];
            }

            let is_default_html_label = raw_label
                .map(|label| label.eq_ignore_ascii_case("html"))
                .unwrap_or(false);

            if looks_like_html(content) || is_default_html_label {
                let label = if is_default_html_label {
                    None
                } else {
                    raw_label
                };
                return render_html(label, content);
            }
        }
    }

    fallback_lines(payload)
}

fn render_label(payload: &Payload) -> Vec<DetailLine> {
    let label = payload
        .content_string("label")
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
        .unwrap_or("label payload");

    vec![parse_plain_line(label)]
}

fn render_count(content: &str, raw_label: Option<&str>) -> Vec<DetailLine> {
    let mut lines = Vec::new();

    if let Some(label) = raw_label
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
        .filter(|label| !label.eq_ignore_ascii_case("count"))
    {
        lines.push(parse_plain_line(&format!("Label: {}", label)));
        lines.push(parse_plain_line(""));
    }

    lines.push(parse_plain_line(content.trim()));
    lines
}

fn render_sf_dump(content: &str, raw_label: Option<&str>) -> Vec<DetailLine> {
    let mut lines = Vec::new();

    if let Some(label) = raw_label
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
        .filter(|label| !label.eq_ignore_ascii_case("json"))
    {
        lines.push(parse_plain_line(&format!("Label: {}", label)));
        lines.push(parse_plain_line(""));
    }

    let mut dump_lines = parse_sf_dump(content);
    if dump_lines.is_empty() {
        dump_lines.push(parse_plain_line(content.trim()));
    }
    lines.extend(dump_lines);
    lines
}

fn render_measure(payload: &Payload) -> Vec<DetailLine> {
    let content = match payload.content_object() {
        Some(object) => object,
        None => return fallback_lines(payload),
    };

    let mut lines = Vec::new();

    if let Some(name) = content.get("name").and_then(|value| value.as_str()) {
        lines.push(detail_key_value("Name", name));
    }

    if let Some(value) = content.get("total_time") {
        if let Some(formatted) = format_duration(value) {
            lines.push(detail_key_value("Total time", &formatted));
        }
    }

    if let Some(value) = content.get("time_since_last_call") {
        if let Some(formatted) = format_duration(value) {
            lines.push(detail_key_value("Since last call", &formatted));
        }
    }

    if let Some(value) = content.get("max_memory_usage_during_total_time") {
        if let Some(formatted) = format_bytes(value) {
            lines.push(detail_key_value("Max memory (total)", &formatted));
        }
    }

    if let Some(value) = content.get("max_memory_usage_since_last_call") {
        if let Some(formatted) = format_bytes(value) {
            lines.push(detail_key_value("Max memory (delta)", &formatted));
        }
    }

    if let Some(new_timer) = content
        .get("is_new_timer")
        .and_then(|value| value.as_bool())
    {
        lines.push(detail_key_value(
            "New timer",
            if new_timer { "yes" } else { "no" },
        ));
    }

    if lines.is_empty() {
        fallback_lines(payload)
    } else {
        lines
    }
}

fn render_trace(payload: &Payload) -> Vec<DetailLine> {
    let mut lines = Vec::new();

    if let Some(label) = payload
        .content_string("label")
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
    {
        lines.push(parse_plain_line(&format!("Label: {}", label)));
        lines.push(parse_plain_line(""));
    }

    let frames = payload
        .content_object()
        .and_then(|map| map.get("frames"))
        .and_then(|value| value.as_array());

    let frames = match frames {
        Some(frames) if !frames.is_empty() => frames,
        _ => {
            lines.push(parse_plain_line("(no frames)"));
            return lines;
        }
    };

    for (index, frame) in frames.iter().enumerate() {
        if let Some(frame) = frame.as_object() {
            push_frame_lines(index, frame, 0, &mut lines);
            lines.push(parse_plain_line(""));
        }
    }

    if let Some(last) = lines.last() {
        if last.segments.len() == 1 && last.segments[0].text.is_empty() {
            // remove trailing blank line
            lines.pop();
        }
    }

    lines
}

fn render_exception(payload: &Payload) -> Vec<DetailLine> {
    let content = match payload.content_object() {
        Some(content) => content,
        None => return fallback_lines(payload),
    };

    let class = content
        .get("class")
        .and_then(|value| value.as_str())
        .unwrap_or("Exception");

    let mut lines = Vec::new();
    lines.push(DetailLine {
        indent: 0,
        segments: vec![
            DetailSegment {
                text: "Exception".to_string(),
                style: SegmentStyle::Key,
            },
            DetailSegment {
                text: ": ".to_string(),
                style: SegmentStyle::Plain,
            },
            DetailSegment {
                text: class.to_string(),
                style: SegmentStyle::Type,
            },
        ],
    });

    if let Some(message) = content.get("message") {
        push_value_lines(&mut lines, 1, "message", message);
    } else {
        lines.push(DetailLine {
            indent: 1,
            segments: vec![
                DetailSegment {
                    text: "message: ".to_string(),
                    style: SegmentStyle::Key,
                },
                DetailSegment {
                    text: "(missing)".to_string(),
                    style: SegmentStyle::Plain,
                },
            ],
        });
    }

    if let Some(frames) = content.get("frames").and_then(|value| value.as_array()) {
        if let Some(first_frame) = frames.first().and_then(|value| value.as_object()) {
            let file = first_frame
                .get("file_name")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let line_number = first_frame
                .get("line_number")
                .and_then(|value| value.as_i64());

            if file.is_some() || line_number.is_some() {
                let mut segments = Vec::new();
                segments.push(DetailSegment {
                    text: "location: ".to_string(),
                    style: SegmentStyle::Key,
                });

                if let Some(file) = file {
                    segments.push(DetailSegment {
                        text: file.to_string(),
                        style: SegmentStyle::String,
                    });
                } else {
                    segments.push(DetailSegment {
                        text: "(unknown)".to_string(),
                        style: SegmentStyle::Null,
                    });
                }

                if let Some(line_number) = line_number {
                    if file.is_some() {
                        segments.push(DetailSegment {
                            text: ":".to_string(),
                            style: SegmentStyle::Plain,
                        });
                    }
                    segments.push(DetailSegment {
                        text: line_number.to_string(),
                        style: SegmentStyle::Number,
                    });
                }

                lines.push(DetailLine {
                    indent: 1,
                    segments,
                });
            }
        }

        if !frames.is_empty() {
            lines.push(DetailLine {
                indent: 1,
                segments: vec![DetailSegment {
                    text: format!("stack trace ({} frames)", frames.len()),
                    style: SegmentStyle::Key,
                }],
            });

            let frame_count = frames.len();
            for (index, frame) in frames.iter().enumerate() {
                if let Some(frame) = frame.as_object() {
                    push_frame_lines(index, frame, 2, &mut lines);
                    if index + 1 < frame_count {
                        lines.push(empty_line(2));
                    }
                }
            }
        }
    }

    if let Some(meta) = content.get("meta") {
        if !meta.is_null() {
            if !(matches!(meta, Value::Array(items) if items.is_empty())) {
                push_value_lines(&mut lines, 1, "meta", meta);
            }
        }
    }

    for (key, value) in content {
        match key.as_str() {
            "class" | "message" | "frames" | "meta" => continue,
            other_key => {
                push_value_lines(&mut lines, 1, other_key, value);
            }
        }
    }

    lines
}

fn render_caller(payload: &Payload) -> Vec<DetailLine> {
    let mut lines = Vec::new();

    if let Some(label) = payload
        .content_string("label")
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
    {
        lines.push(parse_plain_line(&format!("Label: {}", label)));
        lines.push(parse_plain_line(""));
    }

    let frame = payload
        .content_object()
        .and_then(|map| map.get("frame"))
        .and_then(|value| value.as_object());

    if let Some(frame) = frame {
        push_frame_lines(0, frame, 0, &mut lines);
    } else {
        return fallback_lines(payload);
    }

    lines
}

fn push_frame_lines(
    index: usize,
    frame: &Map<String, Value>,
    base_indent: usize,
    lines: &mut Vec<DetailLine>,
) {
    let class = frame
        .get("class")
        .and_then(|value| value.as_str())
        .unwrap_or("(anonymous)")
        .trim();
    let method = frame
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();

    let file = frame
        .get("file_name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let line_number = frame
        .get("line_number")
        .and_then(|value| value.as_i64())
        .map(|number| number.to_string());

    let vendor = frame
        .get("vendor_frame")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    let mut header_segments = Vec::new();
    header_segments.push(DetailSegment {
        text: format!("#{:<2} ", index + 1),
        style: SegmentStyle::Plain,
    });
    header_segments.push(DetailSegment {
        text: class.to_string(),
        style: SegmentStyle::Key,
    });
    if !method.is_empty() {
        header_segments.push(DetailSegment {
            text: "::".to_string(),
            style: SegmentStyle::Plain,
        });
        header_segments.push(DetailSegment {
            text: method.to_string(),
            style: SegmentStyle::Type,
        });
    }
    if vendor {
        header_segments.push(DetailSegment {
            text: " [vendor]".to_string(),
            style: SegmentStyle::Boolean,
        });
    }
    lines.push(DetailLine {
        indent: base_indent,
        segments: header_segments,
    });

    if let Some(file) = file {
        let mut location_segments = Vec::new();
        location_segments.push(DetailSegment {
            text: file.to_string(),
            style: SegmentStyle::String,
        });
        if let Some(line_number) = line_number {
            location_segments.push(DetailSegment {
                text: ":".to_string(),
                style: SegmentStyle::Plain,
            });
            location_segments.push(DetailSegment {
                text: line_number,
                style: SegmentStyle::Number,
            });
        }
        lines.push(DetailLine {
            indent: base_indent + 1,
            segments: location_segments,
        });
    }
}

fn push_value_lines(lines: &mut Vec<DetailLine>, indent: usize, label: &str, value: &Value) {
    match value {
        Value::String(text) => {
            if text.is_empty() {
                lines.push(DetailLine {
                    indent,
                    segments: vec![
                        DetailSegment {
                            text: format!("{}: ", label),
                            style: SegmentStyle::Key,
                        },
                        DetailSegment {
                            text: "\"\"".to_string(),
                            style: SegmentStyle::String,
                        },
                    ],
                });
                return;
            }

            let mut pieces = text.lines();
            if let Some(first) = pieces.next() {
                lines.push(DetailLine {
                    indent,
                    segments: vec![
                        DetailSegment {
                            text: format!("{}: ", label),
                            style: SegmentStyle::Key,
                        },
                        DetailSegment {
                            text: first.to_string(),
                            style: SegmentStyle::String,
                        },
                    ],
                });
            }

            for extra in pieces {
                if extra.is_empty() {
                    lines.push(empty_line(indent + 1));
                } else {
                    lines.push(DetailLine {
                        indent: indent + 1,
                        segments: vec![DetailSegment {
                            text: extra.to_string(),
                            style: SegmentStyle::String,
                        }],
                    });
                }
            }
        }
        Value::Number(number) => {
            lines.push(DetailLine {
                indent,
                segments: vec![
                    DetailSegment {
                        text: format!("{}: ", label),
                        style: SegmentStyle::Key,
                    },
                    DetailSegment {
                        text: number.to_string(),
                        style: SegmentStyle::Number,
                    },
                ],
            });
        }
        Value::Bool(boolean) => {
            lines.push(DetailLine {
                indent,
                segments: vec![
                    DetailSegment {
                        text: format!("{}: ", label),
                        style: SegmentStyle::Key,
                    },
                    DetailSegment {
                        text: boolean.to_string(),
                        style: SegmentStyle::Boolean,
                    },
                ],
            });
        }
        Value::Null => {
            lines.push(DetailLine {
                indent,
                segments: vec![
                    DetailSegment {
                        text: format!("{}: ", label),
                        style: SegmentStyle::Key,
                    },
                    DetailSegment {
                        text: "null".to_string(),
                        style: SegmentStyle::Null,
                    },
                ],
            });
        }
        other => {
            let json = serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string());
            let mut first_line = true;
            for line in json.lines() {
                let trimmed = line.trim();
                let indent_width = line.chars().take_while(|ch| ch.is_whitespace()).count();
                let extra_indent = indent_width / 2;

                if first_line {
                    lines.push(DetailLine {
                        indent,
                        segments: vec![
                            DetailSegment {
                                text: format!("{}: ", label),
                                style: SegmentStyle::Key,
                            },
                            DetailSegment {
                                text: trimmed.to_string(),
                                style: SegmentStyle::Plain,
                            },
                        ],
                    });
                    first_line = false;
                } else if trimmed.is_empty() {
                    lines.push(empty_line(indent + 1 + extra_indent));
                } else {
                    lines.push(DetailLine {
                        indent: indent + 1 + extra_indent,
                        segments: vec![DetailSegment {
                            text: trimmed.to_string(),
                            style: SegmentStyle::Plain,
                        }],
                    });
                }
            }
        }
    }
}

fn empty_line(indent: usize) -> DetailLine {
    DetailLine {
        indent,
        segments: vec![DetailSegment {
            text: String::new(),
            style: SegmentStyle::Plain,
        }],
    }
}

fn detail_key_value(label: &str, value: &str) -> DetailLine {
    DetailLine {
        indent: 0,
        segments: vec![
            DetailSegment {
                text: format!("{}: ", label),
                style: SegmentStyle::Key,
            },
            DetailSegment {
                text: value.to_string(),
                style: SegmentStyle::Plain,
            },
        ],
    }
}

fn format_duration(value: &Value) -> Option<String> {
    let number = value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))?;
    Some(format!("{:.3} ms", number))
}

fn format_bytes(value: &Value) -> Option<String> {
    let mut bytes = value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))?;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut unit_index = 0;
    while bytes >= 1024.0 && unit_index + 1 < units.len() {
        bytes /= 1024.0;
        unit_index += 1;
    }
    Some(format!("{:.2} {}", bytes, units[unit_index]))
}

fn json_value_preview(value: &Value) -> String {
    match value {
        Value::String(text) => text.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Null => "null".to_string(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn ordered_map_entries<'a>(map: &'a Map<String, Value>) -> Vec<(&'a str, &'a Value)> {
    let priority = [
        "PHP version",
        "PHP ini file",
        "PHP scanned ini file",
        "Memory limit",
        "Max post size",
        "Max file upload size",
        "Extensions",
    ];

    let mut seen: HashSet<&'a str> = HashSet::new();
    let mut ordered = Vec::new();

    for key in &priority {
        if let Some(value) = map.get(*key) {
            ordered.push((*key, value));
            seen.insert(*key);
        }
    }

    for (key, value) in map {
        if !seen.contains(key.as_str()) {
            ordered.push((key.as_str(), value));
        }
    }

    ordered
}

fn render_html(label: Option<&str>, html: &str) -> Vec<DetailLine> {
    let mut lines = Vec::new();

    if let Some(label) = label {
        if label.eq_ignore_ascii_case("html") {
            // implied default label, skip showing a label line
        } else {
            lines.push(parse_plain_line(&format!("Label: {}", label)));
            lines.push(parse_plain_line(""));
        }
    }

    let normalized = TAG_GAP_RE.replace_all(html, ">\n<");
    let mut indent = 0usize;

    for raw_line in normalized.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with("</") {
            indent = indent.saturating_sub(1);
        }

        let segments = parse_html_segments(trimmed);
        lines.push(DetailLine { indent, segments });

        if trimmed.starts_with("<")
            && !trimmed.starts_with("</")
            && !trimmed.ends_with("/>")
            && !trimmed.starts_with("<!")
            && !trimmed.starts_with("<?")
        {
            indent = indent.saturating_add(1);
        }
    }

    if lines.is_empty() {
        lines.push(parse_plain_line(html));
    }

    lines
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

fn render_table(payload: &Payload) -> Vec<DetailLine> {
    let content = match payload.content_object() {
        Some(content) => content,
        None => return fallback_lines(payload),
    };

    if let Some(values) = content.get("values").and_then(|value| value.as_array()) {
        if let Some(model) = values
            .iter()
            .find_map(|value| value.as_str().and_then(TableModel::from_html))
        {
            return render_table_model(payload, model);
        }

        if values.is_empty() {
            return vec![parse_plain_line("(empty table)")];
        }

        if let Some(table) = TableModel::from_values(values) {
            return render_table_model(payload, table);
        }

        return fallback_lines(payload);
    }

    if let Some(map) = content.get("values").and_then(|value| value.as_object()) {
        let mut lines = Vec::new();

        if let Some(label) = content
            .get("label")
            .and_then(|value| value.as_str())
            .map(|label| label.trim())
            .filter(|label| !label.is_empty())
        {
            lines.push(parse_plain_line(&format!("Label: {}", label)));
            lines.push(parse_plain_line(""));
        }

        let ordered = ordered_map_entries(map);
        for (key, value) in ordered {
            lines.push(detail_key_value(key, &json_value_preview(value)));
        }

        if lines.is_empty() {
            return fallback_lines(payload);
        }

        return lines;
    }

    fallback_lines(payload)
}

fn render_table_model(payload: &Payload, table: TableModel) -> Vec<DetailLine> {
    let mut lines = Vec::new();

    if let Some(label) = payload
        .content_string("label")
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
    {
        lines.push(parse_plain_line(&format!("Label: {}", label)));
        lines.push(parse_plain_line(""));
    }

    for line in table.to_lines() {
        lines.push(parse_plain_line(&line));
    }

    lines
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
    let mut lines = Vec::new();
    let mut indent = 0usize;

    for raw_line in sanitized.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if is_parenthesis_open(trimmed) {
            indent = indent.saturating_add(1);
            continue;
        }

        if is_parenthesis_close(trimmed) {
            indent = indent.saturating_sub(1);
            continue;
        }

        if starts_with_closing_bracket(trimmed) {
            indent = indent.saturating_sub(1);
        }

        let line = parse_highlighted_line(trimmed, indent);
        lines.push(line);

        if ends_with_open_bracket(trimmed) {
            indent = indent.saturating_add(1);
        }
    }

    lines
}

fn parse_highlighted_line(line: &str, indent: usize) -> DetailLine {
    let trimmed = line;
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
    let without_script = SF_SCRIPT_RE.replace_all(input, "");
    let without_style = SF_STYLE_RE.replace_all(&without_script, "");

    let mut sanitized = without_style
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

fn starts_with_closing_bracket(line: &str) -> bool {
    line.starts_with(']')
        || line.starts_with("]")
        || line.starts_with("] ")
        || line.starts_with("],")
        || line.starts_with('}')
        || line.starts_with("},")
}

fn ends_with_open_bracket(line: &str) -> bool {
    let line = line.trim_end_matches(',').trim_end();
    line.ends_with('[')
        || line.ends_with('{')
        || line.ends_with("=> [")
        || line.ends_with("=> {")
        || line.ends_with("=> array(")
        || line.ends_with("=> array:")
        || line == "["
        || line == "{"
        || line.starts_with("stdClass#")
        || line.contains(" {#") && (line.ends_with('▼') || line.ends_with('▶'))
}

fn is_parenthesis_open(line: &str) -> bool {
    line == "("
}

fn is_parenthesis_close(line: &str) -> bool {
    line == ")" || line == "),"
}

static TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]+>").unwrap());
static SF_SCRIPT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap());
static SF_STYLE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap());
static KEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^(\+?\[[^\]]+\]|\+["'][^"']+["']|[-+][\w$]+:)"#).unwrap());
static TYPE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:stdClass#\d+|array:\d+|object\([^)]*\)|[\w\\]+(?:<[^>]+>)?\s*\{#\d+)").unwrap()
});
static BOOL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(true|false)\b").unwrap());
static NULL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^null\b").unwrap());
static NUMBER_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^-?\d+(?:\.\d+)?").unwrap());
static TABLE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<table[^>]*>(.*?)</table>").unwrap());
static TR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<tr[^>]*>(.*?)</tr>").unwrap());
static TH_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<th[^>]*>(.*?)</th>").unwrap());
static TD_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<td[^>]*>(.*?)</td>").unwrap());
static SCRIPT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap());
static TAG_GAP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r">\s*<").unwrap());
static IMG_SRC_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?is)<img[^>]*src\s*=\s*['"]([^'"]+)['"]"##).unwrap());

fn compute_has_children(lines: &[DetailLine]) -> Vec<bool> {
    let mut result = vec![false; lines.len()];
    for (index, line) in lines.iter().enumerate() {
        let current_indent = line.indent;
        let mut walker = index + 1;
        while walker < lines.len() {
            let next_indent = lines[walker].indent;
            if next_indent <= current_indent {
                break;
            }
            if next_indent == current_indent + 1 {
                result[index] = true;
                break;
            }
            walker += 1;
        }
    }
    result
}

struct TableModel {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl TableModel {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }

        if values.iter().all(|value| value.is_object()) {
            let mut headers: Vec<String> = Vec::new();
            for value in values {
                if let Some(object) = value.as_object() {
                    for key in object.keys() {
                        if !headers.iter().any(|existing| existing == key) {
                            headers.push(key.clone());
                        }
                    }
                }
            }

            let mut rows = Vec::new();
            for value in values {
                if let Some(object) = value.as_object() {
                    let mut cells = Vec::new();
                    for header in &headers {
                        let cell = object
                            .get(header)
                            .map(format_table_value)
                            .unwrap_or_default();
                        cells.push(cell);
                    }
                    rows.push(cells);
                }
            }

            return Some(Self { headers, rows });
        }

        if values.iter().all(|value| value.is_array()) {
            let column_count = values
                .iter()
                .filter_map(|value| value.as_array().map(|array| array.len()))
                .max()
                .unwrap_or(0);

            let headers = (0..column_count)
                .map(|idx| format!("col {}", idx + 1))
                .collect::<Vec<_>>();

            let mut rows = Vec::new();
            for value in values {
                if let Some(array) = value.as_array() {
                    let mut cells = Vec::new();
                    for idx in 0..column_count {
                        let cell = array.get(idx).map(format_table_value).unwrap_or_default();
                        cells.push(cell);
                    }
                    rows.push(cells);
                }
            }

            return Some(Self { headers, rows });
        }

        if values.iter().all(|value| value.is_string()) {
            let mut headers: Vec<String> = Vec::new();
            let mut rows = Vec::new();

            for value in values {
                if let Some(row_map) = value.as_str().and_then(parse_var_dumper_row) {
                    for key in row_map.keys() {
                        if !headers.iter().any(|existing| existing == key) {
                            headers.push(key.clone());
                        }
                    }
                    rows.push(row_map);
                }
            }

            if !rows.is_empty() && !headers.is_empty() {
                let row_cells = rows
                    .into_iter()
                    .map(|row| {
                        headers
                            .iter()
                            .map(|header| row.get(header).cloned().unwrap_or_default())
                            .collect()
                    })
                    .collect();

                return Some(Self {
                    headers,
                    rows: row_cells,
                });
            }
        }

        let headers = vec!["value".to_string()];
        let rows = values
            .iter()
            .map(|value| vec![format_table_value(value)])
            .collect::<Vec<_>>();

        Some(Self { headers, rows })
    }

    fn from_html(html: &str) -> Option<Self> {
        let table_segment = TABLE_RE
            .captures(html)
            .and_then(|capture| capture.get(1))
            .map(|m| m.as_str())?;

        let headers: Vec<String> = TH_RE
            .captures_iter(table_segment)
            .map(|cap| {
                let raw = cap.get(1).map(|m| m.as_str()).unwrap_or("");
                clean_html_text(raw)
            })
            .collect();

        if headers.is_empty() {
            return None;
        }

        let mut rows = Vec::new();
        for row_cap in TR_RE.captures_iter(table_segment) {
            let row_html = row_cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let cells: Vec<String> = TD_RE
                .captures_iter(row_html)
                .map(|cap| {
                    let raw = cap.get(1).map(|m| m.as_str()).unwrap_or("");
                    clean_html_text(raw)
                })
                .collect();
            if !cells.is_empty() {
                rows.push(cells);
            }
        }

        if rows.is_empty() {
            return None;
        }

        Some(Self { headers, rows })
    }

    fn to_lines(&self) -> Vec<String> {
        let mut widths: Vec<usize> = self
            .headers
            .iter()
            .map(|header| display_width(header))
            .collect();

        for row in &self.rows {
            for (idx, cell) in row.iter().enumerate() {
                if let Some(width) = widths.get_mut(idx) {
                    *width = (*width).max(display_width(cell));
                }
            }
        }

        let border = format_border(&widths, '-');
        let separator = format_border(&widths, '=');
        let header_line = format_row(&self.headers, &widths);

        let mut lines = Vec::new();
        lines.push(border.clone());
        lines.push(header_line);
        lines.push(separator);
        for row in &self.rows {
            lines.push(format_row(row, &widths));
        }
        lines.push(border);

        lines
    }
}

fn format_table_value(value: &Value) -> String {
    match value {
        Value::String(text) => clean_html_text(text),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(array) => format!("[array {}]", array.len()),
        Value::Object(object) => format!("{{object {} keys}}", object.len()),
    }
}

fn value_to_plain(value: &Value) -> String {
    match value {
        Value::String(text) => {
            let cleaned = clean_html_text(text);
            if cleaned.is_empty() {
                text.clone()
            } else {
                cleaned
            }
        }
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(array) => format!("[array {}]", array.len()),
        Value::Object(object) => format!("{{object {} keys}}", object.len()),
    }
}

fn parse_html_segments(line: &str) -> Vec<DetailSegment> {
    let mut segments = Vec::new();
    let mut rest = line;

    while !rest.is_empty() {
        if let Some(pos) = rest.find('<') {
            if pos > 0 {
                let (text, tail) = rest.split_at(pos);
                if !text.is_empty() {
                    segments.push(DetailSegment {
                        text: text.to_string(),
                        style: SegmentStyle::String,
                    });
                }
                rest = tail;
                continue;
            }

            if let Some(end) = rest.find('>') {
                let (tag, tail) = rest.split_at(end + 1);
                segments.push(DetailSegment {
                    text: tag.to_string(),
                    style: SegmentStyle::Type,
                });
                rest = tail;
                continue;
            }
        }

        segments.push(DetailSegment {
            text: rest.to_string(),
            style: SegmentStyle::String,
        });
        break;
    }

    if segments.is_empty() {
        segments.push(DetailSegment {
            text: line.to_string(),
            style: SegmentStyle::String,
        });
    }

    segments
}

fn looks_like_html(input: &str) -> bool {
    let trimmed = input.trim();
    trimmed.starts_with('<') && trimmed.contains('>')
}

fn contains_sf_dump(input: &str) -> bool {
    input.contains("sf-dump")
}

fn contains_image_tag(html: &str) -> bool {
    IMG_SRC_RE.is_match(html)
}

fn extract_image_src(html: &str) -> Option<&str> {
    IMG_SRC_RE
        .captures(html)
        .and_then(|capture| capture.get(1))
        .map(|m| m.as_str())
}

fn display_width(text: &str) -> usize {
    text.chars().count()
}

fn truncate(text: &str, max_chars: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max_chars {
        return flat;
    }

    let truncated: String = flat.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{}...", truncated)
}

fn format_border(widths: &[usize], fill: char) -> String {
    let mut line = String::from("+");
    for width in widths {
        line.push_str(&fill.to_string().repeat(width + 2));
        line.push('+');
    }
    line
}

fn format_row(cells: &[String], widths: &[usize]) -> String {
    let mut line = String::from("|");
    for (idx, width) in widths.iter().enumerate() {
        let value = cells.get(idx).map(|cell| cell.as_str()).unwrap_or("");
        line.push(' ');
        line.push_str(&format!("{value:<width$}", value = value, width = *width));
        line.push(' ');
        line.push('|');
    }
    line
}

fn clean_html_text(input: &str) -> String {
    let stripped = strip_html(input);
    truncate(&stripped, 80)
}

fn strip_html(input: &str) -> String {
    let mut text = input.replace("<br>", "\n").replace("<br />", "\n");
    text = SCRIPT_RE.replace_all(&text, "").into_owned();
    let stripped = TAG_RE.replace_all(&text, "");
    decode_html_entities(stripped.trim()).into_owned()
}

fn parse_var_dumper_row(input: &str) -> Option<BTreeMap<String, String>> {
    let stripped = strip_html(input);
    let mut map = BTreeMap::new();

    for line in stripped.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed == "["
            || trimmed == "]"
            || trimmed.ends_with('[')
            || trimmed.starts_with('[')
        {
            continue;
        }

        if let Some((key_part, value_part)) = trimmed.split_once("=>") {
            let key = key_part
                .trim()
                .trim_matches(|c| c == '"' || c == '\'' || c == '`');
            let value_raw = value_part.trim().trim_matches(',');

            if value_raw.ends_with('[') {
                continue;
            }

            if key.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }

            let value_clean = value_raw
                .trim_matches(|c| c == '"' || c == '\'' || c == '`')
                .to_string();
            map.insert(key.to_string(), value_clean);
        }
    }

    if map.is_empty() { None } else { Some(map) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_nested_sf_dump_with_object_markers() {
        let dump = r#"
<span class="sf-dump">array:2 [<br />
  "name" => "Ray"<br />
  "meta" => array:1 [<br />
    "city" => "Ghent"<br />
  ]<br />
  "user" => App\User {#1 ▼<br />
    +name: "Freek"<br />
    +roles: array:1 [<br />
      0 => "admin"<br />
    ]<br />
  }<br />
]<br />
</span>
"#;

        let lines = parse_sf_dump(dump);
        let indents: Vec<usize> = lines.iter().map(|line| line.indent).collect();
        assert_eq!(indents, vec![0, 1, 1, 2, 1, 1, 2, 2, 3, 2, 1, 0]);

        let type_segment_present = lines.iter().any(|line| {
            line.segments.iter().any(|segment| {
                matches!(segment.style, SegmentStyle::Type)
                    && segment.text.contains("App\\User {#1")
            })
        });
        assert!(
            type_segment_present,
            "expected App\\User {{#1 to be treated as a type segment"
        );
    }

    #[test]
    fn renders_table_payload_as_ascii() {
        let html = r#"
<script>window.Sfdump</script>
<table>
  <thead>
    <tr><th>Name</th><th>Email</th></tr>
  </thead>
  <tbody>
    <tr><td>Alice</td><td>alice@example.com</td></tr>
    <tr><td>Bob</td><td>bob@example.com</td></tr>
  </tbody>
</table>
"#;
        let payload: Payload = serde_json::from_value(json!({
            "type": "table",
            "content": {
                "values": [html],
                "label": "Users"
            }
        }))
        .expect("payload should deserialize");

        let lines = render_table(&payload);
        assert_eq!(lines[0].segments[0].text, "Label: Users");
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.segments
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect();

        assert!(rendered.iter().any(|line| line.contains("Name")));
        assert!(rendered.iter().any(|line| line.contains("Alice")));
    }

    #[test]
    fn renders_log_prefers_clipboard_data_over_script() {
        let payload_json = r#"
        {
            "type": "log",
            "content": {
                "meta": [{
                    "clipboard_data": "[\\n    0 => [    \\n        'id' => 1001,\\n        'status' => 'pending',\\n        'total' => 49.5,\\n    ],\\n    1 => [    \\n        'id' => 1002,\\n        'status' => 'paid',\\n        'total' => 125,\\n    ],\\n]"
                }],
                "values": ["<script> SfDump = window.SfDump || (function (doc) { doc.documentElement.classList.add('sf-js-enabled'); });</script>"]
            }
        }
        "#;

        let payload: Payload =
            serde_json::from_str(payload_json).expect("payload should deserialize");
        let lines = render_log(&payload);
        assert!(!lines.is_empty());
        let joined = lines
            .iter()
            .map(|line| line.segments[0].text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("'id' => 1001"),
            "unexpected log output: {}",
            joined
        );
        assert!(!joined.contains("sf-dump"), "script leak: {}", joined);
    }
}
