use super::*;
use std::fs;
use std::process::Command;

fn command(tool: &str, args: &[&str]) -> String {
    let output = Command::new(tool).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{tool}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn file_url(path: &Path) -> String {
    Url::from_file_path(path).unwrap().to_string()
}

fn git_commit(repo: &Repository, reference: &str, bytes: &[u8], parent: Option<Oid>) -> Oid {
    git_commit_message(repo, reference, bytes, parent, "import")
}

fn git_commit_message(
    repo: &Repository,
    reference: &str,
    bytes: &[u8],
    parent: Option<Oid>,
    message: &str,
) -> Oid {
    let id = repo.blob(bytes).unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    builder.insert("example.txt", id, 0o100644).unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let sig = git2::Signature::now("Historical fixture", "fixture@example.invalid").unwrap();
    let parents: Vec<_> = parent
        .map(|p| repo.find_commit(p).unwrap())
        .into_iter()
        .collect();
    repo.commit(
        Some(reference),
        &sig,
        &sig,
        message,
        &tree,
        &parents.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

struct Fixture {
    _directory: tempfile::TempDir,
    active: PathBuf,
    backup: PathBuf,
    pair: LegacyPairPins,
    witness: HistoricalSvnWitness,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let active = directory.path().join("svn");
        let backup = directory.path().join("restore");
        command("svnadmin", &["create", active.to_str().unwrap()]);
        let root = file_url(&active);
        let branch = format!("{root}/trunk");
        let target = format!("{branch}/example.txt");
        let data = directory.path().join("data");
        fs::write(&data, b"one\n").unwrap();
        command(
            "svnmucc",
            &[
                "-m",
                "r1",
                "mkdir",
                &branch,
                "put",
                data.to_str().unwrap(),
                &target,
            ],
        );
        fs::write(&data, b"two\n").unwrap();
        command(
            "svnmucc",
            &["-m", "r2", "put", data.to_str().unwrap(), &target],
        );
        command(
            "svnadmin",
            &[
                "hotcopy",
                active.to_str().unwrap(),
                backup.to_str().unwrap(),
            ],
        );
        fs::write(&data, b"three\n").unwrap();
        command(
            "svnmucc",
            &["-m", "r3", "put", data.to_str().unwrap(), &target],
        );
        let git = directory.path().join("fixture/project.git");
        fs::create_dir_all(git.parent().unwrap()).unwrap();
        let repo = Repository::init_bare(&git).unwrap();
        let first = git_commit(&repo, "refs/heads/main", b"one\n", None);
        let tip = git_commit(&repo, "refs/heads/main", b"two\n", Some(first));
        let pair = LegacyPairPins {
            repo_id: "test-registration".into(),
            scope_uuid: "".into(),
            generation: 0,
            svn_url: root.clone(),
            svn_branch: "trunk".into(),
            svn_username: "".into(),
            git_provider: "github".into(),
            git_api_url: file_url(directory.path()),
            git_repo: "fixture/project".into(),
            git_branch: "main".into(),
            last_svn_rev: 2,
            last_git_sha: tip.to_string(),
            mappings: vec![(1, first.to_string()), (2, tip.to_string())],
            import_completion_svn_rev: None,
            import_log_entry_count: None,
            storage_sha256: "a".repeat(64),
        };
        let witness = HistoricalSvnWitness {
            source: HistoricalSvnSource::Restored {
                repository: backup.clone(),
                original_root_url: root,
            },
            configured_git_url: file_url(&git),
            git_mirror: git,
            credential_context_sha256: None,
            minimum_live_revision: None,
        };
        Self {
            _directory: directory,
            active,
            backup,
            pair,
            witness,
        }
    }
    async fn verify(&self) -> Result<VerifiedHistoricalLineage> {
        verify_registered(&self.pair, &self.witness, "", None, None).await
    }
}

#[tokio::test]
async fn both_sources_bind_the_same_history_with_separate_review_authority() {
    let mut f = Fixture::new();
    let old = f.verify().await.unwrap();
    let mirror = Repository::open_bare(&f.witness.git_mirror).unwrap();
    let trees = f
        .pair
        .mappings
        .iter()
        .map(|(_, sha)| mirror.find_commit(oid(sha).unwrap()).unwrap().tree_id())
        .collect::<Vec<_>>();
    assert_eq!(old.projected_tree_ids(), trees);
    assert_eq!(old.lineage().incarnation_created_rev, 1);
    assert_eq!(old.lineage().svn_root_url, f.pair.svn_url);
    assert!(!old.is_planned_cutover());
    f.witness.source = HistoricalSvnSource::PlannedCutover;
    let pre = f.verify().await.unwrap();
    assert!(pre.is_planned_cutover());
    assert_eq!(pre.history_sha256(), old.history_sha256());
    assert_ne!(pre.review_sha256(), old.review_sha256());
    assert_eq!(
        pre.review_sha256(),
        f.verify().await.unwrap().review_sha256()
    );
    // Historical proof survives ordinary later SVN work and the migration's
    // new scope, but the reviewed physical state is deliberately different.
    command(
        "svnmucc",
        &[
            "-m",
            "r4",
            "mkdir",
            &format!("{}/trunk/later", f.pair.svn_url),
        ],
    );
    f.witness.minimum_live_revision = Some(pre.reviewed_svn_floor());
    let later = f.verify().await.unwrap();
    assert_eq!(pre.review_sha256(), later.review_sha256());
    assert_eq!(pre.provenance(), later.provenance());
    f.pair.scope_uuid = "new-scope".into();
    f.pair.generation = 1;
    let migrated = f.verify().await.unwrap();
    assert_eq!(pre.history_sha256(), migrated.history_sha256());
    assert_ne!(pre.review_sha256(), migrated.review_sha256());
    assert_eq!(
        command("svnlook", &["youngest", f.active.to_str().unwrap()]).trim(),
        "4"
    );
    assert_eq!(
        command("svnlook", &["youngest", f.backup.to_str().unwrap()]).trim(),
        "2"
    );
}

#[tokio::test]
async fn refuses_changed_uuid_and_same_uuid_replacement_even_with_matching_bytes() {
    let f = Fixture::new();
    let old_uuid = command("svnlook", &["uuid", f.backup.to_str().unwrap()]);
    command("svnadmin", &["setuuid", f.backup.to_str().unwrap()]);
    assert_eq!(refusal(f.verify().await), HistoricalVerifyError::Archive);
    command(
        "svnadmin",
        &["setuuid", f.backup.to_str().unwrap(), old_uuid.trim()],
    );
    let branch = format!("{}/trunk", f.pair.svn_url);
    command(
        "svnmucc",
        &[
            "-m",
            "replace-copy",
            "rm",
            &branch,
            "cp",
            "2",
            &branch,
            &branch,
        ],
    );
    assert!(f.verify().await.is_err());
}

#[tokio::test]
async fn refuses_changed_log_metadata_and_substituted_archive() {
    let mut f = Fixture::new();
    let message = f._directory.path().join("log");
    fs::write(&message, b"changed old transaction").unwrap();
    command(
        "svnadmin",
        &[
            "setrevprop",
            f.backup.to_str().unwrap(),
            "-r",
            "1",
            "corp:review",
            message.to_str().unwrap(),
        ],
    );
    assert_eq!(refusal(f.verify().await), HistoricalVerifyError::Archive);
    command(
        "svnadmin",
        &[
            "delrevprop",
            f.backup.to_str().unwrap(),
            "-r",
            "1",
            "corp:review",
        ],
    );
    command(
        "svnadmin",
        &[
            "setrevprop",
            f.backup.to_str().unwrap(),
            "-r",
            "1",
            "svn:log",
            message.to_str().unwrap(),
        ],
    );
    assert_eq!(refusal(f.verify().await), HistoricalVerifyError::Archive);
    f.witness.source = HistoricalSvnSource::Restored {
        repository: f.active.clone(),
        original_root_url: f.pair.svn_url.clone(),
    };
    assert_eq!(refusal(f.verify().await), HistoricalVerifyError::Archive);
}

#[tokio::test]
async fn refuses_incomplete_mapping_and_false_intermediate_tree_and_git_ref() {
    let mut f = Fixture::new();
    f.pair.mappings.remove(0);
    assert_eq!(
        refusal(f.verify().await),
        HistoricalVerifyError::GitAncestry
    );
    let mirror = Repository::open_bare(&f.witness.git_mirror).unwrap();
    let wrong = git_commit(&mirror, "refs/heads/other", b"wrong\n", None);
    let tip = git_commit(&mirror, "refs/heads/other", b"two\n", Some(wrong));
    f.pair.last_git_sha = tip.to_string();
    f.pair.mappings = vec![(1, wrong.to_string()), (2, tip.to_string())];
    assert_eq!(refusal(f.verify().await), HistoricalVerifyError::GitRef);
    f.pair.git_branch = "other".into();
    assert_eq!(refusal(f.verify().await), HistoricalVerifyError::SvnTree);
}

fn refusal(result: Result<VerifiedHistoricalLineage>) -> HistoricalVerifyError {
    let error = result.err().expect("expected refusal");
    let category = HistoricalVerifyError::classify(&error);
    // Even an operator printing the full chain cannot accidentally emit a
    // repository-controlled path, revision property, URL or secret.
    assert_eq!(format!("{error:#}"), category.to_string());
    category
}

#[tokio::test]
async fn matching_repository_suffix_on_another_authority_is_refused_before_contact() {
    let mut f = Fixture::new();
    let receiver = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    receiver.set_nonblocking(true).unwrap();
    f.witness.configured_git_url = format!(
        "https://{}/fixture/project.git",
        receiver.local_addr().unwrap()
    );
    f.witness.credential_context_sha256 = Some("a".repeat(64));
    let result = verify_registered(&f.pair, &f.witness, "", None, Some("disposable-token")).await;
    assert_eq!(refusal(result), HistoricalVerifyError::Registration);
    assert!(matches!(receiver.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
}

#[tokio::test]
async fn active_head_can_advance_during_a_scan_but_cannot_recede_or_replace_the_branch() {
    let f = Fixture::new();
    let live = SvnClient::new_isolated_refresh(&branch_url(&f.pair).unwrap(), "", None).unwrap();
    let trees = mapped_trees(&f.pair, &f.witness.git_mirror).unwrap();
    let before = observe_svn(
        &live,
        &f.pair.svn_url,
        &f.pair,
        &f.witness.git_mirror,
        &trees,
    )
    .await
    .unwrap();
    // Move the repository-wide HEAD, first without touching the branch and
    // then with ordinary pending work on that same branch.
    command(
        "svnmucc",
        &[
            "-m",
            "other team",
            "mkdir",
            &format!("{}/other-team", f.pair.svn_url),
        ],
    );
    require_stable_source(&live, &before, &f.pair, false)
        .await
        .unwrap();
    let branch = branch_url(&f.pair).unwrap();
    command(
        "svnmucc",
        &[
            "-m",
            "pending team work",
            "mkdir",
            &format!("{branch}/later"),
        ],
    );
    require_stable_source(&live, &before, &f.pair, false)
        .await
        .unwrap();
    assert!(require_stable_source(&live, &before, &f.pair, true)
        .await
        .is_err());
    command(
        "svnmucc",
        &[
            "-m",
            "replace branch",
            "rm",
            &branch,
            "cp",
            "2",
            &branch,
            &branch,
        ],
    );
    assert!(require_stable_source(&live, &before, &f.pair, false)
        .await
        .is_err());
    // Same UUID, same approved tree, same registered path, and an earlier
    // repository HEAD from the retained pre-r3 snapshot.
    fs::rename(&f.active, f._directory.path().join("replaced")).unwrap();
    fs::rename(&f.backup, &f.active).unwrap();
    assert!(require_stable_source(&live, &before, &f.pair, false)
        .await
        .is_err());
}

#[derive(Clone, Copy)]
enum SkippedChange {
    BinaryMime,
    FileContents,
    UnknownProperty,
}

async fn sparse_import(skipped: SkippedChange) -> Fixture {
    let mut f = Fixture::new();
    // Reuse the registration and approved local Git repository. Build a
    // differently shaped, independently restored history in this fixture only.
    let base = f._directory.path();
    fs::rename(&f.active, base.join("original-svn")).unwrap();
    fs::rename(&f.backup, base.join("original-backup")).unwrap();
    command("svnadmin", &["create", f.active.to_str().unwrap()]);
    let branch = branch_url(&f.pair).unwrap();
    let target = format!("{branch}/example.txt");
    let data = base.join("sparse-data");
    command("svnmucc", &["-m", "empty branch", "mkdir", &branch]);
    fs::write(&data, b"one\n").unwrap();
    command(
        "svnmucc",
        &["-m", "first file", "put", data.to_str().unwrap(), &target],
    );
    match skipped {
        SkippedChange::BinaryMime => {
            command(
                "svnmucc",
                &[
                    "-m",
                    "binary metadata",
                    "propset",
                    "svn:mime-type",
                    "application/octet-stream",
                    &target,
                ],
            );
        }
        SkippedChange::FileContents => {
            fs::write(&data, b"unrepresented intermediate bytes\n").unwrap();
            command(
                "svnmucc",
                &["-m", "data", "put", data.to_str().unwrap(), &target],
            );
        }
        SkippedChange::UnknownProperty => {
            command(
                "svnmucc",
                &[
                    "-m",
                    "unknown metadata",
                    "propset",
                    "custom:meaning",
                    "x",
                    &target,
                ],
            );
        }
    }
    fs::write(&data, b"two\n").unwrap();
    command(
        "svnmucc",
        &[
            "-m",
            "second file revision",
            "put",
            data.to_str().unwrap(),
            &target,
        ],
    );
    command(
        "svnadmin",
        &[
            "hotcopy",
            f.active.to_str().unwrap(),
            base.join("before-audit-head").to_str().unwrap(),
        ],
    );
    command(
        "svnmucc",
        &[
            "-m",
            "other team",
            "mkdir",
            &format!("{}/elsewhere", f.pair.svn_url),
        ],
    );
    command(
        "svnadmin",
        &[
            "hotcopy",
            f.active.to_str().unwrap(),
            f.backup.to_str().unwrap(),
        ],
    );
    command(
        "svnmucc",
        &["-m", "pending work", "mkdir", &format!("{branch}/pending")],
    );
    f.pair.git_branch = "sparse".into();
    let mirror = open_mirror(&f.witness.git_mirror).unwrap();
    let live = SvnClient::new_isolated_refresh(&branch, "", None).unwrap();
    let log = live.copy_history_at_rev(6).await.unwrap();
    let reference = "refs/heads/sparse";
    let mut parent = None;
    f.pair.mappings.clear();
    for (rev, bytes) in [(2, b"one\n"), (4, b"two\n")] {
        let e = log.iter().find(|e| e.revision == rev).unwrap();
        let message = format!(
            "{}\n\n[reposync] imported from SVN r{}\nSVN-Author: {}\nSVN-Date: {}",
            e.message, rev, e.author, e.date
        );
        let commit = git_commit_message(&mirror, reference, bytes, parent, &message);
        f.pair.mappings.push((rev, commit.to_string()));
        parent = Some(commit);
    }
    f.pair.last_git_sha = parent.unwrap().to_string();
    f.pair.last_svn_rev = 4;
    f.pair.import_completion_svn_rev = Some(5);
    f.pair.import_log_entry_count = Some(4);
    f
}

#[tokio::test]
async fn empty_creation_and_noop_property_changes_are_verified_through_the_old_audit_head() {
    let mut f = sparse_import(SkippedChange::BinaryMime).await;
    let approved = f.verify().await.unwrap();
    assert_eq!(approved.verified_branch_revisions(), 4);
    assert_eq!(approved.projected_tree_ids().len(), 2);
    assert_eq!(approved.reviewed_svn_floor(), 6);
    f.witness.minimum_live_revision = Some(6);
    command(
        "svnmucc",
        &["-m", "later", "mkdir", &format!("{}/other", f.pair.svn_url)],
    );
    assert_eq!(
        approved.review_sha256(),
        f.verify().await.unwrap().review_sha256()
    );
    // Roll the active repository back to r5 between preview and apply, keeping
    // the old watermark, named Git ref, complete import and UUID unchanged.
    fs::rename(&f.active, f._directory.path().join("advanced")).unwrap();
    command(
        "svnadmin",
        &[
            "hotcopy",
            f.backup.to_str().unwrap(),
            f.active.to_str().unwrap(),
        ],
    );
    assert_eq!(refusal(f.verify().await), HistoricalVerifyError::SvnHistory);
    f.witness.minimum_live_revision = None;
    f.witness.source = HistoricalSvnSource::Restored {
        repository: f._directory.path().join("before-audit-head"),
        original_root_url: f.pair.svn_url.clone(),
    };
    assert_eq!(refusal(f.verify().await), HistoricalVerifyError::Archive);
}

#[tokio::test]
async fn gaps_cannot_hide_intermediate_contents_unknown_properties_or_a_misattributed_commit() {
    for change in [SkippedChange::FileContents, SkippedChange::UnknownProperty] {
        let f = sparse_import(change).await;
        assert_eq!(refusal(f.verify().await), HistoricalVerifyError::SvnTree);
    }
    let mut f = sparse_import(SkippedChange::BinaryMime).await;
    f.pair.import_log_entry_count = Some(3);
    assert_eq!(refusal(f.verify().await), HistoricalVerifyError::SvnHistory);
    f.pair.import_log_entry_count = Some(4);
    let mirror = open_mirror(&f.witness.git_mirror).unwrap();
    let parent = oid(&f.pair.mappings[0].1).unwrap();
    let id = git_commit_message(
        &mirror,
        "refs/heads/misassigned",
        b"two\n",
        Some(parent),
        "wrong original SVN revision",
    );
    f.pair.git_branch = "misassigned".into();
    f.pair.mappings[1].1 = id.to_string();
    f.pair.last_git_sha = id.to_string();
    assert_eq!(
        refusal(f.verify().await),
        HistoricalVerifyError::GitAncestry
    );
}
