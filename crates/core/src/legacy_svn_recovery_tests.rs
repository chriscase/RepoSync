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
        "import",
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
    assert!(f
        .verify()
        .await
        .unwrap_err_text()
        .contains("backup and current SVN history differ"));
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
    assert!(f
        .verify()
        .await
        .unwrap_err_text()
        .contains("backup and current SVN history differ"));
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
    assert!(f
        .verify()
        .await
        .unwrap_err_text()
        .contains("backup and current SVN history differ"));
    f.witness.source = HistoricalSvnSource::Restored {
        repository: f.active.clone(),
        original_root_url: f.pair.svn_url.clone(),
    };
    assert!(f.verify().await.unwrap_err_text().contains("separate"));
}

#[tokio::test]
async fn refuses_incomplete_mapping_and_false_intermediate_tree_and_git_ref() {
    let mut f = Fixture::new();
    f.pair.mappings.remove(0);
    assert!(f.verify().await.unwrap_err_text().contains("ancestry"));
    let mirror = Repository::open_bare(&f.witness.git_mirror).unwrap();
    let wrong = git_commit(&mirror, "refs/heads/other", b"wrong\n", None);
    let tip = git_commit(&mirror, "refs/heads/other", b"two\n", Some(wrong));
    f.pair.last_git_sha = tip.to_string();
    f.pair.mappings = vec![(1, wrong.to_string()), (2, tip.to_string())];
    assert!(f
        .verify()
        .await
        .unwrap_err_text()
        .contains("named Git branch differs"));
    f.pair.git_branch = "other".into();
    assert!(f.verify().await.unwrap_err_text().contains("r1 differs"));
}

trait Failure {
    fn unwrap_err_text(self) -> String;
}
impl Failure for Result<VerifiedHistoricalLineage> {
    fn unwrap_err_text(self) -> String {
        format!("{:#}", self.err().expect("expected refusal"))
    }
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
    assert!(result
        .unwrap_err_text()
        .contains("Git endpoint differs from registered repository"));
    assert!(matches!(receiver.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
}
