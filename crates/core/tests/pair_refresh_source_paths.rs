//! SVN's peg syntax and URI escaping must not substitute registered paths.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

#[test]
fn peg_ambiguous_and_noncanonical_registered_svn_paths_refuse_read_only() {
    for root in [
        "https://svn.example/repo",
        "http://svn.example/repo",
        "svn://svn.example/repo",
        "file:///disposable/repo",
    ] {
        for id in ["child", "parent"] {
            for path in [
                "trunk/",
                "branches//f",
                "branches/f@HEAD",
                "branches/f@3",
                "branches/f@",
                "branches/f\\other",
                "branches/f;other",
                "branches/naïve",
                "branches/f//other",
                "branches/%40HEAD",
            ] {
                let f = job::Fixture::new();
                f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
                f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1',svn_url='svn://svn.example/repo',svn_branch=CASE WHEN id='child' THEN 'branches/f' ELSE 'trunk' END",[]).unwrap();
                f.db.conn()
                    .execute("UPDATE repositories SET svn_url=?1", [root])
                    .unwrap();
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET svn_branch=?1 WHERE id=?2",
                        [path, id],
                    )
                    .unwrap();
                let before =
                    f.db.conn()
                        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
                        .unwrap()
                        .query_map([], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                            ))
                        })
                        .unwrap()
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap();
                let error =
                    f.db.capture_pair_refresh_execution_configuration("child")
                        .err()
                        .unwrap()
                        .to_string();
                assert!(
                    error.contains("noncanonical SVN endpoint"),
                    "{id}/{path}: {error}"
                );
                let after =
                    f.db.conn()
                        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
                        .unwrap()
                        .query_map([], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                            ))
                        })
                        .unwrap()
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap();
                assert_eq!(after, before);
                assert!(f.no_hold(None));
            }
        }
    }
}

#[tokio::test]
async fn real_svn_aliases_are_not_admitted_as_distinct_nodes() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    let output = std::process::Command::new("svnadmin")
        .arg("create")
        .arg(&repository)
        .output()
        .unwrap();
    assert!(output.status.success());
    let root = reqwest::Url::from_directory_path(&repository)
        .unwrap()
        .to_string()
        .trim_end_matches('/')
        .to_owned();
    let output = std::process::Command::new("svn")
        .args([
            "mkdir",
            &format!("{root}/trunk"),
            &format!("{root}/branches"),
            &format!("{root}/branches/f"),
            "-m",
            "disposable alias layout",
            "--non-interactive",
            "--no-auth-cache",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (canonical, alias) in [("trunk", "trunk/"), ("branches/f", "branches//f")] {
        let original = reposync_core::svn::SvnClient::new(format!("{root}/{canonical}"), "", "")
            .info()
            .await
            .unwrap();
        let aliased = reposync_core::svn::SvnClient::new(format!("{root}/{alias}"), "", "")
            .info()
            .await
            .unwrap();
        assert_eq!(original.url, aliased.url);
        assert_eq!(original.uuid, aliased.uuid);
        let f = job::Fixture::new();
        f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
        f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1',svn_url=?1,svn_branch=CASE WHEN id='child' THEN ?2 ELSE ?3 END",[root.as_str(),alias,canonical]).unwrap();
        let error =
            f.db.capture_pair_refresh_execution_configuration("child")
                .err()
                .unwrap()
                .to_string();
        assert!(error.contains("noncanonical SVN endpoint"));
        assert!(f.no_hold(None));
    }
}

#[test]
fn ordinary_nested_ascii_registered_svn_paths_remain_distinct_and_exact() {
    let f = job::Fixture::new();
    f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
    f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1',svn_url='svn://svn.example/repo',svn_branch=CASE WHEN id='child' THEN 'branches/feature_1/sub-pair.2' ELSE 'trunk' END",[]).unwrap();
    let configuration =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    assert_eq!(
        configuration.pin().child_svn_url,
        "svn://svn.example/repo/branches/feature_1/sub-pair.2"
    );
    assert_eq!(
        configuration.pin().parent_svn_url,
        "svn://svn.example/repo/trunk"
    );
    assert!(f.no_hold(None));
}
