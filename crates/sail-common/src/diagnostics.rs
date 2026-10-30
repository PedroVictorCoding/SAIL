//! Bounded diagnostic formatting: never dump audio or image buffers into runner logs.
use serde_json::{Value, json};

pub fn log_json(value: &Value, limit: usize) -> String {
    fn summary(value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(fields.iter().map(|(key, value)| {
                let value = if matches!(key.as_str(), "pcm" | "audio_base64" | "data_base64" | "data") {
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
    let text = summary(value).to_string();
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
