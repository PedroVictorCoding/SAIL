//! SAIL Tracker: a local-first situation room. The owner defines places, tickers, news
//! topics and routes; the tracker follows them through free, keyless public sources,
//! keeps a local history for baselines, and reports what deviates from normal.
//!
//! Nothing here needs an account or API key. Data and history stay in the SAIL data
//! directory; outbound requests go only to the public sources named in [`engine::Source`].
pub mod anomaly;
pub mod config;
pub mod engine;
pub mod net;
pub mod sources;
pub mod store;

pub use config::TrackerConfig;
pub use engine::{Board, Source, Tracker, TrackerError};
