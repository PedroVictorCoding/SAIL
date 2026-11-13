//! Fail-closed redaction before egress (Kilo). Spec: the module doc in `escalation/mod.rs`.

/// Placeholder → original value, for [`rehydrate`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionMap {
    pub entries: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub map: RedactionMap,
    /// Counts per placeholder class, e.g. ("EMAIL", 2), for the egress receipt.
    pub counts: Vec<(String, u32)>,
}

/// Replace private values with stable placeholders, then strip credential patterns.
/// `known_names` are extra literal strings (people, places) to replace with `[NAME_n]`.
pub fn redact(text: &str, known_names: &[String]) -> Result<Redacted, String> {
    let names = prepare_names(known_names)?;
    let mut out = String::with_capacity(text.len());
    let mut builder = Builder::default();
    let mut index = 0;
    while index < text.len() {
        if let Some(found) = detect(text, index, &names) {
            let value = &text[found.start..found.end];
            out.push_str(&builder.placeholder(found.class, value));
            index = found.end;
            continue;
        }
        let ch = text[index..]
            .chars()
            .next()
            .ok_or_else(|| "text is not valid UTF-8".to_string())?;
        out.push(ch);
        index += ch.len_utf8();
    }
    builder.sort_counts();
    let text = sail_guardian::credentials::redact_credential_patterns(&out);
    if sail_guardian::credentials::redact_credential_patterns(&text) != text {
        return Err("a credential pattern could not be removed".into());
    }
    Ok(Redacted {
        text,
        map: RedactionMap {
            entries: builder.entries,
        },
        counts: builder.counts,
    })
}

/// Put the original values back into a reply.
pub fn rehydrate(text: &str, map: &RedactionMap) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        match after.find(']') {
            Some(close) => {
                let token = &after[..=close];
                match map
                    .entries
                    .iter()
                    .find(|(placeholder, _)| placeholder == token)
                {
                    Some((_, value)) => {
                        out.push_str(value);
                        rest = &after[close + 1..];
                    }
                    None => {
                        out.push('[');
                        rest = &after[1..];
                    }
                }
            }
            None => {
                out.push('[');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[derive(Default)]
struct Builder {
    entries: Vec<(String, String)>,
    counts: Vec<(String, u32)>,
    seen: Vec<(&'static str, String, String)>,
}

impl Builder {
    fn placeholder(&mut self, class: &'static str, value: &str) -> String {
        match self.counts.iter_mut().find(|(name, _)| name == class) {
            Some((_, count)) => *count += 1,
            None => self.counts.push((class.to_owned(), 1)),
        }
        if let Some((_, _, placeholder)) = self
            .seen
            .iter()
            .find(|(known, original, _)| *known == class && original == value)
        {
            return placeholder.clone();
        }
        let next = self
            .seen
            .iter()
            .filter(|(known, _, _)| *known == class)
            .count()
            + 1;
        let placeholder = format!("[{class}_{next}]");
        self.entries.push((placeholder.clone(), value.to_owned()));
        self.seen
            .push((class, value.to_owned(), placeholder.clone()));
        placeholder
    }

    fn sort_counts(&mut self) {
        self.counts
            .sort_by_key(|(class, _)| class_rank(class).unwrap_or(usize::MAX));
    }
}

fn class_rank(class: &str) -> Option<usize> {
    ["EMAIL", "PHONE", "IP", "PATH", "NUMBER", "NAME"]
        .iter()
        .position(|known| *known == class)
}

struct Found {
    start: usize,
    end: usize,
    class: &'static str,
}

fn prepare_names(known_names: &[String]) -> Result<Vec<(Vec<char>, String)>, String> {
    let mut names = Vec::new();
    for name in known_names {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("a known name is empty".into());
        }
        if trimmed.contains('[') || trimmed.contains(']') {
            return Err("a known name contains placeholder syntax".into());
        }
        names.push((
            trimmed.to_lowercase().chars().collect::<Vec<_>>(),
            trimmed.to_owned(),
        ));
    }
    names.sort_by_key(|left| std::cmp::Reverse(left.0.len()));
    Ok(names)
}

fn detect(text: &str, start: usize, names: &[(Vec<char>, String)]) -> Option<Found> {
    let bytes = text.as_bytes();
    if let Some(end) = email_end(bytes, start) {
        return Some(Found {
            start,
            end,
            class: "EMAIL",
        });
    }
    if let Some(end) = path_end(text, start) {
        return Some(Found {
            start,
            end,
            class: "PATH",
        });
    }
    if let Some(end) = ipv4_end(bytes, start) {
        return Some(Found {
            start,
            end,
            class: "IP",
        });
    }
    if let Some(end) = iban_end(bytes, start) {
        return Some(Found {
            start,
            end,
            class: "NUMBER",
        });
    }
    if let Some((end, class)) = digits_end(bytes, start) {
        return Some(Found { start, end, class });
    }
    if let Some(end) = name_end(text, start, names) {
        return Some(Found {
            start,
            end,
            class: "NAME",
        });
    }
    None
}

fn is_local(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn email_end(bytes: &[u8], start: usize) -> Option<usize> {
    if start >= bytes.len() || !is_local(bytes[start]) {
        return None;
    }
    let mut index = start;
    while index < bytes.len() && is_local(bytes[index]) {
        index += 1;
    }
    if index >= bytes.len() || bytes[index] != b'@' {
        return None;
    }
    index += 1;
    let domain_start = index;
    let mut last = b'.';
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.' {
            last = byte;
            index += 1;
        } else {
            break;
        }
    }
    if last == b'.' {
        index -= 1;
    }
    if index <= domain_start + 2 {
        return None;
    }
    let domain = &bytes[domain_start..index];
    if !domain.contains(&b'.')
        || domain
            .first()
            .is_some_and(|byte| *byte == b'.' || *byte == b'-')
        || domain
            .last()
            .is_some_and(|byte| *byte == b'.' || *byte == b'-')
        || domain
            .split(|byte| *byte == b'.')
            .any(|label| label.is_empty())
    {
        return None;
    }
    let tld = domain
        .rsplit(|byte| *byte == b'.')
        .next()
        .unwrap_or_default();
    if tld.len() < 2 || !tld.iter().all(u8::is_ascii_alphabetic) {
        return None;
    }
    Some(index)
}

fn path_end(text: &str, start: usize) -> Option<usize> {
    let rest = text.get(start..)?;
    let prefix = if rest.starts_with("/home/") {
        6
    } else if rest.starts_with("~/") {
        2
    } else {
        return None;
    };
    let bytes = rest.as_bytes();
    let mut end = prefix;
    while end < bytes.len() {
        let byte = bytes[end];
        if byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'' | b'<' | b'>' | b'|' | b'`') {
            break;
        }
        end += 1;
    }
    while end > prefix
        && matches!(
            bytes[end - 1],
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']' | b'}'
        )
    {
        end -= 1;
    }
    if end <= prefix || !rest.is_char_boundary(end) {
        return None;
    }
    Some(start + end)
}

