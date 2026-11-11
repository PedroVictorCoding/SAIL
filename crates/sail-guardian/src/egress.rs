//! External text is sanitized and bound to an exact approval before transport sees it.
//! This first contract deliberately accepts no retrieved memory or conversation history.
use crate::{Capability, CredentialStore, ToolCapability};
use regex::Regex;
use ring::digest::{SHA256, digest};
use sail_protocol::{Content, InferenceRequest, Role, ToolChoice};
use std::sync::LazyLock;

static IDENTIFIERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9.-]+\.[a-z]{2,}",
        r"|(?:https?://|www\.)[^\s<>]+",
        r"|(?:/home/|/Users/|~/)[^\s<>]+",
        r"|\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b",
        r"|(?:\+?[0-9][0-9 ().-]{5,}[0-9])"
    ))
    .expect("static egress identifier expression")
});

/// Not serializable or cloneable: models cannot fabricate a prepared payload.
pub struct PreparedText {
    text: String,
    sha256: String,
    redactions: usize,
}

impl std::fmt::Debug for PreparedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedText")
            .field("sha256", &self.sha256)
            .field("bytes", &self.text.len())
            .field("redactions", &self.redactions)
            .finish()
    }
}

impl PreparedText {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn redactions(&self) -> usize {
        self.redactions
    }

    pub fn capability(&self, provider: &str, endpoint: &str, model: &str) -> Capability {
        Capability::Tool(ToolCapability::Invocation {
            tool: "external:infer".into(),
            arguments: serde_json::json!({
                "provider": provider, "endpoint": endpoint, "model": model,
                "sha256": self.sha256, "bytes": self.text.len(),
                "purpose": "explicit_text", "memory_items": 0
            })
            .to_string(),
        })
    }
}

