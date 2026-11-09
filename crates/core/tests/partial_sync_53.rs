//! #53 SVN→Git filtering with early #60 assertions, on real private SVN and
//! Git repositories driven through the actual `SyncEngine` cycle.
//!
//! Each case starts from a fresh `svnadmin` repository (fresh UUID), commits
//! an SVN-origin baseline, imports it through the pinned snapshot importer and
//! completes that import, then runs ordinary engine cycles with path rules.
//! "Reopen" drops the engine, database and writer fence and opens them again.
//! These cases require `svn`, `svnadmin` and `git`; they fail, never skip,
//! when the toolchain is missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use tokio::sync::RwLock;

use reposync_core::config::{AppConfig, IdentityConfig, SvnLayout};
use reposync_core::db::import_operations::import_target_fingerprint;
use reposync_core::db::lineage_checkpoints::{
    load_mapping_outcome, svn_source_key, CheckpointDirection, MappingOutcome,
};
use reposync_core::db::nullable_commit_map::CommitMapTarget;
use reposync_core::db::team_cycle_mapping_operations::{
    TeamCycleMappingOutcome, TeamCycleMappingState,
};
use reposync_core::db::Database;
use reposync_core::errors::SyncError;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::identity::mapper::IdentityMapper;
use reposync_core::import::{ImportConfig, ImportOutcome, ImportProgress, ImportRunState};
use reposync_core::models::Repository;
use reposync_core::snapshot::{resolve_snapshot_pin, SnapshotRevision};
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_core::writer_fence;
use tempfile::TempDir;

const REPO: &str = "partial-53";

/// This target serializes its own real-fence cases; nothing else shares it.
fn serial() -> &'static tokio::sync::Mutex<()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &LOCK
}

fn require_toolchain() {
    for tool in ["svn", "svnadmin", "git"] {
        let found = Command::new(tool)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(found, "#53 native case requires `{tool}` on PATH");
    }
}

