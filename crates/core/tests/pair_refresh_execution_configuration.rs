//! Registered sources must bind the job, independently of client-supplied pins.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

fn fixture() -> job::Fixture {
    let f = job::Fixture::new();
    f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
    f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1'", []).unwrap();
    f.db.conn()
        .execute(
            "UPDATE repositories SET svn_branch='branches/f' WHERE id='child'",
            [],
        )
        .unwrap();
    f
}

fn rows(f: &job::Fixture) -> Vec<(String, String)> {
    f.db.conn()
        .prepare("SELECT key,value FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn captures_actual_registered_sources_and_private_credentials_without_writing() {
    let f = fixture();
    f.db.set_state("secret_git_token_parent", "disposable-configuration-token")
        .unwrap();
    f.db.set_state("secret_svn_password", "disposable-configuration-password")
        .unwrap();
    f.db.conn()
        .execute("UPDATE repositories SET svn_username='fixture'", [])
        .unwrap();
    f.db.conn()
        .execute(
            "UPDATE repositories SET allowed_paths='[]',blocked_patterns=' [] '",
            [],
        )
        .unwrap();
    let before = rows(&f);
    let configuration =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    let pin = configuration.pin();
    assert_eq!(pin.version, 2);
    assert_eq!(pin.git_remote_url, "https://git.example/owner/repo.git");
    assert_eq!(pin.child_git_branch, "feature");
    assert_eq!(pin.parent_git_branch, "main");
    assert_eq!(pin.child_svn_url, "https://svn.example/repo/branches/f");
    assert_eq!(pin.parent_svn_url, "https://svn.example/repo/trunk");
    assert_eq!(
        configuration.credentials().child_git_token(),
        Some("disposable-configuration-token")
    );
    assert_eq!(
        configuration.credentials().parent_svn_password(),
        Some("disposable-configuration-password")
    );
    assert_eq!(
        pin.credential_context_digest,
        configuration.credentials().context_digest()
    );
    let encoded = serde_json::to_string(pin).unwrap();
    assert!(!encoded.contains("disposable-configuration-token"));
    assert!(!encoded.contains("disposable-configuration-password"));
    assert_eq!(rows(&f), before);
    assert!(f.no_hold(None));
}

#[test]
fn unsupported_configuration_on_either_registration_refuses_without_adoption() {
    for id in ["child", "parent"] {
        for (column, value) in [
            ("sync_mode", "personal"),
            ("enabled", "0"),
            ("lfs_threshold_mb", "1"),
            ("lfs_threshold_mb", "-1"),
            ("allowed_paths", "[\"src\"]"),
            ("blocked_patterns", "[\"*.bin\"]"),
            ("allowed_paths", "{}"),
            ("blocked_patterns", "broken"),
            ("git_provider", "unknown"),
            ("git_branch", "bad..branch"),
        ] {
            let f = fixture();
            f.db.conn()
                .execute(
                    &format!("UPDATE repositories SET {column}=?1 WHERE id=?2"),
                    [value, id],
                )
                .unwrap();
            let before = rows(&f);
            assert!(
                f.db.capture_pair_refresh_execution_configuration("child")
                    .is_err(),
                "{id}/{column}"
            );
            assert_eq!(rows(&f), before);
            assert!(f.no_hold(None));
        }
    }
}

#[test]
fn foreign_noncanonical_and_credential_bearing_endpoints_never_become_execution_inputs() {
    for (column, value) in [
        ("git_repo", "owner/foreign"),
        ("git_repo", "../escape"),
        ("git_api_url", "http://git.example/api/v1"),
        ("git_api_url", "https://different.example/api/v1"),
        ("git_branch", "main"),
        ("svn_branch", "trunk"),
        (
            "svn_url",
            "https://u:disposable-uri-password@svn.example/repo",
        ),
        ("svn_url", "https://svn.example/repo?unexpected=1"),
        ("svn_branch", "branches/../trunk"),
        ("svn_branch", "branches/%66"),
        ("svn_url", "svn+ssh://svn.example/repo"),
    ] {
        let f = fixture();
        f.db.conn()
            .execute(
                &format!("UPDATE repositories SET {column}=?1 WHERE id='child'"),
                [value],
            )
            .unwrap();
        let before = rows(&f);
        let error =
            f.db.capture_pair_refresh_execution_configuration("child")
                .err()
                .unwrap();
        assert!(!error.to_string().contains("disposable-uri-password"));
        assert_eq!(rows(&f), before);
        assert!(f.no_hold(None));
    }
}

#[test]
fn missing_identity_checkpoint_and_fingerprint_key_are_refused_without_repair() {
    for drift in ["identity", "checkpoint", "key", "parent", "unbootstrapped"] {
        let f = fixture();
        match drift {
            "identity" => {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET scope_uuid=NULL WHERE id='child'",
                        [],
                    )
                    .unwrap();
            }
            "checkpoint" => {
                f.db.delete_kv_state("last_svn_rev_child").unwrap();
            }
            "key" => {
                f.db.delete_kv_state("encryption_key").unwrap();
            }
            "parent" => {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET parent_id=NULL WHERE id='child'",
                        [],
                    )
                    .unwrap();
            }
            "unbootstrapped" => {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET last_svn_rev=0,last_git_sha='' WHERE id='child'",
                        [],
                    )
                    .unwrap();
                f.db.set_state("last_svn_rev_child", "0").unwrap();
                let key = f.db.scoped_last_git_sha_kv_key_for_repo("child").unwrap();
                f.db.set_state(&key, "").unwrap();
            }
            _ => unreachable!(),
        }
        let before = rows(&f);
        assert!(
            f.db.capture_pair_refresh_execution_configuration("child")
                .is_err(),
            "{drift}"
        );
        assert_eq!(rows(&f), before);
        assert!(f.no_hold(None));
    }
}

