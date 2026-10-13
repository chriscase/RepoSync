//! Native subprocess proof for the gap between claiming and transferring a job.
use super::*;

fn recovery_engine(root: &Path) -> crate::sync_engine::SyncEngine {
    let mut config: crate::config::AppConfig = toml::from_str(
        r#"
        [daemon]
        data_dir = "/unused"
        [svn]
        url = "file:///unused/svn"
        username = ""
        password_env = ""
        [github]
        repo = "fixture/repo"
        token_env = ""
    "#,
    )
    .unwrap();
    config.daemon.data_dir = root.to_path_buf();
    crate::sync_engine::SyncEngine::new(
        config,
        Database::new(root.join("reposync.db")).unwrap(),
        crate::svn::SvnClient::new("file:///unused/svn", "", ""),
        crate::git::client::GitClient::new(root.join("engine-git")).unwrap(),
        std::sync::Arc::new(
            crate::identity::IdentityMapper::new(&crate::config::IdentityConfig::default())
                .unwrap(),
        ),
    )
}

#[test]
fn native_interrupted_claim_worker() {
    let Ok(root) = std::env::var("REPOSYNC_NATIVE_CLAIM_FIXTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert_eq!(root.canonicalize().unwrap(), root);
    let id = Uuid::parse_str(&std::env::var("REPOSYNC_NATIVE_CLAIM_FIXTURE_ID").unwrap()).unwrap();
    let engine = recovery_engine(&root);
    let lock = crate::data_dir::acquire(&root).unwrap();
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let before = engine.db().pair_refresh_operation(id).unwrap();
    let authority_key = format!("pair_refresh_execution_v3:authority:{id}");
    let authority = engine.db().get_state(&authority_key).unwrap();
    let fence = crate::pair_refresh::recovery::claim_owned_refresh_recovery_writer(
        &engine, &actor, id, &busy, &lock,
    )
    .unwrap();
    assert!(fence.epoch() > before.admitted_writer_epoch);
    assert_eq!(engine.db().pair_refresh_operation(id).unwrap(), before);
    assert_eq!(engine.db().get_state(&authority_key).unwrap(), authority);
    println!(
        "REPOSYNC_INTERRUPTED_CLAIM {}",
        serde_json::json!({"epoch":fence.epoch(),"pid":std::process::id()})
    );
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    // Exit without dropping the fence, exactly between lease commit and transfer.
    std::process::exit(0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_recovery_survives_two_interrupted_claims_before_transfer() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let root = f.temp.path().canonicalize().unwrap();
    let initial = run(Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pair_refresh::live_preparation::authenticated_enrollment_tests::native_recovery_fixture_worker", "--nocapture", "--test-threads=1"])
        .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_ROOT", &root)
        .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_STAGE", "prepared"));
    let marker = initial
        .lines()
        .find_map(|line| {
            line.split_once("REPOSYNC_NATIVE_RECOVERY_READY ")
                .map(|(_, value)| value)
        })
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(marker).unwrap();
    let id = Uuid::parse_str(ready["id"].as_str().unwrap()).unwrap();
    let original = f.db.pair_refresh_operation(id).unwrap();
    let before = journal_state(&f);
    let authority_key = format!("pair_refresh_execution_v3:authority:{id}");
    let authority: serde_json::Value =
        serde_json::from_str(&f.db.get_state(&authority_key).unwrap().unwrap()).unwrap();
    assert!(authority["evidence"]["resources"]["workspace"]
        .get("native_owner")
        .is_some());
    let mut epoch = original.admitted_writer_epoch;
    for _ in 0..2 {
        let output = run(Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "pair_refresh::live_preparation::authenticated_enrollment_tests::native_claim_crash_tests::native_interrupted_claim_worker", "--nocapture", "--test-threads=1"])
            .env("REPOSYNC_NATIVE_CLAIM_FIXTURE_ROOT", &root)
            .env("REPOSYNC_NATIVE_CLAIM_FIXTURE_ID", id.to_string()));
        let marker = output
            .lines()
            .find_map(|line| {
                line.split_once("REPOSYNC_INTERRUPTED_CLAIM ")
                    .map(|(_, value)| value)
            })
            .unwrap();
        let claim: serde_json::Value = serde_json::from_str(marker).unwrap();
        let next = claim["epoch"].as_u64().unwrap();
        assert!(next > epoch);
        epoch = next;
        let lease: serde_json::Value =
            serde_json::from_str(&f.db.get_state("writer_fence_v1:lease").unwrap().unwrap())
                .unwrap();
        assert_eq!(lease["epoch"], claim["epoch"]);
        assert_eq!(lease["holder_pid"], claim["pid"]);
        assert_eq!(journal_state(&f), before);
        assert_eq!(f.db.pair_refresh_operation(id).unwrap(), original);
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
                .is_err()
        );
    }
    let lock = crate::data_dir::acquire(&root).unwrap();
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let fence = crate::pair_refresh::recovery::claim_owned_refresh_recovery_writer(
        &engine, &actor, id, &busy, &lock,
    )
    .unwrap();
    assert!(fence.epoch() > epoch);
    let claimed_lease = f.db.get_state("writer_fence_v1:lease").unwrap();
    assert!(
        crate::pair_refresh::recovery::claim_owned_refresh_recovery_writer(
            &engine, &actor, id, &busy, &lock
        )
        .is_err()
    );
    assert_eq!(
        f.db.get_state("writer_fence_v1:lease").unwrap(),
        claimed_lease
    );
    assert_eq!(journal_state(&f), before);
    let recovery =
        crate::pair_refresh::recovery::prepare_owned_refresh_recovery(&engine, &actor, id, &busy)
            .unwrap();
    let approved = recovery.approval_digest(&engine, &actor).unwrap();
    let recovered = recovery
        .transfer(&engine, &actor, &approved, &busy)
        .unwrap();
    assert_eq!(recovered.operation(), &original);
    let transferred: serde_json::Value =
        serde_json::from_str(&f.db.get_state(&authority_key).unwrap().unwrap()).unwrap();
    assert_eq!(transferred["evidence"], authority["evidence"]);
    assert_eq!(transferred["recoveries"].as_array().unwrap().len(), 1);
    assert_eq!(
        transferred["recoveries"][0]["writer_epoch"].as_u64(),
        Some(fence.epoch())
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        4
    );
    recovered
        .cancel_before_intent(&engine, &actor, &busy)
        .unwrap();
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_recovery_refuses_unbound_or_foreign_native_lease_even_with_matching_epoch_pid() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let root = f.temp.path().canonicalize().unwrap();
    let output = run(Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pair_refresh::live_preparation::authenticated_enrollment_tests::native_recovery_fixture_worker", "--nocapture", "--test-threads=1"])
        .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_ROOT", &root)
        .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_STAGE", "prepared"));
    let marker = output
        .lines()
        .find_map(|line| {
            line.split_once("REPOSYNC_NATIVE_RECOVERY_READY ")
                .map(|(_, value)| value)
        })
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(marker).unwrap();
    let id = Uuid::parse_str(ready["id"].as_str().unwrap()).unwrap();
    let original = f.db.pair_refresh_operation(id).unwrap();
    let journal = journal_state(&f);
    let native_raw = f.db.get_state("writer_fence_v1:native").unwrap().unwrap();
    let lease_raw = f.db.get_state("writer_fence_v1:lease").unwrap().unwrap();
    let native: serde_json::Value = serde_json::from_str(&native_raw).unwrap();
    let lock = crate::data_dir::acquire(&root).unwrap();
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    for case in [
        "missing",
        "host",
        "boot",
        "namespace",
        "pid",
        "unbound_lease",
    ] {
        let mut candidate = native.clone();
        match case {
            "missing" => {
                f.db.conn()
                    .execute(
                        "DELETE FROM kv_state WHERE key='writer_fence_v1:native'",
                        [],
                    )
                    .unwrap();
            }
            "host" => candidate["owner"]["host_digest"] = serde_json::json!("0".repeat(64)),
            "boot" => candidate["owner"]["boot_digest"] = serde_json::json!("0".repeat(64)),
            "namespace" => {
                candidate["owner"]["pid_namespace"] = serde_json::json!("foreign-namespace")
            }
            "pid" => candidate["owner"]["pid"] = serde_json::json!(std::process::id()),
            "unbound_lease" => {
                candidate["lease"]["renewed_at"] = serde_json::json!("foreign-renewal")
            }
            _ => unreachable!(),
        }
        if case != "missing" {
            f.db.set_state("writer_fence_v1:native", &candidate.to_string())
                .unwrap();
        }
        let before = f.db.get_state("writer_fence_v1:native").unwrap();
        assert!(
            crate::pair_refresh::recovery::claim_owned_refresh_recovery_writer(
                &engine, &actor, id, &busy, &lock
            )
            .is_err(),
            "{case}"
        );
        assert_eq!(
            f.db.get_state("writer_fence_v1:native").unwrap(),
            before,
            "{case}"
        );
        assert_eq!(
            f.db.get_state("writer_fence_v1:lease").unwrap().as_deref(),
            Some(lease_raw.as_str()),
            "{case}"
        );
        assert_eq!(f.db.pair_refresh_operation(id).unwrap(), original, "{case}");
        assert_eq!(journal_state(&f), journal, "{case}");
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
                .is_err(),
            "{case}"
        );
        f.db.set_state("writer_fence_v1:native", &native_raw)
            .unwrap();
    }
}
