//! Durable schedules for the Hermes agent (round 21).
//!
//! [`when`] parses and evaluates the schedule grammar. Everything here is pure where
//! the contract says so and never needs a wall clock: the caller passes `now`.
pub mod when;

pub use when::{Spec, Weekdays, When, describe, next_after, parse};
