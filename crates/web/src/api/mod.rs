//! REST API endpoint modules.

pub mod audit;
pub mod auth;
pub mod config;
pub mod conflicts;
#[cfg(feature = "reliability-fixture")]
pub mod copy_inspection;
pub mod repos;
pub mod seed;
pub mod setup;
pub mod status;
pub mod sync_history;
pub mod users;
pub mod webhooks;
