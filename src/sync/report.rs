//! What a running synchronization reports: the condition that holds it back, how a wait ended,
//! the advice that says what to do, and the `state` the status shows.

#[doc(hidden)]
pub mod catalog;
pub mod command;
pub(crate) mod ending;
pub(crate) mod listing;
mod outcome;
pub(crate) mod remedy;
pub(crate) mod state;
pub(crate) mod verdict;

pub use outcome::{UnknownWaitReason, WaitReason};
pub use verdict::UnmappedObjects;
