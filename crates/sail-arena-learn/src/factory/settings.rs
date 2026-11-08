//! Owner settings for the Course Factory and the daily model budget it spends.
//!
//! The factory is background work on the owner's own machine, so the owner decides how much
//! of the model it may use. Settings and the spent budget live in the Arena Learn database
//! (`learn_factory_settings_*`), so a restart keeps both.
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// Upper bound the owner may set for daily model time. A day has 1,440 minutes; anything
/// above that is a typo, not a plan.
pub const MAX_MODEL_MINUTES_PER_DAY: u32 = 1_440;
/// Smallest and largest authoring context the owner may choose. Below 2k tokens a lesson
/// prompt can't hold its schema and evidence; above 32k it no longer fits the local model.
pub const MIN_CONTEXT_TOKENS: u32 = 2_000;
pub const MAX_CONTEXT_TOKENS: u32 = 32_000;
/// Length of one budget day. Days are UTC so the count doesn't jump with daylight saving.
pub const DAY_MS: i64 = 86_400_000;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("invalid setting: {0}")]
    Invalid(String),
    #[error("settings storage: {0}")]
    Store(String),
}

fn store_err(e: impl std::fmt::Display) -> SettingsError {
    SettingsError::Store(e.to_string())
}

/// What the owner controls. Serialized as the `/factory/settings` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactorySettings {
    /// Master switch. Off means no job runs at all, and nothing queued is lost.
    pub enabled: bool,
    /// Run model jobs only while no foreground model work (Dialogue, voice, Learn) happens.
    pub idle_only: bool,
    /// Model wall time the factory may spend per UTC day.
    pub max_model_minutes_per_day: u32,
    /// Authoring context budget handed to the context builder.
    pub context_tokens: u32,
}

impl Default for FactorySettings {
    /// On, idle-only and modest: two hours of model time a day is enough to stay ahead of a
    /// learner doing a few lessons daily, without making the machine feel busy.
    fn default() -> Self {
        Self {
            enabled: true,
            idle_only: true,
            max_model_minutes_per_day: 120,
            context_tokens: 6_000,
        }
    }
}

impl FactorySettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.max_model_minutes_per_day > MAX_MODEL_MINUTES_PER_DAY {
            return Err(SettingsError::Invalid(format!(
                "max_model_minutes_per_day must be at most {MAX_MODEL_MINUTES_PER_DAY}"
            )));
        }
        if !(MIN_CONTEXT_TOKENS..=MAX_CONTEXT_TOKENS).contains(&self.context_tokens) {
            return Err(SettingsError::Invalid(format!(
                "context_tokens must be between {MIN_CONTEXT_TOKENS} and {MAX_CONTEXT_TOKENS}"
            )));
        }
        Ok(())
    }
}

/// Settings, the global pause and the per-day model budget.
pub struct SettingsStore {
    conn: Mutex<Connection>,
}

impl SettingsStore {
    pub fn open(path: &Path) -> Result<Self, SettingsError> {
        Self::new(Connection::open(path).map_err(store_err)?)
    }

    pub fn open_in_memory() -> Result<Self, SettingsError> {
        Self::new(Connection::open_in_memory().map_err(store_err)?)
    }

    pub fn new(conn: Connection) -> Result<Self, SettingsError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS learn_factory_settings_values (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS learn_factory_settings_budget (
                 day      INTEGER PRIMARY KEY,
                 model_ms INTEGER NOT NULL
             );",
        )
        .map_err(store_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A poisoned lock only means another thread panicked mid-call; SQLite itself keeps
        // the data consistent, so carry on with the connection.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, key: &str) -> Result<Option<String>, SettingsError> {
        self.conn()
            .query_row(
                "SELECT value FROM learn_factory_settings_values WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(store_err)
    }

    fn put(&self, key: &str, value: &str) -> Result<(), SettingsError> {
        self.conn()
            .execute(
                "INSERT INTO learn_factory_settings_values (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
            .map_err(store_err)
    }

    /// Stored settings, or the defaults when none were saved. A stored row that no longer
    /// parses or validates fails closed: the defaults come back **switched off**, so a
    /// damaged row can't re-enable generation the owner turned off. Saving replaces it.
    pub fn settings(&self) -> Result<FactorySettings, SettingsError> {
        let Some(json) = self.get("settings")? else {
            return Ok(FactorySettings::default());
        };
        match serde_json::from_str::<FactorySettings>(&json) {
            Ok(s) if s.validate().is_ok() => Ok(s),
            _ => {
                tracing::warn!(
                    "course factory: stored settings are unreadable; generation is off until they're saved again"
                );
                Ok(FactorySettings {
                    enabled: false,
                    ..FactorySettings::default()
                })
            }
        }
    }

    pub fn save(&self, settings: &FactorySettings) -> Result<(), SettingsError> {
        settings.validate()?;
        let json = serde_json::to_string(settings).map_err(store_err)?;
        self.put("settings", &json)
    }

    /// The owner's global pause, separate from `enabled` so "pause for now" doesn't
    /// overwrite the chosen settings.
    pub fn paused(&self) -> Result<bool, SettingsError> {
        Ok(self.get("paused")?.as_deref() == Some("1"))
    }

    pub fn set_paused(&self, paused: bool) -> Result<(), SettingsError> {
        self.put("paused", if paused { "1" } else { "0" })
    }

    /// Model time spent on the UTC day containing `now_ms`.
    pub fn spent_ms(&self, now_ms: i64) -> Result<i64, SettingsError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT model_ms FROM learn_factory_settings_budget WHERE day = ?1",
                params![now_ms.div_euclid(DAY_MS)],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(0))
    }

