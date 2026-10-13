//! Recovery startup must preserve existing evidence before authentication.
use reposync_core::db::Database;

fn state(db: &Database) -> Vec<(String, String, String)> {
    db.conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn recovery_open_keeps_corrupt_evidence_and_lease_byte_identical() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("reposync.db");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    db.ensure_repository_scope_schema_upgrade().unwrap();
    db.set_state("writer_fence_v1:lease", "unknown-owner-evidence")
        .unwrap();
    db.set_state(
        "pair_refresh_execution_v3:authority:retained",
        "corrupt-retained-evidence",
    )
    .unwrap();
    let saved = state(&db);
    let opened = Database::open_existing_for_pair_refresh_recovery(&path).unwrap();
    assert_eq!(state(&opened), saved);
    drop(opened);
    assert_eq!(state(&db), saved);
    assert!(!temp.path().join("reposync.lock").exists());
}

#[test]
fn recovery_open_refuses_missing_database_without_creating_any_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("reposync.db");
    assert!(Database::open_existing_for_pair_refresh_recovery(&path).is_err());
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn recovery_open_refuses_schema_or_journal_conversion_and_symlink_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("reposync.db");
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    db.ensure_repository_scope_schema_upgrade().unwrap();
    db.set_state("marker", "preserve").unwrap();
    let saved = state(&db);
    for version in [12, 14] {
        db.conn()
            .pragma_update(None, "user_version", version)
            .unwrap();
        assert!(Database::open_existing_for_pair_refresh_recovery(&path).is_err());
        assert_eq!(
            db.conn()
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            version
        );
        assert_eq!(state(&db), saved);
    }
    db.conn().pragma_update(None, "user_version", 13).unwrap();
    let alias = temp.path().join("alias.db");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    assert!(Database::open_existing_for_pair_refresh_recovery(&alias).is_err());
    assert_eq!(state(&db), saved);
    db.conn()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
        .unwrap();
    assert!(Database::open_existing_for_pair_refresh_recovery(&path).is_err());
    assert_eq!(
        db.conn()
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert_eq!(state(&db), saved);
}

fn config() -> reposync_core::config::AppConfig {
    toml::from_str(
        r#"
[daemon]
data_dir="/unused"
[svn]
url="file:///unused"
username=""
password_env=""
[github]
repo="fixture/repo"
token_env=""
"#,
    )
    .unwrap()
}

#[test]
fn recovery_secret_loading_never_migrates_plaintext_or_creates_keys() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db.set_state("secret_svn_password", "disposable-svn")
        .unwrap();
    db.set_state("secret_git_token", "disposable-git").unwrap();
    db.set_state("secret_admin_password", "disposable-admin")
        .unwrap();
    let saved = state(&db);
    let mut cfg = config();
    cfg.resolve_recovery_secrets_from_db(&db).unwrap();
    assert_eq!(cfg.svn.password.as_deref(), Some("disposable-svn"));
    assert_eq!(cfg.github.token.as_deref(), Some("disposable-git"));
    assert_eq!(cfg.web.admin_password.as_deref(), Some("disposable-admin"));
    assert_eq!(state(&db), saved);
    assert!(db.get_state("encryption_key").unwrap().is_none());
    assert!(db.get_encrypted_secret("svn_password").unwrap().is_none());
    assert!(db.get_encrypted_secret("git_token").unwrap().is_none());
    assert!(db
        .get_state("secret_admin_password_hash")
        .unwrap()
        .is_none());
}

#[test]
fn recovery_secret_loading_uses_existing_key_and_refuses_missing_key_without_fallback() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let key = [51; 32];
    let (ciphertext, nonce) =
        reposync_core::crypto::encrypt_credential("disposable-encrypted", &key).unwrap();
    db.store_encrypted_secret("git_token", &ciphertext, &nonce)
        .unwrap();
    db.set_state("secret_git_token", "do-not-resurrect")
        .unwrap();
    let saved = state(&db);
    assert!(config().resolve_recovery_secrets_from_db(&db).is_err());
    assert_eq!(state(&db), saved);
    assert_eq!(
        db.get_encrypted_secret("git_token").unwrap(),
        Some((ciphertext.clone(), nonce.clone()))
    );
    db.set_state("encryption_key", &hex::encode(key)).unwrap();
    let saved = state(&db);
    let mut cfg = config();
    cfg.resolve_recovery_secrets_from_db(&db).unwrap();
    assert_eq!(cfg.github.token.as_deref(), Some("disposable-encrypted"));
    assert_eq!(state(&db), saved);
    assert_eq!(
        db.get_encrypted_secret("git_token").unwrap(),
        Some((ciphertext, nonce))
    );
}

#[test]
fn recovery_git_open_preserves_sidecar_without_using_it_or_creating_storage() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing");
    assert!(
        reposync_core::git::GitClient::open_existing_for_pair_refresh_recovery(&missing).is_err()
    );
    assert!(!missing.exists());
    let git = temp.path().join("git");
    let repo = git2::Repository::init(&git).unwrap();
    let sidecar = repo.path().join("reposync/clean-http-token");
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    std::fs::write(&sidecar, b"disposable-legacy-token").unwrap();
    let opened =
        reposync_core::git::GitClient::open_existing_for_pair_refresh_recovery(&git).unwrap();
    assert_eq!(std::fs::read(&sidecar).unwrap(), b"disposable-legacy-token");
    assert!(opened.stored_http_auth_token().is_none());
}

#[test]
fn ordinary_startup_refuses_orphan_native_jobs_before_checkpoint_or_migration() {
    for suffix in ["document", "authority", "hold", "request"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("reposync.db");
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        db.ensure_repository_scope_schema_upgrade().unwrap();
        db.conn()
            .execute_batch("PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        db.set_state("writer_fence_v1:lease", "preserve-unknown-owner")
            .unwrap();
        db.set_state("secret_git_token", "preserve-plaintext")
            .unwrap();
        db.set_state(
            &format!("pair_refresh_execution_v3:{suffix}:orphan"),
            "preserve-evidence",
        )
        .unwrap();
        let before = state(&db);
        let wal = path.with_extension("db-wal");
        let main_bytes = std::fs::read(&path).unwrap();
        let wal_bytes = std::fs::read(&wal).unwrap();
        assert!(
            Database::open_with_exclusive_owner(temp.path()).is_err(),
            "{suffix}"
        );
        assert_eq!(state(&db), before, "{suffix}");
        assert_eq!(std::fs::read(&path).unwrap(), main_bytes, "{suffix}");
        assert_eq!(std::fs::read(&wal).unwrap(), wal_bytes, "{suffix}");
        assert!(db.get_state("encryption_key").unwrap().is_none());
        assert!(db.get_encrypted_secret("git_token").unwrap().is_none());
    }
}
