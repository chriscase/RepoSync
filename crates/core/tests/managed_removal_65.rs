//! Codec/authority assertions; native effects are qualified separately.
use reposync_core::db::managed_remove::{
    completed_for_foreign_journal, validated_foreign_document, DeletionPhase, DeletionReceipt,
    ManagedRemoveRemoteOutcome, ManagedRemoveState, RemovalAdvance, RemovalChoices,
    ReviewedRemoval, ReviewedRemovalPlan, REVIEWED_REMOVAL_POLICY,
};
use reposync_core::db::Database;
use reposync_core::models::Repository;

fn repository() -> Repository {
    let now = chrono::Utc::now().to_rfc3339();
    Repository {
        id: "rs65-codec".into(),
        name: "Synthetic codec repository".into(),
        svn_url: "file:///synthetic".into(),
        svn_branch: "trunk".into(),
        svn_username: String::new(),
        git_provider: "github".into(),
        git_api_url: "file:///synthetic.git".into(),
        git_repo: "fixture/repo".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 60,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        parent_id: None,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    }
}

fn prepared() -> (
    Database,
    reposync_core::db::managed_remove::ManagedRemoveOperation,
) {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let repo = repository();
    db.insert_repository(&repo).unwrap();
    let RemovalAdvance::Cleanup { operation } = db
        .prepare_managed_remove(&repo.id, "synthetic-admin", "synthetic-request")
        .unwrap()
    else {
        panic!("expected quiet storage fixture")
    };
    (db, operation)
}

#[test]
fn rs65_legacy_codec_preserves_valid_bytes_defaults_and_refuses_unknown_versions() {
    let (db, op) = prepared();
    let done = db
        .complete_managed_remove(&op.repo_id, &op.id, &ManagedRemoveRemoteOutcome::default())
        .unwrap();
    let raw = serde_json::to_string(&done).unwrap();
    assert!(!raw.contains("reviewed"));
    assert!(completed_for_foreign_journal(&raw, &done.id, &done.repo_id).unwrap());
    assert!(!completed_for_foreign_journal(&raw, "foreign-id", &done.repo_id).unwrap());
    assert!(!completed_for_foreign_journal(&raw, &done.id, "foreign-owner").unwrap());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&raw).unwrap(),
        validated_foreign_document(&raw).unwrap()
    );
    let mut old = serde_json::to_value(&done).unwrap();
    for key in ["restore_supported", "remote_git", "remote_svn"] {
        old.as_object_mut().unwrap().remove(key);
    }
    assert!(
        completed_for_foreign_journal(&old.to_string(), &done.id, &done.repo_id).unwrap(),
        "valid omitted legacy defaults remain readable"
    );
    let mut future = done;
    future.version = 3;
    assert!(validated_foreign_document(&serde_json::to_string(&future).unwrap()).is_err());
}

