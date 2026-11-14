//! Durable schedules for the Hermes agent (round 21).
//!
//! [`when`] parses and evaluates the schedule grammar; [`store`] persists jobs and
//! runs and plans the slots a tick must execute. Both are pure where the contract
//! says so and never need a wall clock: the caller passes `now`.
pub mod store;
pub mod when;

pub use store::{
    Action, Creator, Deliver, Due, FAILURES_BEFORE_PAUSE, Job, JobState, MAX_JOBS, MAX_MODEL_JOBS,
    Outcome, Plan, Run, Store, finish, plan,
};
pub use when::{Spec, Weekdays, When, describe, next_after, parse};
