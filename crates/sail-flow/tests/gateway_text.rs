use sail_flow::gateway::text::{approval_card, chunks, plain};

#[test]
fn empty_and_short_inputs() {
    assert!(chunks("", 10).is_empty());
    assert!(chunks("abc", 0).is_empty());
    assert_eq!(chunks("abc", 10), vec!["abc".to_string()]);
    assert_eq!(chunks("abc", 3), vec!["abc".to_string()]);
}

#[test]
fn hard_wraps_without_splitting_characters() {
    let text = "x".repeat(45);
    let parts = chunks(&text, 10);
    assert_eq!(
        parts
            .iter()
            .map(|part| part.chars().count())
            .collect::<Vec<_>>(),
        vec![10, 10, 10, 10, 5]
    );
    assert_eq!(parts.concat(), text);

    let text = "é".repeat(25);
    let parts = chunks(&text, 10);
    assert_eq!(
        parts
            .iter()
            .map(|part| part.chars().count())
            .collect::<Vec<_>>(),
        vec![10, 10, 5]
    );
    assert_eq!(parts.concat(), text);
    assert!(parts.iter().all(|part| !part.is_empty()));
}

#[test]
fn prefers_paragraph_then_line_then_sentence_then_space() {
    let text = format!("{}\n\n{}", "a".repeat(30), "b".repeat(30));
    let parts = chunks(&text, 40);
    assert_eq!(parts.len(), 2);
    assert!(parts[0].ends_with("\n\n"));
    assert_eq!(parts.concat(), text);

    let text = format!("{}\n{}", "a".repeat(10), "b".repeat(30));
    let parts = chunks(&text, 20);
    assert!(parts[0].ends_with('\n'));
    assert_eq!(parts.concat(), text);

    let text = format!("One. Two. {}", "x".repeat(30));
    let parts = chunks(&text, 12);
    assert_eq!(parts[0], "One. Two. ");
    assert_eq!(parts.concat(), text);

    let text = "word ".repeat(20);
    let parts = chunks(&text, 20);
    assert!(parts.iter().all(|part| part.chars().count() <= 20));
    assert_eq!(parts.concat(), text);
}

#[test]
fn short_fences_stay_whole_and_long_fences_wrap() {
    let fence = "```\ncode\n```\n";
    let text = format!("{fence}{}", "tail ".repeat(20));
    let parts = chunks(&text, 20);
    assert!(
        parts[0].contains("```\ncode\n```"),
        "short fence must stay in one chunk: {:?}",
        parts[0]
    );
    assert_eq!(parts.concat(), text);

    let long = format!("```\n{}\n```", "x".repeat(50));
    let parts = chunks(&long, 20);
    assert_eq!(parts.concat(), long);
    assert!(parts.iter().all(|part| !part.is_empty()));
}

#[test]
fn plain_strips_markers_and_headings_but_keeps_lists_and_code() {
    assert_eq!(
        plain("**bold** and __under__ and `code`"),
        "bold and under and code"
    );
    assert_eq!(plain("# Heading\n- item\n## Sub"), "Heading\n- item\nSub");
    assert_eq!(plain("  ### Indented"), "  Indented");
    assert_eq!(plain("#NoSpace"), "#NoSpace");
    assert_eq!(plain("```\n**keep** `raw`\n```"), "**keep** `raw`");
    assert_eq!(plain("- one\n- two"), "- one\n- two");
    assert_eq!(plain(""), "");
}

#[test]
fn approval_card_is_exact() {
    assert_eq!(
        approval_card("send an email", "You asked for a draft.", "a1b2c3"),
        "SAIL wants to send an email\nYou asked for a draft.\nApprove with /approve a1b2c3 or the buttons."
    );
}

#[test]
fn text_helpers_are_pure() {
    let text = format!("{}\n\n{}", "a".repeat(30), "b".repeat(30));
    assert_eq!(chunks(&text, 40), chunks(&text, 40));
    assert_eq!(plain("# Heading\n**bold**"), plain("# Heading\n**bold**"));
    assert_eq!(
        approval_card("act", "why", "abc123"),
        approval_card("act", "why", "abc123")
    );
}
