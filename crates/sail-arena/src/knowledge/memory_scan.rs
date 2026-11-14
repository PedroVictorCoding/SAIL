//! The injection scan every candidate memory line must pass before validation.
//!
//! Memory is text the model may later see as instructions, so the scanner rejects text
//! shaped like model instructions, exfiltration, invisible characters or credentials.
//! It is pure and has no regex dependency: the patterns are small, explicit matchers.
use serde::{Deserialize, Serialize};

/// One reason a candidate line must not become memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanFinding {
    /// Stable rule id, e.g. `prompt_injection_ignore` or `leaked_token`.
    pub rule: String,
    /// At most 80 characters; a credential is reduced to its first 8 characters and `…`.
    pub excerpt: String,
}

/// Pure. Flags text that should never become memory.
pub fn scan(text: &str) -> Vec<ScanFinding> {
    let lower = text.to_lowercase();
    let mut findings = Vec::new();
    instruction_findings(text, &lower, &mut findings);
    exfiltration_findings(&lower, &mut findings);
    if let Some(character) = text.chars().find(|character| is_invisible(*character)) {
        findings.push(ScanFinding {
            rule: "invisible_unicode".to_string(),
            excerpt: format!("U+{:04X}", character as u32),
        });
    }
    credential_findings(text, &mut findings);
    findings
}

fn instruction_findings(text: &str, lower: &str, out: &mut Vec<ScanFinding>) {
    const IGNORE: &[&str] = &[
        "ignore previous instructions",
        "ignore previous instruction",
        "ignore prior instructions",
        "ignore all previous instructions",
        "ignore all prior instructions",
        "ignore the previous instructions",
        "ignore as instruções anteriores",
        "ignore as instrucoes anteriores",
        "ignore todas as instruções anteriores",
        "ignore todas as instrucoes anteriores",
        "desconsidere as instruções anteriores",
        "desconsidere as instrucoes anteriores",
        "desconsidere todas as instruções anteriores",
        "desconsidere todas as instrucoes anteriores",
    ];
    if let Some(matched) = first_phrase(lower, IGNORE) {
        push(out, "prompt_injection_ignore", &matched);
    }

    const SYSTEM: &[&str] = &[
        "you are now",
        "new system prompt",
        "override system prompt",
        "system prompt",
        "prompt do sistema",
        "prompt de sistema",
        "novo prompt do sistema",
        "novo prompt de sistema",
        "você agora é",
        "voce agora e",
        "agora você é",
        "agora voce e",
    ];
    if let Some(matched) = first_phrase(lower, SYSTEM) {
        push(out, "sys_prompt_override", &matched);
    }

    const ALWAYS: &[&str] = &[
        "always reply",
        "always respond",
        "always answer",
        "sempre responda",
        "responda sempre",
    ];
    if let Some(matched) = first_phrase(lower, ALWAYS) {
        push(out, "always_reply", &matched);
    }

    if tool_call_shaped(text) {
        push(out, "tool_call_json", text);
    }
}

fn first_phrase(haystack: &str, phrases: &[&str]) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for phrase in phrases {
        if let Some(index) = phrase_index(haystack, phrase)
            && best.is_none_or(|(best, _)| index < best)
        {
            best = Some((index, phrase));
        }
    }
    best.map(|(_, phrase)| phrase.to_string())
}