fn run(dir: &Path, program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("LC_ALL", "C.UTF-8")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Partial fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Partial fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// SVN CLI operand for a literal working-copy path (peg-escape any `@`).
fn op(path: &str) -> String {
    format!("{path}@")
}

/// An engine and the real writer-fence guard held for it. Fields drop in
/// order: the engine and its database first, then the guard, which releases
/// the lease. Dropping this is a true close; the next open reclaims.
struct OwnedEngine {
    engine: SyncEngine,
    _fence: writer_fence::WriterFenceGuard,
}

impl std::ops::Deref for OwnedEngine {
    type Target = SyncEngine;
    fn deref(&self) -> &SyncEngine {
        &self.engine
    }
}

/// The durable writer-fence lease, read through a fresh handle.
fn fence_lease(db_path: &Path) -> Option<String> {
    use rusqlite::OptionalExtension;
    Database::new(db_path)
        .unwrap()
        .conn()
        .query_row(
            "SELECT value FROM kv_state WHERE key = 'writer_fence_v1:lease'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

struct Fixture {
    tmp: TempDir,
    svn_url: String,
    wc: PathBuf,
    bare: PathBuf,
    git_work: PathBuf,
    data_dir: PathBuf,
    db_path: PathBuf,
    developer: Option<PathBuf>,
}

impl Fixture {
    /// Fresh SVN repository and baseline r1, imported and completed before
    /// any mapping or watermark exists.
    async fn new(baseline: &[(&str, &str)]) -> Self {
        require_toolchain();
        let tmp = TempDir::new().unwrap();
        let repo_dir = tmp.path().join("svn_repo");
        run(
            tmp.path(),
            "svnadmin",
            &["create", repo_dir.to_str().unwrap()],
        );
        let hook = repo_dir.join("hooks/pre-revprop-change");
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let svn_url = format!("file://{}", repo_dir.display());
        let wc = tmp.path().join("svn_wc");
        run(
            tmp.path(),
            "svn",
            &[
                "checkout",
                &svn_url,
                wc.to_str().unwrap(),
                "--non-interactive",
            ],
        );
        let fixture_paths = Fixture {
            svn_url: svn_url.clone(),
            wc: wc.clone(),
            bare: tmp.path().join("origin.git"),
            git_work: tmp.path().join("git_work"),
            data_dir: tmp.path().join("data"),
            // The canonical name: WriterFenceGuard::drop releases its lease in
            // `<data_dir>/reposync.db`. Any other name leaves the lease behind.
            db_path: tmp.path().join("data").join("reposync.db"),
            developer: None,
            tmp,
        };
        for (path, bytes) in baseline {
            fixture_paths.write(path, bytes);
        }
        // Observation-only: the projection producer records what it actually read
        // and planned here (debug builds); dumped only if this case panics.
        std::env::set_var(
            "REPOSYNC_RS53_OBSERVE_DIR",
            fixture_paths.tmp.path().join("observe"),
        );
        let baseline_rev = fixture_paths.commit("baseline r1");
        fixture_paths.import(baseline_rev).await;
        fixture_paths
    }

    fn write(&self, path: &str, bytes: &str) {
        let file = self.wc.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let existed = file.exists();
        std::fs::write(&file, bytes).unwrap();
        if !existed {
            run(&self.wc, "svn", &["add", "--parents", "--force", &op(path)]);
        }
    }

    fn svn(&self, args: &[&str]) {
        run(&self.wc, "svn", args);
    }

    fn commit(&self, message: &str) -> i64 {
        let out = run(
            &self.wc,
            "svn",
            &["commit", "-m", message, ".", "--non-interactive"],
        );
        run(&self.wc, "svn", &["update", "--non-interactive"]);
        out.lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("Committed revision ")
                    .map(|rev| rev.trim_end_matches('.').parse::<i64>().unwrap())
            })
            .expect("committed revision")
    }

    async fn import(&self, selected: i64) {
        git2::Repository::init_bare(&self.bare).unwrap();
        let git_client = Arc::new(std::sync::Mutex::new(
            GitClient::init(&self.git_work).unwrap(),
        ));
        run(&self.git_work, "git", &["checkout", "-B", "main"]);
        run(
            &self.git_work,
            "git",
            &[
                "remote",
                "add",
                "origin",
                &format!("file://{}", self.bare.display()),
            ],
        );
        std::fs::create_dir_all(&self.data_dir).unwrap();
        let db = Database::new(&self.db_path).unwrap();
        db.initialize().unwrap();
        let fence = writer_fence::claim(&db, &self.data_dir).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let repo = Repository {
            id: REPO.into(),
            name: "Partial".into(),
            svn_url: self.svn_url.clone(),
            svn_branch: String::new(),
            svn_username: String::new(),
            git_provider: "local".into(),
            git_api_url: format!("file://{}", self.tmp.path().display()),
            git_repo: "origin".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 5,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 0,
            last_git_sha: String::new(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        };
        db.insert_repository(&repo).unwrap();
        let op = db
            .create_import_operation(
                REPO,
                "fixture",
                "partial-53",
                &import_target_fingerprint(&repo, &self.git_work),
            )
            .unwrap();
        let svn = SvnClient::new(&self.svn_url, "", "");
        let pin = resolve_snapshot_pin(&svn, SnapshotRevision::Number(selected))
            .await
            .unwrap();
        db.pin_snapshot_import(REPO, &op.id, pin.clone()).unwrap();
        db.start_import_operation(REPO, &op.id).unwrap();
        let outcome = reposync_core::import::run_snapshot_import(
            &svn,
            &git_client,
            &db,
            &FilePolicy::new(0, Vec::new()),
            &ImportConfig {
                committer_name: "Test User".into(),
                committer_email: "test@example.com".into(),
                remote_name: "origin".into(),
                branch: "main".into(),
                push_token: None,
                message_prefix: None,
                trunk_path: String::new(),
            },
            &pin,
            ImportRunState {
                progress: Arc::new(RwLock::new(ImportProgress::default())),
                ws_broadcast: None,
                repo_id: Some(REPO.into()),
                operation_id: Some(op.id.clone()),
                cancel_signal: None,
            },
        )
        .await
        .unwrap();
        let ImportOutcome::Completed {
            svn_rev, git_sha, ..
        } = outcome
        else {
            panic!("baseline import must complete: {outcome:?}");
        };
        assert_eq!(svn_rev, selected);
        db.complete_import_operation(REPO, &op.id, svn_rev, &git_sha)
            .unwrap();
        drop(db);
        drop(fence);
        assert_eq!(
            fence_lease(&self.db_path),
            None,
            "the import's writer fence is really released"
        );
    }

    /// A real reopen: a new database handle, startup holds, a new engine.
    /// Admit `allowed`/`blocked` on the repository row (JSON arrays, NULL
    /// when empty) and build the engine's rules from that row exactly as the
    /// daemon scheduler does. This is the production authority for policy.
    fn engine(&self, allowed: &[&str], blocked: &[&str]) -> OwnedEngine {
        self.admit_policy(allowed, blocked);
        self.engine_with_rules_from_row()
    }

    fn admit_policy(&self, allowed: &[&str], blocked: &[&str]) {
        let encode = |rules: &[&str]| -> Option<String> {
            (!rules.is_empty()).then(|| serde_json::to_string(rules).unwrap())
        };
        let db = Database::new(&self.db_path).unwrap();
        db.conn()
            .execute(
                "UPDATE repositories SET allowed_paths = ?1, blocked_patterns = ?2 WHERE id = ?3",
                rusqlite::params![encode(allowed), encode(blocked), REPO],
            )
            .unwrap();
    }

    fn engine_with_rules_from_row(&self) -> OwnedEngine {
        let mut owned = self.engine_without_rules();
        let engine = &mut owned.engine;
        let row = engine.db().get_repository(REPO).unwrap().unwrap();
        let parse = |raw: Option<&str>| -> Vec<String> {
            raw.map(|raw| serde_json::from_str::<Vec<String>>(raw).unwrap())
                .unwrap_or_default()
        };
        let allowed = parse(row.allowed_paths.as_deref());
        let blocked = parse(row.blocked_patterns.as_deref());
        if !allowed.is_empty() || !blocked.is_empty() {
            engine.set_path_rules(allowed, blocked);
        }
        owned
    }

    /// An engine whose in-memory rules are NOT the admitted row policy.
    fn engine_with_stale_rules(&self, allowed: &[&str]) -> OwnedEngine {
        let mut owned = self.engine_without_rules();
        owned
            .engine
            .set_path_rules(allowed.iter().map(|s| s.to_string()).collect(), Vec::new());
        owned
    }

    /// A real open: a new database handle, a real writer-fence claim held for
    /// this engine's whole life, startup holds, a new engine.
    fn engine_without_rules(&self) -> OwnedEngine {
        let mut config: AppConfig = toml::from_str(&format!(
            "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"{}\"\nusername = \"\"\npassword_env = \"RS53_UNUSED\"\n[github]\nrepo = \"fixture/partial\"\ntoken_env = \"RS53_UNUSED\"\n",
            self.data_dir.display(),
            self.svn_url
        ))
        .unwrap();
        config.svn.layout = SvnLayout::Custom;
        config.svn.password = Some(String::new());
        config.github.token = Some(String::new());
        let db = Database::new(&self.db_path).unwrap();
        db.initialize().unwrap();
        let fence = writer_fence::claim(&db, &self.data_dir).unwrap();
        db.hold_interrupted_svn_commits().unwrap();
        db.hold_interrupted_git_pushes().unwrap();
        db.hold_interrupted_team_cycle_mappings().unwrap();
        let mut engine = SyncEngine::new(
            config,
            db,
            SvnClient::new(&self.svn_url, "", ""),
            GitClient::new(&self.git_work).unwrap(),
            Arc::new(
                IdentityMapper::new(&IdentityConfig {
                    email_domain: Some("example.invalid".into()),
                    ..Default::default()
                })
                .unwrap(),
            ),
        );
        engine.set_repo_id(REPO.into());
        OwnedEngine {
            engine,
            _fence: fence,
        }
    }

    fn git(&self, args: &[&str]) -> String {
        let mut full = vec!["--git-dir", self.bare.to_str().unwrap()];
        full.extend_from_slice(args);
        run(self.tmp.path(), "git", &full)
    }

    fn git_tip(&self) -> String {
        self.git(&["rev-parse", "main"]).trim().to_owned()
    }

    fn git_commits(&self) -> usize {
        self.git(&["rev-list", "--count", "main"])
            .trim()
            .parse()
            .unwrap()
    }

    fn git_file(&self, path: &str) -> Option<String> {
        let listed = self.git(&["ls-tree", "-r", "--name-only", "-z", "main"]);
        listed
            .split('\0')
            .any(|name| name == path)
            .then(|| self.git(&["show", &format!("main:{path}")]))
    }

    fn git_paths(&self) -> Vec<String> {
        let listed = self.git(&["ls-tree", "-r", "--name-only", "-z", "main"]);
        listed
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn svn_head(&self) -> i64 {
        let out = run(
            &self.wc,
            "svn",
            &["info", "--show-item", "revision", &self.svn_url],
        );
        out.trim().parse().unwrap()
    }

    fn svn_file(&self, path: &str) -> Option<String> {
        let output = Command::new("svn")
            .args(["cat", &format!("{}/{}@HEAD", self.svn_url, path)])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap())
    }

    /// A Git developer clone that pushes to the bare origin.
    fn developer(&mut self) -> PathBuf {
        if self.developer.is_none() {
            let dir = self.tmp.path().join("developer");
            run(
                self.tmp.path(),
                "git",
                &[
                    "clone",
                    "-b",
                    "main",
                    self.bare.to_str().unwrap(),
                    dir.to_str().unwrap(),
                ],
            );
            self.developer = Some(dir);
        }
        self.developer.clone().unwrap()
    }

    fn git_push(&mut self, files: &[(&str, &str)], message: &str) -> String {
        let dev = self.developer();
        run(&dev, "git", &["pull", "--ff-only", "origin", "main"]);
        for (path, bytes) in files {
            let file = dev.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, bytes).unwrap();
            run(&dev, "git", &["add", "--", path]);
        }
        run(&dev, "git", &["commit", "-m", message]);
        run(&dev, "git", &["push", "origin", "main"]);
        run(&dev, "git", &["rev-parse", "HEAD"]).trim().to_owned()
    }
}

/// Observation-only dump, printed only when a case is already failing.
/// It reads Git, files and records; it never changes them.
impl Drop for Fixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}", self.observations());
        }
        std::env::remove_var("REPOSYNC_RS53_OBSERVE_DIR");
    }
}

