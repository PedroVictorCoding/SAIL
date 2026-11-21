//! SAIL Calendar in the daemon (round 28): the one `CalendarService`, its feed sync loop,
//! and the read-only views other parts of Dialogue use (briefing events, proactive
//! observations, voice agenda). No model call anywhere in here.

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, TimeZone, Utc};
use chrono_tz::Tz;
use sail_calendar::contracts::{EventStatus, EventTime, FeedChangeKind, Occurrence};
use sail_calendar::fetch::ReqwestFetcher;
use sail_calendar::service::CalendarService;
use sail_discovery::detect::{CalendarChangeKind, Observation};
use tracing::warn;

static SERVICE: OnceLock<Arc<CalendarService>> = OnceLock::new();

/// How often the loop asks which feeds are due. Fixed a priori: each feed has its own
/// 15–1440 min interval; one minute only bounds how late a due feed starts.
const SYNC_TICK: StdDuration = StdDuration::from_secs(60);

/// How far ahead Discovery looks for prepared events (#452: "the next 48 h").
const PREPARE_HORIZON_HOURS: i64 = 48;

/// How far back a feed change is still news (the store keeps 7 days).
const CHANGE_LOOKBACK_DAYS: i64 = 7;

/// Open the calendar store, install it, and start the sync loop. `None` (with a
/// warning) when the store can't open; the rest of SAIL runs without a calendar.
pub fn start(dir: &Path) -> Option<Arc<CalendarService>> {
    if let Some(service) = SERVICE.get() {
        return Some(service.clone());
    }
    let fetcher = match ReqwestFetcher::new() {
        Ok(fetcher) => Arc::new(fetcher),
        Err(error) => {
            warn!(%error, "calendar fetcher unavailable; SAIL Calendar disabled");
            return None;
        }
    };
    let service = match CalendarService::open(dir, fetcher) {
        Ok(service) => service,
        Err(error) => {
            warn!(%error, "calendar store unavailable; SAIL Calendar disabled");
            return None;
        }
    };
    let service = SERVICE.get_or_init(|| service).clone();
    if tokio::runtime::Handle::try_current().is_ok() {
        let looping = service.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(SYNC_TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                looping.sync_due(Utc::now()).await;
            }
        });
    }
    Some(service)
}

/// The installed service, if the daemon started one.
pub fn global() -> Option<Arc<CalendarService>> {
    SERVICE.get().cloned()
}

/// The owner's calendar zone (preferences), Europe/Lisbon when unset or invalid.
pub fn owner_tz(service: &CalendarService) -> Tz {
    service
        .preferences()
        .ok()
        .and_then(|prefs| prefs.time_zone.parse().ok())
        .unwrap_or(chrono_tz::Europe::Lisbon)
}

/// Start of an occurrence as an instant; all-day events start at local midnight.
pub fn occurrence_start(occurrence: &Occurrence, tz: Tz) -> DateTime<Utc> {
    match &occurrence.time {
        EventTime::Timed { start, .. } => *start,
        EventTime::AllDay { start_date, .. } => start_date
            .and_hms_opt(0, 0, 0)
            .and_then(|midnight| tz.from_local_datetime(&midnight).earliest())
            .map(|local| local.with_timezone(&Utc))
            .unwrap_or_else(Utc::now),
    }
}

/// Visible, not-cancelled occurrences in `[start, end)`. Empty without a calendar.
pub fn occurrences(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<Occurrence> {
    let Some(service) = global() else {
        return Vec::new();
    };
    service
        .window(start, end, None, false)
        .map(|window| {
            window
                .occurrences
                .into_iter()
                .filter(|occ| occ.status != EventStatus::Cancelled)
                .collect()
        })
        .unwrap_or_default()
}

/// Observations for the proactive loop: prepared events in the next 48 h and feed
/// moves/cancellations from the last 7 days (#452).
pub fn observations(now: DateTime<Utc>) -> Vec<Observation> {
    let Some(service) = global() else {
        return Vec::new();
    };
    let tz = owner_tz(&service);
    let mut out: Vec<Observation> = occurrences(now, now + Duration::hours(PREPARE_HORIZON_HOURS))
        .into_iter()
        .filter(|occ| occ.prepare)
        .map(|occ| Observation::CalendarEvent {
            start: occurrence_start(&occ, tz),
            all_day: matches!(occ.time, EventTime::AllDay { .. }),
            id: occ.key,
            title: occ.title,
            prepare: true,
            location: occ.location,
        })
        .collect();
    for change in service.recent_changes(now - Duration::days(CHANGE_LOOKBACK_DAYS)) {
        out.push(Observation::CalendarChange {
            id: change.event_id,
            title: change.title,
            kind: match change.kind {
                FeedChangeKind::Moved => CalendarChangeKind::Moved,
                FeedChangeKind::Cancelled => CalendarChangeKind::Cancelled,
            },
            new_start: change.new_start,
            observed_at: change.observed_at,
        });
    }
    out
}
