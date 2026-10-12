//! Real disposable svnserve controls, with no global environment mutation.
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::*;

#[path = "../../tests/fixtures/owned_process_group.rs"]
// Original private fixture declarations stay unchanged; reuse their exact body.
#[allow(clippy::duplicate_mod)]
mod process_fixture;

struct Fixture {
    // Stop and reap the server before removing its repository.
    _server: process_fixture::OwnedProcessGroup,
    root: tempfile::TempDir,
    client: SvnClient,
    repository: PathBuf,
}

fn command(args: &[&str]) -> String {
    let output = Command::new(args[0]).args(&args[1..]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

impl Fixture {
    async fn new(hidden: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repo");
        command(&["svnadmin", "create", repository.to_str().unwrap()]);
        let seed = root.path().join("seed");
        std::fs::create_dir_all(seed.join("trunk/hidden-dir")).unwrap();
        std::fs::write(seed.join("trunk/visible"), "visible").unwrap();
        std::fs::write(seed.join("trunk/hidden-file"), "hidden-file").unwrap();
        std::fs::write(seed.join("trunk/hidden-dir/nested"), "hidden-dir").unwrap();
        command(&[
            "svn",
            "import",
            seed.to_str().unwrap(),
            reqwest::Url::from_directory_path(&repository)
                .unwrap()
                .as_str(),
            "-m",
            "disposable visibility fixture",
            "--non-interactive",
        ]);
        std::fs::write(repository.join("conf/svnserve.conf"),
            "[general]\nanon-access = none\nauth-access = write\npassword-db = passwd\nauthz-db = authz\nrealm = disposable visibility\n").unwrap();
        std::fs::write(
            repository.join("conf/passwd"),
            "[users]\nreader = disposable-reader\n",
        )
        .unwrap();
        let authz = if hidden {
            "[repo:/]\nreader = r\n[repo:/trunk/hidden-dir]\nreader =\n[repo:/trunk/hidden-file]\nreader =\n"
        } else {
            "[repo:/]\nreader = r\n"
        };
        std::fs::write(repository.join("conf/authz"), authz).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let server = process_fixture::OwnedProcessGroup::spawn(
            Command::new("svnserve")
                .args([
                    "-d",
                    "--foreground",
                    "--listen-host",
                    "127.0.0.1",
                    "--listen-port",
                    &port.to_string(),
                    "-r",
                    root.path().to_str().unwrap(),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let client = SvnClient::new_isolated_refresh(
            &format!("svn://127.0.0.1:{port}/repo/trunk"),
            "reader",
            Some("disposable-reader"),
        )
        .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if client.info().await.is_ok() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "owned fixture server unavailable"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            _server: server,
            root,
            client,
            repository,
        }
    }

    fn youngest(&self) -> String {
        command(&["svnlook", "youngest", self.repository.to_str().unwrap()])
    }

    fn mutate(checkout: &VisibleSvnCheckout, sql: &str) {
        Connection::open(checkout.path().join(".svn/wc.db"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
}

#[tokio::test]
async fn full_visibility_checkout_proves_all_nodes_and_never_changes_repository() {
    let f = Fixture::new(false).await;
    let before = f.youngest();
    let checkout = observe_subtree(&f.client, 1).await.unwrap();
    assert_eq!(checkout.node_count(), 5);
    assert_eq!(checkout.presence_digest().len(), 64);
    assert_eq!(checkout.info().latest_rev, 1);
    assert_eq!(
        std::fs::read_to_string(checkout.path().join("hidden-file")).unwrap(),
        "hidden-file"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.path().join("hidden-dir/nested")).unwrap(),
        "hidden-dir"
    );
    assert_eq!(f.youngest(), before);
    let path = checkout.path().to_owned();
    drop(checkout);
    assert!(!path.exists());
}

#[tokio::test]
async fn authz_filtered_successful_clean_checkout_is_refused_without_publication() {
    let f = Fixture::new(true).await;
    let wc = f.root.path().join("filtered");
    f.client
        .checkout_full_pinned_without_externals(&wc, 1)
        .await
        .unwrap();
    let status = f
        .client
        .run_svn_in_dir_public(&wc, &["status", "--xml", "--no-ignore"])
        .await
        .unwrap();
    assert!(!status.contains("<entry"));
    assert!(wc.join("visible").is_file());
    assert!(!wc.join("hidden-file").exists());
    assert!(!wc.join("hidden-dir").exists());
    let conn = Connection::open_with_flags(wc.join(".svn/wc.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let excluded: i64 = conn
        .query_row(
            "SELECT count(*) FROM NODES WHERE presence='server-excluded'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(excluded, 2);
    drop(conn);
    let before = f.youngest();
    assert!(observe_subtree(&f.client, 1).await.is_err());
    assert_eq!(f.youngest(), before);
    assert_eq!(
        std::fs::read_to_string(wc.join("visible")).unwrap(),
        "visible"
    );
}

#[tokio::test]
async fn sparse_or_incomplete_metadata_cannot_be_accepted_as_full_visibility() {
    let f = Fixture::new(false).await;
    let checkout = observe_subtree(&f.client, 1).await.unwrap();
    Fixture::mutate(
        &checkout,
        "UPDATE NODES SET depth='empty' WHERE local_relpath='' AND op_depth=0",
    );
    assert!(inspect_fresh_checkout(checkout.path(), checkout.info()).is_err());
    let second = observe_subtree(&f.client, 1).await.unwrap();
    Fixture::mutate(
        &second,
        "UPDATE NODES SET presence='incomplete' WHERE local_relpath='visible' AND op_depth=0",
    );
    assert!(inspect_fresh_checkout(second.path(), second.info()).is_err());
    assert_eq!(f.youngest().trim(), "1");
}

#[tokio::test]
async fn unknown_schema_and_switched_descendant_refuse_without_metadata_repair() {
    let f = Fixture::new(false).await;
    let checkout = observe_subtree(&f.client, 1).await.unwrap();
    Fixture::mutate(&checkout, "PRAGMA user_version=32");
    assert!(inspect_fresh_checkout(checkout.path(), checkout.info()).is_err());
    let conn = Connection::open_with_flags(
        checkout.path().join(".svn/wc.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 32);
    drop(conn);
    let second = observe_subtree(&f.client, 1).await.unwrap();
    Fixture::mutate(
        &second,
        "UPDATE NODES SET repos_path='other/visible' WHERE local_relpath='visible'",
    );
    assert!(inspect_fresh_checkout(second.path(), second.info()).is_err());
    assert_eq!(f.youngest().trim(), "1");
}

#[tokio::test]
async fn ordinary_ambient_client_and_invalid_peg_refuse_before_checkout() {
    let f = Fixture::new(false).await;
    let ordinary = SvnClient::new(f.client.url(), "reader", "disposable-reader");
    assert!(observe_subtree(&ordinary, 1).await.is_err());
    assert!(observe_subtree(&f.client, 0).await.is_err());
    assert_eq!(f.youngest().trim(), "1");
}

#[tokio::test]
async fn reverted_changes_remain_pending_even_when_current_bytes_match_checkpoint() {
    let f = Fixture::new(false).await;
    let admin = f.root.path().join("admin");
    let url = reqwest::Url::from_directory_path(&f.repository)
        .unwrap()
        .join("trunk")
        .unwrap();
    command(&[
        "svn",
        "checkout",
        url.as_str(),
        admin.to_str().unwrap(),
        "--non-interactive",
    ]);
    std::fs::write(admin.join("visible"), "changed").unwrap();
    command(&[
        "svn",
        "commit",
        admin.to_str().unwrap(),
        "-m",
        "disposable advance",
        "--non-interactive",
    ]);
    std::fs::write(admin.join("visible"), "visible").unwrap();
    command(&[
        "svn",
        "commit",
        admin.to_str().unwrap(),
        "-m",
        "disposable reversal",
        "--non-interactive",
    ]);
    let observed = observe_subtree(&f.client, 3).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(observed.path().join("visible")).unwrap(),
        "visible"
    );
    assert!(observed.require_unchanged_since(1).is_err());
    observed.require_unchanged_since(3).unwrap();
    assert_eq!(f.youngest().trim(), "3");
}

#[tokio::test]
async fn removed_authz_hidden_change_is_still_pending_after_it_disappears() {
    let f = Fixture::new(false).await;
    std::fs::write(
        f.repository.join("conf/authz"),
        "[repo:/]\nreader = r\n[repo:/trunk/temporary]\nreader =\n",
    )
    .unwrap();
    let admin = f.root.path().join("admin");
    let url = reqwest::Url::from_directory_path(&f.repository)
        .unwrap()
        .join("trunk")
        .unwrap();
    command(&[
        "svn",
        "checkout",
        url.as_str(),
        admin.to_str().unwrap(),
        "--non-interactive",
    ]);
    std::fs::write(admin.join("temporary"), "hidden change").unwrap();
    command(&[
        "svn",
        "add",
        admin.join("temporary").to_str().unwrap(),
        "--non-interactive",
    ]);
    command(&[
        "svn",
        "commit",
        admin.to_str().unwrap(),
        "-m",
        "disposable hidden advance",
        "--non-interactive",
    ]);
    command(&[
        "svn",
        "delete",
        admin.join("temporary").to_str().unwrap(),
        "--non-interactive",
    ]);
    command(&[
        "svn",
        "commit",
        admin.to_str().unwrap(),
        "-m",
        "disposable hidden removal",
        "--non-interactive",
    ]);
    let observed = observe_subtree(&f.client, 3).await.unwrap();
    assert!(!observed.path().join("temporary").exists());
    assert_eq!(observed.node_count(), 5);
    assert!(observed.require_unchanged_since(1).is_err());
    observed.require_unchanged_since(3).unwrap();
    assert_eq!(f.youngest().trim(), "3");
}

fn git_commit(repo: &git2::Repository, parent: Option<git2::Oid>) -> git2::Oid {
    let mut index = repo.index().unwrap();
    index
        .add_all(["."], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::new(
        "fixture",
        "fixture@example.invalid",
        &git2::Time::new(1000, 0),
    )
    .unwrap();
    let parent_commit = parent.map(|id| repo.find_commit(id).unwrap());
    let parents: Vec<_> = parent_commit.iter().collect();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "disposable pair fixture",
        &tree,
        &parents,
    )
    .unwrap()
}

async fn pair_fixture() -> (
    Fixture,
    SvnClient,
    git2::Repository,
    super::super::svn_preparation::CleanChildSvnPins,
) {
    let f = Fixture::new(false).await;
    let root_url = reqwest::Url::from_directory_path(&f.repository).unwrap();
    let parent_url = root_url.join("trunk").unwrap();
    let child_url = root_url.join("child").unwrap();
    command(&[
        "svn",
        "copy",
        parent_url.as_str(),
        child_url.as_str(),
        "-r",
        "1",
        "-m",
        "disposable fork",
        "--non-interactive",
    ]);
    let admin = f.root.path().join("admin");
    command(&[
        "svn",
        "checkout",
        parent_url.as_str(),
        admin.to_str().unwrap(),
        "--non-interactive",
    ]);
    std::fs::write(admin.join("visible"), "parent advanced").unwrap();
    command(&[
        "svn",
        "commit",
        admin.to_str().unwrap(),
        "-m",
        "disposable parent advance",
        "--non-interactive",
    ]);
    let info = f.client.info().await.unwrap();
    let child = SvnClient::new_isolated_refresh(
        &format!("{}/child", info.root_url),
        "reader",
        Some("disposable-reader"),
    )
    .unwrap();
    let lineage = super::super::lineage::inspect_svn_refresh_lineage(
        &child,
        &f.client,
        &super::super::lineage::SvnRefreshLineagePins {
            child: super::super::lineage::SvnLineagePins {
                repository_uuid: info.uuid,
                child_path: "/child".into(),
                parent_path: "/trunk".into(),
                child_revision: 2,
                child_mapping_revision: 2,
                parent_baseline_revision: 1,
            },
            repository_root_url: info.root_url,
            child_url: child.url().into(),
            parent_url: f.client.url().into(),
            parent_revision: 3,
        },
    )
    .await
    .unwrap();
    let git_path = f.root.path().join("git");
    let git = git2::Repository::init(&git_path).unwrap();
    std::fs::create_dir_all(git_path.join("hidden-dir")).unwrap();
    std::fs::write(git_path.join("visible"), "visible").unwrap();
    std::fs::write(git_path.join("hidden-file"), "hidden-file").unwrap();
    std::fs::write(git_path.join("hidden-dir/nested"), "hidden-dir").unwrap();
    let baseline = git_commit(&git, None);
    let baseline_tree = git.find_commit(baseline).unwrap().tree_id().to_string();
    std::fs::write(git_path.join("visible"), "parent advanced").unwrap();
    let parent = git_commit(&git, Some(baseline));
    let parent_tree = git.find_commit(parent).unwrap().tree_id().to_string();
    (
        f,
        child,
        git,
        super::super::svn_preparation::CleanChildSvnPins {
            lineage,
            baseline_git_tree: baseline_tree,
            parent_git_tree: parent_tree,
            previous_mergeinfo: None,
        },
    )
}

#[tokio::test]
async fn visible_pair_preparation_binds_five_snapshots_without_publishing() {
    let (f, child, git, pins) = pair_fixture().await;
    let staging = tempfile::tempdir().unwrap();
    let before_git = git.head().unwrap().target();
    let (prepared, presence) = super::super::svn_preparation::prepare_clean_child_svn_visible(
        &child,
        &f.client,
        git.path(),
        staging.path(),
        uuid::Uuid::new_v4(),
        &pins,
        3,
    )
    .await
    .unwrap();
    assert_eq!(
        presence.iter().map(|p| p.role.as_str()).collect::<Vec<_>>(),
        [
            "baseline",
            "parent",
            "child",
            "current_parent",
            "current_child"
        ]
    );
    assert!(presence
        .iter()
        .all(|p| p.node_count == 5 && p.presence_digest.len() == 64));
    assert_eq!(
        std::fs::read_to_string(prepared.working_copy.join("visible")).unwrap(),
        "parent advanced"
    );
    assert_eq!(prepared.result_git_tree, pins.parent_git_tree);
    assert_eq!(f.youngest().trim(), "3");
    assert_eq!(git.head().unwrap().target(), before_git);
}

#[tokio::test]
async fn hidden_parent_or_child_refuses_pair_preparation_without_remote_changes() {
    let (f, child, git, pins) = pair_fixture().await;
    let before_git = git.head().unwrap().target();
    for branch in ["trunk", "child"] {
        std::fs::write(
            f.repository.join("conf/authz"),
            format!("[repo:/]\nreader = r\n[repo:/{branch}/hidden-file]\nreader =\n"),
        )
        .unwrap();
        let staging = tempfile::tempdir().unwrap();
        assert!(
            super::super::svn_preparation::prepare_clean_child_svn_visible(
                &child,
                &f.client,
                git.path(),
                staging.path(),
                uuid::Uuid::new_v4(),
                &pins,
                3,
            )
            .await
            .is_err()
        );
        assert_eq!(f.youngest().trim(), "3");
        assert_eq!(git.head().unwrap().target(), before_git);
    }
}

#[tokio::test]
async fn empty_root_property_claim_requires_the_actual_pristine_metadata() {
    let f = Fixture::new(false).await;
    let observed = observe_subtree(&f.client, 1).await.unwrap();
    assert!(has_proved_empty_root_properties(observed.path(), observed.info()).unwrap());
    Fixture::mutate(
        &observed,
        "UPDATE NODES SET properties=x'2861206229' WHERE local_relpath='' AND op_depth=0",
    );
    assert!(!has_proved_empty_root_properties(observed.path(), observed.info()).unwrap());
    assert_eq!(f.youngest().trim(), "1");
}