#[test]
fn rs65_prepared_v2_is_typed_but_never_completed_authority() {
    let (db, mut op) = prepared();
    let scope =
        reposync_core::db::repo_scope_identity::repository_scope_uuid(&db.conn(), &op.repo_id)
            .unwrap();
    let mut plan = ReviewedRemovalPlan {
        version: 2,
        policy: REVIEWED_REMOVAL_POLICY.into(),
        actor: op.initiator_id.clone(),
        repo_id: op.repo_id.clone(),
        scope_uuid: scope,
        parent_id: None,
        parent_scope_uuid: None,
        configuration_digest: "a".repeat(64),
        managed_root: "/synthetic-owned-root".into(),
        choices: RemovalChoices::default(),
        git: None,
        svn: None,
        approval_digest: String::new(),
    };
    plan.approval_digest = plan.digest().unwrap();
    plan.validate().unwrap();
    let receipt = |side: &str| DeletionReceipt {
        phase: DeletionPhase::NotRequested,
        operation_marker: format!("{}:{side}", op.id),
        confirmed_svn_revision: None,
        observed_git_sha: None,
        evidence_digest: None,
        errors: Vec::new(),
    };
    let git = receipt("git");
    let svn = receipt("svn");
    op.version = 2;
    op.target_fingerprint = plan.approval_digest.clone();
    op.reviewed = Some(Box::new(ReviewedRemoval {
        plan,
        git,
        svn,
        cleanup_started: false,
        cleanup_completed: false,
        cleanup_errors: Vec::new(),
    }));
    let raw = serde_json::to_string(&op).unwrap();
    assert!(validated_foreign_document(&raw).is_ok());
    assert!(!completed_for_foreign_journal(&raw, &op.id, &op.repo_id).unwrap());
    let mut fake_done = op.clone();
    fake_done.state = ManagedRemoveState::Completed;
    assert!(
        validated_foreign_document(&serde_json::to_string(&fake_done).unwrap()).is_err(),
        "a terminal label alone is not native completion"
    );
    let mut blended = op.clone();
    blended.version = 1;
    assert!(validated_foreign_document(&serde_json::to_string(&blended).unwrap()).is_err());
    let unknown = raw.replacen("\"version\":2", "\"version\":2,\"unknown\":true", 1);
    assert!(validated_foreign_document(&unknown).is_err());
    let duplicate = raw.replacen("\"version\":2", "\"version\":2,\"version\":2", 1);
    assert!(validated_foreign_document(&duplicate).is_err());
    let nested_duplicate = raw.replacen(
        "\"policy\":\"reviewed_managed_removal_v2\"",
        "\"policy\":\"reviewed_managed_removal_v2\",\"policy\":\"reviewed_managed_removal_v2\"",
        1,
    );
    assert!(validated_foreign_document(&nested_duplicate).is_err());
    let nested_unknown = raw.replacen(
        "\"configuration_digest\":",
        "\"unrecognized_plan_evidence\":false,\"configuration_digest\":",
        1,
    );
    assert!(validated_foreign_document(&nested_unknown).is_err());
    let mut changed = op;
    changed.reviewed.as_mut().unwrap().plan.actor = "foreign".into();
    assert!(validated_foreign_document(&serde_json::to_string(&changed).unwrap()).is_err());
}

