//! HTML-to-text normalisation and bounded tag stripping.
//!
//! Moodle course content, forum posts, section summaries and assignment briefs are provided
//! as untrusted HTML. This module converts HTML to clean, plain text for extraction and
//! model prompts without introducing heavy third-party dependencies.

use crate::contracts::MAX_EXTRACTION_TEXT_BYTES;

/// Maximum bytes produced by HTML-to-text conversion (8 times the single extraction doc cap).
pub const MAX_TEXT_OUTPUT_BYTES: usize = MAX_EXTRACTION_TEXT_BYTES * 8;

/// Convert untrusted HTML into plain text.
///
/// Strips tags, drops `<script>`, `<style>` and comments, translates block tags (`<p>`,
/// `<div>`, `<li>`, `<tr>`, `<h1>`-`<h6>`, `<br>`) to newlines, prefixes list items with `"- "`,
/// decodes common named and numeric HTML entities, collapses whitespace, and caps the result at
/// [`MAX_TEXT_OUTPUT_BYTES`].
pub fn html_to_text(html: &str) -> String {
    let mut raw_buf = String::with_capacity(html.len().min(MAX_TEXT_OUTPUT_BYTES));
    let mut chars = html.char_indices().peekable();

    while let Some((_, ch)) = chars.next() {
        if raw_buf.len() >= MAX_TEXT_OUTPUT_BYTES {
            break;
        }

        if ch == '<' {
            // Check for HTML comments <!-- ... -->
            let rest = &html[chars.peek().map(|(idx, _)| *idx).unwrap_or(html.len())..];
            if rest.starts_with("!--") {
                // Skip past comment
                for _ in 0..3 {
                    chars.next();
                }
                while let Some((_, c)) = chars.next() {
                    if c == '-' {
                        let sub = &html[chars.peek().map(|(idx, _)| *idx).unwrap_or(html.len())..];
                        if sub.starts_with("->") {
                            chars.next(); // -
                            chars.next(); // >
                            break;
                        }
                    }
                }
                continue;
            }

            // Read tag name
            let mut tag_name = String::with_capacity(16);
            let mut is_closing = false;

            if let Some(&(_, '/')) = chars.peek() {
                is_closing = true;
                chars.next();
            }

            while let Some(&(_, tc)) = chars.peek() {
                if tc.is_ascii_alphanumeric() {
                    tag_name.push(tc.to_ascii_lowercase());
                    chars.next();
                } else {
                    break;
                }
            }

            // Check for <script> or <style> tags to skip their entire content until </script> or </style>
            if !is_closing && (tag_name == "script" || tag_name == "style") {
                // Consume the rest of the opening tag
                skip_tag_attributes(&mut chars);
                // Consume until matching </script> or </style>
                skip_until_closing_tag(&mut chars, html, &tag_name);
                continue;
            }

            // Handle block tags that become newlines / list markers
            match tag_name.as_str() {
                "br" => {
                    raw_buf.push('\n');
                }
                "p" | "div" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    raw_buf.push('\n');
                }
                "li" => {
                    if !is_closing {
                        raw_buf.push('\n');
                        raw_buf.push_str("- ");
                    } else {
                        raw_buf.push('\n');
                    }
                }
                _ => {}
            }

            skip_tag_attributes(&mut chars);
        } else if ch == '&' {
            // Read entity
            let mut entity = String::with_capacity(12);
            let mut found_semicolon = false;

            while let Some(&(_, ec)) = chars.peek() {
                if ec == ';' {
                    found_semicolon = true;
                    chars.next();
                    break;
                } else if ec.is_ascii_alphanumeric() || ec == '#' {
                    entity.push(ec);
                    chars.next();
                    if entity.len() > 10 {
                        break;
                    }
                } else {
                    break;
                }
            }

            if found_semicolon {
                if let Some(decoded) = decode_entity(&entity) {
                    raw_buf.push(decoded);
                } else {
                    raw_buf.push('&');
                    raw_buf.push_str(&entity);
                    raw_buf.push(';');
                }
            } else {
                raw_buf.push('&');
                raw_buf.push_str(&entity);
            }
        } else {
            raw_buf.push(ch);
        }
    }

    collapse_whitespace(&raw_buf, MAX_TEXT_OUTPUT_BYTES)
}

/// Skip the rest of an HTML tag until '>'
fn skip_tag_attributes(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    let mut in_quote: Option<char> = None;
    for (_, c) in chars.by_ref() {
        if let Some(q) = in_quote {
            if c == q {
                in_quote = None;
            }
        } else if c == '"' || c == '\'' {
            in_quote = Some(c);
        } else if c == '>' {
            break;
        }
    }
}

/// Skip characters until matching `</target_tag>` is reached.
fn skip_until_closing_tag(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    full_html: &str,
    target_tag: &str,
) {
    while let Some((_, c)) = chars.next() {
        if c == '<' && matches!(chars.peek(), Some(&(_, '/'))) {
            chars.next();
            let after_slash = chars.peek().map(|(idx, _)| *idx).unwrap_or(full_html.len());
            let slice = &full_html[after_slash..];
            if slice.len() >= target_tag.len()
                && slice[..target_tag.len()].eq_ignore_ascii_case(target_tag)
            {
                // Tag matched! Consume the tag name
                for _ in 0..target_tag.len() {
                    chars.next();
                }
                skip_tag_attributes(chars);
                break;
            }
        }
    }
}