impl Fixture {
    /// Secondary, bounded, read-only evidence. It never panics (no unwrap on any
    /// path), so it cannot mask the failing assertion, whose message the panic hook
    /// has already printed. Each git command keeps its own stdout, stderr and exit.
    fn observations(&self) -> String {
        const FILE_CAP: usize = 64 * 1024;
        const ROW_CAP: usize = 200;
        const TOTAL_CAP: usize = 512 * 1024;
        fn bounded(bytes: &[u8], cap: usize) -> String {
            let shown = &bytes[..bytes.len().min(cap)];
            let mut text = String::from_utf8_lossy(shown).into_owned();
            if bytes.len() > cap {
                text += &format!("\n<truncated: {} of {} bytes shown>\n", cap, bytes.len());
            }
            text
        }
        let git = |dir: &Path, args: &[&str]| -> String {
            match Command::new("git").args(args).current_dir(dir).output() {
                Ok(out) => format!(
                    "exit={:?}\nstdout:\n{}stderr:\n{}",
                    out.status.code(),
                    bounded(&out.stdout, FILE_CAP),
                    bounded(&out.stderr, FILE_CAP)
                ),
                Err(e) => format!("<git not run: {e}>\n"),
            }
        };
        let bare = self.bare.to_string_lossy().into_owned();
        let mut out = String::from("=== RS53 OBSERVATION (secondary, read-only, bounded) ===\n");
        let mut files: Vec<PathBuf> = std::fs::read_dir(self.tmp.path().join("observe"))
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        files.sort();
        for file in files {
            let bytes = std::fs::read(&file).unwrap_or_default();
            out += &format!(
                "--- producer {} ---\n{}\n",
                file.display(),
                bounded(&bytes, FILE_CAP)
            );
        }
        out += "--- bare main: commit tree subject ---\n";
        out += &git(
            self.tmp.path(),
            &["--git-dir", &bare, "log", "--format=%H %T %s", "main"],
        );
        out += "--- bare main tree ---\n";
        out += &git(
            self.tmp.path(),
            &["--git-dir", &bare, "ls-tree", "-r", "main"],
        );
        out += "--- bridge index ---\n";
        out += &git(&self.git_work, &["ls-files", "-s"]);
        out += "--- bridge worktree status ---\n";
        out += &git(
            &self.git_work,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        );
        match Database::new(&self.db_path) {
            Ok(db) => {
                let conn = db.conn();
                for sql in [
                    "SELECT id, last_svn_rev, last_git_sha, scope_uuid, allowed_paths, blocked_patterns FROM repositories",
                    "SELECT id, svn_rev, git_sha, direction, status FROM sync_records ORDER BY rowid",
                    "SELECT svn_rev, git_sha, direction, repo_id FROM commit_map ORDER BY id",
                    "SELECT * FROM canonical_checkpoints",
                    "SELECT * FROM canonical_mapping_outcomes",
                    "SELECT key, value FROM kv_state WHERE key LIKE '%git%' OR key LIKE '%svn_rev%' OR key LIKE 'team_cycle_mapping%' ORDER BY key",
                ] {
                    out += &format!("--- {sql} ---\n");
                    let mut stmt = match conn.prepare(sql) {
                        Ok(stmt) => stmt,
                        Err(e) => {
                            out += &format!("<{e}>\n");
                            continue;
                        }
                    };
                    let n = stmt.column_count();
                    let rows = stmt.query_map([], |row| {
                        Ok((0..n)
                            .map(|i| match row.get_ref(i) {
                                Ok(v) => format!("{v:?}"),
                                Err(e) => format!("<{e}>"),
                            })
                            .collect::<Vec<_>>()
                            .join(" | "))
                    });
                    match rows {
                        Ok(rows) => {
                            for (i, row) in rows.enumerate() {
                                if i == ROW_CAP {
                                    out += "<truncated: row cap reached>\n";
                                    break;
                                }
                                match row {
                                    Ok(row) => out += &format!("{row}\n"),
                                    Err(e) => out += &format!("<{e}>\n"),
                                }
                            }
                        }
                        Err(e) => out += &format!("<{e}>\n"),
                    }
                }
            }
            Err(e) => out += &format!("<database not opened: {e}>\n"),
        }
        if out.len() > TOTAL_CAP {
            let text = bounded(out.as_bytes(), TOTAL_CAP);
            return text;
        }
        out
    }
}

/// Current repository scope and generation, read without side effects: the scope
/// from the registration row, the generation from that scope's single canonical
/// SVN→Git checkpoint row.
fn scope_and_generation(engine: &SyncEngine) -> (String, i64) {
    let conn = engine.db().conn();
    let scope: String = conn
        .query_row(
            "SELECT scope_uuid FROM repositories WHERE id = ?1",
            [REPO],
            |r| r.get(0),
        )
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT generation FROM canonical_checkpoints WHERE scope_uuid = ?1 AND direction = 'svn_to_git'")
        .unwrap();
    let generations: Vec<i64> = stmt
        .query_map([&scope], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        generations.len(),
        1,
        "one current SVN→Git checkpoint for scope {scope}"
    );
    (scope, generations[0])
}

