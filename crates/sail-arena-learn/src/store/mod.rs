//! Storage adapters for [`crate::core::ports::EventStore`].
mod sqlite;
pub use sqlite::SqliteEventStore;
