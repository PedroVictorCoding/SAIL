//! K5: the public calendar service used by HTTP, voice and the daemon sync loop.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::contracts::{
    BusyRange, CalendarChanged, CalendarError, DeleteScope, Event, EventDraft, EventPatch,
    EventStatus, EventTime, EventsWindow, FeedChange, Freshness, ImportCandidate, ImportPreview,
    ImportResult, NewFeed, Occurrence, Preferences, SkipReason, Source, SourceFreshness,
    SourceKind, SourcePatch,
};
use crate::fetch::{FeedFetcher, FetchOutcome, normalize_feed_url, url_hint};
use crate::ics::parse::parse;
use crate::ics::write::write;
use crate::recur::{expand, preset_rrule};
use crate::store::{LOCAL_SOURCE_ID, Store};
use crate::sync::diff;

/// At most 20 feed sources exist (invariant 7).
const MAX_SOURCES: usize = 20;
/// A request window is at most 400 days (invariant 7).
const MAX_WINDOW_DAYS: i64 = 400;
/// A manual sync is allowed at most once a minute per source (invariant 7).
const MANUAL_SYNC_SECS: i64 = 60;
/// Preview ids stay valid 15 minutes (contract).
const PREVIEW_TTL_MINUTES: i64 = 15;
/// Feed refresh default (owner decision).
const DEFAULT_REFRESH_MINUTES: u32 = 30;
/// Feed refresh bounds (invariant 7).
const MIN_REFRESH_MINUTES: u32 = 15;
const MAX_REFRESH_MINUTES: u32 = 1_440;
/// Title/location/notes bounds (invariant 7).
const MAX_TITLE_CHARS: usize = 200;
const MAX_LOCATION_CHARS: usize = 300;
const MAX_NOTES_CHARS: usize = 4_000;

struct Preview {
    expires: DateTime<Utc>,
    events: Vec<Event>,
}

/// Calendar service. All methods are deterministic except the explicit feed fetches.
pub struct CalendarService {
    store: Arc<Store>,
    fetcher: Arc<dyn FeedFetcher>,
    previews: Mutex<HashMap<String, Preview>>,
}

impl CalendarService {
    /// Open the store under `dir` and load preferences.
    pub fn open(dir: &Path, fetcher: Arc<dyn FeedFetcher>) -> Result<Arc<Self>, CalendarError> {
        let store = Arc::new(Store::open(dir)?);
        Ok(Arc::new(Self {
            store,
            fetcher,
            previews: Mutex::new(HashMap::new()),
        }))
    }

    pub fn sources(&self) -> Result<Vec<Source>, CalendarError> {
        self.store.sources()
    }