async fn native_command(dir: &std::path::Path, program: &str, args: &[&str]) -> String {
    let mut command = tokio::process::Command::new(program);
    command
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Synthetic removal fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Synthetic removal fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
    let out = reposync_core::process::run(command, std::time::Duration::from_secs(15), None)
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

struct NativeFixture {
    root: tempfile::TempDir,
    db: Database,
    data: std::path::PathBuf,
    child: Repository,
    svn: String,
    bare: std::path::PathBuf,
}
async fn native_fixture() -> NativeFixture {
    let root = tempfile::tempdir().unwrap();
    let p = root.path();
    let svn = p.join("svn");
    native_command(p, "svnadmin", &["create", svn.to_str().unwrap()]).await;
    let svn = reqwest::Url::from_directory_path(&svn)
        .unwrap()
        .to_string()
        .trim_end_matches('/')
        .to_string();
    native_command(
        p,
        "svn",
        &[
            "mkdir",
            &format!("{svn}/trunk"),
            &format!("{svn}/branches"),
            "-m",
            "fixture directories",
        ],
    )
    .await;
    native_command(
        p,
        "svn",
        &[
            "copy",
            &format!("{svn}/trunk"),
            &format!("{svn}/branches/dev"),
            "-m",
            "fixture branch",
        ],
    )
    .await;
    let bare = p.join("upstream.git");
    native_command(p, "git", &["init", "--bare", bare.to_str().unwrap()]).await;
    let work = p.join("seed");
    std::fs::create_dir(&work).unwrap();
    native_command(&work, "git", &["init", "-b", "main"]).await;
    std::fs::write(work.join("keep.txt"), "exact synthetic content\n").unwrap();
    native_command(&work, "git", &["add", "keep.txt"]).await;
    native_command(&work, "git", &["commit", "-m", "synthetic root"]).await;
    native_command(
        &work,
        "git",
        &[
            "push",
            bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
            "HEAD:refs/heads/dev",
        ],
    )
    .await;
    let db = Database::new(p.join("state.db")).unwrap();
    db.initialize().unwrap();
    let mut parent = repository();
    parent.id = format!("parent-{}", uuid::Uuid::new_v4());
    parent.svn_url = svn.clone();
    parent.git_api_url = reqwest::Url::from_directory_path(p).unwrap().to_string();
    parent.git_repo = "upstream".into();
    db.insert_repository(&parent).unwrap();
    let mut child = parent.clone();
    child.id = format!("child-{}", uuid::Uuid::new_v4());
    child.name = "Synthetic native child".into();
    child.parent_id = Some(parent.id);
    child.git_branch = "dev".into();
    child.svn_branch = "branches/dev".into();
    db.insert_repository(&child).unwrap();
    let data = p.join("data");
    std::fs::create_dir_all(data.join("repos").join(&child.id)).unwrap();
    std::fs::write(
        data.join("repos").join(&child.id).join("owned.txt"),
        "custody\n",
    )
    .unwrap();
    std::fs::create_dir_all(data.join("repos/sibling")).unwrap();
    std::fs::write(data.join("repos/sibling/keep.txt"), "sibling\n").unwrap();
    NativeFixture {
        root,
        db,
        data,
        child,
        svn,
        bare,
    }
}

#[tokio::test]
async fn rs65_reviewed_native_completion_is_retained_repeatable_and_restorable() {
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use reposync_core::managed_remove::reviewed;
        let f = native_fixture().await;
        let choices = RemovalChoices {
            delete_git: true,
            delete_svn: true,
        };
        let plan = reviewed::capture(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            choices.clone(),
        )
        .await
        .unwrap();
        let op = reviewed::begin(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            choices,
            &plan.approval_digest,
            "native-request",
        )
        .await
        .unwrap();
        assert_eq!(op.version, 2);
        assert!(f.db.get_repository(&f.child.id).unwrap().is_some());
        let done = reviewed::run(&f.db, &f.data, &f.child.id, "synthetic-admin", &op.id)
            .await
            .unwrap();
        assert_eq!(done.state, ManagedRemoveState::Completed, "{done:?}");
        assert_eq!(
            (&done.remote_git[..], &done.remote_svn[..]),
            ("deleted", "deleted")
        );
        assert!(f.db.get_repository(&f.child.id).unwrap().is_none());
        assert!(!f.data.join("repos").join(&f.child.id).exists());
        assert_eq!(
            std::fs::read(f.data.join("repos/sibling/keep.txt")).unwrap(),
            b"sibling\n"
        );
        let refs = native_command(
            f.root.path(),
            "git",
            &["--git-dir", f.bare.to_str().unwrap(), "show-ref"],
        )
        .await;
        assert!(refs.contains("refs/heads/main"));
        assert!(!refs.contains("refs/heads/dev"));
        let before = native_command(
            f.root.path(),
            "svnlook",
            &["youngest", f.root.path().join("svn").to_str().unwrap()],
        )
        .await;
        assert_eq!(
            reviewed::run(&f.db, &f.data, &f.child.id, "synthetic-admin", &op.id)
                .await
                .unwrap(),
            done
        );
        assert_eq!(
            native_command(
                f.root.path(),
                "svnlook",
                &["youngest", f.root.path().join("svn").to_str().unwrap()]
            )
            .await,
            before
        );
        let raw = serde_json::to_string(&done).unwrap();
        assert!(completed_for_foreign_journal(&raw, &done.id, &done.repo_id).unwrap());
        assert!(
            f.db.removal_tombstone(&f.child.id)
                .unwrap()
                .unwrap()
                .restore_supported
        );
        f.db.restore_managed_registration(&f.child.id).unwrap();
        assert!(
            !f.db.get_repository(&f.child.id).unwrap().unwrap().enabled,
            "restore never silently enables a pair whose remotes were removed"
        );
    })
    .await
    .expect("bounded real native removal fixture");
}

