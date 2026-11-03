//! Seed data endpoint for POC demonstration.
//!
//! POST /api/seed populates the database with realistic test data
//! including identity mappings, audit entries, conflicts, sync records,
//! and commit-map entries.

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;

use crate::api::auth::validate_session_with_role;
use crate::api::status::AppError;
use crate::AppState;

#[derive(Serialize)]
struct SeedResponse {
    ok: bool,
    message: String,
    counts: SeedCounts,
}

#[derive(Serialize)]
struct SeedCounts {
    identity_mappings: usize,
    audit_entries: usize,
    conflicts: usize,
    sync_records: usize,
    commit_map_entries: usize,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/seed", post(seed_data))
}

async fn seed_data(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<SeedResponse>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;

    let conn = db.conn();

    // -----------------------------------------------------------------------
    // 1. Identity Mappings (stored in kv_state as JSON for the frontend)
    // -----------------------------------------------------------------------
    let identity_mappings = vec![
        ("jdoe", "John Doe", "john.doe@mentorg.com"),
        ("asmith", "Alice Smith", "alice.smith@mentorg.com"),
        ("bwilson", "Bob Wilson", "bob.wilson@mentorg.com"),
        ("cjones", "Carol Jones", "carol.jones@mentorg.com"),
        ("dchen", "David Chen", "david.chen@mentorg.com"),
        ("egarcia", "Elena Garcia", "elena.garcia@mentorg.com"),
        ("fkumar", "Farid Kumar", "farid.kumar@mentorg.com"),
        ("sync-svc", "RepoSync Bot", "reposync@mentorg.com"),
    ];

    let now = chrono::Utc::now();

    // Store identity mappings as JSON in kv_state for dashboard display
    let mappings_json: Vec<serde_json::Value> = identity_mappings
        .iter()
        .map(|(svn, name, email)| {
            serde_json::json!({
                "svn_username": svn,
                "name": name,
                "email": email
            })
        })
        .collect();
    let mappings_str = serde_json::to_string(&mappings_json)
        .map_err(|e| AppError::Internal(format!("json: {}", e)))?;
    conn.execute(
        "INSERT OR REPLACE INTO kv_state (key, value, updated_at) VALUES ('identity_mappings', ?1, ?2)",
        rusqlite::params![mappings_str, now.to_rfc3339()],
    ).map_err(|e| AppError::Internal(format!("insert identity kv: {}", e)))?;

    // -----------------------------------------------------------------------
    // 2. Watermarks
    // -----------------------------------------------------------------------
    conn.execute(
        "INSERT OR REPLACE INTO watermarks (source, value, updated_at) VALUES ('svn', '1847', ?1)",
        rusqlite::params![now.to_rfc3339()],
    ).map_err(|e| AppError::Internal(format!("insert watermark: {}", e)))?;
    conn.execute(
        "INSERT OR REPLACE INTO watermarks (source, value, updated_at) VALUES ('git', 'a3f8c2d1e9b04567890abcdef1234567deadbeef', ?1)",
        rusqlite::params![now.to_rfc3339()],
    ).map_err(|e| AppError::Internal(format!("insert watermark: {}", e)))?;

    // -----------------------------------------------------------------------
    // 3. Commit Map Entries (bidirectional sync history)
    // -----------------------------------------------------------------------
    struct CmEntry {
        svn_rev: i64,
        git_sha: &'static str,
        direction: &'static str,
        svn_author: &'static str,
        git_author: &'static str,
        hours_ago: i64,
    }

    let commit_map_data = vec![
        CmEntry { svn_rev: 1847, git_sha: "a3f8c2d1e9b04567890abcdef1234567deadbeef", direction: "svn_to_git", svn_author: "jdoe", git_author: "John Doe <john.doe@mentorg.com>", hours_ago: 1 },
        CmEntry { svn_rev: 1846, git_sha: "b7e9d4f2a1c83456789012345678abcdef012345", direction: "git_to_svn", svn_author: "asmith", git_author: "Alice Smith <alice.smith@mentorg.com>", hours_ago: 2 },
        CmEntry { svn_rev: 1845, git_sha: "c2d1e9f3b4a56789012345678901abcdef234567", direction: "svn_to_git", svn_author: "bwilson", git_author: "Bob Wilson <bob.wilson@mentorg.com>", hours_ago: 3 },
        CmEntry { svn_rev: 1844, git_sha: "d4f2a1c8e3b56789012345678901abcdef345678", direction: "svn_to_git", svn_author: "jdoe", git_author: "John Doe <john.doe@mentorg.com>", hours_ago: 5 },
        CmEntry { svn_rev: 1843, git_sha: "e9b04567a3f8c2d1890abcdef1234567deadbe01", direction: "git_to_svn", svn_author: "cjones", git_author: "Carol Jones <carol.jones@mentorg.com>", hours_ago: 6 },
        CmEntry { svn_rev: 1842, git_sha: "f1c83456b7e9d4f2789012345678abcdef456789", direction: "svn_to_git", svn_author: "dchen", git_author: "David Chen <david.chen@mentorg.com>", hours_ago: 8 },
        CmEntry { svn_rev: 1841, git_sha: "01a56789c2d1e9f3012345678901abcdef567890", direction: "svn_to_git", svn_author: "egarcia", git_author: "Elena Garcia <elena.garcia@mentorg.com>", hours_ago: 10 },
        CmEntry { svn_rev: 1840, git_sha: "12b56789d4f2a1c8012345678901abcdef678901", direction: "git_to_svn", svn_author: "fkumar", git_author: "Farid Kumar <farid.kumar@mentorg.com>", hours_ago: 12 },
        CmEntry { svn_rev: 1839, git_sha: "23c04567e9b04567890abcdef1234567deadbe23", direction: "svn_to_git", svn_author: "asmith", git_author: "Alice Smith <alice.smith@mentorg.com>", hours_ago: 14 },
        CmEntry { svn_rev: 1838, git_sha: "34d83456f1c83456789012345678abcdef789012", direction: "svn_to_git", svn_author: "bwilson", git_author: "Bob Wilson <bob.wilson@mentorg.com>", hours_ago: 16 },
        CmEntry { svn_rev: 1837, git_sha: "45ea56789c2d1e9f3b4012345678901abcdef890", direction: "git_to_svn", svn_author: "jdoe", git_author: "John Doe <john.doe@mentorg.com>", hours_ago: 20 },
        CmEntry { svn_rev: 1836, git_sha: "56fb5678d4f2a1c8e3b5678901abcdef9012345a", direction: "svn_to_git", svn_author: "cjones", git_author: "Carol Jones <carol.jones@mentorg.com>", hours_ago: 24 },
        CmEntry { svn_rev: 1835, git_sha: "670c4567e9b04567a3f8abcdef1234567deadbe67", direction: "svn_to_git", svn_author: "dchen", git_author: "David Chen <david.chen@mentorg.com>", hours_ago: 28 },
        CmEntry { svn_rev: 1834, git_sha: "781d3456f1c83456b7e912345678abcdef012345b", direction: "git_to_svn", svn_author: "egarcia", git_author: "Elena Garcia <elena.garcia@mentorg.com>", hours_ago: 32 },
        CmEntry { svn_rev: 1833, git_sha: "892ea56701a56789c2d145678901abcdef1234567", direction: "svn_to_git", svn_author: "fkumar", git_author: "Farid Kumar <farid.kumar@mentorg.com>", hours_ago: 36 },
    ];

    for entry in &commit_map_data {
        let ts = (now - chrono::Duration::hours(entry.hours_ago)).to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![entry.svn_rev, entry.git_sha, entry.direction, ts, entry.svn_author, entry.git_author],
        ).map_err(|e| AppError::Internal(format!("insert commit_map: {}", e)))?;
    }

    // -----------------------------------------------------------------------
    // 4. Sync Records (detailed sync history with commit messages)
    // -----------------------------------------------------------------------
    struct SrEntry {
        svn_rev: Option<i64>,
        git_sha: Option<&'static str>,
        direction: &'static str,
        author: &'static str,
        message: &'static str,
        hours_ago: i64,
        status: &'static str,
    }

    let sync_records = vec![
        SrEntry { svn_rev: Some(1847), git_sha: Some("a3f8c2d1"), direction: "svn_to_git", author: "jdoe", message: "Fix memory leak in connection pool handler", hours_ago: 1, status: "applied" },
        SrEntry { svn_rev: Some(1846), git_sha: Some("b7e9d4f2"), direction: "git_to_svn", author: "asmith", message: "Add retry logic for transient API failures", hours_ago: 2, status: "applied" },
        SrEntry { svn_rev: Some(1845), git_sha: Some("c2d1e9f3"), direction: "svn_to_git", author: "bwilson", message: "Update OpenSSL dependency to 3.2.1 for CVE-2024-xxxx", hours_ago: 3, status: "applied" },
        SrEntry { svn_rev: Some(1844), git_sha: Some("d4f2a1c8"), direction: "svn_to_git", author: "jdoe", message: "Refactor auth middleware to support SAML tokens", hours_ago: 5, status: "applied" },
        SrEntry { svn_rev: Some(1843), git_sha: Some("e9b04567"), direction: "git_to_svn", author: "cjones", message: "Add integration tests for webhook delivery", hours_ago: 6, status: "applied" },
        SrEntry { svn_rev: Some(1842), git_sha: Some("f1c83456"), direction: "svn_to_git", author: "dchen", message: "Implement batch processing for large SVN changesets", hours_ago: 8, status: "applied" },
        SrEntry { svn_rev: Some(1841), git_sha: Some("01a56789"), direction: "svn_to_git", author: "egarcia", message: "Fix timezone handling in audit log timestamps", hours_ago: 10, status: "applied" },
        SrEntry { svn_rev: Some(1840), git_sha: Some("12b56789"), direction: "git_to_svn", author: "fkumar", message: "Add Prometheus metrics endpoint for monitoring", hours_ago: 12, status: "applied" },
        SrEntry { svn_rev: Some(1839), git_sha: Some("23c04567"), direction: "svn_to_git", author: "asmith", message: "Optimize diff algorithm for binary-heavy repos", hours_ago: 14, status: "applied" },
        SrEntry { svn_rev: Some(1838), git_sha: Some("34d83456"), direction: "svn_to_git", author: "bwilson", message: "Add rate limiting to webhook endpoints", hours_ago: 16, status: "applied" },
        SrEntry { svn_rev: None, git_sha: Some("99e0bad1"), direction: "git_to_svn", author: "jdoe", message: "WIP: Experimental branch merge strategy", hours_ago: 18, status: "failed" },
        SrEntry { svn_rev: Some(1837), git_sha: Some("45ea5678"), direction: "git_to_svn", author: "jdoe", message: "Document team mode configuration options", hours_ago: 20, status: "applied" },
        SrEntry { svn_rev: Some(1836), git_sha: Some("56fb5678"), direction: "svn_to_git", author: "cjones", message: "Fix race condition in concurrent sync cycles", hours_ago: 24, status: "applied" },
        SrEntry { svn_rev: Some(1835), git_sha: Some("670c4567"), direction: "svn_to_git", author: "dchen", message: "Add support for SVN externals in sync scope", hours_ago: 28, status: "applied" },
        SrEntry { svn_rev: Some(1834), git_sha: Some("781d3456"), direction: "git_to_svn", author: "egarcia", message: "Implement conflict auto-resolution for trivial merges", hours_ago: 32, status: "applied" },
        SrEntry { svn_rev: Some(1833), git_sha: Some("892ea567"), direction: "svn_to_git", author: "fkumar", message: "Initial team mode daemon scaffolding", hours_ago: 36, status: "applied" },
    ];

    for sr in &sync_records {
        let id = uuid::Uuid::new_v4().to_string();
        let ts = (now - chrono::Duration::hours(sr.hours_ago)).to_rfc3339();
        let synced = (now - chrono::Duration::hours(sr.hours_ago) + chrono::Duration::seconds(5)).to_rfc3339();
        conn.execute(
            "INSERT INTO sync_records (id, svn_rev, git_sha, direction, author, message, timestamp, synced_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![id, sr.svn_rev, sr.git_sha, sr.direction, sr.author, sr.message, ts, synced, sr.status],
        ).map_err(|e| AppError::Internal(format!("insert sync_record: {}", e)))?;
    }

    // -----------------------------------------------------------------------
    // 5. Conflicts (mix of statuses)
    // -----------------------------------------------------------------------
    struct ConflictData {
        file_path: &'static str,
        conflict_type: &'static str,
        svn_content: &'static str,
        git_content: &'static str,
        base_content: &'static str,
        svn_rev: i64,
        git_sha: &'static str,
        status: &'static str,
        resolution: Option<&'static str>,
        resolved_by: Option<&'static str>,
        hours_ago: i64,
    }

    let conflicts = vec![
        ConflictData {
            file_path: "src/config/database.rs",
            conflict_type: "content",
            svn_content: "use std::time::Duration;\n\nconst POOL_SIZE: usize = 10;\nconst TIMEOUT: Duration = Duration::from_secs(30);\n\npub fn init_pool() -> Pool {\n    Pool::builder()\n        .max_size(POOL_SIZE)\n        .connection_timeout(TIMEOUT)\n        .build()\n}\n",
            git_content: "use std::time::Duration;\n\nconst POOL_SIZE: usize = 25;\nconst TIMEOUT: Duration = Duration::from_secs(60);\nconst IDLE_TIMEOUT: Duration = Duration::from_secs(300);\n\npub fn init_pool() -> Pool {\n    Pool::builder()\n        .max_size(POOL_SIZE)\n        .connection_timeout(TIMEOUT)\n        .idle_timeout(Some(IDLE_TIMEOUT))\n        .build()\n}\n",
            base_content: "use std::time::Duration;\n\nconst POOL_SIZE: usize = 10;\nconst TIMEOUT: Duration = Duration::from_secs(30);\n\npub fn init_pool() -> Pool {\n    Pool::builder()\n        .max_size(POOL_SIZE)\n        .connection_timeout(TIMEOUT)\n        .build()\n}\n",
            svn_rev: 1844,
            git_sha: "d4f2a1c8e3b567",
            status: "detected",
            resolution: None,
            resolved_by: None,
            hours_ago: 4,
        },
        ConflictData {
            file_path: "src/api/handlers.rs",
            conflict_type: "content",
            svn_content: "pub async fn list_users(db: &Database) -> Result<Vec<User>> {\n    let users = db.query(\"SELECT * FROM users ORDER BY name\").await?;\n    Ok(users)\n}\n\npub async fn get_user(db: &Database, id: i64) -> Result<User> {\n    db.query_one(\"SELECT * FROM users WHERE id = $1\", &[&id]).await\n}\n",
            git_content: "pub async fn list_users(db: &Database, page: u32) -> Result<PagedResult<User>> {\n    let offset = (page - 1) * 50;\n    let users = db.query(\"SELECT * FROM users ORDER BY name LIMIT 50 OFFSET $1\", &[&offset]).await?;\n    let total = db.query_one(\"SELECT COUNT(*) FROM users\", &[]).await?;\n    Ok(PagedResult { items: users, total, page, per_page: 50 })\n}\n\npub async fn get_user(db: &Database, id: i64) -> Result<User> {\n    db.query_one(\"SELECT * FROM users WHERE id = $1\", &[&id]).await\n}\n",
            base_content: "pub async fn list_users(db: &Database) -> Result<Vec<User>> {\n    let users = db.query(\"SELECT * FROM users ORDER BY name\").await?;\n    Ok(users)\n}\n",
            svn_rev: 1843,
            git_sha: "e9b04567a3f8c2",
            status: "detected",
            resolution: None,
            resolved_by: None,
            hours_ago: 5,
        },
        ConflictData {
            file_path: "Cargo.toml",
            conflict_type: "content",
            svn_content: "[package]\nname = \"project\"\nversion = \"2.4.1\"\n\n[dependencies]\ntokio = \"1.36\"\nserde = \"1.0\"\naxum = \"0.7\"\n",
            git_content: "[package]\nname = \"project\"\nversion = \"2.5.0\"\n\n[dependencies]\ntokio = \"1.37\"\nserde = \"1.0\"\naxum = \"0.7\"\ntower = \"0.4\"\n",
            base_content: "[package]\nname = \"project\"\nversion = \"2.4.0\"\n\n[dependencies]\ntokio = \"1.36\"\nserde = \"1.0\"\naxum = \"0.7\"\n",
            svn_rev: 1840,
            git_sha: "12b56789d4f2a1",
            status: "resolved",
            resolution: Some("accept_git"),
            resolved_by: Some("asmith"),
            hours_ago: 13,
        },
        ConflictData {
            file_path: "docs/deployment.md",
            conflict_type: "edit_delete",
            svn_content: "# Deployment Guide\n\nThis document has been updated with new deployment steps.\n\n## Prerequisites\n- Docker 24+\n- Kubernetes 1.28+\n",
            git_content: "",
            base_content: "# Deployment Guide\n\nLegacy deployment instructions.\n",
            svn_rev: 1838,
            git_sha: "34d83456f1c834",
            status: "resolved",
            resolution: Some("accept_svn"),
            resolved_by: Some("bwilson"),
            hours_ago: 17,
        },
        ConflictData {
            file_path: "src/sync/engine.rs",
            conflict_type: "content",
            svn_content: "impl SyncEngine {\n    pub fn new(config: &Config) -> Self {\n        Self {\n            poll_interval: config.poll_interval,\n            max_retries: 3,\n        }\n    }\n}\n",
            git_content: "impl SyncEngine {\n    pub fn new(config: &Config) -> Self {\n        Self {\n            poll_interval: config.poll_interval,\n            max_retries: config.max_retries.unwrap_or(5),\n            backoff: ExponentialBackoff::default(),\n        }\n    }\n}\n",
            base_content: "impl SyncEngine {\n    pub fn new(config: &Config) -> Self {\n        Self {\n            poll_interval: config.poll_interval,\n        }\n    }\n}\n",
            svn_rev: 1836,
            git_sha: "56fb5678d4f2a1",
            status: "deferred",
            resolution: None,
            resolved_by: None,
            hours_ago: 25,
        },
        ConflictData {
            file_path: "src/utils/logger.rs",
            conflict_type: "content",
            svn_content: "pub fn init_logger(level: &str) {\n    tracing_subscriber::fmt()\n        .with_env_filter(level)\n        .with_target(false)\n        .init();\n}\n",
            git_content: "pub fn init_logger(level: &str) {\n    tracing_subscriber::fmt()\n        .with_env_filter(level)\n        .with_target(true)\n        .with_file(true)\n        .with_line_number(true)\n        .json()\n        .init();\n}\n",
            base_content: "pub fn init_logger(level: &str) {\n    tracing_subscriber::fmt()\n        .with_env_filter(level)\n        .init();\n}\n",
            svn_rev: 1835,
            git_sha: "670c4567e9b045",
            status: "resolved",
            resolution: Some("custom"),
            resolved_by: Some("dchen"),
            hours_ago: 29,
        },
    ];

    for c in &conflicts {
        let id = uuid::Uuid::new_v4().to_string();
        let created = (now - chrono::Duration::hours(c.hours_ago)).to_rfc3339();
        let resolved_at = if c.status == "resolved" {
            Some((now - chrono::Duration::hours(c.hours_ago - 1)).to_rfc3339())
        } else {
            None
        };
        conn.execute(
            "INSERT INTO conflicts (id, file_path, conflict_type, svn_content, git_content,
             base_content, svn_rev, git_sha, status, resolution, resolved_by, created_at, resolved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                id, c.file_path, c.conflict_type, c.svn_content, c.git_content,
                c.base_content, c.svn_rev, c.git_sha, c.status, c.resolution,
                c.resolved_by, created, resolved_at
            ],
        ).map_err(|e| AppError::Internal(format!("insert conflict: {}", e)))?;
    }

    // -----------------------------------------------------------------------
    // 6. Audit Log Entries (comprehensive activity trail)
    // -----------------------------------------------------------------------
    struct AuditData {
        action: &'static str,
        direction: Option<&'static str>,
        svn_rev: Option<i64>,
        git_sha: Option<&'static str>,
        author: Option<&'static str>,
        details: &'static str,
        success: bool,
        hours_ago: i64,
    }

    let audit_entries = vec![
        AuditData { action: "sync_cycle", direction: Some("svn_to_git"), svn_rev: Some(1847), git_sha: Some("a3f8c2d1"), author: None, details: "Synced r1847 -> a3f8c2d1: Fix memory leak in connection pool handler", success: true, hours_ago: 1 },
        AuditData { action: "sync_cycle", direction: Some("git_to_svn"), svn_rev: Some(1846), git_sha: Some("b7e9d4f2"), author: None, details: "Synced b7e9d4f2 -> r1846: Add retry logic for transient API failures", success: true, hours_ago: 2 },
        AuditData { action: "sync_cycle", direction: Some("svn_to_git"), svn_rev: Some(1845), git_sha: Some("c2d1e9f3"), author: None, details: "Synced r1845 -> c2d1e9f3: Update OpenSSL dependency to 3.2.1", success: true, hours_ago: 3 },
        AuditData { action: "conflict_detected", direction: None, svn_rev: Some(1844), git_sha: Some("d4f2a1c8"), author: None, details: "Content conflict in src/config/database.rs: both sides modified pool configuration", success: true, hours_ago: 4 },
        AuditData { action: "sync_cycle", direction: Some("svn_to_git"), svn_rev: Some(1844), git_sha: Some("d4f2a1c8"), author: None, details: "Partial sync r1844 -> d4f2a1c8: 3 files synced, 1 conflict detected", success: true, hours_ago: 5 },
        AuditData { action: "conflict_detected", direction: None, svn_rev: Some(1843), git_sha: Some("e9b04567"), author: None, details: "Content conflict in src/api/handlers.rs: pagination changes vs original", success: true, hours_ago: 5 },
        AuditData { action: "sync_cycle", direction: Some("git_to_svn"), svn_rev: Some(1843), git_sha: Some("e9b04567"), author: None, details: "Synced e9b04567 -> r1843: Add integration tests for webhook delivery", success: true, hours_ago: 6 },
        AuditData { action: "webhook_received", direction: None, svn_rev: None, git_sha: None, author: None, details: "GitHub push webhook: 2 commits on main by alice.smith", success: true, hours_ago: 6 },
        AuditData { action: "sync_cycle", direction: Some("svn_to_git"), svn_rev: Some(1842), git_sha: Some("f1c83456"), author: None, details: "Synced r1842 -> f1c83456: Implement batch processing for large SVN changesets", success: true, hours_ago: 8 },
        AuditData { action: "sync_cycle", direction: Some("svn_to_git"), svn_rev: Some(1841), git_sha: Some("01a56789"), author: None, details: "Synced r1841 -> 01a56789: Fix timezone handling in audit log timestamps", success: true, hours_ago: 10 },
        AuditData { action: "sync_cycle", direction: Some("git_to_svn"), svn_rev: Some(1840), git_sha: Some("12b56789"), author: None, details: "Synced 12b56789 -> r1840: Add Prometheus metrics endpoint", success: true, hours_ago: 12 },
        AuditData { action: "conflict_resolved", direction: None, svn_rev: Some(1840), git_sha: Some("12b56789"), author: Some("asmith"), details: "Resolved Cargo.toml conflict: accepted Git version (v2.5.0 with tower dep)", success: true, hours_ago: 12 },
        AuditData { action: "sync_cycle", direction: Some("svn_to_git"), svn_rev: Some(1839), git_sha: Some("23c04567"), author: None, details: "Synced r1839 -> 23c04567: Optimize diff algorithm", success: true, hours_ago: 14 },
        AuditData { action: "sync_cycle", direction: Some("svn_to_git"), svn_rev: Some(1838), git_sha: Some("34d83456"), author: None, details: "Synced r1838 -> 34d83456: Add rate limiting to webhook endpoints", success: true, hours_ago: 16 },
        AuditData { action: "conflict_resolved", direction: None, svn_rev: Some(1838), git_sha: Some("34d83456"), author: Some("bwilson"), details: "Resolved docs/deployment.md edit/delete conflict: kept SVN version", success: true, hours_ago: 16 },
        AuditData { action: "sync_error", direction: Some("git_to_svn"), svn_rev: None, git_sha: Some("99e0bad1"), author: None, details: "Failed to sync 99e0bad1 -> SVN: commit rejected - svn: E160042: Cannot merge conflicting branch", success: false, hours_ago: 18 },
        AuditData { action: "webhook_received", direction: None, svn_rev: Some(1838), git_sha: None, author: Some("bwilson"), details: "SVN post-commit webhook: r1838 by bwilson", success: true, hours_ago: 16 },
        AuditData { action: "sync_cycle", direction: Some("git_to_svn"), svn_rev: Some(1837), git_sha: Some("45ea5678"), author: None, details: "Synced 45ea5678 -> r1837: Document team mode configuration options", success: true, hours_ago: 20 },
        AuditData { action: "sync_cycle", direction: Some("svn_to_git"), svn_rev: Some(1836), git_sha: Some("56fb5678"), author: None, details: "Synced r1836 -> 56fb5678: Fix race condition in concurrent sync cycles", success: true, hours_ago: 24 },
        AuditData { action: "conflict_resolved", direction: None, svn_rev: Some(1835), git_sha: Some("670c4567"), author: Some("dchen"), details: "Resolved src/utils/logger.rs conflict with custom merge: combined structured logging with file info", success: true, hours_ago: 28 },
        AuditData { action: "sync_cycle", direction: Some("svn_to_git"), svn_rev: Some(1835), git_sha: Some("670c4567"), author: None, details: "Synced r1835 -> 670c4567: Add support for SVN externals", success: true, hours_ago: 28 },
        AuditData { action: "sync_cycle", direction: Some("git_to_svn"), svn_rev: Some(1834), git_sha: Some("781d3456"), author: None, details: "Synced 781d3456 -> r1834: Implement conflict auto-resolution", success: true, hours_ago: 32 },
        AuditData { action: "daemon_started", direction: None, svn_rev: None, git_sha: None, author: None, details: "RepoSync daemon v0.1.0 started - team mode, poll interval 15s", success: true, hours_ago: 48 },
        AuditData { action: "auth_login", direction: None, svn_rev: None, git_sha: None, author: Some("admin"), details: "Dashboard login from 10.20.30.40", success: true, hours_ago: 4 },
        AuditData { action: "config_updated", direction: None, svn_rev: None, git_sha: None, author: Some("admin"), details: "Updated identity mappings: added 8 author entries", success: true, hours_ago: 47 },
    ];

    for ae in &audit_entries {
        let ts = (now - chrono::Duration::hours(ae.hours_ago)).to_rfc3339();
        conn.execute(
            "INSERT INTO audit_log (action, direction, svn_rev, git_sha, author, details, created_at, success)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![ae.action, ae.direction, ae.svn_rev, ae.git_sha, ae.author, ae.details, ts, ae.success as i32],
        ).map_err(|e| AppError::Internal(format!("insert audit: {}", e)))?;
    }

    Ok(Json(SeedResponse {
        ok: true,
        message: "POC demonstration data seeded successfully".into(),
        counts: SeedCounts {
            identity_mappings: identity_mappings.len(),
            audit_entries: audit_entries.len(),
            conflicts: conflicts.len(),
            sync_records: sync_records.len(),
            commit_map_entries: commit_map_data.len(),
        },
    }))
}
