//! K5: the public calendar service used by HTTP, voice and the daemon sync loop.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::contracts::{
    BusyRange, CalendarChanged, CalendarError, DeleteScope, Event, EventDraft, EventPatch,
    EventsWindow, FeedChange, ImportPreview, ImportResult, NewFeed, Preferences, Source,
    SourcePatch,
};
use crate::fetch::FeedFetcher;

pub struct CalendarService {
    _private: (),
}

impl CalendarService {
    pub fn open(_dir: &Path, _fetcher: Arc<dyn FeedFetcher>) -> Result<Arc<Self>, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn sources(&self) -> Result<Vec<Source>, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub async fn add_feed(
        &self,
        _feed: NewFeed,
        _now: DateTime<Utc>,
    ) -> Result<Source, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn update_source(&self, _id: &str, _patch: SourcePatch) -> Result<Source, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn remove_source(&self, _id: &str) -> Result<(), CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub async fn sync_source(
        &self,
        _id: &str,
        _now: DateTime<Utc>,
    ) -> Result<Source, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub async fn sync_due(&self, _now: DateTime<Utc>) -> Vec<FeedChange> {
        Vec::new()
    }

    pub fn window(
        &self,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
        _sources: Option<&[String]>,
        _include_cancelled: bool,
    ) -> Result<EventsWindow, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn event(&self, _id: &str) -> Result<Event, CalendarError> {
        Err(CalendarError::NotFound)
    }

    pub fn create(&self, _draft: EventDraft, _now: DateTime<Utc>) -> Result<Event, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn update(
        &self,
        _id: &str,
        _patch: EventPatch,
        _now: DateTime<Utc>,
    ) -> Result<Event, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn delete(
        &self,
        _id: &str,
        _scope: DeleteScope,
        _occurrence: Option<&str>,
    ) -> Result<(), CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn import_preview(
        &self,
        _text: &str,
        _now: DateTime<Utc>,
    ) -> Result<ImportPreview, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn import_commit(
        &self,
        _preview_id: &str,
        _now: DateTime<Utc>,
    ) -> Result<ImportResult, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn export(
        &self,
        _sources: Option<&[String]>,
        _start: Option<DateTime<Utc>>,
        _end: Option<DateTime<Utc>>,
    ) -> Result<String, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn freebusy(
        &self,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
    ) -> Result<Vec<BusyRange>, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn preferences(&self) -> Result<Preferences, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn set_preferences(&self, _prefs: Preferences) -> Result<Preferences, CalendarError> {
        Err(CalendarError::Storage(
            "the calendar service is not implemented yet".into(),
        ))
    }

    pub fn recent_changes(&self, _since: DateTime<Utc>) -> Vec<FeedChange> {
        Vec::new()
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<CalendarChanged> {
        tokio::sync::broadcast::channel(1).1
    }
}