#[tokio::test]
async fn rs65_stale_review_and_post_claim_git_drift_preserve_all_local_custody() {
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use reposync_core::managed_remove::reviewed;
        let f = native_fixture().await;
        let choices = RemovalChoices {
            delete_git: true,
            delete_svn: true,
        };
        let plan = reviewed::capture(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            choices.clone(),
        )
        .await
        .unwrap();
        let op = reviewed::begin(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            choices,
            &plan.approval_digest,
            "drift-request",
        )
        .await
        .unwrap();
        let work = f.root.path().join("seed");
        std::fs::write(work.join("keep.txt"), "new remote content\n").unwrap();
        native_command(&work, "git", &["commit", "-am", "advance after review"]).await;
        native_command(
            &work,
            "git",
            &["push", f.bare.to_str().unwrap(), "HEAD:refs/heads/dev"],
        )
        .await;
        let svn_before = native_command(
            f.root.path(),
            "svnlook",
            &["youngest", f.root.path().join("svn").to_str().unwrap()],
        )
        .await;
        let held = reviewed::run(&f.db, &f.data, &f.child.id, "synthetic-admin", &op.id)
            .await
            .unwrap();
        assert_eq!(held.state, ManagedRemoveState::ReconciliationRequired);
        let again = reviewed::run(&f.db, &f.data, &f.child.id, "synthetic-admin", &op.id)
            .await
            .unwrap();
        assert_eq!(
            held.reviewed, again.reviewed,
            "retry cannot erase uncertainty or append evidence"
        );
        assert!(f.db.get_repository(&f.child.id).unwrap().is_some());
        assert_eq!(
            std::fs::read(f.data.join("repos").join(&f.child.id).join("owned.txt")).unwrap(),
            b"custody\n"
        );
        assert_eq!(
            native_command(
                f.root.path(),
                "svnlook",
                &["youngest", f.root.path().join("svn").to_str().unwrap()]
            )
            .await,
            svn_before
        );
        assert!(!completed_for_foreign_journal(
            &serde_json::to_string(&held).unwrap(),
            &held.id,
            &held.repo_id
        )
        .unwrap());
        assert!(f
            .db
            .prepare_managed_remove(&f.child.id, "synthetic-admin", "legacy-takeover")
            .is_err());
        assert!(!f.svn.is_empty());
    })
    .await
    .expect("bounded stale native removal fixture");
}

#[tokio::test]
async fn rs65_verified_remote_results_survive_cleanup_failure_and_database_reopen() {
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use reposync_core::managed_remove::reviewed::{self, Fault};
        let f = native_fixture().await;
        let options = RemovalChoices {
            delete_git: true,
            delete_svn: true,
        };
        let plan = reviewed::capture(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            options.clone(),
        )
        .await
        .unwrap();
        let op = reviewed::begin(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            options,
            &plan.approval_digest,
            "cleanup-request",
        )
        .await
        .unwrap();
        reviewed::set_fault(&f.child.id, Fault::BeforeCleanup);
        assert!(
            reviewed::run(&f.db, &f.data, &f.child.id, "synthetic-admin", &op.id)
                .await
                .unwrap_err()
                .to_string()
                .contains("controlled")
        );
        let retained = f.db.managed_removal(&f.child.id).unwrap().unwrap();
        assert!(retained.reviewed.as_ref().unwrap().git.phase.is_verified());
        assert!(retained.reviewed.as_ref().unwrap().svn.phase.is_verified());
        let revision = native_command(
            f.root.path(),
            "svnlook",
            &["youngest", f.root.path().join("svn").to_str().unwrap()],
        )
        .await;
        let owned = f.data.join("repos").join(&f.child.id);
        let backup = f.root.path().join("retained-owned-tree");
        std::fs::rename(&owned, &backup).unwrap();
        let outside = f.root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "unrelated\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &owned).unwrap();
        #[cfg(not(unix))]
        panic!("this real no-follow fixture requires Unix");
        let reopened = Database::new(f.root.path().join("state.db")).unwrap();
        reopened.initialize().unwrap();
        let failed = reviewed::run(&reopened, &f.data, &f.child.id, "synthetic-admin", &op.id)
            .await
            .unwrap();
        assert_eq!(failed.state, ManagedRemoveState::Failed);
        assert_eq!(failed.remote_git, "deleted");
        assert_eq!(failed.remote_svn, "deleted");
        assert_eq!(failed.reviewed.as_ref().unwrap().cleanup_errors.len(), 1);
        assert_eq!(
            std::fs::read(outside.join("keep.txt")).unwrap(),
            b"unrelated\n"
        );
        assert!(reopened.get_repository(&f.child.id).unwrap().is_some());
        let repeat = reviewed::run(&reopened, &f.data, &f.child.id, "synthetic-admin", &op.id)
            .await
            .unwrap();
        assert_eq!(
            repeat, failed,
            "identical failed preflight does not grow evidence"
        );
        std::fs::remove_file(&owned).unwrap();
        std::fs::rename(&backup, &owned).unwrap();
        let done = reviewed::run(&reopened, &f.data, &f.child.id, "synthetic-admin", &op.id)
            .await
            .unwrap();
        assert_eq!(done.state, ManagedRemoveState::Completed);
        assert_eq!(
            done.reviewed.as_ref().unwrap().cleanup_errors,
            failed.reviewed.as_ref().unwrap().cleanup_errors,
            "completion retains original cleanup cause"
        );
        assert_eq!(
            native_command(
                f.root.path(),
                "svnlook",
                &["youngest", f.root.path().join("svn").to_str().unwrap()]
            )
            .await,
            revision,
            "retry performs no second remote deletion"
        );
    })
    .await
    .expect("bounded partial cleanup/reopen fixture");
}

