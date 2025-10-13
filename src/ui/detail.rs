use std::time::{SystemTime, UNIX_EPOCH};

use html_escape::decode_html_entities;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;

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
        PayloadKind::Table => render_table(payload),
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

fn render_table(payload: &Payload) -> Vec<DetailLine> {
    let content = match payload.content_object() {
        Some(content) => content,
        None => return fallback_lines(payload),
    };

    let values = match content.get("values").and_then(|value| value.as_array()) {
        Some(values) => values,
        None => return fallback_lines(payload),
    };

    if let Some(model) = values
        .iter()
        .find_map(|value| value.as_str().and_then(TableModel::from_html))
    {
        return render_table_model(payload, model);
    }

    if values.is_empty() {
        return vec![parse_plain_line("(empty table)")];
    }

    let table = match TableModel::from_values(values) {
        Some(table) => table,
        None => return fallback_lines(payload),
    };

    render_table_model(payload, table)
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
            .map(|cap| clean_html_text(cap.get(1).map(|m| m.as_str()).unwrap_or("")))
            .collect();

        if headers.is_empty() {
            return None;
        }

        let mut rows = Vec::new();
        for row_cap in TR_RE.captures_iter(table_segment) {
            let row_html = row_cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let cells: Vec<String> = TD_RE
                .captures_iter(row_html)
                .map(|cap| clean_html_text(cap.get(1).map(|m| m.as_str()).unwrap_or("")))
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
        Value::String(text) => truncate(text, 80),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(array) => format!("[array {}]", array.len()),
        Value::Object(object) => format!("{{object {} keys}}", object.len()),
    }
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
    let without_script = SCRIPT_RE.replace_all(input, "");
    let stripped = TAG_RE.replace_all(&without_script, "");
    let decoded = decode_html_entities(stripped.trim()).into_owned();
    truncate(&decoded, 80)
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
}
