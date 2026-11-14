//! Pure text helpers: Telegram-sized chunks, plain-text rendering and approval cards.
//!
//! SAIL sends without `parse_mode`, so Markdown is reduced to plain text. Chunking keeps
//! each message under Telegram's 4096-character limit while preferring paragraph, line,
//! sentence and space boundaries, and avoids cutting inside a fenced code block unless
//! the block alone is longer than the limit.
const DEFAULT_FENCE: &str = "```";

/// Split for Telegram's 4096-character limit: at most `max` chars per chunk (default callers
/// pass 4000), preferring paragraph breaks, then line breaks, then sentence ends, then
/// spaces; never inside a ``` fenced block unless the block alone is longer than `max`.
/// Never returns an empty chunk.
pub fn chunks(text: &str, max: usize) -> Vec<String> {
    if text.is_empty() || max == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let chars: Vec<char> = rest.chars().collect();
        if chars.len() <= max {
            out.push(rest.to_string());
            break;
        }
        let cut = choose_cut(&chars, max);
        let byte = rest
            .char_indices()
            .nth(cut)
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        let (head, tail) = rest.split_at(byte);
        out.push(head.to_string());
        rest = tail;
    }
    out
}

fn choose_cut(chars: &[char], max: usize) -> usize {
    let limit = max.min(chars.len().saturating_sub(1));
    let fences = fence_ranges(chars);
    let mut best_allowed = [0usize; 4];
    let mut best_any = [0usize; 4];
    for index in 0..limit {
        let (priority, cut) =
            if chars[index] == '\n' && index + 1 < chars.len() && chars[index + 1] == '\n' {
                (0, index + 2)
            } else if chars[index] == '\n' {
                (1, index + 1)
            } else if matches!(chars[index], '.' | '!' | '?')
                && (index + 1 == chars.len() || chars[index + 1].is_whitespace())
            {
                let space =
                    usize::from(index + 1 < chars.len() && chars[index + 1].is_whitespace());
                (2, index + 1 + space)
            } else if chars[index].is_whitespace() {
                (3, index + 1)
            } else {
                continue;
            };
        if cut > max {
            continue;
        }
        best_any[priority] = cut;
        if cut_allowed(cut, &fences, max) {
            best_allowed[priority] = cut;
        }
    }
    best_allowed
        .iter()
        .chain(best_any.iter())
        .copied()
        .find(|cut| *cut > 0)
        .unwrap_or(limit)
        .max(1)
}

fn cut_allowed(cut: usize, fences: &[(usize, usize)], max: usize) -> bool {
    for (start, end) in fences {
        if *start < cut && cut < *end {
            return end.saturating_sub(*start) > max;
        }
    }
    true
}

fn fence_ranges(chars: &[char]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut line_start = 0;
    let mut open: Option<usize> = None;
    for index in 0..=chars.len() {
        if index == chars.len() || chars[index] == '\n' {
            let line: String = chars[line_start..index].iter().collect();
            if line.trim_start().starts_with(DEFAULT_FENCE) {
                match open.take() {
                    Some(start) => {
                        let end = if index < chars.len() {
                            index + 1
                        } else {
                            index
                        };
                        ranges.push((start, end));
                    }
                    None => open = Some(line_start),
                }
            }
            if index == chars.len() {
                break;
            }
            line_start = index + 1;
        }
    }
    if let Some(start) = open {
        ranges.push((start, chars.len()));
    }
    ranges
}

/// Plain text for Telegram (SAIL sends without parse_mode): strip Markdown emphasis markers
/// (**, __, `), turn "# Heading" into "Heading", keep list bullets and fenced code content.
pub fn plain(text: &str) -> String {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in text.split('\n') {
        if line.trim_start().starts_with(DEFAULT_FENCE) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            out.push(line.to_string());
            continue;
        }
        let mut rendered = strip_heading(line);
        rendered = rendered.replace("**", "").replace("__", "");
        rendered = rendered.replace('`', "");
        out.push(rendered);
    }
    out.join("\n")
}

fn strip_heading(line: &str) -> String {
    let trimmed = line.trim_start();
    let hashes = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if hashes == 0 {
        return line.to_string();
    }
    let after = &trimmed[hashes..];
    if !after.is_empty() && !after.starts_with(char::is_whitespace) {
        return line.to_string();
    }
    let indent = &line[..line.len() - trimmed.len()];
    format!("{indent}{}", after.trim_start())
}

/// The approval card text.
pub fn approval_card(action: &str, rationale: &str, short: &str) -> String {
    format!("SAIL wants to {action}\n{rationale}\nApprove with /approve {short} or the buttons.")
}