#[tokio::test]
async fn rs65_git_success_then_svn_rejection_retains_each_side_across_reopen() {
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use reposync_core::managed_remove::reviewed;
        let f = native_fixture().await;
        let options = RemovalChoices {
            delete_git: true,
            delete_svn: true,
        };
        let plan = reviewed::capture(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            options.clone(),
        )
        .await
        .unwrap();
        let op = reviewed::begin(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            options.clone(),
            &plan.approval_digest,
            "partial-request",
        )
        .await
        .unwrap();
        let hook = f.root.path().join("svn/hooks/pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'synthetic rejection after Git success' >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let before = native_command(
            f.root.path(),
            "svnlook",
            &["youngest", f.root.path().join("svn").to_str().unwrap()],
        )
        .await;
        let held = reviewed::run(&f.db, &f.data, &f.child.id, "synthetic-admin", &op.id)
            .await
            .unwrap();
        assert_eq!(held.state, ManagedRemoveState::ReconciliationRequired);
        assert_eq!(held.remote_git, "deleted");
        assert_eq!(held.remote_svn, "reconciliation_required");
        assert!(held.reviewed.as_ref().unwrap().svn.errors[0]
            .contains("synthetic rejection after Git success"));
        assert!(f.db.get_repository(&f.child.id).unwrap().is_some());
        assert!(f
            .data
            .join("repos")
            .join(&f.child.id)
            .join("owned.txt")
            .is_file());
        let reopened = Database::new(f.root.path().join("state.db")).unwrap();
        reopened.initialize().unwrap();
        assert_eq!(
            reviewed::run(&reopened, &f.data, &f.child.id, "synthetic-admin", &op.id)
                .await
                .unwrap(),
            held,
            "unknown side cannot lose errors, change identity or silently retry"
        );
        let changed = RemovalChoices {
            delete_git: false,
            delete_svn: false,
        };
        assert!(reviewed::begin(
            &reopened,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            changed,
            &plan.approval_digest,
            "partial-request"
        )
        .await
        .is_err());
        assert_eq!(
            native_command(
                f.root.path(),
                "svnlook",
                &["youngest", f.root.path().join("svn").to_str().unwrap()]
            )
            .await,
            before
        );
    })
    .await
    .expect("bounded partial remote/reopen fixture");
}

#[tokio::test]
async fn rs65_review_rejects_symlink_and_missing_parent_before_claim_or_effects() {
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use reposync_core::managed_remove::reviewed;
        let f = native_fixture().await;
        let owned = f.data.join("repos").join(&f.child.id);
        let backup = f.root.path().join("preserved-owned-tree");
        std::fs::rename(&owned, &backup).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&backup, &owned).unwrap();
        assert!(reviewed::capture(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            RemovalChoices {
                delete_git: true,
                delete_svn: true
            }
        )
        .await
        .is_err());
        assert!(f.db.managed_removal(&f.child.id).unwrap().is_none());
        std::fs::remove_file(&owned).unwrap();
        std::fs::rename(&backup, &owned).unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET parent_id='missing-parent' WHERE id=?1",
                [&f.child.id],
            )
            .unwrap();
        assert!(reviewed::capture(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            RemovalChoices {
                delete_git: true,
                delete_svn: true
            }
        )
        .await
        .is_err());
        assert!(f.db.get_repository(&f.child.id).unwrap().is_some());
        assert!(owned.join("owned.txt").is_file());
    })
    .await
    .expect("bounded confinement/dependency fixture");
}

