-- Frozen schema-shape input from RepoSync87379741779a6259f7eeb52a68cc6f061174e5ef.
-- Exact registered migration SQL1-12, with the old runner's user_version updates.
-- This is not an old-executable/importer qualification receipt.

-- Migration1: initial schema

        CREATE TABLE IF NOT EXISTS commit_map (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            svn_rev     INTEGER NOT NULL,
            git_sha     TEXT    NOT NULL,
            direction   TEXT    NOT NULL CHECK (direction IN ('svn_to_git', 'git_to_svn')),
            synced_at   TEXT    NOT NULL,
            svn_author  TEXT    NOT NULL DEFAULT '',
            git_author  TEXT    NOT NULL DEFAULT ''
        );

        CREATE INDEX IF NOT EXISTS idx_commit_map_svn_rev ON commit_map (svn_rev);
        CREATE INDEX IF NOT EXISTS idx_commit_map_git_sha ON commit_map (git_sha);

        CREATE TABLE IF NOT EXISTS sync_state (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            state        TEXT    NOT NULL,
            started_at   TEXT    NOT NULL,
            completed_at TEXT,
            details      TEXT
        );

        CREATE TABLE IF NOT EXISTS conflicts (
            id              TEXT PRIMARY KEY,
            file_path       TEXT NOT NULL,
            conflict_type   TEXT NOT NULL,
            svn_content     TEXT,
            git_content     TEXT,
            base_content    TEXT,
            svn_rev         INTEGER,
            git_sha         TEXT,
            status          TEXT NOT NULL DEFAULT 'detected',
            resolution      TEXT,
            resolved_by     TEXT,
            created_at      TEXT NOT NULL,
            resolved_at     TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_conflicts_status ON conflicts (status);

        CREATE TABLE IF NOT EXISTS watermarks (
            source      TEXT PRIMARY KEY,
            value       TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS audit_log (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            action      TEXT NOT NULL,
            direction   TEXT,
            svn_rev     INTEGER,
            git_sha     TEXT,
            author      TEXT,
            details     TEXT,
            created_at  TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_audit_log_created_at ON audit_log (created_at);
        CREATE INDEX IF NOT EXISTS idx_audit_log_action ON audit_log (action);

        CREATE TABLE IF NOT EXISTS sync_records (
            id          TEXT PRIMARY KEY,
            svn_rev     INTEGER,
            git_sha     TEXT,
            direction   TEXT NOT NULL,
            author      TEXT NOT NULL DEFAULT '',
            message     TEXT NOT NULL DEFAULT '',
            timestamp   TEXT NOT NULL,
            synced_at   TEXT NOT NULL,
            status      TEXT NOT NULL DEFAULT 'pending'
        );

        CREATE INDEX IF NOT EXISTS idx_sync_records_direction ON sync_records (direction);
        CREATE INDEX IF NOT EXISTS idx_sync_records_synced_at ON sync_records (synced_at);

        CREATE TABLE IF NOT EXISTS kv_state (
            key         TEXT PRIMARY KEY,
            value       TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );
        
PRAGMA user_version = 1;

-- Migration2: personal branch PR sync log

        CREATE TABLE IF NOT EXISTS pr_sync_log (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            pr_number       INTEGER NOT NULL,
            pr_title        TEXT NOT NULL DEFAULT '',
            pr_branch       TEXT NOT NULL DEFAULT '',
            merge_sha       TEXT NOT NULL,
            merge_strategy  TEXT NOT NULL DEFAULT 'unknown',
            svn_rev_start   INTEGER,
            svn_rev_end     INTEGER,
            commit_count    INTEGER NOT NULL DEFAULT 0,
            status          TEXT NOT NULL DEFAULT 'pending',
            error_message   TEXT,
            detected_at     TEXT NOT NULL,
            completed_at    TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_pr_sync_log_merge_sha ON pr_sync_log (merge_sha);
        CREATE INDEX IF NOT EXISTS idx_pr_sync_log_status ON pr_sync_log (status);
        
PRAGMA user_version = 2;

-- Migration3: add success column to audit_log

        ALTER TABLE audit_log ADD COLUMN success INTEGER NOT NULL DEFAULT 1;
        CREATE INDEX IF NOT EXISTS idx_audit_log_success ON audit_log (success);
        
PRAGMA user_version = 3;

-- Migration4: import progress persistence

        CREATE TABLE IF NOT EXISTS import_progress (
            id               INTEGER PRIMARY KEY CHECK (id = 1),
            phase            TEXT NOT NULL DEFAULT 'idle',
            current_rev      INTEGER NOT NULL DEFAULT 0,
            total_revs       INTEGER NOT NULL DEFAULT 0,
            commits_created  INTEGER NOT NULL DEFAULT 0,
            batches_pushed   INTEGER NOT NULL DEFAULT 0,
            lfs_unique_count INTEGER NOT NULL DEFAULT 0,
            files_skipped    INTEGER NOT NULL DEFAULT 0,
            errors_json      TEXT NOT NULL DEFAULT '[]',
            started_at       TEXT,
            completed_at     TEXT,
            updated_at       TEXT NOT NULL DEFAULT ''
        );
        
PRAGMA user_version = 4;

-- Migration5: multi-user auth: users, credentials, sessions

        CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            display_name TEXT NOT NULL,
            email TEXT NOT NULL,
            password_hash TEXT NOT NULL,
            role TEXT NOT NULL DEFAULT 'user',
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS user_credentials (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            service TEXT NOT NULL,
            server_url TEXT NOT NULL,
            username TEXT NOT NULL,
            encrypted_value TEXT NOT NULL,
            nonce TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(user_id, service, server_url)
        );

        CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        
PRAGMA user_version = 5;

-- Migration6: multi-repository support

        CREATE TABLE IF NOT EXISTS repositories (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            svn_url TEXT NOT NULL,
            svn_branch TEXT NOT NULL DEFAULT '',
            svn_username TEXT NOT NULL DEFAULT '',
            git_provider TEXT NOT NULL DEFAULT 'github',
            git_api_url TEXT NOT NULL DEFAULT '',
            git_repo TEXT NOT NULL DEFAULT '',
            git_branch TEXT NOT NULL DEFAULT 'main',
            sync_mode TEXT NOT NULL DEFAULT 'direct',
            poll_interval_secs INTEGER NOT NULL DEFAULT 60,
            lfs_threshold_mb INTEGER NOT NULL DEFAULT 0,
            auto_merge INTEGER NOT NULL DEFAULT 1,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_by TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        ALTER TABLE commit_map ADD COLUMN repo_id TEXT;
        ALTER TABLE sync_records ADD COLUMN repo_id TEXT;
        ALTER TABLE audit_log ADD COLUMN repo_id TEXT;
        ALTER TABLE conflicts ADD COLUMN repo_id TEXT;

        CREATE TABLE IF NOT EXISTS import_progress_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            repo_id TEXT,
            phase TEXT NOT NULL DEFAULT 'idle',
            current_rev INTEGER NOT NULL DEFAULT 0,
            total_revs INTEGER NOT NULL DEFAULT 0,
            commits_created INTEGER NOT NULL DEFAULT 0,
            batches_pushed INTEGER NOT NULL DEFAULT 0,
            lfs_unique_count INTEGER NOT NULL DEFAULT 0,
            files_skipped INTEGER NOT NULL DEFAULT 0,
            errors_json TEXT NOT NULL DEFAULT '[]',
            started_at TEXT,
            completed_at TEXT,
            updated_at TEXT NOT NULL DEFAULT ''
        );
        INSERT INTO import_progress_new (id, repo_id, phase, current_rev, total_revs, commits_created, batches_pushed, lfs_unique_count, files_skipped, errors_json, started_at, completed_at, updated_at)
            SELECT id, NULL, phase, current_rev, total_revs, commits_created, batches_pushed, lfs_unique_count, files_skipped, errors_json, started_at, completed_at, updated_at FROM import_progress;
        DROP TABLE import_progress;
        ALTER TABLE import_progress_new RENAME TO import_progress;
        
PRAGMA user_version = 6;

-- Migration7: repository watermark columns

        ALTER TABLE repositories ADD COLUMN last_svn_rev INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE repositories ADD COLUMN last_git_sha TEXT NOT NULL DEFAULT '';
        ALTER TABLE repositories ADD COLUMN last_sync_at TEXT;
        ALTER TABLE repositories ADD COLUMN sync_status TEXT NOT NULL DEFAULT 'idle';
        ALTER TABLE repositories ADD COLUMN total_syncs INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE repositories ADD COLUMN total_errors INTEGER NOT NULL DEFAULT 0;
        
PRAGMA user_version = 7;

-- Migration8: add parent_id for branch sync

        ALTER TABLE repositories ADD COLUMN parent_id TEXT;
        
PRAGMA user_version = 8;

-- Migration9: encrypted secrets table

        CREATE TABLE IF NOT EXISTS encrypted_secrets (
            key TEXT PRIMARY KEY,
            ciphertext TEXT NOT NULL,
            nonce TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        
PRAGMA user_version = 9;

-- Migration10: compound indexes for multi-repo queries and resolved conflict content

        CREATE INDEX IF NOT EXISTS idx_repositories_enabled_sync ON repositories(enabled, last_sync_at);
        CREATE INDEX IF NOT EXISTS idx_commit_map_repo_svn ON commit_map(repo_id, svn_rev);
        CREATE INDEX IF NOT EXISTS idx_commit_map_repo_git ON commit_map(repo_id, git_sha);
        ALTER TABLE conflicts ADD COLUMN resolved_content TEXT;
        
PRAGMA user_version = 10;

-- Migration11: path rules and circuit breaker fields for repositories

        ALTER TABLE repositories ADD COLUMN allowed_paths TEXT;
        ALTER TABLE repositories ADD COLUMN blocked_patterns TEXT;
        ALTER TABLE repositories ADD COLUMN consecutive_errors INTEGER NOT NULL DEFAULT 0;
        
PRAGMA user_version = 11;

-- Migration12: per-repo Teams webhook URL

        ALTER TABLE repositories ADD COLUMN teams_webhook_url TEXT;
        
PRAGMA user_version = 12;
