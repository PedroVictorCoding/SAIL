//! The pure lending decision and `lend.json` persistence.
//!
//! `decide`, `next_window_start` and `window_end` are pure: time, the rest result and the
//! RPC health all arrive as parameters, so tests call them twice and compare.
use crate::{MeshError, Result, write_private};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
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
    if matches!(input.mode, LendMode::Pause) {
        return LendState::Paused;
    }
    let auto = match input.mode {
        LendMode::Auto => true,
        LendMode::Play { until: None } => false,
        LendMode::Play { until: Some(until) } => match DateTime::parse_from_rfc3339(until) {
            Ok(until) if input.utc_now <= until.with_timezone(&Utc) => false,
            // A past or unreadable `until` never extends lending: it behaves like Auto.
            _ => true,
        },
        // Pause returned above; never trust a mode to keep a PC lending after a bad read.
        LendMode::Pause => true,
    };
    if let Some(busy) = input.busy
        && busy.full
    {
        return LendState::Resting {
            reason: busy.reason.clone(),
            full: true,
        };
    }
    if auto && !inside(input.window, input.local_now) {
        return LendState::OutsideWindow {
            next_start: rfc3339(next_window_start(
                input.window,
                input.local_now,
                input.utc_now,
            )),
        };
    }
    if let Some(busy) = input.busy {
        return LendState::Resting {
            reason: busy.reason.clone(),
            full: false,
        };
    }
    if let Some(error) = input.rpc_error {
        return LendState::Unavailable {
            reason: error.to_owned(),
        };
    }
    if !input.rpc_up {
        return LendState::Loading;
    }
    LendState::Lending
}

/// The next window start strictly after `local_now`, as UTC.
pub fn next_window_start(
    window: &LendWindow,
    local_now: NaiveDateTime,
    utc_now: DateTime<Utc>,
) -> DateTime<Utc> {
    let (start, end) = match clock_pair(window) {
        Ok(pair) => pair,
        // An unreadable window never starts; report now rather than invent a time.
        Err(_) => return utc_now,
    };
    if start == end {
        // A window that is always open has no next start.
        return utc_now;
    }
    let minutes = minutes_of(local_now);
    let today = local_now.date().and_hms_opt(start / 60, start % 60, 0);
    let candidate = if minutes >= start {
        local_now
            .date()
            .succ_opt()
            .and_then(|date| date.and_hms_opt(start / 60, start % 60, 0))
    } else {
        today
    };
    let Some(local_start) = candidate else {
        return utc_now;
    };
    local_to_utc(local_start, local_now, utc_now)
}

/// The current window's end if inside it, else None (for "Lend now: until 08:00").
pub fn window_end(
    window: &LendWindow,
    local_now: NaiveDateTime,
    utc_now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let (start, end) = clock_pair(window).ok()?;
    if start == end || !inside(window, local_now) {
        return None;
    }
    let minutes = minutes_of(local_now);
    let today = local_now.date().and_hms_opt(end / 60, end % 60, 0);
    let local_end = if start > end && minutes >= start {
        // Wrapped midnight: the end is tomorrow morning.
        local_now
            .date()
            .succ_opt()
            .and_then(|date| date.and_hms_opt(end / 60, end % 60, 0))
    } else {
        today
    }?;
    Some(local_to_utc(local_end, local_now, utc_now))
}

/// Parse a local window written as `"01:00-08:00"`.
pub fn parse_window(text: &str) -> Result<LendWindow> {
    let (start, end) = text
        .split_once('-')
        .ok_or_else(|| MeshError::new("A window looks like 01:00-08:00"))?;
    let start = parse_clock(start)?;
    let end = parse_clock(end)?;
    Ok(LendWindow {
        start: format!("{:02}:{:02}", start / 60, start % 60),
        end: format!("{:02}:{:02}", end / 60, end % 60),
    })
}