/// Names come from the trusted owner/entity inventory, never the model. This does not
/// make arbitrary private memory safe to send: memory is excluded by the input contract.
pub fn prepare_text(
    request: &InferenceRequest,
    vault: &CredentialStore,
    entity_names: &[String],
) -> Result<PreparedText, &'static str> {
    if request.version != sail_protocol::VERSION
        || request.messages.len() != 1
        || request.messages[0].role != Role::User
        || !request.tools.is_empty()
        || request.tool_choice != ToolChoice::None
        || !request.messages[0].tool_calls.is_empty()
        || request.messages[0].tool_call_id.is_some()
        || request.messages[0].name.is_some()
    {
        return Err("external explicit-text inference excludes history, tools and memory");
    }
    let Content::Text(input) = &request.messages[0].content else {
        return Err("external inference accepts text only");
    };
    if input.is_empty()
        || input.len() > 32_768
        || input
            .chars()
            .any(|c| (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'))
    {
        return Err("external text is empty, oversized or contains control characters");
    }
    if entity_names.len() > 4096
        || entity_names
            .iter()
            .any(|n| n.trim().is_empty() || n.len() > 512)
    {
        return Err("invalid trusted entity inventory");
    }
    let mut text = vault.redact_text(input);
    let mut redactions = usize::from(text != *input);
    let mut index = 0;
    let mut replacements = std::collections::HashMap::new();
    text = IDENTIFIERS
        .replace_all(&text, |matched: &regex::Captures<'_>| {
            let value = matched.get(0).expect("full match").as_str().to_string();
            redactions += 1;
            replacements
                .entry(value)
                .or_insert_with(|| {
                    index += 1;
                    format!("[IDENTIFIER_{index}]")
                })
                .clone()
        })
        .into_owned();
    // Longest names first, and case insensitive, so overlapping names cannot leak suffixes.
    let mut names = entity_names.to_vec();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    for (index, name) in names.iter().enumerate() {
        let pattern = Regex::new(&format!("(?i){}", regex::escape(name)))
            .map_err(|_| "entity redaction failed")?;
        redactions += pattern.find_iter(&text).count();
        text = pattern
            .replace_all(&text, format!("[ENTITY_{}]", index + 1))
            .into_owned();
    }
    if text.len() > 32_768 {
        return Err("sanitized external text exceeds its budget");
    }
    // Bind generation and request identity too: changing a budget invalidates approval.
    let binding = serde_json::to_vec(&serde_json::json!({
        "id": request.request_id, "text": text, "generation": request.generation
    }))
    .map_err(|_| "cannot bind external request")?;
    let sha256 = digest(&SHA256, &binding)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(PreparedText {
        text,
        sha256,
        redactions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_text_rejects_each_disallowed_request_field() {
        use sail_protocol::{
            ContentPart, FunctionCall, FunctionDefinition, ImageUrl, Message, ToolCall,
            ToolDefinition,
        };
        let vault = CredentialStore::new();
        let valid = InferenceRequest::text("ordinary text");
        let mut cases = Vec::new();
        let mut request = valid.clone();
        request.version += 1;
        cases.push(request);
        let mut request = valid.clone();
        request.messages.clear();
        cases.push(request);
        let mut request = valid.clone();
        request.messages[0].role = Role::System;
        cases.push(request);
        let mut request = valid.clone();
        request.tool_choice = ToolChoice::Auto;
        cases.push(request);
        let mut request = valid.clone();
        request.tools.push(ToolDefinition {
            kind: "function".into(),
            function: FunctionDefinition {
                name: "read".into(),
                description: String::new(),
                parameters: serde_json::json!({}),
            },
        });
        cases.push(request);
        let mut request = valid.clone();
        request.messages[0].tool_calls.push(ToolCall {
            id: "call".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "read".into(),
                arguments: "{}".into(),
            },
        });
        cases.push(request);
        let mut request = valid.clone();
        request.messages[0].tool_call_id = Some("call".into());
        cases.push(request);
        let mut request = valid.clone();
        request.messages[0].name = Some("memory".into());
        cases.push(request);
        let mut request = valid.clone();
        request.messages[0].content = Content::Parts(vec![ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "private-image".into(),
            },
        }]);
        cases.push(request);
        let mut request = valid.clone();
        request
            .messages
            .push(Message::text(Role::User, "prior turn"));
        cases.push(request);
        for request in cases {
            assert!(
                prepare_text(&request, &vault, &[]).is_err(),
                "accepted disallowed request: {request:?}"
            );
        }
    }

    #[test]
    fn text_inventory_and_sanitized_output_budgets_include_their_exact_boundaries() {
        let vault = CredentialStore::new();
        for length in [1, 32767, 32768] {
            let input = "x".repeat(length);
            assert_eq!(
                prepare_text(&InferenceRequest::text(&input), &vault, &[])
                    .unwrap()
                    .text(),
                input
            );
        }
        for input in [
            String::new(),
            "x".repeat(32769),
            "x".repeat(32770),
            // Oversized raw input must fail even when redaction would shrink it.
            format!("{}@example.org", "x".repeat(32768)),
            "x\0x".into(),
            "x\u{200b}x".into(),
            "x\u{202e}x".into(),
        ] {
            assert!(prepare_text(&InferenceRequest::text(input), &vault, &[]).is_err());
        }
        assert_eq!(
            prepare_text(&InferenceRequest::text("a\nb\rc\td"), &vault, &[])
                .unwrap()
                .text(),
            "a\nb\rc\td"
        );
        let request = InferenceRequest::text("ordinary text");
        for names in [vec!["x".repeat(512)], vec!["unused".into(); 4096]] {
            assert!(prepare_text(&request, &vault, &names).is_ok());
        }
        for names in [
            vec!["".into()],
            vec!["  ".into()],
            vec!["x".repeat(513)],
            vec!["x".repeat(514)],
            vec!["unused".into(); 4097],
            vec!["unused".into(); 4098],
        ] {
            assert!(prepare_text(&request, &vault, &names).is_err());
        }
        // Ten bytes per [ENTITY_1], plus eight untouched bytes, exactly fills the budget.
        let expanded = format!("{}zzzzzzzz", "a".repeat(3276));
        assert_eq!(
            prepare_text(&InferenceRequest::text(&expanded), &vault, &["a".into()])
                .unwrap()
                .text()
                .len(),
            32768
        );
        assert!(
            prepare_text(
                &InferenceRequest::text(format!("a{expanded}")),
                &vault,
                &["a".into()]
            )
            .is_err()
        );
    }

    #[test]
    fn redaction_receipts_and_debug_output_describe_the_actual_sanitized_payload() {
        let vault = CredentialStore::new();
        let plain = prepare_text(&InferenceRequest::text("ordinary text"), &vault, &[]).unwrap();
        assert_eq!(plain.text(), "ordinary text");
        assert_eq!(plain.redactions(), 0);
        let debug = format!("{plain:?}");
        assert!(debug.contains("PreparedText"));
        assert!(debug.contains(plain.sha256()));
        assert!(debug.contains("bytes: 13"));
        assert!(!debug.contains("ordinary text"));
        let identifiers = prepare_text(
            &InferenceRequest::text("a@example.org a@example.org b@example.org"),
            &vault,
            &[],
        )
        .unwrap();
        assert_eq!(
            identifiers.text(),
            "[IDENTIFIER_1] [IDENTIFIER_1] [IDENTIFIER_2]"
        );
        assert_eq!(identifiers.redactions(), 3);
        let entities = prepare_text(
            &InferenceRequest::text("Alice and Bob"),
            &vault,
            &["Alice".into(), "Bob".into()],
        )
        .unwrap();
        assert_eq!(entities.text(), "[ENTITY_1] and [ENTITY_2]");
        assert_eq!(entities.redactions(), 2);
        vault
            .store_credential(
                sail_common::types::SubsystemId::Bridge,
                "key",
                "secret-api-key",
                "external:test:infer",
                chrono::Duration::hours(1),
            )
            .unwrap();
        let secret = prepare_text(&InferenceRequest::text("secret-api-key"), &vault, &[]).unwrap();
        assert_eq!(secret.redactions(), 1);
        assert!(!secret.text().contains("secret-api-key"));
    }

    #[test]
    fn wildcard_learned_rules_cannot_authorize_egress() {
        use crate::{RiskLevel, policy::LearnedPattern};
        let prepared = prepare_text(
            &InferenceRequest::text("explain ownership"),
            &CredentialStore::new(),
            &[],
        )
        .unwrap();
        let capability =
            prepared.capability("test", "https://example.org/v1/chat/completions", "model");
        assert_eq!(capability.risk_level(), RiskLevel::Critical);
        for tool_name in ["*", "external:infer"] {
            assert!(
                !LearnedPattern::AnyToolInvocation {
                    tool_name: tool_name.into()
                }
                .matches(&capability)
            );
        }
        assert!(matches!(
            LearnedPattern::from_capability(&capability),
            LearnedPattern::ExactCapability(_)
        ));
        let ordinary = Capability::Tool(ToolCapability::Invocation {
            tool: "other:tool".into(),
            arguments: "{}".into(),
        });
        assert_eq!(ordinary.risk_level(), RiskLevel::High);
        assert!(matches!(
            LearnedPattern::from_capability(&ordinary),
            LearnedPattern::AnyToolInvocation { .. }
        ));
        let command = Capability::Tool(ToolCapability::Invocation {
            tool: "command:execute".into(),
            arguments: "{}".into(),
        });
        assert_eq!(command.risk_level(), RiskLevel::Critical);
    }

    #[test]
    fn redacts_entities_identifiers_and_vault_secrets_without_logging_text() {
        let request = InferenceRequest::text(
            "Alice at alice@example.org uses /home/alice/key on 192.168.0.1. Call +353 123 456 789; alice@example.org",
        );
        let result = prepare_text(&request, &CredentialStore::new(), &["Alice".into()]).unwrap();
        for private in ["Alice", "alice", "example.org", "/home/", "192.168", "353"] {
            assert!(!result.text().contains(private), "{private}");
        }
        assert!(result.redactions() >= 5);
        assert!(!format!("{result:?}").contains("IDENTIFIER"));
    }

    #[test]
    fn history_and_multimodal_data_are_rejected_and_approval_binds_generation() {
        let vault = CredentialStore::new();
        let mut request = InferenceRequest::text("Explain ownership");
        let first = prepare_text(&request, &vault, &[]).unwrap();
        request.generation.max_tokens += 1;
        assert_ne!(
            first.sha256(),
            prepare_text(&request, &vault, &[]).unwrap().sha256()
        );
        request
            .messages
            .push(sail_protocol::Message::text(Role::System, "private memory"));
        assert!(prepare_text(&request, &vault, &[]).is_err());
    }
}
