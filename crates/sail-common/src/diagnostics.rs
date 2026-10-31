//! Bounded diagnostic formatting: never dump audio or image buffers into runner logs.
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
type Redactor = Arc<dyn Fn(&str) -> String + Send + Sync>;
static REDACTOR: RwLock<Option<Redactor>> = RwLock::new(None);
pub fn register_redactor(redactor: Redactor) {
    *REDACTOR.write().unwrap() = Some(redactor);
}
pub fn redact(text: &str) -> String {
    REDACTOR
        .read()
        .unwrap()
        .as_ref()
        .map_or_else(|| text.to_owned(), |redactor| redactor(text))
}

/// Sanitize string values before JSON encoding/truncation, including escaped secrets.
pub fn redact_json(value: &Value) -> Value {
    fn visit(value: &Value, depth: usize) -> Value {
        if depth > 16 {
            return Value::String("<nested data omitted>".into());
        }
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| {
                        let private = matches!(
                            key.to_ascii_lowercase().as_str(),
                            "authorization"
                                | "password"
                                | "secret"
                                | "api_key"
                                | "access_token"
                                | "approval_token"
                        );
                        (
                            key.clone(),
                            if private {
                                Value::String("[REDACTED]".into())
                            } else {
                                visit(value, depth + 1)
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| visit(item, depth + 1)).collect())
            }
            Value::String(text) => {
                // Tool arguments and command plans may themselves be JSON strings.
                // Decode those before redaction so escaped credential bytes cannot leak.
                if matches!(text.trim_start().chars().next(), Some('{') | Some('[')) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                        return Value::String(visit(&parsed, depth + 1).to_string());
                    }
                }
                Value::String(redact(text))
            }
            _ => value.clone(),
        }
    }
    visit(value, 0)
}

pub fn log_json(value: &Value, limit: usize) -> String {
    fn summary(value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(fields.iter().map(|(key, value)| {
                let value = if matches!(key.to_ascii_lowercase().as_str(), "pcm" | "audio_base64" | "data_base64" | "data" | "authorization" | "password" | "secret" | "api_key" | "access_token" | "approval_token") {
                    json!({"payload_omitted":true,"size":value.as_str().map(str::len).or_else(||value.as_array().map(Vec::len))})
                } else { summary(value) };
                (key.clone(), value)
            }).collect()),
            Value::Array(items) => {
                let mut values:Vec<_>=items.iter().take(16).map(summary).collect();
                if items.len()>16 {values.push(json!({"remaining_items":items.len()-16}));}
                Value::Array(values)
            },
            Value::String(text) if text.starts_with("data:image/") => json!("<image data URL omitted>"),
            Value::String(text) if text.chars().count()>1024 => json!(format!("{}… <{} bytes total>",text.chars().take(1024).collect::<String>(),text.len())),
            _=>value.clone(),
        }
    }
    let text = summary(&redact_json(value)).to_string();
    if text.chars().count() > limit {
        format!(
            "{}… <truncated>",
            text.chars().take(limit).collect::<String>()
        )
    } else {
        text
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_payloads_are_omitted_and_unicode_is_bounded() {
        let log = log_json(
            &json!({"pcm":"private-audio","params":{"data":[1,2,3]},"image_url":{"url":"data:image/png;base64,private-image"},"text":"Olá"}),
            1000,
        );
        assert!(!log.contains("private-audio"));
        assert!(!log.contains("private-image"));
        assert!(log.contains("Olá"));
        assert!(log.contains("payload_omitted"));
        assert!(
            log_json(&json!({"text":"á".repeat(10000)}), 32)
                .chars()
                .count()
                < 64
        );
    }
}
#[cfg(test)]
mod redaction_tests {
    use super::*;
    #[test]
    fn escaped_secrets_in_nested_json_arguments_are_redacted_before_truncation() {
        register_redactor(Arc::new(|text| {
            text.replace("s\\\"ecret-value", "[REDACTED]")
        }));
        let arguments = json!({"query":"s\\\"ecret-value"}).to_string();
        let output = log_json(
            &json!({"arguments":arguments,"Authorization":"Bearer private"}),
            4096,
        );
        assert!(!output.contains("ecret-value"));
        assert!(!output.contains("Bearer private"));
        assert!(output.contains("REDACTED"));
    }
}
