//! Egress redaction: each class, stable placeholders, round trip and secrets.

use sail_mind::escalation::redact::{redact, rehydrate};

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn redacts_each_class_with_stable_placeholders() {
    let text = "Email ana@example.com, phone +34 600 123 456, ip 192.168.1.10, \
                path /home/ana/secret.txt, card 4111111111111111, and Ana met Bob.";
    let redacted = redact(text, &names(&["Ana", "Bob"])).expect("redaction");
    assert!(redacted.text.contains("[EMAIL_1]"), "{}", redacted.text);
    assert!(redacted.text.contains("[PHONE_1]"), "{}", redacted.text);
    assert!(redacted.text.contains("[IP_1]"), "{}", redacted.text);
    assert!(redacted.text.contains("[PATH_1]"), "{}", redacted.text);
    assert!(redacted.text.contains("[NUMBER_1]"), "{}", redacted.text);
    assert!(redacted.text.contains("[NAME_1]"), "{}", redacted.text);
    assert!(redacted.text.contains("[NAME_2]"), "{}", redacted.text);
    assert!(!redacted.text.contains("ana@example.com"));
    assert!(!redacted.text.contains("192.168.1.10"));
    assert!(!redacted.text.contains("4111111111111111"));
    assert!(!redacted.text.contains("/home/ana"));
    assert_eq!(
        redacted.counts,
        vec![
            ("EMAIL".to_string(), 1),
            ("PHONE".to_string(), 1),
            ("IP".to_string(), 1),
            ("PATH".to_string(), 1),
            ("NUMBER".to_string(), 1),
            ("NAME".to_string(), 2),
        ]
    );
    assert_eq!(rehydrate(&redacted.text, &redacted.map), text);
}

#[test]
fn the_same_value_gets_the_same_placeholder() {
    let text = "Mail ana@example.com, then mail ana@example.com again.";
    let redacted = redact(text, &[]).expect("redaction");
    assert_eq!(redacted.text.matches("[EMAIL_1]").count(), 2);
    assert_eq!(redacted.counts, vec![("EMAIL".to_string(), 2)]);
    assert_eq!(redacted.map.entries.len(), 1);
    assert_eq!(rehydrate(&redacted.text, &redacted.map), text);
}

#[test]
fn names_are_case_insensitive_and_longest_first() {
    let text = "Ana María asked; ana answered; ANA left.";
    let redacted = redact(text, &names(&["ana", "Ana María"])).expect("redaction");
    assert!(redacted.text.starts_with("[NAME_1] asked"));
    assert!(redacted.text.contains("[NAME_2] answered"));
    assert!(redacted.text.contains("[NAME_3] left"));
    assert_eq!(rehydrate(&redacted.text, &redacted.map), text);
}

#[test]
fn names_only_match_whole_words() {
    let text = "The announcement mentions Ann once.";
    let redacted = redact(text, &names(&["Ann"])).expect("redaction");
    assert!(redacted.text.contains("announcement"));
    assert!(redacted.text.contains("[NAME_1] once"));
}

#[test]
fn home_paths_and_tilde_paths_are_redacted() {
    let text = "Read ~/notes/today.md and /home/ana/.ssh/id_rsa, please.";
    let redacted = redact(text, &[]).expect("redaction");
    assert!(redacted.text.contains("[PATH_1]"));
    assert!(redacted.text.contains("[PATH_2]"));
    assert!(!redacted.text.contains("~/notes"));
    assert!(!redacted.text.contains("/home/ana"));
    // The sentence comma and period survive.
    assert!(redacted.text.contains(", please."));
    assert_eq!(rehydrate(&redacted.text, &redacted.map), text);
}

#[test]
fn phone_numbers_with_separators_are_redacted_but_versions_are_ip_first() {
    let text = "Call 555-123-4567 or (555) 765-4321; host 10.0.0.7.";
    let redacted = redact(text, &[]).expect("redaction");
    assert!(redacted.text.contains("[PHONE_1]"));
    assert!(redacted.text.contains("[PHONE_2]"));
    assert!(redacted.text.contains("[IP_1]"));
    assert!(!redacted.text.contains("555-123-4567"));
    assert!(!redacted.text.contains("10.0.0.7"));
    assert_eq!(rehydrate(&redacted.text, &redacted.map), text);
}

#[test]
fn credentials_are_removed_and_never_rehydrated() {
    let text = "Use token ghp_abcdef1234567890 and key sk_live_abcdef123456.";
    let redacted = redact(text, &[]).expect("redaction");
    assert!(!redacted.text.contains("ghp_abcdef1234567890"));
    assert!(!redacted.text.contains("sk_live_abcdef123456"));
    assert!(redacted.text.contains("[REDACTED_SECRET]"));
    let restored = rehydrate(&redacted.text, &redacted.map);
    assert!(!restored.contains("ghp_"));
    assert!(!restored.contains("sk_live_"));
}

#[test]
fn unknown_placeholders_are_left_alone() {
    let redacted = redact("Mail ana@example.com.", &[]).expect("redaction");
    let restored = rehydrate("[FOO_1] said [EMAIL_1] and [unknown]", &redacted.map);
    assert_eq!(restored, "[FOO_1] said ana@example.com and [unknown]");
}

#[test]
fn placeholder_syntax_in_a_known_name_fails_closed() {
    let error = redact("Who is Ana?", &names(&["[NAME_1]"])).expect_err("must fail");
    assert!(error.contains("placeholder syntax"));
    let error = redact("Who is Ana?", &names(&["   "])).expect_err("must fail");
    assert!(error.contains("empty"));
}

#[test]
fn non_ascii_text_is_untouched_around_replacements() {
    let text = "Zoë from 東京 wrote ana@example.com about naïve plans.";
    let redacted = redact(text, &names(&["Zoë"])).expect("redaction");
    assert!(redacted.text.contains("[NAME_1] from 東京 wrote [EMAIL_1]"));
    assert_eq!(rehydrate(&redacted.text, &redacted.map), text);
}

#[test]
fn counts_only_include_used_classes_in_a_stable_order() {
    let redacted = redact("Bob and ana@example.com.", &names(&["Bob"])).expect("redaction");
    assert_eq!(
        redacted.counts,
        vec![("EMAIL".to_string(), 1), ("NAME".to_string(), 1),]
    );
}