#[tokio::test]
async fn rs65_real_http_git_uses_child_credentials_and_honors_revocation_and_ancestry() {
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use axum::{
            body::Bytes,
            extract::OriginalUri,
            http::{HeaderMap, Method, Response},
            routing::any,
            Router,
        };
        use base64::Engine;
        use reposync_core::managed_remove::reviewed;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let f = native_fixture().await;
        native_command(
            f.root.path(),
            "git",
            &[
                "--git-dir",
                f.bare.to_str().unwrap(),
                "config",
                "http.receivepack",
                "true",
            ],
        )
        .await;
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode("x-access-token:synthetic-child-token")
        );
        let accepted = Arc::new(AtomicUsize::new(0));
        let root = f.root.path().to_path_buf();
        let app = Router::new().fallback(any({
            let accepted = accepted.clone();
            move |method: Method, OriginalUri(uri): OriginalUri, headers: HeaderMap, body: Bytes| {
                let accepted = accepted.clone();
                let root = root.clone();
                let expected = expected.clone();
                async move {
                    if headers.get("authorization").and_then(|v| v.to_str().ok())
                        != Some(expected.as_str())
                    {
                        return Response::builder()
                            .status(401)
                            .header("WWW-Authenticate", "Basic realm=\"synthetic-git\"")
                            .body(axum::body::Body::empty())
                            .unwrap();
                    }
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let mut cmd = tokio::process::Command::new("git");
                    cmd.arg("http-backend")
                        .current_dir(&root)
                        .env("GIT_PROJECT_ROOT", &root)
                        .env("GIT_HTTP_EXPORT_ALL", "1")
                        .env("REMOTE_USER", "synthetic")
                        .env("REQUEST_METHOD", method.as_str())
                        .env("PATH_INFO", uri.path())
                        .env("QUERY_STRING", uri.query().unwrap_or(""))
                        .env(
                            "CONTENT_TYPE",
                            headers
                                .get("content-type")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or(""),
                        )
                        .env("CONTENT_LENGTH", body.len().to_string());
                    let out = reposync_core::process::run_with_input(
                        cmd,
                        &body,
                        std::time::Duration::from_secs(15),
                        None,
                    )
                    .await
                    .unwrap();
                    assert!(
                        out.status.success(),
                        "actual git http-backend: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    let boundary = out
                        .stdout
                        .windows(4)
                        .position(|b| b == b"\r\n\r\n")
                        .expect("actual CGI response header");
                    let mut response = Response::builder().status(200);
                    for line in std::str::from_utf8(&out.stdout[..boundary])
                        .unwrap()
                        .split("\r\n")
                    {
                        if let Some((name, value)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("Status") {
                                response = response.status(
                                    value
                                        .trim()
                                        .split_whitespace()
                                        .next()
                                        .unwrap()
                                        .parse::<u16>()
                                        .unwrap(),
                                );
                            } else {
                                response = response.header(name, value.trim());
                            }
                        }
                    }
                    response
                        .body(axum::body::Body::from(out.stdout[boundary + 4..].to_vec()))
                        .unwrap()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap()
        });
        let mut child = f.db.get_repository(&f.child.id).unwrap().unwrap();
        child.git_api_url = format!("http://{addr}");
        f.db.update_repository(&child).unwrap();
        let parent = child.parent_id.as_ref().unwrap();
        f.db.set_state(&format!("secret_git_token_{parent}"), "wrong-parent-token")
            .unwrap();
        f.db.set_state("secret_git_token", "wrong-global-token")
            .unwrap();
        f.db.set_state(
            &format!("secret_git_token_{}", child.id),
            "synthetic-child-token",
        )
        .unwrap();
        let options = RemovalChoices {
            delete_git: true,
            delete_svn: false,
        };
        let plan = reviewed::capture(
            &f.db,
            &f.data,
            &child.id,
            "synthetic-admin",
            options.clone(),
        )
        .await
        .unwrap();
        let op = reviewed::begin(
            &f.db,
            &f.data,
            &child.id,
            "synthetic-admin",
            options.clone(),
            &plan.approval_digest,
            "authenticated-child-delete",
        )
        .await
        .unwrap();
        let done = reviewed::run(&f.db, &f.data, &child.id, "synthetic-admin", &op.id)
            .await
            .unwrap();
        assert_eq!(done.state, ManagedRemoveState::Completed, "{done:?}");
        assert_eq!(done.remote_svn, "untouched");
        assert!(
            accepted.load(Ordering::SeqCst) >= 3,
            "real authenticated discovery and receive-pack must execute"
        );
        f.db.restore_managed_registration(&child.id).unwrap();
        native_command(
            &f.root.path().join("seed"),
            "git",
            &["push", f.bare.to_str().unwrap(), "HEAD:refs/heads/dev"],
        )
        .await;
        f.db.set_state(
            &format!("secret_git_token_{parent}"),
            "synthetic-child-token",
        )
        .unwrap();
        f.db.set_state(&format!("secret_git_token_{}", child.id), "")
            .unwrap();
        let before = accepted.load(Ordering::SeqCst);
        assert!(reviewed::capture(
            &f.db,
            &f.data,
            &child.id,
            "synthetic-admin",
            options.clone()
        )
        .await
        .is_err());
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            before,
            "explicit child revocation cannot fall back to a valid parent"
        );
        f.db.conn()
            .execute(
                "DELETE FROM kv_state WHERE key=?1",
                [format!("secret_git_token_{}", child.id)],
            )
            .unwrap();
        reviewed::capture(
            &f.db,
            &f.data,
            &child.id,
            "synthetic-admin",
            options.clone(),
        )
        .await
        .unwrap();
        assert!(
            accepted.load(Ordering::SeqCst) > before,
            "ancestor credentials work through the actual transport"
        );
        f.db.conn()
            .execute(
                "DELETE FROM kv_state WHERE key=?1",
                [format!("secret_git_token_{parent}")],
            )
            .unwrap();
        f.db.set_state("secret_git_token", "synthetic-child-token")
            .unwrap();
        reviewed::capture(&f.db, &f.data, &child.id, "synthetic-admin", options)
            .await
            .unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    })
    .await
    .expect("bounded authenticated native Git fixture");
}