/// Parse a duration written as `"3h"`, `"90m"` or `"1h30m"`.
pub fn parse_duration(text: &str) -> Result<Duration> {
    let mut seconds: u64 = 0;
    let mut number: Option<u64> = None;
    let mut unit_seen = false;
    for character in text.chars() {
        if character.is_ascii_digit() {
            let digit = u64::from(character as u8 - b'0');
            number = Some(
                number
                    .unwrap_or(0)
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(digit))
                    .ok_or_else(|| MeshError::new("That duration is too long"))?,
            );
            continue;
        }
        if character.is_whitespace() && number.is_none() {
            continue;
        }
        let multiplier = match character.to_ascii_lowercase() {
            'h' => 60 * 60,
            'm' => 60,
            _ => {
                return Err(MeshError::new(format!(
                    "Use hours and minutes, for example 3h, 90m or 1h30m (got {text:?})"
                )));
            }
        };
        let value = number
            .take()
            .ok_or_else(|| MeshError::new(format!("A number is missing before {character}")))?;
        seconds = value
            .checked_mul(multiplier)
            .and_then(|part| seconds.checked_add(part))
            .ok_or_else(|| MeshError::new("That duration is too long"))?;
        unit_seen = true;
    }
    if number.is_some() {
        return Err(MeshError::new(format!(
            "Add h (hours) or m (minutes) after a number (got {text:?})"
        )));
    }
    if !unit_seen || seconds == 0 {
        return Err(MeshError::new(format!(
            "Use a duration above zero, for example 3h (got {text:?})"
        )));
    }
    Ok(Duration::from_secs(seconds))
}

/// The saved mode; missing or malformed means `Auto`.
pub fn load_mode(path: &Path) -> LendMode {
    let Ok(data) = std::fs::read(path) else {
        return LendMode::Auto;
    };
    serde_json::from_slice(&data).unwrap_or(LendMode::Auto)
}

/// Save the mode atomically for a running worker to pick up within 2 s.
pub fn save_mode(path: &Path, mode: &LendMode) -> Result<()> {
    let data = serde_json::to_vec(mode)
        .map_err(|e| MeshError::new(format!("Cannot encode the lend mode: {e}")))?;
    write_private(path, &data)
        .map_err(|e| MeshError::new(format!("Cannot save {}: {e}", path.display())))
}

/// Local minutes since midnight.
fn minutes_of(local_now: NaiveDateTime) -> u32 {
    use chrono::Timelike;
    local_now.hour() * 60 + local_now.minute()
}

/// Shift a local wall-clock time to UTC using the same instant given in both zones.
fn local_to_utc(
    local: NaiveDateTime,
    local_now: NaiveDateTime,
    utc_now: DateTime<Utc>,
) -> DateTime<Utc> {
    let offset = utc_now.naive_utc() - local_now;
    let utc = local.checked_add_signed(offset).unwrap_or(local);
    DateTime::from_naive_utc_and_offset(utc, Utc)
}

fn rfc3339(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_clock(text: &str) -> Result<u32> {
    let text = text.trim();
    let (hours, minutes) = text
        .split_once(':')
        .ok_or_else(|| MeshError::new(format!("Use HH:MM for a time (got {text:?})")))?;
    let hours = parse_two_digits(hours, 24)?;
    let minutes = parse_two_digits(minutes, 60)?;
    Ok(hours * 60 + minutes)
}

fn parse_two_digits(text: &str, limit: u32) -> Result<u32> {
    if text.is_empty() || text.len() > 2 || !text.chars().all(|c| c.is_ascii_digit()) {
        return Err(MeshError::new(format!(
            "Use HH:MM for a time (got {text:?})"
        )));
    }
    let value: u32 = text
        .parse()
        .map_err(|_| MeshError::new(format!("Use HH:MM for a time (got {text:?})")))?;
    if value >= limit {
        return Err(MeshError::new(format!(
            "{text:?} is outside the clock (00:00-23:59)"
        )));
    }
    Ok(value)
}

fn clock_pair(window: &LendWindow) -> Result<(u32, u32)> {
    Ok((parse_clock(&window.start)?, parse_clock(&window.end)?))
}

fn inside(window: &LendWindow, local_now: NaiveDateTime) -> bool {
    let Ok((start, end)) = clock_pair(window) else {
        return false;
    };
    let minutes = minutes_of(local_now);
    if start == end {
        return true;
    }
    if start < end {
        start <= minutes && minutes < end
    } else {
        minutes >= start || minutes < end
    }
}
