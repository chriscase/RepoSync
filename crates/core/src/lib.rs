//! RepoSync core library.
//!
//! This crate provides the foundational components for bidirectional SVN/Git
//! synchronization: configuration, database persistence, identity mapping,
//! conflict detection and resolution, repository clients, and the sync engine.

pub mod auto_reconcile;
pub mod busy;
pub mod config;
pub mod conflict;
pub mod crypto;
pub mod data_dir;
pub mod db;
pub mod echo_receipt_scope;
pub mod echo_suppression;
pub mod errors;
pub mod file_policy;
pub mod git;
pub mod git_push;
pub mod history_inspect;
pub mod identity;
pub mod import;
pub mod late_pair;
pub mod late_pair_publish;
pub mod ldap_auth;
pub mod lfs;
pub mod managed_remove;
pub mod models;
pub mod notify;
pub mod pair_refresh;
pub mod path_projection;
pub mod pending_frontier;
pub mod personal_config;
pub mod process;
pub mod skip_commit;
pub mod snapshot;
pub mod svn;
pub mod svn_commit;
pub mod sync_engine;
pub mod sync_status;
pub mod writer_fence;

// Re-exports for convenience.
pub use config::AppConfig;
pub use db::Database;
pub use identity::IdentityMapper;
pub use personal_config::PersonalConfig;
pub use sync_engine::SyncEngine;
