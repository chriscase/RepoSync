//! Disposable storage fixture for new writer-exclusion controls.
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use reposync_core::busy::{self, PairBusyGuard};
use reposync_core::data_dir::DataDirOwner;
use reposync_core::db::pair_refresh_operations::{
    capture_refresh_registration, require_no_pair_refresh_hold, RefreshJobPins,
};
use reposync_core::db::Database;
use reposync_core::pair_refresh::lineage::{
    SvnCopyProof, SvnLineagePins, SvnRefreshLineagePins, SvnRefreshLineageProof,
};
use reposync_core::pair_refresh::preparation::{CleanChildGitPins, PreparedCleanChildGit};
use reposync_core::pair_refresh::publication::PinnedPushPolicy;
use reposync_core::pair_refresh::svn_preparation::{CleanChildSvnPins, PreparedCleanChildSvn};
use rusqlite::params;
use uuid::Uuid;

static SERIAL: Mutex<()> = Mutex::new(());

pub struct Fixture {
    pub db: Database,
    _owner: DataDirOwner,
    pub dir: tempfile::TempDir,
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    pub fn new() -> Self {
        let serial = SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db, owner) = Database::open_with_exclusive_owner(dir.path()).unwrap();
        seed(&db);
        Self {
            db,
            _owner: owner,
            dir,
            _serial: serial,
        }
    }
    pub fn pins(&self) -> RefreshJobPins {
        pins(&self.db, self.dir.path())
    }
    pub fn busy(&self) -> PairBusyGuard {
        busy::try_acquire_pair("parent", "child").unwrap()
    }
    pub fn admit(&self, pins: RefreshJobPins, guard: &PairBusyGuard) -> Uuid {
        let digest = pins.approval_digest().unwrap();
        self.db
            .admit_pair_refresh("admin", "request", &digest, pins, guard)
            .unwrap()
            .id
    }
    pub fn no_hold(&self, repo: Option<&str>) -> bool {
        require_no_pair_refresh_hold(&self.db.conn(), repo).is_ok()
    }
}

fn seed(db: &Database) {
    for (id, parent, sha, rev, branch) in [
        ("parent", None, "b".repeat(40), 4, "main"),
        ("child", Some("parent"), "a".repeat(40), 3, "feature"),
        ("unrelated", None, "e".repeat(40), 2, "other"),
    ] {
        db.conn().execute("INSERT INTO repositories(id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors,parent_id,scope_uuid) VALUES(?1,?1,'https://svn.example/repo','trunk','','gitea','https://git.example','repo',?2,'team',5,0,0,1,'t','t',?3,?4,'idle',0,0,?5,?6)", params![id, branch, rev, sha, parent, Uuid::new_v4().to_string()]).unwrap();
        db.set_state(&format!("last_svn_rev_{id}"), &rev.to_string())
            .unwrap();
        reposync_core::echo_receipt_scope::write_scoped_last_git_sha_kv(&db.conn(), id, &sha, "t")
            .unwrap();
    }
}

fn pins(db: &Database, root: &std::path::Path) -> RefreshJobPins {
    let operation_id = Uuid::new_v4();
    let child = capture_refresh_registration(&db.conn(), "child").unwrap();
    let parent = capture_refresh_registration(&db.conn(), "parent").unwrap();
    let svn_pins = SvnRefreshLineagePins {
        child: SvnLineagePins {
            repository_uuid: "svn-repository".into(),
            child_path: "/branches/f".into(),
            parent_path: "/trunk".into(),
            child_revision: 3,
            child_mapping_revision: 3,
            parent_baseline_revision: 2,
        },
        repository_root_url: "https://svn.example/repo".into(),
        child_url: "https://svn.example/repo/branches/f".into(),
        parent_url: "https://svn.example/repo/trunk".into(),
        parent_revision: 4,
    };
    let copy = SvnCopyProof {
        repository_uuid: "svn-repository".into(),
        child_path: "/branches/f".into(),
        created_revision: 3,
        parent_path: "/trunk".into(),
        copy_from_revision: 2,
        inspected_revision: 3,
    };
    let properties = BTreeMap::from([(
        "".into(),
        BTreeMap::from([("svn:mergeinfo".into(), "/trunk:3-4".into())]),
    )]);
    RefreshJobPins {
        git: PreparedCleanChildGit {
            pins: CleanChildGitPins {
                child_tip: child.last_git_sha.clone(),
                parent_tip: parent.last_git_sha.clone(),
                parent_baseline_tip: "a".repeat(40),
            },
            operation_id,
            prepared_at: 1_790_000_000,
            prepared_sha: "d".repeat(40),
            tree_sha: "c".repeat(40),
            parent_advance_commits: 1,
        },
        svn: PreparedCleanChildSvn {
            operation_id,
            pins: CleanChildSvnPins {
                lineage: SvnRefreshLineageProof {
                    pins: svn_pins,
                    child_copy: copy,
                    parent_originating_revision: 1,
                },
                baseline_git_tree: "f".repeat(40),
                parent_git_tree: "c".repeat(40),
                previous_mergeinfo: None,
            },
            working_copy: root.join("prepared"),
            mergeinfo: "/trunk:3-4".into(),
            result_git_tree: "c".repeat(40),
            result_properties: properties,
        },
        child,
        parent,
        git_remote_url: "https://git.example/owner/repo.git".into(),
        git_branch: "feature".into(),
        transport_policy: PinnedPushPolicy::SmartHttpV1,
        credential_context_digest: "e".repeat(64),
    }
}
