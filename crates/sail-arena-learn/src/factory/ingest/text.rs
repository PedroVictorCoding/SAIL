//! Markdown and plain text keep explicit line anchors and heading hierarchy.
use super::*;

/// Parse UTF-8 notes without interpreting links, markup or embedded instructions.
pub fn extract(
    bytes: &[u8],
    source: &str,
    revision: u64,
    markdown: bool,
) -> Result<Extracted, IngestError> {
    if bytes.len() > MAX_EXTRACTED_BYTES {
        return Err(IngestError::Limit("text bytes"));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| IngestError::Invalid("text must be UTF-8".into()))?;
    let mut builder = Builder::new(source, revision);
    let mut path: Vec<String> = Vec::new();
    let mut pending = String::new();
    let mut first = 1;
    let mut last = 1;
    let mut fence: Option<char> = None;
    let flush = |b: &mut Builder,
                 s: &mut String,
                 first,
                 last,
                 path: &[String],
                 code|
     -> Result<(), IngestError> {
        let kind = if code {
            FragmentKind::Code
        } else {
            classify(s.trim())
        };
        b.push(
            s,
            kind,
            path,
            SourceLocation::Lines { first, last },
            QualityStatus::Clean,
        )?;
        s.clear();
        Ok(())
    };
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_EXTRACTED_BYTES {
            return Err(IngestError::Limit("text lines"));
        }
        let number = index as u32 + 1;
        let trimmed = line.trim();
        let marker = if markdown && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
            trimmed.chars().next()
        } else {
            None
        };
        if marker.is_some() && (fence.is_none() || fence == marker) {
            flush(
                &mut builder,
                &mut pending,
                first,
                last,
                &path,
                fence.is_some(),
            )?;
            fence = if fence.is_some() { None } else { marker };
            first = number + 1;
            continue;
        }
        let depth = if markdown && fence.is_none() {
            trimmed.bytes().take_while(|c| *c == b'#').count()
        } else {
            0
        };
        let inferred = !markdown
            && trimmed.len() <= 120
            && (trimmed.starts_with("Chapter ") || trimmed.starts_with("Section "));
        let heading =
            inferred || (depth > 0 && depth <= 6 && trimmed.as_bytes().get(depth) == Some(&b' '));
        if heading || (trimmed.is_empty() && fence.is_none()) {
            flush(
                &mut builder,
                &mut pending,
                first,
                last,
                &path,
                fence.is_some(),
            )?;
            first = number + 1;
            if heading {
                let title = if inferred {
                    trimmed
                } else {
                    trimmed[depth..].trim()
                };
                if title.len() > MAX_FRAGMENT_BYTES {
                    return Err(IngestError::Limit("heading bytes"));
                }
                path.truncate(if inferred { 0 } else { depth - 1 });
                path.push(title.into());
                builder.push(
                    title,
                    FragmentKind::Heading,
                    &path,
                    SourceLocation::Lines {
                        first: number,
                        last: number,
                    },
                    if inferred {
                        QualityStatus::LowConfidence {
                            reason: "plain-text chapter heading inferred".into(),
                        }
                    } else {
                        QualityStatus::Clean
                    },
                )?;
            }
        } else {
            if pending.is_empty() {
                first = number;
            }
            pending.push_str(line);
            pending.push('\n');
            last = number;
            if pending.len() >= MAX_FRAGMENT_BYTES {
                flush(
                    &mut builder,
                    &mut pending,
                    first,
                    last,
                    &path,
                    fence.is_some(),
                )?;
                first = number + 1;
            }
        }
    }
    flush(
        &mut builder,
        &mut pending,
        first,
        last,
        &path,
        fence.is_some(),
    )?;
    Ok(indexed(
        if markdown {
            SourceType::Markdown
        } else {
            SourceType::Text
        },
        builder.fragments,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headings_code_kinds_and_stable_anchors() {
        let bytes = b"# Signals\n\nDefinition: a pulse is an event.\n\n## Examples\n\nExample: a bell rings.\n\n```rust\nlet pulse = 1;\n```\n";
        let a = extract(bytes, "s", 1, true).unwrap();
        let b = extract(bytes, "s", 1, true).unwrap();
        assert_eq!(a.fragments, b.fragments);
        assert_eq!(a.fragments.len(), 5);
        assert_eq!(a.fragments[1].kind, FragmentKind::Definition);
        assert_eq!(a.fragments[4].kind, FragmentKind::Code);
        assert_eq!(a.fragments[3].chapter_path, ["Signals", "Examples"]);
        assert_eq!(
            a.fragments[1].location,
            SourceLocation::Lines { first: 3, last: 3 }
        );
        assert_ne!(
            a.fragments[0].fragment_id,
            extract(bytes, "s", 2, true).unwrap().fragments[0].fragment_id
        );
    }
    #[test]
    fn unicode_long_prose_and_hostile_text_are_bounded() {
        let input = "語".repeat(MAX_FRAGMENT_BYTES);
        let result = extract(input.as_bytes(), "s", 1, false).unwrap();
        assert!(
            result
                .fragments
                .iter()
                .all(|f| f.text.len() <= MAX_FRAGMENT_BYTES)
        );
        assert!(extract(&[255], "s", 1, false).is_err());
        assert!(super::super::extract("x.exe", b"x", "s", 1).is_err());
    }
    #[test]
    fn plain_text_chapters_keep_line_anchors_and_uncertainty() {
        let result = extract(
            b"Chapter 1 Signals\n\nA pulse marks an event.\n",
            "s",
            1,
            false,
        )
        .unwrap();
        assert_eq!(result.fragments[0].kind, FragmentKind::Heading);
        assert!(matches!(
            result.fragments[0].quality,
            QualityStatus::LowConfidence { .. }
        ));
        assert_eq!(result.fragments[1].chapter_path, ["Chapter 1 Signals"]);
        assert_eq!(
            result.fragments[1].location,
            SourceLocation::Lines { first: 3, last: 3 }
        );
    }
}