#[test]
fn stored_job_cannot_substitute_endpoints_credentials_or_parent_configuration() {
    let f = fixture();
    let configuration =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    let mut pins = f.pins();
    pins.git_remote_url = configuration.pin().git_remote_url.clone();
    pins.credential_context_digest = configuration.pin().credential_context_digest.clone();
    configuration.require_job_configuration(&pins).unwrap();
    for drift in ["git", "svn", "branch", "credential", "policy"] {
        let mut changed = pins.clone();
        match drift {
            "git" => changed.git_remote_url = "https://foreign.example/owner/repo.git".into(),
            "svn" => {
                changed.svn.pins.lineage.pins.child_url =
                    "https://foreign.example/repo/branches/f".into()
            }
            "branch" => changed.git_branch = "foreign".into(),
            "credential" => changed.credential_context_digest = "0".repeat(64),
            "policy" => {
                changed.transport_policy =
                    reposync_core::pair_refresh::publication::PinnedPushPolicy::GitProtocolV1
            }
            _ => unreachable!(),
        }
        assert!(
            configuration.require_job_configuration(&changed).is_err(),
            "{drift}"
        );
    }
    f.db.conn()
        .execute(
            "UPDATE repositories SET git_branch='new-parent' WHERE id='parent'",
            [],
        )
        .unwrap();
    let current =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    assert!(current.require_job_configuration(&pins).is_err());
    assert_eq!(current.pin().parent_git_branch, "new-parent");
    assert!(f.no_hold(None));
}

#[test]
fn revocation_refuses_source_capture_and_never_invents_a_fallback() {
    for key in [
        "secret_git_token_child",
        "secret_git_token_parent",
        "secret_svn_password",
    ] {
        let f = fixture();
        f.db.set_state(key, "").unwrap();
        let before = rows(&f);
        let error =
            f.db.capture_pair_refresh_execution_configuration("child")
                .err()
                .unwrap();
        assert!(
            error.to_string().contains("explicit credential revocation"),
            "{error}"
        );
        assert_eq!(rows(&f), before);
        assert!(f.no_hold(None));
    }
}
