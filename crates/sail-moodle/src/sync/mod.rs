//! Snapshot diffing into [`crate::contracts::Change`]s and the file mirror plan. Both are
//! pure: they read snapshots and the known file index, and touch neither the network nor
//! the store.
pub mod diff;
pub mod mirror;