/// The team publication record of `rev`, equal to `sha` everywhere: exactly one
/// applied svn_to_git `sync_records` row; the canonical AppliedAndVerified outcome
/// in the current scope/generation; the push journal's observed SHA and tree.
/// The team publication finalizer (`git_push_operations::finalize_tx`) inserts no
/// applied `commit_map` row; imports and other legacy paths do populate that table.
/// An applied revision has no NULL row either.
fn assert_team_applied(engine: &SyncEngine, fx: &Fixture, rev: i64, sha: &str) {
    let applied: Vec<String> = {
        let conn = engine.db().conn();
        let mut stmt = conn
            .prepare(
                "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND svn_rev = ?2 \
                 AND direction = 'svn_to_git' AND status = 'applied'",
            )
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![REPO, rev], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows
    };
    assert_eq!(
        applied,
        vec![sha.to_owned()],
        "one applied sync record for r{rev}"
    );
    let (scope, generation) = scope_and_generation(engine);
    let outcome = load_mapping_outcome(
        &engine.db().conn(),
        &scope,
        generation,
        CheckpointDirection::SvnToGit,
        &svn_source_key(rev),
    )
    .unwrap()
    .expect("canonical SVN→Git outcome");
    assert_eq!(outcome.outcome, MappingOutcome::AppliedAndVerified);
    assert_eq!(outcome.target_git_sha.as_deref(), Some(sha));
    let op = engine
        .db()
        .latest_git_push_operation(REPO)
        .unwrap()
        .expect("push journal");
    assert_eq!(op.source_svn_rev, rev);
    assert_eq!(op.last_confirmed_git_sha.as_deref(), Some(sha));
    let tree = fx.git(&["rev-parse", &format!("{sha}^{{tree}}")]);
    assert_eq!(op.last_confirmed_git_tree.as_deref(), Some(tree.trim()));
    assert_eq!(
        filtered_row_count(engine, rev),
        0,
        "no NULL row for an applied revision"
    );
}

/// The current SVN→Git checkpoint is `rev`, filtered with no target.
fn assert_svn_checkpoint_filtered(engine: &SyncEngine, rev: i64) {
    let (scope, generation) = scope_and_generation(engine);
    let (key, outcome, target): (String, String, Option<String>) = engine
        .db()
        .conn()
        .query_row(
            "SELECT handled_source_key, outcome, target_git_sha FROM canonical_checkpoints \
             WHERE scope_uuid = ?1 AND generation = ?2 AND direction = 'svn_to_git'",
            rusqlite::params![scope, generation],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(key, svn_source_key(rev));
    assert_eq!(
        outcome,
        MappingOutcome::IntentionallyFilteredNoTarget.as_str()
    );
    assert_eq!(target, None);
}

fn rows(engine: &SyncEngine) -> Vec<(i64, Option<String>, CommitMapTarget)> {
    let mut listed: Vec<_> = engine
        .db()
        .list_commit_map_rows(Some(REPO), 1000)
        .unwrap()
        .into_iter()
        .map(|row| (row.svn_rev, row.git_sha, row.target))
        .collect();
    listed.sort_by_key(|row| row.0);
    listed
}

fn filtered_row_count(engine: &SyncEngine, rev: i64) -> usize {
    rows(engine)
        .into_iter()
        .filter(|(r, sha, target)| {
            *r == rev && sha.is_none() && *target == CommitMapTarget::FilteredNoTarget
        })
        .count()
}

fn latest_cycle_mapping(
    engine: &SyncEngine,
) -> Option<reposync_core::db::team_cycle_mapping_operations::TeamCycleMappingOperation> {
    let id = engine
        .db()
        .get_state(&format!("team_cycle_mapping_v1:latest:{REPO}"))
        .unwrap()?;
    engine
        .db()
        .get_team_cycle_mapping_operation(REPO, &id)
        .unwrap()
}

fn audit_count(engine: &SyncEngine, action: &str) -> i64 {
    engine
        .db()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = ?1 AND repo_id = ?2",
            [action, REPO],
            |row| row.get(0),
        )
        .unwrap()
}

fn svn_watermark(engine: &SyncEngine) -> i64 {
    engine.db().get_repo_watermark(REPO).unwrap().0
}

fn inbound_git_cursor(engine: &SyncEngine) -> Option<String> {
    reposync_core::echo_receipt_scope::read_scoped_last_git_sha_kv(engine.db(), REPO).unwrap()
}

/// Assert that `rev` was finalized as filtered exactly once, atomically.
fn assert_filtered_once(engine: &SyncEngine, rev: i64) {
    assert_eq!(
        filtered_row_count(engine, rev),
        1,
        "one proven NULL row for r{rev}"
    );
    let op = latest_cycle_mapping(engine).expect("filtered journal operation");
    assert_eq!(op.outcome, TeamCycleMappingOutcome::SvnFiltered);
    assert_eq!(op.state, TeamCycleMappingState::Completed);
    assert_eq!(op.source_svn_rev, Some(rev));
    assert!(op.source_git_sha.is_none());
    assert!(engine
        .db()
        .active_team_cycle_mapping_operation(REPO)
        .unwrap()
        .is_none());
    assert!(
        reposync_core::echo_receipt_scope::read_svn_no_target_receipt(engine.db(), REPO, rev)
            .unwrap()
            .is_some(),
        "scoped SVN no-target receipt for r{rev}"
    );
}

const BASELINE: &[(&str, &str)] = &[
    ("team-a/a.txt", "team-a baseline\n"),
    ("team-a/old.txt", "team-a old\n"),
    ("team-b/b.txt", "team-b baseline\n"),
    ("team-b/c.txt", "team-b c\n"),
    ("docs/readme.txt", "docs baseline\n"),
];