fn ipv4_end(bytes: &[u8], start: usize) -> Option<usize> {
    if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'.') {
        return None;
    }
    let mut index = start;
    for octet in 0..4 {
        let digits_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let digits = index - digits_start;
        if digits == 0 || digits > 3 {
            return None;
        }
        let value: u32 = std::str::from_utf8(&bytes[digits_start..index])
            .ok()?
            .parse()
            .ok()?;
        if value > 255 {
            return None;
        }
        if octet < 3 {
            if index >= bytes.len() || bytes[index] != b'.' {
                return None;
            }
            index += 1;
        }
    }
    if index < bytes.len() {
        if bytes[index].is_ascii_alphanumeric() {
            return None;
        }
        if bytes[index] == b'.' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
            return None;
        }
    }
    Some(index)
}

fn iban_end(bytes: &[u8], start: usize) -> Option<usize> {
    if start + 4 > bytes.len()
        || !bytes[start].is_ascii_uppercase()
        || !bytes[start + 1].is_ascii_uppercase()
        || !bytes[start + 2].is_ascii_digit()
        || !bytes[start + 3].is_ascii_digit()
    {
        return None;
    }
    let mut index = start + 4;
    while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
        index += 1;
    }
    if index - start < 15 || (index < bytes.len() && bytes[index] == b'_') {
        return None;
    }
    Some(index)
}

fn digits_end(bytes: &[u8], start: usize) -> Option<(usize, &'static str)> {
    let mut index = start;
    let plus = index < bytes.len() && bytes[index] == b'+';
    if plus {
        index += 1;
    }
    if index >= bytes.len() || !bytes[index].is_ascii_digit() {
        return None;
    }
    let mut digits = 0u32;
    let mut separators = 0u32;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_digit() {
            digits += 1;
            index += 1;
            continue;
        }
        if matches!(byte, b' ' | b'-' | b'.' | b'(' | b')') {
            let mut next = index + 1;
            while next < bytes.len() && matches!(bytes[next], b' ' | b'-' | b'.' | b'(' | b')') {
                next += 1;
            }
            if next < bytes.len() && bytes[next].is_ascii_digit() {
                separators += 1;
                index = next;
                continue;
            }
        }
        break;
    }
    if digits < 7 {
        return None;
    }
    if !plus && separators == 0 && digits >= 12 {
        return Some((index, "NUMBER"));
    }
    Some((index, "PHONE"))
}

fn name_end(text: &str, start: usize, names: &[(Vec<char>, String)]) -> Option<usize> {
    if start > 0 {
        let previous = text[..start].chars().next_back()?;
        if previous.is_alphanumeric() || previous == '_' {
            return None;
        }
    }
    for (lower, _) in names {
        let mut chars = text[start..].chars();
        let mut end = start;
        let mut matched = true;
        for expected in lower {
            match chars.next() {
                Some(actual) if actual.to_lowercase().eq(std::iter::once(*expected)) => {
                    end += actual.len_utf8();
                }
                _ => {
                    matched = false;
                    break;
                }
            }
        }
        if !matched {
            continue;
        }
        if let Some(next) = text[end..].chars().next()
            && (next.is_alphanumeric() || next == '_')
        {
            continue;
        }
        return Some(end);
    }
    None
}