    pub async fn add_feed(
        &self,
        feed: NewFeed,
        now: DateTime<Utc>,
    ) -> Result<Source, CalendarError> {
        let name = trim(&feed.name, MAX_TITLE_CHARS);
        if name.is_empty() {
            return Err(CalendarError::Invalid("the calendar name is empty".into()));
        }
        let feeds = self
            .store
            .sources()?
            .into_iter()
            .filter(|source| source.kind == SourceKind::IcsFeed)
            .count();
        if feeds >= MAX_SOURCES {
            return Err(CalendarError::TooManySources);
        }
        if let Some(color) = &feed.color {
            validate_color(color)?;
        }
        let url = normalize_feed_url(&feed.url)?;
        let text = match self.fetcher.fetch(&url, None, None).await? {
            FetchOutcome::NotModified => {
                return Err(CalendarError::FetchFailed(
                    "the calendar returned no body".into(),
                ));
            }
            FetchOutcome::Body { text, .. } => text,
        };
        let preferences = self
            .store
            .preferences()
            .unwrap_or_else(|_| default_preferences());
        let tz = parse_tz(&preferences.time_zone);
        let id = format!("src_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let parsed = parse(&text, &id, tz, now);
        let source = Source {
            id: id.clone(),
            kind: SourceKind::IcsFeed,
            name,
            color: feed.color.unwrap_or_else(|| color_for(feeds)),
            visible: true,
            writable: false,
            url_hint: Some(url_hint(&url)),
            refresh_minutes: DEFAULT_REFRESH_MINUTES,
            event_count: parsed.events.len() as u32,
            last_sync_at: Some(now),
            last_error: None,
            freshness: Freshness::Fresh,
            truncated: parsed.truncated,
        };
        self.store.apply_feed(&id, &parsed.events, &[], &[])?;
        self.store.save_source(&source, Some(url.as_str()))?;
        Ok(source)
    }

    pub fn update_source(&self, id: &str, patch: SourcePatch) -> Result<Source, CalendarError> {
        let mut source = self.store.source(id)?.ok_or(CalendarError::NotFound)?;
        if let Some(name) = patch.name {
            let name = trim(&name, MAX_TITLE_CHARS);
            if name.is_empty() {
                return Err(CalendarError::Invalid("the calendar name is empty".into()));
            }
            source.name = name;
        }
        if let Some(color) = patch.color {
            validate_color(&color)?;
            source.color = color;
        }
        if let Some(visible) = patch.visible {
            source.visible = visible;
        }
        if let Some(refresh) = patch.refresh_minutes {
            if !(MIN_REFRESH_MINUTES..=MAX_REFRESH_MINUTES).contains(&refresh) {
                return Err(CalendarError::Invalid(
                    "the refresh interval must be 15-1440 minutes".into(),
                ));
            }
            source.refresh_minutes = refresh;
        }
        self.store.save_source(&source, None)?;
        Ok(source)
    }

    pub fn remove_source(&self, id: &str) -> Result<(), CalendarError> {
        if id == LOCAL_SOURCE_ID {
            return Err(CalendarError::Invalid(
                "the SAIL calendar cannot be removed".into(),
            ));
        }
        if self.store.source(id)?.is_none() {
            return Err(CalendarError::NotFound);
        }
        self.store.delete_source(id)
    }

    pub async fn sync_source(&self, id: &str, now: DateTime<Utc>) -> Result<Source, CalendarError> {
        let mut source = self.store.source(id)?.ok_or(CalendarError::NotFound)?;
        if source.kind != SourceKind::IcsFeed {
            return Err(CalendarError::Invalid("this source has no feed".into()));
        }
        if let Some(last) = source.last_sync_at
            && now - last < Duration::seconds(MANUAL_SYNC_SECS)
        {
            return Err(CalendarError::RateLimited);
        }
        let url = self
            .store
            .feed_url(id)
            .and_then(|raw| normalize_feed_url(&raw).ok())
            .ok_or_else(|| CalendarError::Storage("the feed URL is missing".into()))?;
        let (etag, last_modified) = self.store.feed_http(id)?;
        match self
            .fetcher
            .fetch(&url, etag.as_deref(), last_modified.as_deref())
            .await
        {
            Ok(FetchOutcome::NotModified) => {
                let recovered = source.last_error.is_some();
                source.last_sync_at = Some(now);
                source.last_error = None;
                self.save_after_sync(&source, recovered)?;
                Ok(source)
            }
            Ok(FetchOutcome::Body {
                text,
                etag,
                last_modified,
            }) => {
                let preferences = self.store.preferences()?;
                let tz = parse_tz(&preferences.time_zone);
                let parsed = parse(&text, id, tz, now);
                let old = self.store.events(Some(&[id.to_string()]))?;
                let delta = diff(&old, &parsed.events, now, tz);
                self.store
                    .apply_feed(id, &delta.upserts, &delta.removed, &delta.changes)?;
                self.store
                    .set_feed_http(id, etag.as_deref(), last_modified.as_deref())?;
                let visible_change = source.last_error.is_some()
                    || source.event_count != parsed.events.len() as u32
                    || source.truncated != parsed.truncated;
                source.event_count = parsed.events.len() as u32;
                source.last_sync_at = Some(now);
                source.last_error = None;
                source.truncated = parsed.truncated;
                self.save_after_sync(&source, visible_change)?;
                Ok(source)
            }
            Err(error) => {
                let message = plain_fetch_error(&error);
                let changed = source.last_error.as_deref() != Some(message.as_str());
                source.last_error = Some(message);
                self.save_after_sync(&source, changed)?;
                Err(error)
            }
        }
    }

    /// Persist a source after a sync; bump the revision only when what the owner sees
    /// (an error appearing or clearing, the event count) changed.
    fn save_after_sync(&self, source: &Source, visible: bool) -> Result<(), CalendarError> {
        if visible {
            self.store.save_source(source, None)
        } else {
            self.store.record_sync(source)
        }
    }

    pub async fn sync_due(&self, now: DateTime<Utc>) -> Vec<FeedChange> {
        if sail_common::resources::global().voice_active() {
            return Vec::new();
        }
        let Ok(sources) = self.store.sources() else {
            return Vec::new();
        };
        let mut changes = Vec::new();
        for source in sources {
            if source.kind != SourceKind::IcsFeed {
                continue;
            }
            let due = source.last_sync_at.is_none_or(|last| {
                now - last >= Duration::minutes(i64::from(source.refresh_minutes))
            });
            if !due {
                continue;
            }
            match self.sync_source(&source.id, now).await {
                Ok(_) => {
                    changes.extend(self.store.recent_changes(now - Duration::seconds(5)));
                }
                Err(_) => continue,
            }
            if sail_common::resources::global().voice_active() {
                break;
            }
        }
        changes
    }

    pub fn window(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        sources: Option<&[String]>,
        include_cancelled: bool,
    ) -> Result<EventsWindow, CalendarError> {
        validate_window(start, end)?;
        let all = self.store.sources()?;
        let selected: Vec<Source> = all
            .iter()
            .filter(|source| {
                source.visible && sources.is_none_or(|ids| ids.iter().any(|id| id == &source.id))
            })
            .cloned()
            .collect();
        let ids: Vec<String> = selected.iter().map(|source| source.id.clone()).collect();
        let events = self.store.events(Some(&ids))?;
        let preferences = self.store.preferences()?;
        let tz = parse_tz(&preferences.time_zone);
        let (occurrences, truncated) = expand(&events, start, end, tz, include_cancelled);
        Ok(EventsWindow {
            occurrences,
            truncated,
            sources: selected
                .iter()
                .map(|source| SourceFreshness {
                    id: source.id.clone(),
                    freshness: source.freshness,
                    last_sync_at: source.last_sync_at,
                })
                .collect(),
            revision: self.store.revision()?,
        })
    }

    pub fn event(&self, id: &str) -> Result<Event, CalendarError> {
        self.store.event(id)?.ok_or(CalendarError::NotFound)
    }

    pub fn create(&self, draft: EventDraft, now: DateTime<Utc>) -> Result<Event, CalendarError> {
        validate_title(&draft.title)?;
        validate_time(&draft.time)?;
        let repeat_until = draft.repeat_until;
        let rrule = preset_rrule(draft.repeat, repeat_until, &draft.time);
        let event = Event {
            id: format!("loc_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
            source_id: LOCAL_SOURCE_ID.into(),
            uid: uuid::Uuid::new_v4().to_string(),
            recurrence_id: None,
            title: trim(&draft.title, MAX_TITLE_CHARS),
            time: draft.time,
            location: draft
                .location
                .map(|value| trim(&value, MAX_LOCATION_CHARS))
                .filter(|value| !value.is_empty()),
            notes: draft
                .notes
                .map(|value| trim(&value, MAX_NOTES_CHARS))
                .filter(|value| !value.is_empty()),
            status: EventStatus::Confirmed,
            rrule,
            exdates: Vec::new(),
            repeat: draft.repeat,
            repeat_until,
            prepare: false,
            writable: true,
            updated_at: now,
        };
        self.store.put_event(&event)?;
        Ok(event)
    }

    pub fn update(
        &self,
        id: &str,
        patch: EventPatch,
        now: DateTime<Utc>,
    ) -> Result<Event, CalendarError> {
        let mut event = self.store.event(id)?.ok_or(CalendarError::NotFound)?;
        let only_prepare = patch.title.is_none()
            && patch.time.is_none()
            && patch.location.is_none()
            && patch.notes.is_none()
            && patch.repeat.is_none()
            && patch.repeat_until.is_none();
        if !event.writable && !only_prepare {
            return Err(CalendarError::ReadOnly);
        }
        if let Some(title) = patch.title {
            validate_title(&title)?;
            event.title = trim(&title, MAX_TITLE_CHARS);
        }
        if let Some(time) = patch.time {
            validate_time(&time)?;
            event.time = time;
        }
        if let Some(location) = patch.location {
            event.location = location
                .map(|value| trim(&value, MAX_LOCATION_CHARS))
                .filter(|value| !value.is_empty());
        }
        if let Some(notes) = patch.notes {
            event.notes = notes
                .map(|value| trim(&value, MAX_NOTES_CHARS))
                .filter(|value| !value.is_empty());
        }
        if let Some(repeat) = patch.repeat {
            event.repeat = repeat;
        }
        if let Some(until) = patch.repeat_until {
            event.repeat_until = until;
        }
        if event.writable {
            event.rrule = preset_rrule(event.repeat, event.repeat_until, &event.time);
        }
        event.updated_at = now;
        self.store.put_event(&event)?;
        if let Some(prepare) = patch.prepare {
            self.store.set_prepare(id, prepare)?;
            event.prepare = prepare;
        }
        Ok(event)
    }

    pub fn delete(
        &self,
        id: &str,
        scope: DeleteScope,
        occurrence: Option<&str>,
    ) -> Result<(), CalendarError> {
        let mut event = self.store.event(id)?.ok_or(CalendarError::NotFound)?;
        if !event.writable {
            return Err(CalendarError::ReadOnly);
        }
        match scope {
            DeleteScope::All => self.store.delete_event(id),
            DeleteScope::This => {
                let occurrence = occurrence.ok_or_else(|| {
                    CalendarError::Invalid("deleting one occurrence needs its key".into())
                })?;
                if event.rrule.is_none() {
                    return Err(CalendarError::Invalid("this event does not repeat".into()));
                }
                let original = occurrence
                    .rsplit_once('@')
                    .map(|(_, start)| start)
                    .unwrap_or(occurrence);
                if !event.exdates.iter().any(|value| value == original) {
                    event.exdates.push(original.to_string());
                }
                self.store.put_event(&event)
            }
        }
    }

    pub fn import_preview(
        &self,
        text: &str,
        now: DateTime<Utc>,
    ) -> Result<ImportPreview, CalendarError> {
        let preferences = self.store.preferences()?;
        let tz = parse_tz(&preferences.time_zone);
        let parsed = parse(text, LOCAL_SOURCE_ID, tz, now);
        let local = self.store.events(Some(&[LOCAL_SOURCE_ID.to_string()]))?;
        let existing: std::collections::HashSet<(String, Option<String>)> = local
            .iter()
            .map(|event| (event.uid.clone(), event.recurrence_id.clone()))
            .collect();
        let events: Vec<ImportCandidate> = parsed
            .events
            .iter()
            .map(|event| ImportCandidate {
                uid: event.uid.clone(),
                title: event.title.clone(),
                time: event.time.clone(),
                recurring: event.rrule.is_some(),
                duplicate: existing.contains(&(event.uid.clone(), event.recurrence_id.clone())),
            })
            .collect();
        let mut skipped = Vec::new();
        for warning in &parsed.warnings {
            if warning.contains("skipped") {
                skipped.push(SkipReason {
                    reason: warning.clone(),
                    count: 1,
                });
            }
        }
        if parsed.truncated {
            skipped.push(SkipReason {
                reason: "the file had more events than can be imported at once".into(),
                count: parsed.events.len() as u32,
            });
        }
        let preview_id = format!("imp_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let warnings = parsed.warnings.clone();
        let preview = Preview {
            expires: now + Duration::minutes(PREVIEW_TTL_MINUTES),
            events: parsed.events,
        };
        let mut previews = self
            .previews
            .lock()
            .map_err(|_| CalendarError::Storage("the preview cache is poisoned".into()))?;
        previews.retain(|_, preview| preview.expires >= now);
        previews.insert(preview_id.clone(), preview);
        Ok(ImportPreview {
            preview_id,
            events,
            skipped,
            warnings,
        })
    }

    pub fn import_commit(
        &self,
        preview_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ImportResult, CalendarError> {
        let preview = {
            let mut previews = self
                .previews
                .lock()
                .map_err(|_| CalendarError::Storage("the preview cache is poisoned".into()))?;
            let preview = previews.remove(preview_id).ok_or(CalendarError::NotFound)?;
            if preview.expires < now {
                return Err(CalendarError::NotFound);
            }
            preview
        };
        let local = self.store.events(Some(&[LOCAL_SOURCE_ID.to_string()]))?;
        let mut existing: std::collections::HashSet<(String, Option<String>)> = local
            .iter()
            .map(|event| (event.uid.clone(), event.recurrence_id.clone()))
            .collect();
        let mut imported = 0u32;
        let mut duplicates = 0u32;
        for source in preview.events {
            let key = (source.uid.clone(), source.recurrence_id.clone());
            if existing.contains(&key) {
                duplicates += 1;
                continue;
            }
            existing.insert(key);
            let event = Event {
                id: format!("loc_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
                source_id: LOCAL_SOURCE_ID.into(),
                uid: source.uid,
                recurrence_id: source.recurrence_id,
                title: source.title,
                time: source.time,
                location: source.location,
                notes: source.notes,
                status: source.status,
                rrule: source.rrule,
                exdates: source.exdates,
                repeat: source.repeat,
                repeat_until: source.repeat_until,
                prepare: false,
                writable: true,
                updated_at: now,
            };
            self.store.put_event(&event)?;
            imported += 1;
        }
        Ok(ImportResult {
            imported,
            duplicates,
        })
    }

    pub fn export(
        &self,
        sources: Option<&[String]>,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> Result<String, CalendarError> {
        let events = self.store.events(sources)?;
        let filtered: Vec<Event> = match (start, end) {
            (Some(start), Some(end)) => {
                validate_window(start, end)?;
                let tz = parse_tz(&self.store.preferences()?.time_zone);
                let (occurrences, _) = expand(&events, start, end, tz, true);
                let keep: std::collections::HashSet<&str> =
                    occurrences.iter().map(|o| o.event_id.as_str()).collect();
                events
                    .into_iter()
                    .filter(|event| keep.contains(event.id.as_str()))
                    .collect()
            }
            _ => events,
        };
        Ok(write(&filtered, "SAIL Calendar"))
    }

    pub fn freebusy(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<BusyRange>, CalendarError> {
        validate_window(start, end)?;
        let visible: Vec<String> = self
            .store
            .sources()?
            .into_iter()
            .filter(|source| source.visible)
            .map(|source| source.id)
            .collect();
        let events = self.store.events(Some(&visible))?;
        let tz = parse_tz(&self.store.preferences()?.time_zone);
        let (occurrences, _) = expand(&events, start, end, tz, false);
        let mut ranges: Vec<BusyRange> = occurrences
            .into_iter()
            .filter(|occurrence| {
                matches!(
                    occurrence.status,
                    EventStatus::Confirmed | EventStatus::Tentative
                )
            })
            .filter_map(|occurrence| busy_range(&occurrence, tz))
            .collect();
        ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<BusyRange> = Vec::new();
        for range in ranges {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => {
                    if range.end > last.end {
                        last.end = range.end;
                    }
                }
                _ => merged.push(range),
            }
        }
        Ok(merged)
    }

    pub fn preferences(&self) -> Result<Preferences, CalendarError> {
        self.store.preferences()
    }

    pub fn set_preferences(&self, prefs: Preferences) -> Result<Preferences, CalendarError> {
        validate_preferences(&prefs)?;
        self.store.set_preferences(&prefs)?;
        Ok(prefs)
    }

    pub fn recent_changes(&self, since: DateTime<Utc>) -> Vec<FeedChange> {
        self.store.recent_changes(since)
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<CalendarChanged> {
        self.store.subscribe()
    }
}

fn busy_range(occurrence: &Occurrence, tz: chrono_tz::Tz) -> Option<BusyRange> {
    match &occurrence.time {
        EventTime::Timed { start, end, .. } => Some(BusyRange {
            start: *start,
            end: *end,
        }),
        EventTime::AllDay {
            start_date,
            end_date,
        } => Some(BusyRange {
            start: tz
                .from_local_datetime(&start_date.and_hms_opt(0, 0, 0)?)
                .earliest()?
                .with_timezone(&Utc),
            end: tz
                .from_local_datetime(&end_date.and_hms_opt(0, 0, 0)?)
                .earliest()?
                .with_timezone(&Utc),
        }),
    }
}

fn validate_window(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<(), CalendarError> {
    if end <= start {
        return Err(CalendarError::Invalid(
            "the end must be after the start".into(),
        ));
    }
    if end - start > Duration::days(MAX_WINDOW_DAYS) {
        return Err(CalendarError::Invalid(
            "the window is longer than 400 days".into(),
        ));
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<(), CalendarError> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(CalendarError::Invalid(
            "the title must be 1-200 characters".into(),
        ));
    }
    Ok(())
}

fn validate_time(time: &EventTime) -> Result<(), CalendarError> {
    match time {
        EventTime::Timed { start, end, tz } => {
            if end < start {
                return Err(CalendarError::Invalid(
                    "the event ends before it starts".into(),
                ));
            }
            if tz.parse::<chrono_tz::Tz>().is_err() {
                return Err(CalendarError::Invalid("unknown time zone".into()));
            }
        }
        EventTime::AllDay {
            start_date,
            end_date,
        } => {
            if end_date <= start_date {
                return Err(CalendarError::Invalid(
                    "the all-day end must be after the start".into(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_preferences(prefs: &Preferences) -> Result<(), CalendarError> {
    if prefs.time_zone.parse::<chrono_tz::Tz>().is_err() {
        return Err(CalendarError::Invalid("unknown time zone".into()));
    }
    if !matches!(prefs.first_weekday.as_str(), "monday" | "sunday") {
        return Err(CalendarError::Invalid(
            "the first weekday must be monday or sunday".into(),
        ));
    }
    if !matches!(
        prefs.default_view.as_str(),
        "month" | "week" | "day" | "agenda"
    ) {
        return Err(CalendarError::Invalid("unknown default view".into()));
    }
    if prefs.day_start_hour > 23 || prefs.day_end_hour < 1 || prefs.day_end_hour > 24 {
        return Err(CalendarError::Invalid("day hours are out of range".into()));
    }
    if prefs.day_end_hour <= prefs.day_start_hour {
        return Err(CalendarError::Invalid(
            "the day end must be after the day start".into(),
        ));
    }
    Ok(())
}

fn validate_color(color: &str) -> Result<(), CalendarError> {
    if color.len() == 7
        && color.starts_with('#')
        && color[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(CalendarError::Invalid("the colour must be #rrggbb".into()))
    }
}

fn color_for(index: usize) -> String {
    const COLORS: [&str; 8] = [
        "#4f7cff", "#2f9e72", "#c05621", "#9b2c8a", "#2b6cb0", "#b7791f", "#805ad5", "#c53030",
    ];
    COLORS[index % COLORS.len()].to_string()
}

fn trim(value: &str, max: usize) -> String {
    value.trim().chars().take(max).collect()
}

fn default_preferences() -> Preferences {
    Preferences {
        time_zone: "Europe/Lisbon".into(),
        first_weekday: "monday".into(),
        default_view: "week".into(),
        day_start_hour: 7,
        day_end_hour: 23,
    }
}

fn parse_tz(name: &str) -> chrono_tz::Tz {
    name.parse().unwrap_or(chrono_tz::Europe::Lisbon)
}

fn plain_fetch_error(error: &CalendarError) -> String {
    match error {
        CalendarError::NotCalendar => "the address did not return a calendar".into(),
        CalendarError::TooLarge => "the feed is larger than 5 MB".into(),
        CalendarError::BlockedAddress => "the address is inside a private network".into(),
        CalendarError::FetchFailed(_) => "the feed could not be fetched".into(),
        CalendarError::Invalid(_) => "the feed address is not valid".into(),
        _ => "the feed could not be synced".into(),
    }
}
