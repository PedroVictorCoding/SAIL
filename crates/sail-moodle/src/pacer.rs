//! Conservative use of the school's Moodle.
//!
//! [`PacedApi`] wraps any [`MoodleApi`] and is the only way the service talks to Moodle:
//!
//! - one request at a time, at least [`MIN_REQUEST_GAP`] apart;
//! - at most [`MAX_REQUESTS_PER_SYNC`] requests per sync and [`MAX_REQUESTS_PER_DAY`] per
//!   UTC day (counted in the store, so restarts don't reset it);
//! - at most [`MAX_DOWNLOAD_BYTES_PER_DAY`] of file downloads per day.
//!
//! A sync that runs out of budget stops cleanly with what it has and finishes next time.
//! The numbers are far below what the official Moodle app does on every app open.
use crate::contracts::*;
use crate::store::Store;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Minimum gap between two Moodle requests.
pub const MIN_REQUEST_GAP: Duration = Duration::from_millis(1500);
/// Most Moodle requests in one sync.
pub const MAX_REQUESTS_PER_SYNC: u64 = 250;
/// Most Moodle requests per UTC day, across all syncs.
pub const MAX_REQUESTS_PER_DAY: u64 = 1_200;
/// Most bytes downloaded per UTC day.
pub const MAX_DOWNLOAD_BYTES_PER_DAY: u64 = 1024 * 1024 * 1024;

/// The marker in [`MoodleError::Unavailable`] when a budget, not Moodle, stopped a call.
pub const BUDGET_EXHAUSTED: &str = "SAIL's Moodle request budget is used up";

/// Whether an error is the pacer's budget stop rather than a Moodle failure.
pub fn is_budget_stop(error: &MoodleError) -> bool {
    matches!(error, MoodleError::Unavailable(m) if m.starts_with(BUDGET_EXHAUSTED))
}

/// A [`MoodleApi`] that paces and counts every call. One per sync.
pub struct PacedApi {
    inner: Arc<dyn MoodleApi>,
    store: Arc<Store>,
    gap: Duration,
    last: tokio::sync::Mutex<Option<Instant>>,
    used: AtomicU64,
}

impl PacedApi {
    pub fn new(inner: Arc<dyn MoodleApi>, store: Arc<Store>) -> Self {
        Self::with_gap(inner, store, MIN_REQUEST_GAP)
    }

    /// Tests use a short gap.
    pub fn with_gap(inner: Arc<dyn MoodleApi>, store: Arc<Store>, gap: Duration) -> Self {
        Self {
            inner,
            store,
            gap,
            last: tokio::sync::Mutex::new(None),
            used: AtomicU64::new(0),
        }
    }

    /// Requests this sync has made.
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    /// Wait for our turn and take one request from the budgets. The lock is held for the
    /// whole call (see [`Self::run`]), so calls never overlap.
    async fn admit(&self, last: &mut Option<Instant>) -> Result<(), MoodleError> {
        if self.used() >= MAX_REQUESTS_PER_SYNC {
            return Err(MoodleError::Unavailable(format!(
                "{BUDGET_EXHAUSTED} for this sync"
            )));
        }
        let today = self.store.usage_today().unwrap_or_default();
        if today.requests >= MAX_REQUESTS_PER_DAY {
            return Err(MoodleError::Unavailable(format!(
                "{BUDGET_EXHAUSTED} for today"
            )));
        }
        if let Some(prev) = *last {
            let since = prev.elapsed();
            if since < self.gap {
                tokio::time::sleep(self.gap - since).await;
            }
        }
        self.used.fetch_add(1, Ordering::Relaxed);
        let _ = self.store.add_usage(1, 0);
        Ok(())
    }

    async fn run<T, F>(&self, call: F) -> Result<T, MoodleError>
    where
        F: std::future::Future<Output = Result<T, MoodleError>>,
    {
        let mut last = self.last.lock().await;
        self.admit(&mut last).await?;
        let result = call.await;
        *last = Some(Instant::now());
        result
    }
}

#[async_trait::async_trait]
impl MoodleApi for PacedApi {
    async fn site_info(&self) -> Result<SiteInfo, MoodleError> {
        self.run(self.inner.site_info()).await
    }

    async fn courses(&self, user_id: i64) -> Result<Vec<Course>, MoodleError> {
        self.run(self.inner.courses(user_id)).await
    }

    async fn course_contents(&self, course_id: i64) -> Result<Vec<Section>, MoodleError> {
        self.run(self.inner.course_contents(course_id)).await
    }