#[tokio::test]
async fn rs65_restart_after_intent_and_each_remote_effect_holds_without_replay() {
    tokio::time::timeout(std::time::Duration::from_secs(90),async {
        use reposync_core::managed_remove::reviewed::{self,Fault};
        for point in [Fault::AfterGitIntent,Fault::AfterGitEffect,Fault::AfterSvnEffect] {
            let f=native_fixture().await;let options=RemovalChoices{delete_git:true,delete_svn:true};
            let plan=reviewed::capture(&f.db,&f.data,&f.child.id,"synthetic-admin",options.clone()).await.unwrap();
            let op=reviewed::begin(&f.db,&f.data,&f.child.id,"synthetic-admin",options,&plan.approval_digest,"restart-window").await.unwrap();
            reviewed::set_fault(&f.child.id,point.clone());
            assert!(reviewed::run(&f.db,&f.data,&f.child.id,"synthetic-admin",&op.id).await.unwrap_err().to_string().contains("controlled"));
            let refs= native_command(f.root.path(),"git",&["--git-dir",f.bare.to_str().unwrap(),"show-ref"]).await;
            assert_eq!(refs.contains("refs/heads/dev"),point==Fault::AfterGitIntent);
            let revision=native_command(f.root.path(),"svnlook",&["youngest",f.root.path().join("svn").to_str().unwrap()]).await;
            assert_eq!(revision.trim(),if point==Fault::AfterSvnEffect {"3"}else{"2"});
            let reopened=Database::new(f.root.path().join("state.db")).unwrap();reopened.initialize().unwrap();
            let held=reviewed::run(&reopened,&f.data,&f.child.id,"synthetic-admin",&op.id).await.unwrap();
            assert_eq!(held.state,ManagedRemoveState::ReconciliationRequired,"{point:?}: {held:?}");
            assert_eq!(held.id,op.id);assert!(!held.reviewed.as_ref().unwrap().cleanup_started);
            if point==Fault::AfterSvnEffect {assert_eq!(held.remote_git,"deleted");assert!(held.reviewed.as_ref().unwrap().svn.errors[0].contains("r3"));}
            assert!(reopened.get_repository(&f.child.id).unwrap().is_some());assert!(f.data.join("repos").join(&f.child.id).join("owned.txt").is_file());
            assert_eq!(native_command(f.root.path(),"git",&["--git-dir",f.bare.to_str().unwrap(),"show-ref"]).await,refs);
            assert_eq!(native_command(f.root.path(),"svnlook",&["youngest",f.root.path().join("svn").to_str().unwrap()]).await,revision);
            assert_eq!(reviewed::run(&reopened,&f.data,&f.child.id,"synthetic-admin",&op.id).await.unwrap(),held,"unknown predecessor/result evidence remains terminal and does not grow on repetition");
        }
    }).await.expect("bounded three-window actual native restart fixture");
}

