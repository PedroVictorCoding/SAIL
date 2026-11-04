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
