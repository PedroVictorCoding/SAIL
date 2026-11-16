//! Hands-free addressing: did this transcript speak to SAIL?
//!
//! Pure and deterministic. An unaddressed transcript must never reach the transcript UI,
//! live context, memory, logs, the model or disk; the caller only learns
//! [`Addressed::No`].

use chrono::{DateTime, Duration, Utc};

/// Wake names, fixed a priori (the owner may extend them in settings later; not this
/// round). Matched as whole words and only within the first three words of a transcript.
///
/// The `"sale"` homophone and its multi-word twins were added from the 2026-11-16 host
/// measurement: Whisper transcribed "sail" as "sale" in 4 of 5 clear English requests.
pub const WAKE_NAMES: &[&str] = &[
    "sail",
    "sale",
    "hey sail",
    "hey sale",
    "hi sail",
    "hi sale",
    "ok sail",
    "ok sale",
    "okay sail",
    "okay sale",
    "olá sail",
    "ola sail",
    "ó sail",
    "oi sail",
];

/// Seconds after SAIL finishes speaking during which no name is needed
/// (owner decision: 8 s).
pub const FOLLOW_UP_SECS: i64 = 8;

/// Longest transcript considered for addressing, in characters. Beyond this the input is
/// rejected rather than scanned (fixed a priori to bound work on hostile input).
pub const MAX_TRANSCRIPT_CHARS: usize = 2_000;

/// Words that may precede a bare "SAIL" without turning it into a normal sentence word.
/// Fixed a priori from the owner's rule for overheard speech.
pub const FILLERS: &[&str] = &[
    "um", "uh", "uhm", "erm", "er", "ah", "eh", "oh", "so", "well", "então", "olha",
];

/// Result of checking one transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addressed {
    /// SAIL was named. `rest` is the original-case text after the name, with leading
    /// punctuation and spaces trimmed. A bare name gives an empty `rest`.
    Yes { rest: String },
    /// Not addressed. The text must be discarded immediately.
    No,
}

struct Word<'a> {
    text: &'a str,
    end: usize,
}

/// Split on everything that is not a Unicode alphanumeric character. Punctuation and
/// whitespace therefore separate words, so "Sail." is the word "sail".
fn words(text: &str) -> Vec<Word<'_>> {
    let mut words = Vec::new();
    let mut start = None;
    for (offset, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            start.get_or_insert(offset);
        } else if let Some(from) = start.take() {
            words.push(Word {
                text: &text[from..offset],
                end: offset,
            });
        }
    }
    if let Some(from) = start {
        words.push(Word {
            text: &text[from..],
            end: text.len(),
        });
    }
    words
}

/// Check whether `transcript` addresses SAIL.
///
/// The transcript is lowercased for matching and split on non-alphanumeric characters.
/// Multi-word names ("hey sail", "hey sale") may start within the first three words and
/// match whole words only. The bare name matches whole words only as the first word, or at
/// words 2–3 when every preceding word is a [`FILLERS`] word: "we should sail this weekend"
/// is overheard speech, not a command. A bare `"sale"` additionally needs a sentence break
/// right after it (`,` `.` `!` `?` or end of transcript): "Sale, read me the news" is a
/// command, "Sale ends Friday" and "The sale ends on Friday" are not. `rest` comes from the
/// original transcript, so its case is preserved. Input over [`MAX_TRANSCRIPT_CHARS`] is
/// rejected without scanning.
pub fn addressed(transcript: &str) -> Addressed {
    if transcript.chars().count() > MAX_TRANSCRIPT_CHARS {
        return Addressed::No;
    }
    let words = words(transcript);
    if words.is_empty() {
        return Addressed::No;
    }
    let lowered: Vec<String> = words.iter().map(|word| word.text.to_lowercase()).collect();
    let wake_words: Vec<Vec<String>> = WAKE_NAMES
        .iter()
        .map(|name| name.split_whitespace().map(str::to_lowercase).collect())
        .collect();

    for start in 0..lowered.len().min(3) {
        for name in &wake_words {
            let Some(last) = start.checked_add(name.len()) else {
                continue;
            };
            if last > lowered.len() || lowered[start..last] != name[..] {
                continue;
            }
            if name.len() == 1
                && start > 0
                && !lowered[..start]
                    .iter()
                    .all(|word| FILLERS.contains(&word.as_str()))
            {
                continue;
            }
            let after = words[last - 1].end;
            if name.len() == 1
                && name[0] == "sale"
                && !matches!(
                    transcript[after..].chars().next(),
                    None | Some(',' | '.' | '!' | '?')
                )
            {
                continue;
            }
            let rest = transcript[after..].trim_start_matches(|c: char| {
                c.is_whitespace() || matches!(c, ',' | '.' | '!' | '?' | ':' | '-')
            });
            return Addressed::Yes {
                rest: rest.to_owned(),
            };
        }
    }
    Addressed::No
}

/// The follow-up window: after SAIL answers, a follow-up within [`FOLLOW_UP_SECS`] needs
/// no wake name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FollowUpWindow {
    open_until: Option<DateTime<Utc>>,
}

impl FollowUpWindow {
    /// A closed window.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept name-free speech until `now + FOLLOW_UP_SECS`.
    pub fn open(&mut self, now: DateTime<Utc>) {
        self.open_until = Some(now + Duration::seconds(FOLLOW_UP_SECS));
    }

    /// True while the window is open (inclusive of its last instant).
    pub fn accepts(&self, now: DateTime<Utc>) -> bool {
        self.open_until.is_some_and(|until| now <= until)
    }

    /// Close the window immediately.
    pub fn close(&mut self) {
        self.open_until = None;
    }
}