    /// Adds model time to today's total. Negative or absurd values are clamped: a clock
    /// jump must not hand back budget or lock the factory out for years.
    pub fn record_model_ms(&self, now_ms: i64, ms: i64) -> Result<(), SettingsError> {
        let ms = ms.clamp(0, DAY_MS);
        let conn = self.conn();
        conn.execute(
            "INSERT INTO learn_factory_settings_budget (day, model_ms) VALUES (?1, ?2)
             ON CONFLICT(day) DO UPDATE SET model_ms = model_ms + excluded.model_ms",
            params![now_ms.div_euclid(DAY_MS), ms],
        )
        .map_err(store_err)?;
        // Keep a month of history for the inspector and drop the rest.
        conn.execute(
            "DELETE FROM learn_factory_settings_budget WHERE day < ?1",
            params![now_ms.div_euclid(DAY_MS) - 31],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Model time left today under `settings`. Zero means model jobs wait for tomorrow.
    pub fn remaining_ms(
        &self,
        settings: &FactorySettings,
        now_ms: i64,
    ) -> Result<i64, SettingsError> {
        let allowed = i64::from(settings.max_model_minutes_per_day) * 60_000;
        Ok((allowed - self.spent_ms(now_ms)?).max(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_until_saved_and_round_trip() {
        let store = SettingsStore::open_in_memory().unwrap();
        assert_eq!(store.settings().unwrap(), FactorySettings::default());
        let s = FactorySettings {
            enabled: false,
            idle_only: false,
            max_model_minutes_per_day: 30,
            context_tokens: 8_000,
        };
        store.save(&s).unwrap();
        assert_eq!(store.settings().unwrap(), s);
    }

    #[test]
    fn out_of_range_settings_are_refused() {
        let store = SettingsStore::open_in_memory().unwrap();
        let tiny = FactorySettings {
            context_tokens: 100,
            ..FactorySettings::default()
        };
        assert!(store.save(&tiny).is_err());
        let endless = FactorySettings {
            max_model_minutes_per_day: 10_000,
            ..FactorySettings::default()
        };
        assert!(store.save(&endless).is_err());
        assert_eq!(store.settings().unwrap(), FactorySettings::default());
    }

    #[test]
    fn a_damaged_settings_row_fails_closed() {
        let store = SettingsStore::open_in_memory().unwrap();
        store.put("settings", "{\"enabled\":tru").unwrap();
        assert!(!store.settings().unwrap().enabled);
        store.save(&FactorySettings::default()).unwrap();
        assert!(store.settings().unwrap().enabled);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = r#"{"enabled":true,"idle_only":true,"max_model_minutes_per_day":1,
                       "context_tokens":6000,"publish_without_checks":true}"#;
        assert!(serde_json::from_str::<FactorySettings>(json).is_err());
    }

    #[test]
    fn pause_is_independent_of_settings() {
        let store = SettingsStore::open_in_memory().unwrap();
        assert!(!store.paused().unwrap());
        store.set_paused(true).unwrap();
        assert!(store.paused().unwrap());
        assert!(store.settings().unwrap().enabled);
    }

    #[test]
    fn budget_is_per_utc_day_and_clamped() {
        let store = SettingsStore::open_in_memory().unwrap();
        let s = FactorySettings {
            max_model_minutes_per_day: 10,
            ..FactorySettings::default()
        };
        let day1 = 20 * DAY_MS + 1_000;
        store.record_model_ms(day1, 4 * 60_000).unwrap();
        store.record_model_ms(day1 + 10, -999_999).unwrap();
        assert_eq!(store.remaining_ms(&s, day1).unwrap(), 6 * 60_000);
        store.record_model_ms(day1, 60 * 60_000).unwrap();
        assert_eq!(store.remaining_ms(&s, day1).unwrap(), 0);
        assert_eq!(store.remaining_ms(&s, day1 + DAY_MS).unwrap(), 10 * 60_000);
    }

    #[test]
    fn settings_survive_reopen() {
        let dir = std::env::temp_dir().join(format!("sail-cf-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("learn.db");
        {
            let store = SettingsStore::open(&path).unwrap();
            store.set_paused(true).unwrap();
            store.record_model_ms(5 * DAY_MS, 1_000).unwrap();
        }
        let store = SettingsStore::open(&path).unwrap();
        assert!(store.paused().unwrap());
        assert_eq!(store.spent_ms(5 * DAY_MS).unwrap(), 1_000);
        let _ = std::fs::remove_dir_all(dir);
    }
}
