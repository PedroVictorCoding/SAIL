//! Small, dependency-free HTML to readable text for the research fetcher.
//!
//! It is deliberately modest: drop script/style/navigation blocks, prefer `<p>` paragraphs,
//! fall back to tag-stripped text. It never panics on malformed or non-ASCII input, and it
//! is not a browser parser.

/// Page title, when one is present.
pub fn title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = lower[start..].find('>')?;
    let content_start = start + open_end + 1;
    let end = lower[content_start..].find("</title>")?;
    let raw = &html[content_start..content_start + end];
    let cleaned = collapse(&entities(raw));
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Readable text: up to `max_paragraphs` substantial paragraphs joined by blank lines, or
/// the tag-stripped body when the page has no useful paragraphs.
pub fn readable(html: &str, max_paragraphs: usize) -> String {
    let mut cleaned = html.to_string();
    for tag in [
        "script", "style", "svg", "noscript", "nav", "header", "footer",
    ] {
        cleaned = strip_tag_blocks(&cleaned, tag);
    }
    let lower = cleaned.to_ascii_lowercase();
    let mut paragraphs: Vec<String> = Vec::new();
    let mut position = 0;
    while paragraphs.len() < max_paragraphs {
        let Some(relative) = lower[position..].find("<p") else {
            break;
        };
        let start = position + relative;
        let after = start + 2;
        let is_paragraph = lower[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_whitespace() || c == '>');
        if !is_paragraph {
            position = after;
            continue;
        }
        let Some(relative_end) = lower[after..].find('>') else {
            break;
        };
        let content_start = after + relative_end + 1;
        let Some(relative_close) = lower[content_start..].find("</p>") else {
            break;
        };
        let inner = &cleaned[content_start..content_start + relative_close];
        let text = collapse(&entities(&strip_tags(inner)));
        if text.chars().count() >= 45 && !paragraphs.contains(&text) {
            paragraphs.push(text);
        }
        position = content_start + relative_close + 4;
    }
    if paragraphs.is_empty() {
        let text = collapse(&entities(&strip_tags(&cleaned)));
        return text;
    }
    paragraphs.join("\n\n")
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut name = String::new();
    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                name.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let tag = name
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .to_ascii_lowercase();
                if matches!(
                    tag.as_str(),
                    "p" | "div"
                        | "br"
                        | "li"
                        | "tr"
                        | "td"
                        | "th"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "section"
                        | "article"
                        | "blockquote"
                ) {
                    out.push('\n');
                }
            }
            _ if in_tag => name.push(ch),
            _ => out.push(ch),
        }
    }
    out
}

fn strip_tag_blocks(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut position = 0;
    while let Some(relative) = lower[position..].find(&open) {
        let start = position + relative;
        out.push_str(&html[position..start]);
        let Some(relative_end) = lower[start..].find(&close) else {
            return out;
        };
        position = start + relative_end + close.len();
    }
    out.push_str(&html[position..]);
    out
}

fn entities(text: &str) -> String {
    text.replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&nbsp;", " ")
        .replace("&euro;", "€")
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_title_and_paragraphs() {
        let html = r#"<html><head><title> A &amp; B </title></head>
            <body><nav>Menu</nav><script>var x = "<p>not text</p>";</script>
            <p>This is the first substantial paragraph of the page, long enough to keep.</p>
            <p>Short.</p>
            <p>This second paragraph is also long enough to be kept by the extractor.</p>
            </body></html>"#;
        assert_eq!(title(html).as_deref(), Some("A & B"));
        let text = readable(html, 4);
        assert!(text.contains("first substantial paragraph"));
        assert!(text.contains("second paragraph"));
        assert!(!text.contains("Short."));
        assert!(!text.contains("not text"));
        assert!(!text.contains("Menu"));
    }

    #[test]
    fn malformed_and_non_ascii_input_does_not_panic() {
        for html in [
            "",
            "<p>",
            "<title>unterminated",
            "<p>€uro — text with ünïcode and a long enough sentence to be kept.</p>",
            "<p>a</p><p>b</p>",
        ] {
            let _ = title(html);
            let _ = readable(html, 3);
        }
    }

    #[test]
    fn falls_back_to_tag_stripped_text() {
        let html = "<div>one&nbsp;two</div><div>three</div>";
        let out = readable(html, 3);
        assert!(out.contains("one two"), "{out}");
        assert!(out.contains("three"), "{out}");
    }
}
