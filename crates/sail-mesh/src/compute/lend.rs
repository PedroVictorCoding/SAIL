//! The pure lending decision and `lend.json` persistence.
//!
//! `decide`, `next_window_start` and `window_end` are pure: time, the rest result and the
//! RPC health all arrive as parameters, so tests call them twice and compare.
use crate::{MeshError, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use sail_protocol::mesh::{LendMode, LendState, LendWindow, MeshBusy};
use std::{path::Path, time::Duration};

/// File holding the owner's lending mode in the worker's data folder.
pub const LEND_FILE: &str = "lend.json";

/// Inputs to one lending decision. Pure.
pub struct LendInputs<'a> {
    pub mode: &'a LendMode,
    pub window: &'a LendWindow,
    /// Worker's local wall-clock time, and the same instant in UTC (for `until`/`next_start`).
    pub local_now: NaiveDateTime,
    pub utc_now: DateTime<Utc>,
    /// The worker's existing rest result (`RestPolicy::busy`).
    pub busy: Option<&'a MeshBusy>,
    /// RPC server health as last seen by the supervisor.
    pub rpc_up: bool,
    pub rpc_error: Option<&'a str>,
}

/// Rules, in order:
/// 1. Pause → Paused.
/// 2. Play whose `until` has passed → evaluated as Auto (the supervisor then saves Auto).
/// 3. busy.full (game/music) → Resting (also in Play).
/// 4. Auto outside the window → OutsideWindow.
/// 5. busy (not full) → Resting.
/// 6. rpc_error → Unavailable.
/// 7. !rpc_up → Loading.
/// 8. Else Lending.
pub fn decide(input: &LendInputs<'_>) -> LendState {
    let _ = input;
    LendState::Unavailable {
        reason: "lending is not implemented yet".into(),
    }
}

/// The next window start strictly after `local_now`, as UTC.
pub fn next_window_start(
    window: &LendWindow,
    local_now: NaiveDateTime,
    utc_now: DateTime<Utc>,
) -> DateTime<Utc> {
    let _ = (window, local_now);
    utc_now
}

/// The current window's end if inside it, else None (for "Lend now: until 08:00").
pub fn window_end(
    window: &LendWindow,
    local_now: NaiveDateTime,
    utc_now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let _ = (window, local_now, utc_now);
    None
}

/// Parse a local window written as `"01:00-08:00"`.
pub fn parse_window(text: &str) -> Result<LendWindow> {
    Err(MeshError::new(format!(
        "lending windows are not implemented yet ({text:?})"
    )))
}

/// Parse a duration written as `"3h"`, `"90m"` or `"1h30m"`.
pub fn parse_duration(text: &str) -> Result<Duration> {
    Err(MeshError::new(format!(
        "lending durations are not implemented yet ({text:?})"
    )))
}

/// The saved mode; missing or malformed means `Auto`.
pub fn load_mode(path: &Path) -> LendMode {
    let _ = path;
    LendMode::Auto
}

/// Save the mode atomically for a running worker to pick up within 2 s.
pub fn save_mode(path: &Path, mode: &LendMode) -> Result<()> {
    let _ = (path, mode);
    Err(MeshError::new(
        "saving the lend mode is not implemented yet",
    ))
}
