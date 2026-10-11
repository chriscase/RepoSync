//! Real SVN input observations are read-only and include nested pending changes.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

use std::path::{Path, PathBuf};
use std::process::Command;

use reposync_core::pair_refresh::svn_sources::observe_registered_svn_sources;
use reposync_core::svn::SvnClient;

struct Sources {
    f: job::Fixture,
    root: String,
    child: PathBuf,
    parent: PathBuf,
}

fn svn(arguments: &[&str], directory: Option<&Path>) -> String {
    let mut cmd = Command::new("svn");
    cmd.args(arguments)
        .args(["--non-interactive", "--no-auth-cache"]);
    if let Some(directory) = directory {
        cmd.current_dir(directory);
    }
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn sources() -> Sources {
    let f = job::Fixture::new();
    let repository = f.dir.path().join("svn-repository");
    assert!(Command::new("svnadmin")
        .arg("create")
        .arg(&repository)
        .status()
        .unwrap()
        .success());
    let root = reqwest::Url::from_file_path(repository.canonicalize().unwrap())
        .unwrap()
        .to_string();
    svn(
        &[
            "mkdir",
            &format!("{root}/trunk"),
            &format!("{root}/branches"),
            "-m",
            "fixture directories",
        ],
        None,
    );
    let parent = f.dir.path().join("parent-working-copy");
    let child = f.dir.path().join("child-working-copy");
    svn(
        &[
            "checkout",
            &format!("{root}/trunk"),
            parent.to_str().unwrap(),
        ],
        None,
    );
    std::fs::create_dir(parent.join("nested")).unwrap();
    std::fs::write(parent.join("nested/data.txt"), "baseline\n").unwrap();
    svn(&["add", "nested"], Some(&parent));
    svn(&["commit", "-m", "fixture baseline"], Some(&parent));
    svn(
        &[
            "copy",
            &format!("{root}/trunk@2"),
            &format!("{root}/branches/f"),
            "-m",
            "fixture direct copy",
        ],
        None,
    );
    svn(
        &[
            "checkout",
            &format!("{root}/branches/f"),
            child.to_str().unwrap(),
        ],
        None,
    );
    std::fs::write(parent.join("nested/data.txt"), "parent advance\n").unwrap();
    svn(&["commit", "-m", "fixture parent advance"], Some(&parent));
    f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
    f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1',svn_url=?1", [&root]).unwrap();
    f.db.conn()
        .execute(
            "UPDATE repositories SET svn_branch='branches/f' WHERE id='child'",
            [],
        )
        .unwrap();
    Sources {
        f,
        root,
        child,
        parent,
    }
}

fn rows(s: &Sources) -> Vec<(String, String, String)> {
    s.f.db
        .conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test]
async fn registered_inputs_allow_unrelated_head_without_checkpoint_or_remote_changes() {
    let s = sources();
    svn(
        &[
            "mkdir",
            &format!("{}/unrelated", s.root),
            "-m",
            "fixture unrelated revision",
        ],
        None,
    );
    let configuration =
        s.f.db
            .capture_pair_refresh_execution_configuration("child")
            .unwrap();
    let before = rows(&s);
    let snapshot = observe_registered_svn_sources(&configuration)
        .await
        .unwrap();
    let pin = snapshot.pins();
    assert_eq!(pin.inspected_head_revision, 5);
    assert_eq!(pin.child_checkpoint_revision, 3);
    assert_eq!(pin.parent_checkpoint_revision, 4);
    assert_eq!(pin.repository_root_url, s.root);
    assert_eq!(pin.child_path, "/branches/f");
    assert_eq!(pin.parent_path, "/trunk");
    assert_eq!(
        snapshot.child_client().url(),
        configuration.pin().child_svn_url
    );
    assert_eq!(
        snapshot.parent_client().url(),
        configuration.pin().parent_svn_url
    );
    assert_eq!(rows(&s), before);
    assert_eq!(
        SvnClient::new(&s.root, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert!(s.f.no_hold(None));
}

#[tokio::test]
async fn nested_file_and_property_changes_on_either_branch_refuse_without_writes() {
    for branch in ["child", "parent"] {
        for change in ["bytes", "property"] {
            let s = sources();
            let wc = if branch == "child" {
                &s.child
            } else {
                &s.parent
            };
            if change == "bytes" {
                std::fs::write(wc.join("nested/data.txt"), "pending external work\n").unwrap();
            } else {
                svn(
                    &["propset", "fixture-property", "pending", "nested/data.txt"],
                    Some(wc),
                );
            }
            svn(&["commit", "-m", "fixture pending external work"], Some(wc));
            let configuration =
                s.f.db
                    .capture_pair_refresh_execution_configuration("child")
                    .unwrap();
            let before = rows(&s);
            assert!(
                observe_registered_svn_sources(&configuration)
                    .await
                    .is_err(),
                "{branch}/{change}"
            );
            assert_eq!(rows(&s), before);
            assert_eq!(
                SvnClient::new(&s.root, "", "")
                    .info()
                    .await
                    .unwrap()
                    .latest_rev,
                5
            );
            assert!(s.f.no_hold(None));
        }
    }
}

#[tokio::test]
async fn deleted_or_replaced_registered_branch_is_never_adopted() {
    for replacement in [false, true] {
        let s = sources();
        let url = format!("{}/branches/f", s.root);
        svn(&["delete", &url, "-m", "fixture remove child"], None);
        if replacement {
            svn(
                &[
                    "copy",
                    &format!("{}/trunk@4", s.root),
                    &url,
                    "-m",
                    "fixture replacement child",
                ],
                None,
            );
        }
        let configuration =
            s.f.db
                .capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let before = rows(&s);
        assert!(
            observe_registered_svn_sources(&configuration)
                .await
                .is_err(),
            "replacement={replacement}"
        );
        assert_eq!(rows(&s), before);
        assert_eq!(
            SvnClient::new(&s.root, "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            if replacement { 6 } else { 5 }
        );
        assert!(s.f.no_hold(None));
    }
}

#[tokio::test]
async fn different_repository_identity_or_root_cannot_supply_parent_inputs() {
    let s = sources();
    let foreign = s.f.dir.path().join("foreign-svn");
    assert!(Command::new("svnadmin")
        .arg("create")
        .arg(&foreign)
        .status()
        .unwrap()
        .success());
    let url = reqwest::Url::from_file_path(foreign.canonicalize().unwrap())
        .unwrap()
        .to_string();
    svn(
        &[
            "mkdir",
            &format!("{url}/trunk"),
            "-m",
            "fixture foreign parent",
        ],
        None,
    );
    s.f.db
        .conn()
        .execute(
            "UPDATE repositories SET svn_url=?1 WHERE id='parent'",
            [&url],
        )
        .unwrap();
    let configuration =
        s.f.db
            .capture_pair_refresh_execution_configuration("child")
            .unwrap();
    let before = rows(&s);
    assert!(observe_registered_svn_sources(&configuration)
        .await
        .is_err());
    assert_eq!(rows(&s), before);
    assert!(s.f.no_hold(None));
}

#[tokio::test]
async fn checkpoint_beyond_repository_head_is_not_a_clean_input() {
    let s = sources();
    s.f.db
        .conn()
        .execute(
            "UPDATE repositories SET last_svn_rev=99 WHERE id='child'",
            [],
        )
        .unwrap();
    s.f.db.set_state("last_svn_rev_child", "99").unwrap();
    let configuration =
        s.f.db
            .capture_pair_refresh_execution_configuration("child")
            .unwrap();
    let before = rows(&s);
    assert!(observe_registered_svn_sources(&configuration)
        .await
        .is_err());
    assert_eq!(rows(&s), before);
    assert_eq!(
        SvnClient::new(&s.root, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        4
    );
    assert!(s.f.no_hold(None));
}
