//! Real ref observations must precede atomic legacy checkpoint confirmation.
use std::path::Path;

use reposync_core::db::Database;
use reposync_core::import::{
    confirm_personal_full_import_publication, confirm_personal_snapshot_import_publication,
    LegacyImportRef,
};

struct Origin {
    dir: tempfile::TempDir,
    sha: String,
}

impl Origin {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git");
        git2::Repository::init_bare(&bare).unwrap();
        let workdir = dir.path().join("work");
        let repo = git2::Repository::init(&workdir).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
        std::fs::write(workdir.join("file.txt"), b"confirmed\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::new(
            "Fixture",
            "fixture@example.invalid",
            &git2::Time::new(1_790_000_000, 0),
        )
        .unwrap();
        let sha = repo
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap()
            .to_string();
        let pushed = std::process::Command::new("git")
            .args(["push", "origin", "HEAD:refs/heads/main"])
            .current_dir(&workdir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            pushed.status.success(),
            "{}",
            String::from_utf8_lossy(&pushed.stderr)
        );
        Self { dir, sha }
    }

    fn workdir(&self) -> std::path::PathBuf {
        self.dir.path().join("work")
    }

    fn remote_tip(&self) -> String {
        git2::Repository::open_bare(self.dir.path().join("origin.git"))
            .unwrap()
            .find_reference("refs/heads/main")
            .unwrap()
            .target()
            .unwrap()
            .to_string()
    }
}

fn database() -> Database {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db
}

fn windows(db: &Database) -> Vec<(String, String)> {
    db.conn().prepare("SELECT key,value FROM kv_state WHERE key LIKE 'pair_refresh_writer_window_%' ORDER BY key")
        .unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
        .collect::<Result<_, _>>().unwrap()
}

async fn confirm(db: &Database, origin: &Origin, sha: &str, snapshot: bool) -> anyhow::Result<()> {
    let window = db.begin_ordinary_writer_window(None).unwrap();
    window.mark_unjournalled_publication_possible().unwrap();
    let workdir = origin.workdir();
    let publication = LegacyImportRef {
        workdir: &workdir,
        remote: "origin",
        branch: "main",
        token: None,
        sha,
    };
    if snapshot {
        confirm_personal_snapshot_import_publication(&window, publication, 7, "svn", "git").await?;
    } else {
        confirm_personal_full_import_publication(&window, publication, 7).await?;
    }
    // Model cancellation after confirmed publication: dropping preparation is
    // sufficient, and must preserve the last independently verified checkpoints.
    drop(window);
    Ok(())
}

#[tokio::test]
async fn confirmed_full_and_snapshot_refs_advance_checkpoints_before_cancellation_releases() {
    for snapshot in [false, true] {
        let origin = Origin::new();
        let db = database();
        confirm(&db, &origin, &origin.sha, snapshot).await.unwrap();
        assert_eq!(db.get_watermark("svn_rev").unwrap(), Some("7".into()));
        assert_eq!(
            db.get_watermark("git_sha").unwrap(),
            Some(origin.sha.clone())
        );
        assert!(windows(&db).is_empty());
        assert_eq!(origin.remote_tip(), origin.sha);
        assert_eq!(db.list_commit_map(10).unwrap().len(), usize::from(snapshot));
        drop(db.begin_ordinary_writer_window(None).unwrap());
    }
}

#[tokio::test]
async fn different_remote_ref_retains_uncertainty_and_never_advances_either_checkpoint() {
    for snapshot in [false, true] {
        let origin = Origin::new();
        let db = database();
        let error = confirm(&db, &origin, &"f".repeat(40), snapshot)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("writer window retained"),
            "{error:#}"
        );
        assert!(db.get_watermark("svn_rev").unwrap().is_none());
        assert!(db.get_watermark("git_sha").unwrap().is_none());
        assert!(db.list_commit_map(10).unwrap().is_empty());
        assert_eq!(windows(&db).len(), 1);
        assert!(windows(&db)[0]
            .1
            .contains("unjournalled_publication_possible"));
        assert!(db.begin_ordinary_writer_window(None).is_err());
        assert_eq!(origin.remote_tip(), origin.sha);
    }
}

#[tokio::test]
async fn checkpoint_storage_failure_rolls_back_mapping_and_both_cursors_and_retains_uncertainty() {
    for snapshot in [false, true] {
        let origin = Origin::new();
        let db = database();
        db.conn().execute_batch("CREATE TRIGGER fail_import_git_checkpoint BEFORE INSERT ON watermarks WHEN NEW.source='git_sha' BEGIN SELECT RAISE(ABORT,'fixture checkpoint refusal'); END").unwrap();
        let error = confirm(&db, &origin, &origin.sha, snapshot)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("checkpoint refused; writer window retained"),
            "{error:#}"
        );
        assert!(db.get_watermark("svn_rev").unwrap().is_none());
        assert!(db.get_watermark("git_sha").unwrap().is_none());
        assert!(db.list_commit_map(10).unwrap().is_empty());
        let receipts: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM sync_records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(receipts, 0);
        assert_eq!(windows(&db).len(), 1);
        assert!(windows(&db)[0]
            .1
            .contains("unjournalled_publication_possible"));
        assert!(db.begin_ordinary_writer_window(None).is_err());
        assert_eq!(origin.remote_tip(), origin.sha);
    }
}
