use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct RayRequest {
    pub uuid: String,
    #[serde(default)]
    pub payloads: Vec<Payload>,
    #[serde(default)]
    pub meta: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Payload {
    #[serde(rename = "type")]
    pub kind: PayloadKind,
    #[serde(default)]
    content: Value,
    #[serde(default)]
    pub origin: Option<Origin>,
}

impl Payload {
    pub fn content_object(&self) -> Option<&serde_json::Map<String, Value>> {
        self.content.as_object()
    }

    pub fn content_string(&self, key: &str) -> Option<&str> {
        self.content_object()
            .and_then(|map| map.get(key))
            .and_then(|value| value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadKind {
    Log,
    Custom,
    CreateLock,
    ClearAll,
    Hide,
    ShowApp,
    ShowBrowser,
    Notify,
    Separator,
    Exception,
    Table,
    Text,
    Image,
    JsonString,
    DecodedJson,
    Boolean,
    Size,
    Color,
    Trace,
    Caller,
    Measure,
    PhpInfo,
    NewScreen,
    Remove,
    HideApp,
    Ban,
    Charles,
    Unknown(String),
}

impl<'de> Deserialize<'de> for PayloadKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input = String::deserialize(deserializer)?;
        let kind = match input.as_str() {
            "log" => Self::Log,
            "custom" => Self::Custom,
            "create_lock" => Self::CreateLock,
            "clear_all" => Self::ClearAll,
            "hide" => Self::Hide,
            "show_app" => Self::ShowApp,
            "show_browser" => Self::ShowBrowser,
            "notify" => Self::Notify,
            "separator" => Self::Separator,
            "exception" => Self::Exception,
            "table" => Self::Table,
            "text" => Self::Text,
            "image" => Self::Image,
            "json_string" => Self::JsonString,
            "decoded_json" => Self::DecodedJson,
            "custom_boolean" | "boolean" => Self::Boolean,
            "size" => Self::Size,
            "color" => Self::Color,
            "trace" => Self::Trace,
            "caller" => Self::Caller,
            "measure" => Self::Measure,
            "phpinfo" | "php_info" => Self::PhpInfo,
            "new_screen" => Self::NewScreen,
            "remove" => Self::Remove,
            "hide_app" => Self::HideApp,
            "ban" => Self::Ban,
            "charles" => Self::Charles,
            other => Self::Unknown(other.to_owned()),
        };

        Ok(kind)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Origin {
    pub file: Option<String>,
    #[serde(default)]
    pub line_number: Option<u32>,
    #[serde(default)]
    pub hostname: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_request() {
        let raw = r#"
        {
            "uuid": "123e4567-e89b-12d3-a456-426614174000",
            "payloads": [
                {
                    "type": "log",
                    "content": {
                        "values": ["hello world"],
                        "meta": []
                    },
                    "origin": {
                        "file": "/app/index.php",
                        "line_number": 42,
                        "hostname": "raygun.local"
                    }
                },
                {
                    "type": "custom",
                    "content": {
                        "content": true,
                        "label": "Boolean"
                    }
                }
            ],
            "meta": {
                "php_version": "8.2.20",
                "project_name": "sandbox"
            }
        }
        "#;

        let request: RayRequest = serde_json::from_str(raw).expect("should parse");

        assert_eq!(request.uuid, "123e4567-e89b-12d3-a456-426614174000");
        assert_eq!(request.payloads.len(), 2);
        assert!(matches!(request.payloads[0].kind, PayloadKind::Log));

        let origin = request.payloads[0]
            .origin
            .as_ref()
            .expect("origin expected");
        assert_eq!(origin.file.as_deref(), Some("/app/index.php"));
        assert_eq!(origin.line_number, Some(42));
        assert_eq!(origin.hostname.as_deref(), Some("raygun.local"));

        assert_eq!(
            request
                .meta
                .get("project_name")
                .and_then(|value| value.as_str()),
            Some("sandbox")
        );

        assert!(matches!(
            request.payloads[1].kind,
            PayloadKind::Custom | PayloadKind::Unknown(_)
        ));
    }

    #[test]
    fn preserves_unknown_payloads() {
        let raw = r#"
        {
            "uuid": "abc",
            "payloads": [
                { "type": "quantum_flux", "content": { "data": 1 } }
            ],
            "meta": {}
        }
        "#;

        let request: RayRequest = serde_json::from_str(raw).expect("should parse");

        match &request.payloads[0].kind {
            PayloadKind::Unknown(kind) => assert_eq!(kind, "quantum_flux"),
            other => panic!("unexpected payload kind: {:?}", other),
        }
    }
}
