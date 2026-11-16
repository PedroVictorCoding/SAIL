//! Hands-free addressing: did this transcript speak to SAIL?
//!
//! Pure and deterministic. An unaddressed transcript must never reach the transcript UI,
//! live context, memory, logs, the model or disk; the caller only learns
//! [`Addressed::No`].

use chrono::{DateTime, Duration, Utc};

/// Wake names, fixed a priori (the owner may extend them in settings later; not this
/// round). Matched as whole words and only within the first three words of a transcript.
pub const WAKE_NAMES: &[&str] = &[
    "sail",
    "hey sail",
    "hi sail",
    "ok sail",
    "okay sail",
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

/// Result of checking one transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addressed {
    /// SAIL was named. `rest` is the original-case text after the name, with leading
    /// punctuation and spaces trimmed. A bare name gives an empty `rest`.
    Yes { rest: String },
    /// Not addressed. The text must be discarded immediately.
    No,
}

/// Check whether `transcript` addresses SAIL. K3 implements the rules.
pub fn addressed(_transcript: &str) -> Addressed {
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