    async fn assignments(&self, course_ids: &[i64]) -> Result<Vec<Assignment>, MoodleError> {
        self.run(self.inner.assignments(course_ids)).await
    }

    async fn submission_status(&self, assignment_id: i64) -> Result<SubmissionStatus, MoodleError> {
        self.run(self.inner.submission_status(assignment_id)).await
    }

    async fn events(
        &self,
        course_ids: &[i64],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalendarEvent>, MoodleError> {
        self.run(self.inner.events(course_ids, from, to)).await
    }

    /// Announcements cost one call for the forum list plus up to a few pages per forum
    /// inside the client; they're charged as one here and bounded by the client's caps.
    async fn announcements(
        &self,
        course_ids: &[i64],
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Announcement>, MoodleError> {
        self.run(self.inner.announcements(course_ids, since)).await
    }

    async fn grades(&self, course_id: i64, user_id: i64) -> Result<Vec<GradeItem>, MoodleError> {
        self.run(self.inner.grades(course_id, user_id)).await
    }

    async fn download(
        &self,
        file: &FileRef,
        dest: &std::path::Path,
        cancel: CancellationToken,
    ) -> Result<DownloadedFile, MoodleError> {
        let today = self.store.usage_today().unwrap_or_default();
        if today.bytes.saturating_add(file.size) > MAX_DOWNLOAD_BYTES_PER_DAY {
            return Err(MoodleError::Unavailable(format!(
                "{BUDGET_EXHAUSTED} for today's downloads"
            )));
        }
        let done = self.run(self.inner.download(file, dest, cancel)).await?;
        let _ = self.store.add_usage(0, done.bytes);
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    #[async_trait::async_trait]
    impl MoodleApi for Fake {
        async fn site_info(&self) -> Result<SiteInfo, MoodleError> {
            Ok(SiteInfo {
                site_name: "x".into(),
                user_id: 1,
                full_name: "x".into(),
                release: "4".into(),
                functions: vec![],
                can_download_files: true,
            })
        }
        async fn courses(&self, _: i64) -> Result<Vec<Course>, MoodleError> {
            Ok(vec![])
        }
        async fn course_contents(&self, _: i64) -> Result<Vec<Section>, MoodleError> {
            Ok(vec![])
        }
        async fn assignments(&self, _: &[i64]) -> Result<Vec<Assignment>, MoodleError> {
            Ok(vec![])
        }
        async fn submission_status(&self, _: i64) -> Result<SubmissionStatus, MoodleError> {
            Err(MoodleError::Cancelled)
        }
        async fn events(
            &self,
            _: &[i64],
            _: DateTime<Utc>,
            _: DateTime<Utc>,
        ) -> Result<Vec<CalendarEvent>, MoodleError> {
            Ok(vec![])
        }
        async fn announcements(
            &self,
            _: &[i64],
            _: Option<DateTime<Utc>>,
        ) -> Result<Vec<Announcement>, MoodleError> {
            Ok(vec![])
        }
        async fn grades(&self, _: i64, _: i64) -> Result<Vec<GradeItem>, MoodleError> {
            Ok(vec![])
        }
        async fn download(
            &self,
            _: &FileRef,
            _: &std::path::Path,
            _: CancellationToken,
        ) -> Result<DownloadedFile, MoodleError> {
            Err(MoodleError::Cancelled)
        }
    }

    #[tokio::test]
    async fn calls_are_spaced_counted_and_capped() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let before = store.usage_today().unwrap().requests;
        let api = PacedApi::with_gap(Arc::new(Fake), store.clone(), Duration::from_millis(40));
        let start = Instant::now();
        for _ in 0..3 {
            api.site_info().await.unwrap();
        }
        assert!(
            start.elapsed() >= Duration::from_millis(80),
            "two gaps between three calls"
        );
        assert_eq!(api.used(), 3);
        assert_eq!(store.usage_today().unwrap().requests - before, 3);
        api.used.store(MAX_REQUESTS_PER_SYNC, Ordering::Relaxed);
        let err = api.courses(1).await.unwrap_err();
        assert!(is_budget_stop(&err));
    }

    #[tokio::test]
    async fn daily_cap_is_shared_across_syncs() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let used = store.usage_today().unwrap().requests;
        store
            .add_usage(MAX_REQUESTS_PER_DAY.saturating_sub(used), 0)
            .unwrap();
        let api = PacedApi::with_gap(Arc::new(Fake), store, Duration::ZERO);
        assert!(is_budget_stop(&api.site_info().await.unwrap_err()));
    }
}
