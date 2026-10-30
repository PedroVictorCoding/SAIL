//! Commit speech at prosodic boundaries, never at arbitrary model-token boundaries.

const CONTROL_MARKERS: [&str; 4] = ["<|tool_call", "<tool_call|>", "<|tool_response", "<|\"|>"];
#[derive(Default)]
pub struct SpeechTextGate {
    pending: String,
    blocked: bool,
}
impl SpeechTextGate {
    pub fn push(&mut self, delta: &str) -> String {
        if self.blocked {
            return String::new();
        }
        self.pending.push_str(delta);
        if let Some(index) = CONTROL_MARKERS
            .iter()
            .filter_map(|marker| self.pending.find(marker))
            .min()
        {
            self.blocked = true;
            let safe = self.pending[..index].to_owned();
            self.pending.clear();
            return safe;
        }
        // Retain only a possible split control-token prefix. Ordinary prose streams immediately.
        let keep = self
            .pending
            .char_indices()
            .filter_map(|(index, _)| {
                let suffix = &self.pending[index..];
                CONTROL_MARKERS
                    .iter()
                    .any(|marker| marker.starts_with(suffix))
                    .then_some(index)
            })
            .min()
            .unwrap_or(self.pending.len());
        let safe = self.pending[..keep].to_owned();
        self.pending.drain(..keep);
        safe
    }
    pub fn finish(&mut self) -> String {
        if self.pending.starts_with("<|") || self.pending.starts_with("<tool_") {
            self.blocked = true;
            self.pending.clear();
        }
        std::mem::take(&mut self.pending)
    }
    pub fn blocked(&self) -> bool {
        self.blocked
    }
}

#[derive(Default)]
pub struct SpeechChunker {
    pending: String,
}
impl SpeechChunker {
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.pending.push_str(text);
        self.drain(false)
    }
    pub fn finish(&mut self) -> Vec<String> {
        self.drain(true)
    }
    pub fn discard_pending(&mut self) {
        self.pending.clear();
    }
    fn drain(&mut self, finished: bool) -> Vec<String> {
        let mut result = Vec::new();
        while let Some(end) = boundary(&self.pending, finished) {
            let chunk = self.pending.drain(..end).collect::<String>();
            let chunk = chunk.trim().replace(['`', '*', '#'], "");
            if !chunk.trim().is_empty() {
                result.push(chunk);
            }
        }
        if finished {
            let tail = std::mem::take(&mut self.pending)
                .trim()
                .replace(['`', '*', '#'], "");
            if !tail.trim().is_empty() {
                result.push(tail);
            }
        }
        result
    }
}
fn boundary(text: &str, finished: bool) -> Option<usize> {
    for (index, ch) in text.char_indices() {
        let sentence = matches!(ch, '.' | '?' | '!' | '。' | '？' | '！');
        let clause = matches!(ch, ',' | ';' | ':' | '—') && text[..index].chars().count() >= 140;
        if !sentence && !clause {
            continue;
        }
        let mut end = index + ch.len_utf8();
        // Keep quotation marks and closing brackets with their sentence.
        for next in text[end..].chars() {
            if matches!(next, '"' | '\'' | '”' | '’' | ')' | ']') {
                end += next.len_utf8();
            } else {
                break;
            }
        }
        let next = text[end..].chars().next();
        if next.is_none() && !finished {
            continue;
        }
        if next.is_some_and(|c| !c.is_whitespace()) && !matches!(ch, '。' | '？' | '！') {
            continue;
        }
        if ch == '.' {
            let token = text[..index]
                .split_whitespace()
                .next_back()
                .unwrap_or_default();
            let lower = token.trim_matches(['"', '\'', '(', '[']).to_lowercase();
            if matches!(
                lower.as_str(),
                "mr" | "mrs"
                    | "ms"
                    | "dr"
                    | "prof"
                    | "sr"
                    | "sra"
                    | "srta"
                    | "e.g"
                    | "i.e"
                    | "etc"
                    | "vs"
            ) {
                continue;
            }
            if lower.chars().count() == 1 && lower.chars().all(char::is_alphabetic) {
                continue;
            }
            // Initialisms such as U.S. must not become independent spoken fragments.
            if lower.contains('.')
                && lower
                    .split('.')
                    .all(|p| p.chars().count() == 1 && p.chars().all(char::is_alphabetic))
            {
                continue;
            }
            // Preserve ordered-list prefixes: "1. First item".
            if lower.chars().all(|c| c.is_ascii_digit()) && text[..index].trim() == lower {
                continue;
            }
        }
        return Some(end);
    }
    // A very long unpunctuated sentence is bounded at a complete word, not a token.
    if text.chars().count() > 360 {
        return text
            .char_indices()
            .filter(|(index, c)| (140..=300).contains(index) && c.is_whitespace())
            .map(|(index, _)| index)
            .next_back();
    }
    None
}
pub fn pause_after_ms(text: &str) -> usize {
    match text
        .trim_end_matches(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | '”' | '’' | ')' | ']')
        })
        .chars()
        .next_back()
    {
        Some('.' | '?' | '!' | '。' | '？' | '！') => 140,
        Some(';' | ':') => 85,
        Some(',' | '—') => 60,
        _ => 0,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_sentences_inside_tokens_and_keeps_punctuation() {
        let mut c = SpeechChunker::default();
        assert_eq!(c.push("Yes. This sentence is still"), vec!["Yes."]);
        assert!(c.push(" arriving").is_empty());
        assert_eq!(c.push("! And "), vec!["This sentence is still arriving!"]);
        assert_eq!(c.finish(), vec!["And"]);
    }
    #[test]
    fn abbreviations_decimals_unicode_and_quotes_are_preserved() {
        let mut c = SpeechChunker::default();
        assert_eq!(
            c.push("Dr. Silva measured 3.14 volts. \"Está correto?\" Sim "),
            vec!["Dr. Silva measured 3.14 volts.", "\"Está correto?\""]
        );
        assert_eq!(c.finish(), vec!["Sim"]);
        let mut c = SpeechChunker::default();
        assert!(c.push("Hello.").is_empty());
        assert_eq!(c.push(" "), vec!["Hello."]);
    }
    #[test]
    fn control_tokens_split_across_deltas_are_not_displayed_or_spoken() {
        let text = "A safe sentence. <|tool_call>call:fs_query{}";
        for split in 0..=text.len() {
            if !text.is_char_boundary(split) {
                continue;
            }
            let mut gate = SpeechTextGate::default();
            let mut safe = gate.push(&text[..split]);
            safe.push_str(&gate.push(&text[split..]));
            safe.push_str(&gate.finish());
            assert_eq!(safe, "A safe sentence. ");
            assert!(gate.blocked());
        }
        let mut gate = SpeechTextGate::default();
        assert_eq!(gate.push("Normal text"), "Normal text");
        assert!(!gate.blocked());
    }
    #[test]
    fn long_clauses_and_cadence_use_natural_boundaries() {
        let clause = format!(
            "{}, and the next clause continues",
            "A complete phrase with useful context ".repeat(5).trim()
        );
        let mut c = SpeechChunker::default();
        let chunks = c.push(&clause);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].ends_with(','));
        assert_eq!(pause_after_ms(&chunks[0]), 60);
        assert_eq!(pause_after_ms("Really?\""), 140);
        assert_eq!(pause_after_ms("Hello."), 140);
    }
}
