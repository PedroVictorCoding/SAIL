//! Tool-call argument healing (#237). Small local models often send almost-right
//! arguments. Healing fixes only unambiguous shapes; anything else becomes a short,
//! correctable error returned to the model as the tool result.
use crate::core::Invalid;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// Heals `raw` against `schema` (a JSON Schema object with `properties`) and decodes it.
/// Returns the value and the healing steps applied, for debug logging.
pub fn decode<T: DeserializeOwned>(raw: &str, schema: &Value) -> Result<(T, Vec<String>), Invalid> {
    let (value, steps) = heal(raw, schema)?;
    let typed = serde_json::from_value(value).map_err(|e| {
        // serde messages already name the field ("missing field `prompt`"); keep them short.
        let mut message = e.to_string();
        message.truncate(240);
        Invalid::new(format!("invalid arguments: {message}"))
    })?;
    Ok((typed, steps))
}

pub fn heal(raw: &str, schema: &Value) -> Result<(Value, Vec<String>), Invalid> {
    let mut steps = Vec::new();
    let raw = if raw.trim().is_empty() { "{}" } else { raw };
    let mut value: Value = serde_json::from_str(raw).map_err(|e| {
        Invalid::new(format!(
            "arguments are not valid JSON ({e}); send one JSON object"
        ))
    })?;
    // 1. A JSON string that itself holds an object.
    if let Value::String(inner) = &value
        && let Ok(parsed @ Value::Object(_)) = serde_json::from_str::<Value>(inner)
    {
        value = parsed;
        steps.push("parsed stringified object".into());
    }
    // 2. A single `{"arguments": …}` wrapper (object, or string holding an object).
    if let Value::Object(map) = &value
        && map.len() == 1
        && let Some(inner) = map.get("arguments")
    {
        let unwrapped = match inner {
            Value::Object(_) => Some(inner.clone()),
            Value::String(s) => serde_json::from_str::<Value>(s)
                .ok()
                .filter(Value::is_object),
            _ => None,
        };
        if let Some(inner) = unwrapped {
            value = inner;
            steps.push("unwrapped `arguments`".into());
        }
    }
    let Value::Object(mut map) = value else {
        return Err(Invalid::new("arguments must be a JSON object"));
    };
    // 3. Field-level coercions the schema makes unambiguous.
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        coerce_fields(&mut map, properties, &mut steps);
    }
    Ok((Value::Object(map), steps))
}

fn coerce_fields(
    map: &mut Map<String, Value>,
    properties: &Map<String, Value>,
    steps: &mut Vec<String>,
) {
    for (name, field) in map.iter_mut() {
        let Some(expected) = properties
            .get(name)
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Value::String(text) = field else { continue };
        let healed = match expected {
            "array" | "object" => serde_json::from_str::<Value>(text).ok().filter(|v| {
                (expected == "array" && v.is_array()) || (expected == "object" && v.is_object())
            }),
            "integer" => text.trim().parse::<i64>().ok().map(Value::from),
            "number" => text
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .map(Value::from),
            _ => None,
        };
        if let Some(healed) = healed {
            *field = healed;
            steps.push(format!("coerced `{name}` to {expected}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({"type":"object","properties":{
            "prompt":{"type":"string"},
            "options":{"type":"array"},
            "confidence":{"type":"integer"},
            "meta":{"type":"object"}
        }})
    }

    #[test]
    fn heals_only_unambiguous_shapes() {
        let cases = [
            (r#"{"prompt":"p"}"#, json!({"prompt":"p"}), 0),
            (r#""{\"prompt\":\"p\"}""#, json!({"prompt":"p"}), 1),
            (r#"{"arguments":{"prompt":"p"}}"#, json!({"prompt":"p"}), 1),
            (
                r#"{"arguments":"{\"prompt\":\"p\"}"}"#,
                json!({"prompt":"p"}),
                1,
            ),
            (r#"{"options":"[1,2]"}"#, json!({"options":[1,2]}), 1),
            (r#"{"confidence":" 3 "}"#, json!({"confidence":3}), 1),
            (r#"{"meta":"{\"a\":1}"}"#, json!({"meta":{"a":1}}), 1),
            ("", json!({}), 0),
        ];
        for (raw, expected, steps) in cases {
            let (value, applied) = heal(raw, &schema()).unwrap();
            assert_eq!(value, expected, "{raw}");
            assert_eq!(applied.len(), steps, "{raw}: {applied:?}");
        }
    }

    #[test]
    fn leaves_ambiguous_input_alone_or_rejects_it() {
        // A prompt that happens to look like JSON stays a string.
        let (v, _) = heal(r#"{"prompt":"[1,2]"}"#, &schema()).unwrap();
        assert_eq!(v, json!({"prompt":"[1,2]"}));
        // A wrapper with siblings is not unwrapped.
        let (v, _) = heal(r#"{"arguments":{"a":1},"prompt":"p"}"#, &schema()).unwrap();
        assert_eq!(v["prompt"], "p");
        // Non-numeric integers stay strings (and fail typed decoding later).
        let (v, _) = heal(r#"{"confidence":"high"}"#, &schema()).unwrap();
        assert_eq!(v["confidence"], "high");
        // Trailing prose and non-objects are errors the model can act on.
        assert!(
            heal(r#"{"prompt":"p"} thanks"#, &schema())
                .unwrap_err()
                .0
                .contains("not valid JSON")
        );
        assert!(
            heal("[1]", &schema())
                .unwrap_err()
                .0
                .contains("JSON object")
        );
    }

    #[test]
    fn typed_errors_name_the_field() {
        #[derive(serde::Deserialize, Debug)]
        #[allow(dead_code)]
        struct Args {
            prompt: String,
        }
        let error = decode::<Args>("{}", &schema()).unwrap_err().0;
        assert!(error.contains("prompt"), "{error}");
    }
}