// ---------------------------------------------------------------------------
// SC01-SC06
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sc01_only_admitted_paths_and_bytes_enter_git_and_excluded_work_is_accounted() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    let commits_before = fx.git_commits();
    fx.write("team-a/a.txt", "team-a changed\n");
    fx.write("team-a/new.txt", "team-a new\n");
    let r2 = fx.commit("r2 admitted");
    fx.write("team-b/b.txt", "team-b changed\n");
    fx.write("docs/readme.txt", "docs changed\n");
    let r3 = fx.commit("r3 excluded only");
    let engine = fx.engine(&["team-a/"], &[]);
    let stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(
        stats.svn_to_git_count, 1,
        "only the admitted revision commits"
    );
    assert_eq!(fx.git_commits(), commits_before + 1);
    assert_eq!(
        fx.git_file("team-a/a.txt").as_deref(),
        Some("team-a changed\n")
    );
    assert_eq!(
        fx.git_file("team-a/new.txt").as_deref(),
        Some("team-a new\n")
    );
    assert_eq!(
        fx.git_file("team-b/b.txt").as_deref(),
        Some("team-b baseline\n")
    );
    assert_eq!(
        fx.git_file("docs/readme.txt").as_deref(),
        Some("docs baseline\n")
    );
    assert!(fx
        .git(&["log", "-1", "--format=%B", "main"])
        .contains(&format!("synced from SVN r{r2}")));
    assert_eq!(
        svn_watermark(&engine),
        r3,
        "the scoped frontier accounts for r3"
    );
    assert_filtered_once(&engine, r3);
    // The current remote tip at this point is r2's publication.
    let tip = fx.git_tip();
    assert_team_applied(&engine, &fx, r2, &tip);
    // The later r3 is the current checkpoint: filtered, no target.
    assert_svn_checkpoint_filtered(&engine, r3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sc02_fully_excluded_revision_has_no_git_effect_and_reopen_repeat_is_quiet() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    let tip = fx.git_tip();
    let refs = fx.git(&["for-each-ref"]);
    fx.write("team-b/b.txt", "team-b only\n");
    let r2 = fx.commit("r2 excluded");
    let engine = fx.engine(&["team-a/"], &[]);
    let cursor = inbound_git_cursor(&engine);
    let stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.svn_to_git_count, 0);
    assert_eq!(fx.git_tip(), tip, "no Git commit");
    assert_eq!(fx.git(&["for-each-ref"]), refs, "no ref effect");
    assert_eq!(svn_watermark(&engine), r2);
    assert_eq!(
        inbound_git_cursor(&engine),
        cursor,
        "inbound Git cursor unchanged"
    );
    assert_filtered_once(&engine, r2);
    assert_eq!(audit_count(&engine, "svn_to_git_filtered"), 1);
    let rows_before = rows(&engine);
    assert!(
        fence_lease(&fx.db_path).is_some(),
        "the running engine holds the fence"
    );
    drop(engine);
    assert_eq!(
        fence_lease(&fx.db_path),
        None,
        "closing the engine releases its fence before any reopen"
    );
    let reopened = fx.engine(&["team-a/"], &[]);
    assert!(
        fence_lease(&fx.db_path).is_some(),
        "the reopened engine reclaims the fence"
    );
    for _ in 0..2 {
        let quiet = reopened.run_sync_cycle().await.unwrap();
        assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
    }
    assert_eq!(
        rows(&reopened),
        rows_before,
        "no extra rows after reopen/repeat"
    );
    assert_eq!(audit_count(&reopened, "svn_to_git_filtered"), 1);
    assert_eq!(fx.git_tip(), tip);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sc03_straddling_revision_applies_selected_bytes_and_deletes_only() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    fx.write("team-a/a.txt", "team-a straddle\n");
    fx.svn(&["rm", &op("team-a/old.txt")]);
    fx.write("team-b/b.txt", "team-b straddle\n");
    fx.svn(&["rm", &op("team-b/c.txt")]);
    let r2 = fx.commit("r2 straddles");
    let engine = fx.engine(&["team-a/"], &[]);
    let stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.svn_to_git_count, 1);
    assert_eq!(
        fx.git_file("team-a/a.txt").as_deref(),
        Some("team-a straddle\n")
    );
    assert_eq!(
        fx.git_file("team-a/old.txt"),
        None,
        "selected delete applied"
    );
    assert_eq!(
        fx.git_file("team-b/b.txt").as_deref(),
        Some("team-b baseline\n")
    );
    assert_eq!(
        fx.git_file("team-b/c.txt").as_deref(),
        Some("team-b c\n"),
        "unrelated tree kept"
    );
    assert_eq!(
        fx.git_file("docs/readme.txt").as_deref(),
        Some("docs baseline\n")
    );
    let message = fx.git(&["log", "-1", "--format=%B", "main"]);
    assert!(message.starts_with("r2 straddles"));
    assert!(message.contains(&format!("synced from SVN r{r2}")));
    assert_eq!(
        filtered_row_count(&engine, r2),
        0,
        "a straddling revision is applied, not filtered"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sc04_git_mixed_commit_reaches_svn_with_outside_paths_untouched() {
    let _serial = serial().lock().await;
    let mut fx = Fixture::new(BASELINE).await;
    let engine = fx.engine(&["team-a/"], &[]);
    engine.run_sync_cycle().await.unwrap();
    let head = fx.svn_head();
    fx.git_push(
        &[
            ("team-a/from-git.txt", "from git admitted\n"),
            ("team-b/from-git.txt", "from git outside\n"),
        ],
        "Git mixed commit",
    );
    let stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.git_to_svn_count, 1);
    assert_eq!(fx.svn_head(), head + 1);
    assert_eq!(
        fx.svn_file("team-a/from-git.txt").as_deref(),
        Some("from git admitted\n")
    );
    assert_eq!(
        fx.svn_file("team-b/from-git.txt"),
        None,
        "outside path untouched in SVN"
    );
    assert_eq!(
        fx.svn_file("team-b/b.txt").as_deref(),
        Some("team-b baseline\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sc05_git_excluded_only_commit_produces_no_svn_revision_and_repeats_quietly() {
    let _serial = serial().lock().await;
    let mut fx = Fixture::new(BASELINE).await;
    let engine = fx.engine(&["team-a/"], &[]);
    engine.run_sync_cycle().await.unwrap();
    let head = fx.svn_head();
    let sha = fx.git_push(
        &[("team-b/only.txt", "outside only\n")],
        "Git excluded-only commit",
    );
    let stats = engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.git_to_svn_count, 0);
    assert_eq!(fx.svn_head(), head, "no SVN revision");
    assert!(
        reposync_core::echo_receipt_scope::read_git_no_target_receipt(engine.db(), REPO, &sha)
            .unwrap()
            .is_some(),
        "existing Git no-target receipt"
    );
    let quiet = engine.run_sync_cycle().await.unwrap();
    assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
    assert_eq!(fx.svn_head(), head);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sc06_svn_git_edit_svn_round_trip_keeps_exact_bytes_without_duplication() {
    let _serial = serial().lock().await;
    let mut fx = Fixture::new(BASELINE).await;
    fx.write("team-a/round.txt", "from svn\n");
    fx.write("team-b/noise.txt", "excluded noise\n");
    let r2 = fx.commit("r2 svn origin");
    let engine = fx.engine(&["team-a/"], &[]);
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    assert_eq!(
        fx.git_file("team-a/round.txt").as_deref(),
        Some("from svn\n")
    );
    assert_eq!(fx.git_file("team-b/noise.txt"), None);
    let r2_publication = fx.git_tip();
    assert_team_applied(&engine, &fx, r2, &r2_publication);
    fx.git_push(&[("team-a/round.txt", "edited in git\n")], "Git edit");
    assert_eq!(engine.run_sync_cycle().await.unwrap().git_to_svn_count, 1);
    let head = fx.svn_head();
    assert_eq!(
        fx.svn_file("team-a/round.txt").as_deref(),
        Some("edited in git\n")
    );
    let tip = fx.git_tip();
    let commits = fx.git_commits();
    let mapped = rows(&engine);
    assert_team_applied(&engine, &fx, r2, &r2_publication);
    drop(engine);
    let reopened = fx.engine(&["team-a/"], &[]);
    for _ in 0..2 {
        let quiet = reopened.run_sync_cycle().await.unwrap();
        assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
    }
    assert_eq!(
        (fx.svn_head(), fx.git_tip(), fx.git_commits()),
        (head, tip, commits)
    );
    assert_eq!(
        rows(&reopened),
        mapped,
        "no duplicated mapping after reopen"
    );
}

// ---------------------------------------------------------------------------
// Mandatory controls
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_policy_keeps_the_legacy_full_apply() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    fx.write("team-b/b.txt", "legacy full apply\n");
    let r2 = fx.commit("r2 unfiltered");
    let engine = fx.engine(&[], &[]);
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    assert_eq!(
        fx.git_file("team-b/b.txt").as_deref(),
        Some("legacy full apply\n")
    );
    assert_eq!(filtered_row_count(&engine, r2), 0);
    assert_eq!(audit_count(&engine, "svn_to_git_filtered"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn component_matching_and_literal_names_are_mapped_exactly_once() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(&[
        ("team/seed.txt", "seed\n"),
        ("team-other/seed.txt", "seed\n"),
    ])
    .await;
    fx.write("team/plain.txt", "plain\n");
    fx.write("team/sp ace @-dash.txt", "literal\n");
    fx.write("team/ünïcödé.txt", "unicode\n");
    fx.write("team/%41.txt", "percent stays literal\n");
    fx.write("team-other/y.txt", "sibling prefix\n");
    fx.commit("r2 names");
    let engine = fx.engine(&["team"], &[]);
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    let paths = fx.git_paths();
    for admitted in [
        "team/plain.txt",
        "team/sp ace @-dash.txt",
        "team/ünïcödé.txt",
        "team/%41.txt",
    ] {
        assert!(
            paths.iter().any(|p| p == admitted),
            "{admitted} in {paths:?}"
        );
    }
    assert_eq!(
        fx.git_file("team/%41.txt").as_deref(),
        Some("percent stays literal\n")
    );
    assert!(!paths.iter().any(|p| p == "team/A.txt"), "no URL decoding");
    assert!(
        !paths.iter().any(|p| p == "team-other/y.txt"),
        "component-aware match"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_rules_alone_project_with_the_same_namespace() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    fx.write("team-a/keep.txt", "kept\n");
    fx.write("team-a/drop.log", "blocked\n");
    fx.commit("r2 blocked pattern");
    let engine = fx.engine(&[], &["*.log"]);
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    assert_eq!(fx.git_file("team-a/keep.txt").as_deref(), Some("kept\n"));
    assert_eq!(fx.git_file("team-a/drop.log"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ancestor_directory_delete_removes_admitted_descendants_only() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(&[
        ("team-a/sub/s.txt", "admitted descendant\n"),
        ("team-a/keep.txt", "outside the admitted subtree\n"),
        ("team-b/b.txt", "team-b baseline\n"),
    ])
    .await;
    fx.svn(&["rm", &op("team-a")]);
    fx.commit("r2 deletes the ancestor directory");
    let engine = fx.engine(&["team-a/sub/"], &[]);
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    assert_eq!(
        fx.git_file("team-a/sub/s.txt"),
        None,
        "admitted descendant deleted"
    );
    assert_eq!(
        fx.git_file("team-a/keep.txt").as_deref(),
        Some("outside the admitted subtree\n"),
        "excluded sibling under the same ancestor is never pruned"
    );
    assert_eq!(
        fx.git_file("team-b/b.txt").as_deref(),
        Some("team-b baseline\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copies_never_delete_sources_and_moves_across_the_boundary_are_endpoint_scoped() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    let engine = fx.engine(&["team-a/"], &[]);
    fx.svn(&["copy", &op("team-a/a.txt"), &op("team-b/copied.txt")]);
    let r2 = fx.commit("r2 copy out of scope");
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 0);
    assert_eq!(
        fx.git_file("team-a/a.txt").as_deref(),
        Some("team-a baseline\n"),
        "copy source kept"
    );
    assert_eq!(fx.git_file("team-b/copied.txt"), None);
    assert_filtered_once(&engine, r2);
    fx.svn(&["move", &op("team-b/b.txt"), &op("team-a/moved-in.txt")]);
    fx.commit("r3 move into scope");
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    assert_eq!(
        fx.git_file("team-a/moved-in.txt").as_deref(),
        Some("team-b baseline\n")
    );
    assert_eq!(
        fx.git_file("team-b/b.txt").as_deref(),
        Some("team-b baseline\n"),
        "excluded delete not applied"
    );
    fx.svn(&["move", &op("team-a/old.txt"), &op("team-b/moved-out.txt")]);
    fx.commit("r4 move out of scope");
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    assert_eq!(
        fx.git_file("team-a/old.txt"),
        None,
        "admitted source endpoint deleted"
    );
    assert_eq!(
        fx.git_file("team-b/moved-out.txt"),
        None,
        "blocked endpoint not added"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_directory_and_plain_property_are_filtered_but_git_meaning_property_holds() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    let engine = fx.engine(&["team-a/"], &[]);
    fx.svn(&["mkdir", &op("team-a/empty-dir")]);
    let r2 = fx.commit("r2 empty directory");
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 0);
    assert_filtered_once(&engine, r2);
    fx.svn(&["propset", "custom:note", "value", &op("team-a/a.txt")]);
    let r3 = fx.commit("r3 plain property");
    assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 0);
    assert_filtered_once(&engine, r3);
    let tip = fx.git_tip();
    fx.svn(&["propset", "svn:executable", "*", &op("team-a/a.txt")]);
    let r4 = fx.commit("r4 executable bit");
    fx.write("team-a/after.txt", "later dependent work\n");
    fx.commit("r5 later work");
    let held = engine.run_sync_cycle().await.unwrap_err();
    assert!(
        matches!(&held, SyncError::HistoryBlocked { reason, .. } if reason == "svn_projection_held"),
        "{held:?}"
    );
    assert_eq!(svn_watermark(&engine), r3, "r{r4} and later stay pending");
    assert_eq!(
        filtered_row_count(&engine, r4),
        0,
        "a hold never counts as filtering"
    );
    assert_eq!(fx.git_tip(), tip);
    assert_eq!(fx.git_file("team-a/after.txt"), None);
    assert_eq!(audit_count(&engine, "svn_to_git_projection_held"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_selected_patch_stops_at_the_frontier_and_never_counts_as_filtered() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    fx.write("team-a/a.txt", "selected bytes\n");
    let r2 = fx.commit("r2 selected");
    fx.write("team-b/b.txt", "excluded later\n");
    let r3 = fx.commit("r3 excluded later");
    let engine = fx.engine(&["team-a/"], &[]);
    let tip = fx.git_tip();
    // Debug fixture hook of the unchanged apply path: it replaces r2's patch
    // bytes on this engine, so the real `git apply` subprocess fails.
    engine.set_svn_apply_test_fault(r2, &fx.git_work);
    let failed = engine.run_sync_cycle().await;
    engine.clear_svn_apply_test_fault();
    assert!(failed.is_err(), "{failed:?}");
    assert_eq!(fx.git_tip(), tip);
    assert!(svn_watermark(&engine) < r2, "r{r2} stays pending");
    assert_eq!(filtered_row_count(&engine, r2), 0);
    assert_eq!(
        filtered_row_count(&engine, r3),
        0,
        "later revision stays pending"
    );
    let recovered = engine.run_sync_cycle().await.unwrap();
    assert_eq!(recovered.svn_to_git_count, 1);
    assert_eq!(
        fx.git_file("team-a/a.txt").as_deref(),
        Some("selected bytes\n")
    );
    assert_filtered_once(&engine, r3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filtered_finalizer_write_fault_rolls_back_atomically_then_fresh_cycle_finalizes_once() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    fx.write("team-b/b.txt", "excluded\n");
    let r2 = fx.commit("r2 excluded");
    let engine = fx.engine(&["team-a/"], &[]);
    let receipts_before: i64 = engine
        .db()
        .conn()
        .query_row("SELECT COUNT(*) FROM kv_state", [], |row| row.get(0))
        .unwrap();
    engine
        .db()
        .conn()
        .execute_batch(
            "CREATE TRIGGER rs53_filtered_write_fault BEFORE INSERT ON commit_map \
             WHEN NEW.git_sha IS NULL BEGIN SELECT RAISE(ABORT,'rs53 filtered write fault'); END;",
        )
        .unwrap();
    let failed = engine.run_sync_cycle().await.unwrap_err();
    assert!(
        failed.to_string().contains("rs53 filtered write fault"),
        "{failed}"
    );
    assert_eq!(svn_watermark(&engine), 1, "watermark rolled back");
    assert!(
        rows(&engine).iter().all(|(rev, _, _)| *rev != r2),
        "no NULL row"
    );
    assert!(
        latest_cycle_mapping(&engine).is_none(),
        "no journal operation survives"
    );
    assert!(
        reposync_core::echo_receipt_scope::read_svn_no_target_receipt(engine.db(), REPO, r2)
            .unwrap()
            .is_none()
    );
    let receipts_after: i64 = engine
        .db()
        .conn()
        .query_row("SELECT COUNT(*) FROM kv_state", [], |row| row.get(0))
        .unwrap();
    assert!(receipts_after >= receipts_before);
    drop(engine);
    {
        let reopened = Database::new(&fx.db_path).unwrap();
        reopened.initialize().unwrap();
        let recovery_fence = writer_fence::claim(&reopened, &fx.data_dir).unwrap();
        reopened.hold_interrupted_team_cycle_mappings().unwrap();
        assert!(reopened
            .active_team_cycle_mapping_operation(REPO)
            .unwrap()
            .is_none());
        reopened
            .conn()
            .execute_batch("DROP TRIGGER rs53_filtered_write_fault")
            .unwrap();
        // Close the database first, then release the guard.
        drop(reopened);
        drop(recovery_fence);
    }
    assert_eq!(
        fence_lease(&fx.db_path),
        None,
        "the recovery reopen releases its fence before the next engine claims"
    );
    let engine = fx.engine(&["team-a/"], &[]);
    let fresh = engine.run_sync_cycle().await.unwrap();
    assert_eq!(fresh.svn_to_git_count, 0);
    assert_filtered_once(&engine, r2);
    let quiet = engine.run_sync_cycle().await.unwrap();
    assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
    assert_eq!(filtered_row_count(&engine, r2), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_scope_conflict_with_pending_git_work_stays_held() {
    let _serial = serial().lock().await;
    let mut fx = Fixture::new(BASELINE).await;
    let engine = fx.engine(&["team-a/"], &[]);
    engine.run_sync_cycle().await.unwrap();
    fx.git_push(
        &[("team-a/a.txt", "git side\n")],
        "Git edits the admitted file",
    );
    fx.write("team-a/a.txt", "svn side\n");
    fx.commit("r2 svn edits the same admitted file");
    let watermark = svn_watermark(&engine);
    let held = engine.run_sync_cycle().await.unwrap_err();
    assert!(
        matches!(held, SyncError::UnresolvableConflict { .. }),
        "{held:?}"
    );
    assert_eq!(svn_watermark(&engine), watermark);
    assert_eq!(fx.svn_file("team-a/a.txt").as_deref(), Some("svn side\n"));
    assert_eq!(fx.git_file("team-a/a.txt").as_deref(), Some("git side\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaced_svn_repository_identity_cannot_mint_filtered_completion() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    let repo_dir = fx.tmp.path().join("svn_repo");
    std::fs::remove_dir_all(&repo_dir).unwrap();
    run(
        fx.tmp.path(),
        "svnadmin",
        &["create", repo_dir.to_str().unwrap()],
    );
    let other = fx.tmp.path().join("other_wc");
    run(
        fx.tmp.path(),
        "svn",
        &[
            "checkout",
            &fx.svn_url,
            other.to_str().unwrap(),
            "--non-interactive",
        ],
    );
    std::fs::create_dir_all(other.join("team-b")).unwrap();
    std::fs::write(other.join("team-b/b.txt"), "foreign\n").unwrap();
    run(
        &other,
        "svn",
        &["add", "--parents", "--force", "team-b/b.txt@"],
    );
    run(
        &other,
        "svn",
        &["commit", "-m", "foreign r1", ".", "--non-interactive"],
    );
    std::fs::write(other.join("team-b/b.txt"), "foreign r2\n").unwrap();
    run(
        &other,
        "svn",
        &["commit", "-m", "foreign r2", ".", "--non-interactive"],
    );
    let engine = fx.engine(&["team-a/"], &[]);
    let refused = format!("{:?}", engine.run_sync_cycle().await.unwrap_err());
    assert!(
        refused.contains("svn_identity_changed") || refused.contains("does not match pinned uuid"),
        "changed SVN UUID is refused as an identity change: {refused}"
    );
    assert!(!refused.contains("WriterFenced"), "{refused}");
    assert_eq!(filtered_row_count(&engine, 2), 0);
    assert!(latest_cycle_mapping(&engine).is_none());
    assert_eq!(svn_watermark(&engine), 1);
}

// ---------------------------------------------------------------------------
// RS53-R1 / RS53-R2 regressions
// ---------------------------------------------------------------------------

/// The revision stays at the durable frontier: no Git effect, no watermark
/// movement, no filtered row, later revisions pending, one held audit row.
fn assert_held_at_frontier(
    engine: &SyncEngine,
    fx: &Fixture,
    tip: &str,
    held_rev: i64,
    later_rev: i64,
) {
    assert_eq!(fx.git_tip(), tip, "no Git commit for the held revision");
    assert_eq!(
        fx.git_file("team-a/a.txt").as_deref(),
        Some("team-a baseline\n"),
        "selected content not applied"
    );
    assert_eq!(
        fx.git_file("team-a/later.txt"),
        None,
        "later revision pending"
    );
    assert_eq!(svn_watermark(engine), held_rev - 1);
    assert_eq!(filtered_row_count(engine, held_rev), 0);
    assert_eq!(filtered_row_count(engine, later_rev), 0);
    assert!(latest_cycle_mapping(engine).is_none());
    assert_eq!(audit_count(engine, "svn_to_git_projection_held"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_selected_content_with_a_selected_executable_change_on_another_file_holds() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    let engine = fx.engine(&["team-a/"], &[]);
    let tip = fx.git_tip();
    fx.write("team-a/a.txt", "selected content\n");
    fx.svn(&["propset", "svn:executable", "*", &op("team-a/old.txt")]);
    let held = fx.commit("r2 content plus a mode-only change");
    fx.write("team-a/later.txt", "later dependent work\n");
    let later = fx.commit("r3 later work");
    let error = engine.run_sync_cycle().await.unwrap_err();
    assert!(
        matches!(&error, SyncError::HistoryBlocked { reason, detail }
            if reason == "svn_projection_held" && detail.contains("svn:executable")),
        "{error:?}"
    );
    assert_held_at_frontier(&engine, &fx, &tip, held, later);
    let again = engine.run_sync_cycle().await.unwrap_err();
    assert!(
        matches!(again, SyncError::HistoryBlocked { .. }),
        "the hold persists: {again:?}"
    );
    assert_eq!(fx.git_tip(), tip);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_selected_content_with_a_selected_symlink_holds() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    let engine = fx.engine(&["team-a/"], &[]);
    let tip = fx.git_tip();
    fx.write("team-a/a.txt", "selected content\n");
    std::os::unix::fs::symlink("a.txt", fx.wc.join("team-a/link")).unwrap();
    fx.svn(&["add", &op("team-a/link")]);
    let held = fx.commit("r2 content plus a symlink");
    fx.write("team-a/later.txt", "later dependent work\n");
    let later = fx.commit("r3 later work");
    let error = engine.run_sync_cycle().await.unwrap_err();
    assert!(
        matches!(&error, SyncError::HistoryBlocked { reason, detail }
            if reason == "svn_projection_held" && detail.contains("svn:special")),
        "{error:?}"
    );
    assert_held_at_frontier(&engine, &fx, &tip, held, later);
    assert_eq!(fx.git_file("team-a/link"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r2_engine_policy_that_is_not_the_admitted_policy_is_refused_before_any_effect() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    fx.admit_policy(&["team-a/"], &[]);
    let engine = fx.engine_with_stale_rules(&["team-b/"]);
    let tip = fx.git_tip();
    fx.write("team-a/a.txt", "would be excluded under the stale rules\n");
    let r2 = fx.commit("r2");
    let error = engine.run_sync_cycle().await.unwrap_err();
    assert!(error.to_string().contains("admitted policy"), "{error}");
    assert_eq!(fx.git_tip(), tip);
    assert_eq!(svn_watermark(&engine), 1);
    assert_eq!(filtered_row_count(&engine, r2), 0);
    assert!(latest_cycle_mapping(&engine).is_none());
    assert_eq!(audit_count(&engine, "svn_to_git_filtered"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r2_admitted_policy_change_refuses_the_stale_engine_and_a_fresh_engine_follows_it() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    let stale = fx.engine(&["team-a/"], &[]);
    fx.write("team-b/b.txt", "excluded under team-a\n");
    let r2 = fx.commit("r2 team-b");
    assert_eq!(stale.run_sync_cycle().await.unwrap().svn_to_git_count, 0);
    assert_filtered_once(&stale, r2);
    let tip = fx.git_tip();
    // The operator admits a different policy; this engine still holds the old one.
    fx.admit_policy(&["team-b/"], &[]);
    fx.write("team-b/b.txt", "admitted under team-b\n");
    let r3 = fx.commit("r3 team-b");
    let error = stale.run_sync_cycle().await.unwrap_err();
    assert!(error.to_string().contains("admitted policy"), "{error}");
    assert_eq!(fx.git_tip(), tip);
    assert_eq!(svn_watermark(&stale), r2);
    assert_eq!(filtered_row_count(&stale, r3), 0);
    drop(stale);
    // r3 depends on r2's bytes, which the old policy filtered forever (forward-only,
    // no reimport). The fresh engine must fail closed at the durable frontier:
    // no reset, no repair, no automatic catch-up.
    let fresh = fx.engine_with_rules_from_row();
    let failed = fresh.run_sync_cycle().await.unwrap_err();
    assert!(
        format!("{failed:?}").contains("ApplyFailed"),
        "dependent history fails to apply: {failed:?}"
    );
    assert_eq!(fx.git_tip(), tip, "no Git effect");
    assert_eq!(
        fx.git_file("team-b/b.txt").as_deref(),
        Some("team-b baseline\n"),
        "the diverged file is neither repaired nor reset"
    );
    assert_eq!(
        svn_watermark(&fresh),
        r2,
        "r3 stays pending at the frontier"
    );
    assert_eq!(
        filtered_row_count(&fresh, r3),
        0,
        "a failed apply is never filtering"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r2_fresh_engine_follows_a_widened_admitted_policy_for_a_new_selected_path() {
    let _serial = serial().lock().await;
    let fx = Fixture::new(BASELINE).await;
    let stale = fx.engine(&["team-a/"], &[]);
    fx.write("team-b/b.txt", "excluded under team-a\n");
    let r2 = fx.commit("r2 team-b");
    assert_eq!(stale.run_sync_cycle().await.unwrap().svn_to_git_count, 0);
    assert_filtered_once(&stale, r2);
    fx.admit_policy(&["team-b/"], &[]);
    fx.write("team-b/new-admitted.txt", "new under team-b\n");
    let r3 = fx.commit("r3 new team-b path");
    let refused = stale.run_sync_cycle().await.unwrap_err();
    assert!(refused.to_string().contains("admitted policy"), "{refused}");
    drop(stale);
    let fresh = fx.engine_with_rules_from_row();
    assert_eq!(fresh.run_sync_cycle().await.unwrap().svn_to_git_count, 1);
    assert_eq!(
        fx.git_file("team-b/new-admitted.txt").as_deref(),
        Some("new under team-b\n")
    );
    assert_eq!(
        fx.git_file("team-b/b.txt").as_deref(),
        Some("team-b baseline\n"),
        "no automatic catch-up of filtered history"
    );
    assert_eq!(svn_watermark(&fresh), r3);
    let tip = fx.git_tip();
    assert_team_applied(&fresh, &fx, r3, &tip);
}