/// Decode standard, Portuguese, and numeric HTML entities.
fn decode_entity(entity: &str) -> Option<char> {
    if let Some(num_str) = entity.strip_prefix('#') {
        if let Some(hex_str) = num_str
            .strip_prefix('x')
            .or_else(|| num_str.strip_prefix('X'))
        {
            u32::from_str_radix(hex_str, 16)
                .ok()
                .and_then(char::from_u32)
        } else {
            num_str.parse::<u32>().ok().and_then(char::from_u32)
        }
    } else {
        match entity.to_ascii_lowercase().as_str() {
            "nbsp" => Some(' '),
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            // Portuguese & Western European characters
            "aacute" => Some('á'),
            "agrave" => Some('à'),
            "acirc" => Some('â'),
            "atilde" => Some('ã'),
            "eacute" => Some('é'),
            "egrave" => Some('è'),
            "ecirc" => Some('ê'),
            "iacute" => Some('í'),
            "igrave" => Some('ì'),
            "oacute" => Some('ó'),
            "ograve" => Some('ò'),
            "ocirc" => Some('ô'),
            "otilde" => Some('õ'),
            "uacute" => Some('ú'),
            "ugrave" => Some('ù'),
            "ccedil" => Some('ç'),
            "ordm" => Some('º'),
            "ordf" => Some('ª'),
            _ => {
                // Check uppercase variant if needed
                match entity {
                    "Aacute" => Some('Á'),
                    "Agrave" => Some('À'),
                    "Acirc" => Some('Â'),
                    "Atilde" => Some('Ã'),
                    "Eacute" => Some('É'),
                    "Egrave" => Some('È'),
                    "Ecirc" => Some('Ê'),
                    "Iacute" => Some('Í'),
                    "Oacute" => Some('Ó'),
                    "Ocirc" => Some('Ô'),
                    "Otilde" => Some('Õ'),
                    "Uacute" => Some('Ú'),
                    "Ccedil" => Some('Ç'),
                    _ => None,
                }
            }
        }
    }
}

/// Collapse horizontal whitespace into a single space and limit consecutive newlines to at most 2.
fn collapse_whitespace(text: &str, max_bytes: usize) -> String {
    let mut result = String::with_capacity(text.len().min(max_bytes));
    let mut consecutive_newlines = 0;
    let mut in_whitespace;

    for line in text.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            if consecutive_newlines < 2 && !result.is_empty() {
                result.push('\n');
                consecutive_newlines += 1;
            }
            continue;
        }

        if !result.is_empty() && consecutive_newlines == 0 {
            result.push('\n');
        }
        consecutive_newlines = 0;
        in_whitespace = false;

        for ch in trimmed_line.chars() {
            if result.len() >= max_bytes {
                break;
            }
            if ch.is_whitespace() {
                if !in_whitespace {
                    result.push(' ');
                    in_whitespace = true;
                }
            } else {
                result.push(ch);
                in_whitespace = false;
            }
        }

        if result.len() >= max_bytes {
            break;
        }
    }

    // Ensure we don't exceed max_bytes on char boundary
    if result.len() > max_bytes {
        let mut truncate_at = max_bytes;
        while !result.is_char_boundary(truncate_at) && truncate_at > 0 {
            truncate_at -= 1;
        }
        result.truncate(truncate_at);
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strips_tags_and_preserves_text() {
        let html = "<p>Primeiro par&aacute;grafo.</p><div>Segundo <b>par&aacute;grafo</b>.</div>";
        let text = html_to_text(html);
        assert_eq!(text, "Primeiro parágrafo.\nSegundo parágrafo.");
    }

    #[test]
    fn test_drops_script_style_comments() {
        let html = "<!-- Comentário -->\
                    <style>body { color: red; }</style>\
                    <script type=\"text/javascript\">alert('hello');</script>\
                    <p>Texto real &amp; v&aacute;lido.</p>";
        let text = html_to_text(html);
        assert_eq!(text, "Texto real & válido.");
    }

    #[test]
    fn test_list_items_get_hyphen_prefix() {
        let html = "<ul><li>Item 1</li><li>Item 2</li></ul>";
        let text = html_to_text(html);
        assert_eq!(text, "- Item 1\n- Item 2");
    }

    #[test]
    fn test_portuguese_entities() {
        let html = "Aten&ccedil;&atilde;o: &eacute; necess&aacute;rio inscri&ccedil;&atilde;o no 1.&ordm; teste.";
        let text = html_to_text(html);
        assert_eq!(text, "Atenção: é necessário inscrição no 1.º teste.");
    }

    #[test]
    fn test_numeric_entities() {
        let html = "Pre&#231;o: 10&#x20AC; e teste&#160;espaco";
        let text = html_to_text(html);
        assert_eq!(text, "Preço: 10€ e teste espaco");
    }
}