fn phrase_index(haystack: &str, phrase: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(phrase) {
        let index = from + offset;
        let after = index + phrase.len();
        let before_ok = haystack[..index]
            .chars()
            .next_back()
            .is_none_or(|character| !is_word(character));
        let after_ok = haystack[after..]
            .chars()
            .next()
            .is_none_or(|character| !is_word(character));
        if before_ok && after_ok {
            return Some(index);
        }
        from = index + phrase.len();
    }
    None
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn tool_call_shaped(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.contains("\"tool_calls\"")
        || lower.contains("\"tool_call\"")
        || lower.contains("<|tool_call|>")
        || lower.contains("[tool_call]")
    {
        return true;
    }
    let trimmed = lower.trim_start();
    (trimmed.starts_with('{') || trimmed.starts_with("```"))
        && lower.contains("\"arguments\"")
        && (lower.contains("\"name\"")
            || lower.contains("\"function\"")
            || lower.contains("\"tool\""))
}

fn exfiltration_findings(lower: &str, out: &mut Vec<ScanFinding>) {
    let mut query = None;
    let mut image = None;
    let mut send = None;
    for (index, _) in lower.char_indices() {
        if !(lower[index..].starts_with("http://") || lower[index..].starts_with("https://")) {
            continue;
        }
        let url = &lower[index..url_end(lower, index)];
        if image.is_none() && index >= 2 && lower[..index].ends_with("](") {
            let line_start = lower[..index].rfind('\n').map_or(0, |byte| byte + 1);
            if lower[line_start..index].contains("![") {
                image = Some(url.to_string());
            }
        }
        if query.is_none() && url.contains('?') && url.contains('=') {
            query = Some(url.to_string());
        }
        if send.is_none() {
            let from = lower[..index]
                .char_indices()
                .rev()
                .nth(79)
                .map(|(byte, _)| byte)
                .unwrap_or(0);
            let prefix = &lower[from..index];
            if (prefix.contains("send")
                || prefix.contains("post")
                || prefix.contains("envie")
                || prefix.contains("poste")
                || prefix.contains("mande"))
                && (prefix.contains(" to ")
                    || prefix.contains(" para ")
                    || prefix.contains(" em ")
                    || prefix.contains(" no "))
            {
                send = Some(url.to_string());
            }
        }
    }
    if let Some(url) = query {
        push(out, "url_with_query_data", &url);
    }
    if let Some(url) = image {
        push(out, "md_image_remote", &url);
    }
    if let Some(url) = send {
        push(out, "send_to_url", &url);
    }
}

fn url_end(text: &str, start: usize) -> usize {
    for (offset, character) in text[start..].char_indices() {
        if character.is_whitespace()
            || matches!(character, ')' | ']' | '}' | '"' | '\'' | '<' | '>' | '`')
        {
            return start + offset;
        }
    }
    text.len()
}

fn is_invisible(character: char) -> bool {
    matches!(character as u32,
        0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0xFEFF)
}

fn credential_findings(text: &str, out: &mut Vec<ScanFinding>) {
    if let Some(index) = private_key_at(text) {
        let excerpt: String = text[index..].chars().take(48).collect();
        push(out, "embedded_private_key", &excerpt);
    }
    if let Some((index, length)) = leaked_token_at(text) {
        let matched: String = text[index..].chars().take(length).collect();
        let mut excerpt: String = matched.chars().take(8).collect();
        excerpt.push('…');
        out.push(ScanFinding {
            rule: "leaked_token".to_string(),
            excerpt,
        });
    }
}

fn private_key_at(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(offset) = text[from..].find("-----BEGIN ") {
        let index = from + offset;
        let window: String = text[index..].chars().take(48).collect();
        if window.contains("PRIVATE KEY-----") {
            return Some(index);
        }
        from = index + "-----BEGIN ".len();
    }
    None
}

fn leaked_token_at(text: &str) -> Option<(usize, usize)> {
    const PREFIXES: &[(&str, &str, usize)] = &[
        ("github_pat_", "github_pat", 22),
        ("sk-ant-", "anthropic", 20),
        ("glpat-", "gitlab", 20),
        ("ghp_", "github", 36),
        ("gho_", "github", 36),
        ("ghs_", "github", 36),
        ("ghu_", "github", 36),
        ("AKIA", "aws", 16),
        ("sk-", "openai", 40),
    ];
    for (index, character) in text.char_indices() {
        if index > 0 {
            let before = text[..index].chars().next_back();
            if before.is_some_and(is_word) {
                continue;
            }
        }
        if !character.is_ascii() {
            continue;
        }
        for &(prefix, class, minimum) in PREFIXES {
            let Some(rest) = text[index..].strip_prefix(prefix) else {
                continue;
            };
            let mut length = 0;
            let mut bytes = 0;
            for next in rest.chars() {
                if !token_class(next, class) {
                    break;
                }
                length += 1;
                bytes += next.len_utf8();
            }
            if length < minimum {
                continue;
            }
            let end = index + prefix.len() + bytes;
            let after = text[end..].chars().next();
            if after.is_none_or(|character| !is_word(character)) {
                return Some((index, prefix.len() + bytes));
            }
        }
    }
    None
}

fn token_class(character: char, class: &str) -> bool {
    match class {
        "aws" => character.is_ascii_uppercase() || character.is_ascii_digit(),
        "anthropic" | "gitlab" => {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
        }
        "github_pat" => character.is_ascii_alphanumeric() || character == '_',
        _ => character.is_ascii_alphanumeric(),
    }
}

fn push(out: &mut Vec<ScanFinding>, rule: &str, excerpt: &str) {
    out.push(ScanFinding {
        rule: rule.to_string(),
        excerpt: clip(excerpt),
    });
}

fn clip(text: &str) -> String {
    text.chars()
        .take(80)
        .map(|character| {
            if character.is_control() && character != '\t' {
                '�'
            } else {
                character
            }
        })
        .collect()
}