#[tokio::test]
async fn rs65_dangling_operation_pointer_is_not_replaced_by_a_fresh_claim() {
    let f = native_fixture().await;
    f.db.set_state(
        &format!("managed_remove_v1:active:{}", f.child.id),
        "missing-document",
    )
    .unwrap();
    assert!(reposync_core::managed_remove::reviewed::capture(
        &f.db,
        &f.data,
        &f.child.id,
        "synthetic-admin",
        RemovalChoices::default()
    )
    .await
    .is_err());
    assert_eq!(
        f.db.get_state(&format!("managed_remove_v1:active:{}", f.child.id))
            .unwrap()
            .as_deref(),
        Some("missing-document")
    );
    assert!(f.db.get_repository(&f.child.id).unwrap().unwrap().enabled);
    assert!(f
        .data
        .join("repos")
        .join(&f.child.id)
        .join("owned.txt")
        .is_file());
}

#[tokio::test]
async fn rs65_unrequested_svn_stays_untouched_and_failed_git_cannot_change_choices() {
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        use reposync_core::managed_remove::reviewed;
        let f = native_fixture().await;
        let hook = f.bare.join("hooks/pre-receive");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'synthetic Git deletion denied' >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let before_refs = native_command(
            f.root.path(),
            "git",
            &["--git-dir", f.bare.to_str().unwrap(), "show-ref"],
        )
        .await;
        let before_svn = native_command(
            f.root.path(),
            "svnlook",
            &["youngest", f.root.path().join("svn").to_str().unwrap()],
        )
        .await;
        let options = RemovalChoices {
            delete_git: true,
            delete_svn: false,
        };
        let plan = reviewed::capture(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            options.clone(),
        )
        .await
        .unwrap();
        let op = reviewed::begin(
            &f.db,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            options,
            &plan.approval_digest,
            "git-rejected",
        )
        .await
        .unwrap();
        let held = reviewed::run(&f.db, &f.data, &f.child.id, "synthetic-admin", &op.id)
            .await
            .unwrap();
        assert_eq!(held.state, ManagedRemoveState::ReconciliationRequired);
        assert_eq!(held.remote_svn, "untouched");
        assert!(
            held.reviewed.as_ref().unwrap().git.errors[0].contains("synthetic Git deletion denied")
        );
        let reopened = Database::new(f.root.path().join("state.db")).unwrap();
        reopened.initialize().unwrap();
        assert!(reviewed::begin(
            &reopened,
            &f.data,
            &f.child.id,
            "synthetic-admin",
            RemovalChoices::default(),
            &plan.approval_digest,
            "git-rejected"
        )
        .await
        .is_err());
        assert_eq!(
            reviewed::run(&reopened, &f.data, &f.child.id, "synthetic-admin", &op.id)
                .await
                .unwrap(),
            held
        );
        assert_eq!(
            native_command(
                f.root.path(),
                "git",
                &["--git-dir", f.bare.to_str().unwrap(), "show-ref"]
            )
            .await,
            before_refs
        );
        assert_eq!(
            native_command(
                f.root.path(),
                "svnlook",
                &["youngest", f.root.path().join("svn").to_str().unwrap()]
            )
            .await,
            before_svn
        );
        assert!(reopened.get_repository(&f.child.id).unwrap().is_some());
        assert!(f
            .data
            .join("repos")
            .join(&f.child.id)
            .join("owned.txt")
            .is_file());
    })
    .await
    .expect("bounded failed-side and unrequested-side fixture");
}
