//! Default ports and alternate authorities cannot stand in for distinct nodes.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

fn fixture() -> job::Fixture {
    let f = job::Fixture::new();
    f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
    f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1'", []).unwrap();
    f
}

fn rows(f: &job::Fixture) -> Vec<(String, String, String)> {
    f.db.conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn svn_default_port_survives_the_url_parser_but_registered_capture_refuses_it() {
    let raw = "svn://svn.example:3690/repo/trunk";
    let parsed = reqwest::Url::parse(raw).unwrap();
    assert_eq!(parsed.as_str(), raw);
    assert_eq!(parsed.port(), Some(3690));
    for id in ["child", "parent"] {
        let f = fixture();
        f.db.conn()
            .execute(
                "UPDATE repositories SET svn_url='svn://svn.example/repo'",
                [],
            )
            .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET svn_url='svn://svn.example:3690/repo' WHERE id=?1",
                [id],
            )
            .unwrap();
        let before = rows(&f);
        assert!(
            f.db.capture_pair_refresh_execution_configuration("child")
                .is_err(),
            "{id}"
        );
        assert_eq!(rows(&f), before);
        assert!(f.no_hold(None));
    }
}

#[test]
fn differing_host_scheme_and_port_spellings_refuse_before_distinctness() {
    for (parent, child) in [
        ("svn://svn.example/repo", "svn://SVN.example/repo"),
        ("svn://svn.example/repo", "svn://svn.example./repo"),
        ("svn://svn.example/repo", "svn://alias.example/repo"),
        ("svn://svn.example:3691/repo", "svn://svn.example:3692/repo"),
        ("https://svn.example/repo", "http://svn.example/repo"),
        ("https://svn.example/repo", "https://alias.example/repo"),
        ("file:///repo", "file://localhost/repo"),
    ] {
        let f = fixture();
        f.db.conn()
            .execute(
                "UPDATE repositories SET svn_url=?1 WHERE id='parent'",
                [parent],
            )
            .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET svn_url=?1 WHERE id='child'",
                [child],
            )
            .unwrap();
        let before = rows(&f);
        assert!(
            f.db.capture_pair_refresh_execution_configuration("child")
                .is_err(),
            "{parent}/{child}"
        );
        assert_eq!(rows(&f), before);
        assert!(f.no_hold(None));
    }
}

#[test]
fn one_canonical_authority_with_distinct_paths_retains_nondefault_svn_ports() {
    for root in [
        "svn://svn.example/repo",
        "svn://svn.example:3691/repo",
        "https://svn.example/repo",
        "http://svn.example/repo",
        "file:///repo",
    ] {
        let f = fixture();
        f.db.conn()
            .execute("UPDATE repositories SET svn_url=?1", [root])
            .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET svn_branch='branches/f' WHERE id='child'",
                [],
            )
            .unwrap();
        let before = rows(&f);
        let configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        assert_eq!(
            configuration.pin().child_svn_url,
            format!("{root}/branches/f")
        );
        assert_eq!(configuration.pin().parent_svn_url, format!("{root}/trunk"));
        assert_eq!(rows(&f), before);
        assert!(f.no_hold(None));
    }
}
