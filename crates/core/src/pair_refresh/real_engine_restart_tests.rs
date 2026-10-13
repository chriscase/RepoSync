//! Process-exit recovery on correspondence produced by actual import and cycles.
use super::*;
use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};

#[test]
fn qualified_real_engine_restart_driver() {
    let temp = tempfile::tempdir().unwrap();
    let trust = temp.path().join("pair-refresh-probe-git-config");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!(
                "{}::qualified_real_engine_restart_worker",
                module_path!().split_once("::").unwrap().1
            ),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("REPOSYNC_REAL_ENGINE_RESTART_FIXTURE", "1")
        .env("GIT_CONFIG_GLOBAL", &trust)
        .env("REPOSYNC_PROBE_GIT_CONFIG", &trust)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("REPOSYNC_REAL_ENGINE_RESTART_CONTROLS_VERIFIED"));
    #[cfg(target_os = "linux")]
    assert!(stdout.contains("REPOSYNC_REAL_ENGINE_RESTART_POSITIVE_COUNT 3"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qualified_real_engine_crash_worker() -> Result<()> {
    let Ok(root) = std::env::var("REPOSYNC_REAL_ENGINE_CRASH_ROOT") else {
        return Ok(());
    };
    let root = PathBuf::from(root);
    assert_eq!(root.canonicalize()?, root);
    let stage = std::env::var("REPOSYNC_REAL_ENGINE_CRASH_STAGE")?;
    assert!(matches!(
        stage.as_str(),
        "prepared" | "svn_intent" | "git_intent"
    ));
    let config: AppConfig =
        toml::from_str(&std::fs::read_to_string(root.join("restart-config.toml"))?)?;
    let identity = Arc::new(IdentityMapper::new(&config.identity)?);
    let running = Arc::new(engine(
        &config,
        &root,
        &config.svn.url,
        &root.join("repos/parent/git-repo"),
        "parent",
        &identity,
    )?);
    let _lock = crate::data_dir::acquire(&root)?;
    let _fence = crate::writer_fence::claim(running.db(), &root)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .add_root_certificate(reqwest::Certificate::from_der(&std::fs::read(
            root.join("cert.der"),
        )?)?)
        .build()?;
    let request = format!("real-crash-{stage}");
    let id = if stage == "prepared" {
        let actor = running.capture_pair_refresh_actor_policy("real-engine-session")?;
        let workspace = actor.create_workspace(&running)?;
        let prepared =
            crate::pair_refresh::live_preparation::prepare_registered_in_workspace_for_test(
                running.db(),
                "child",
                workspace,
                client,
            )
            .await?;
        let digest = actor.prepared_approval_digest(&running, &prepared)?;
        let busy = crate::busy::try_acquire_pair("child", "parent").context("pair busy")?;
        let operation = actor.enroll_prepared(&running, &prepared, &request, &digest, &busy)?;
        // Retention survives dropping only this preparation too; process exit
        // below deliberately bypasses the durable writer guard's destructor.
        operation.id
    } else {
        let service = RefreshService::with_fixture_client(running.clone(), client);
        let approval = match service
            .prepare(
                "real-engine-session".into(),
                "child".into(),
                request.clone(),
            )
            .await?
        {
            RefreshResponse::Approval(value) => value,
            other => anyhow::bail!("unexpected {other:?}"),
        };
        let stop_phase = if stage == "svn_intent" {
            "svn_observed"
        } else {
            "git_observed"
        };
        running.db().conn().execute_batch(&format!(
            "CREATE TRIGGER stop_real_receipt BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='{stop_phase}' BEGIN SELECT RAISE(ABORT,'real engine lost receipt'); END;"
        ))?;
        let error = service
            .act(
                "real-engine-session".into(),
                "child",
                &request,
                approval.preparation_id,
                approval.approval_digest,
                RefreshAction::Execute,
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("real engine lost receipt"),
            "{error:#}"
        );
        running
            .db()
            .conn()
            .execute_batch("DROP TRIGGER stop_real_receipt;")?;
        // Do not drop the live worker: exit is the observed native-owner death.
        std::mem::forget(service);
        approval.operation_id
    };
    let operation = running.db().pair_refresh_operation(id)?;
    assert_eq!(serde_json::to_value(operation.phase)?, stage);
    println!(
        "REPOSYNC_REAL_ENGINE_CRASH_READY {}",
        serde_json::json!({"id":id,"pid":std::process::id(),"stage":stage})
    );
    std::io::Write::flush(&mut std::io::stdout())?;
    std::process::exit(0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qualified_real_engine_restart_worker() -> Result<()> {
    if std::env::var("REPOSYNC_REAL_ENGINE_RESTART_FIXTURE")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Ok(());
    }
    let mut positives = 0;
    for stage in ["prepared", "svn_intent", "git_intent"] {
        let f = fixture().await?;
        quiet(&f.child_engine.run_sync_cycle().await?);
        std::fs::write(f.parent_wc.join("advanced.txt"), b"actual parent advance\n")?;
        svn(&["add", "advanced.txt"], Some(&f.parent_wc));
        svn(
            &["commit", "-m", "actual parent advance"],
            Some(&f.parent_wc),
        );
        let stats = f.parent_engine.run_sync_cycle().await?;
        assert_eq!(stats.svn_to_git_count, 1);
        assert_eq!(stats.git_to_svn_count, 0);
        quiet(&f.parent_engine.run_sync_cycle().await?);
        let parent_tip = f.db.get_repo_watermark("parent")?.1;
        checkpoint(&f, "parent", 4, &parent_tip)?;
        checkpoint(&f, "child", 3, &f.baseline)?;
        let parent_before = serde_json::to_value(f.db.get_repository("parent")?)?;
        let config = f.parent_engine.config().clone();
        let root = f.temp.path().canonicalize()?;
        std::fs::write(root.join("restart-config.toml"), toml::to_string(&config)?)?;
        let Fixture {
            db,
            target,
            svn_root,
            client,
            _server,
            temp,
            baseline,
            _lock,
            _fence,
            parent_engine,
            child_engine,
            parent_wc: _,
        } = f;
        drop(parent_engine);
        drop(child_engine);
        drop(_fence);
        drop(_lock);
        let output = run(Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                &format!(
                    "{}::qualified_real_engine_crash_worker",
                    module_path!().split_once("::").unwrap().1
                ),
                "--nocapture",
                "--test-threads=1",
            ])
            .env("REPOSYNC_REAL_ENGINE_CRASH_ROOT", &root)
            .env("REPOSYNC_REAL_ENGINE_CRASH_STAGE", stage));
        let ready: serde_json::Value = serde_json::from_str(
            output
                .lines()
                .find_map(|line| {
                    line.split_once("REPOSYNC_REAL_ENGINE_CRASH_READY ")
                        .map(|(_, value)| value)
                })
                .context("crash marker")?,
        )?;
        let id = uuid::Uuid::parse_str(ready["id"].as_str().context("operation id")?)?;
        let operation = db.pair_refresh_operation(id)?;
        let authority_key = format!("pair_refresh_execution_v3:authority:{id}");
        let authority_raw = db.get_state(&authority_key)?.context("authority")?;
        let authority: serde_json::Value = serde_json::from_str(&authority_raw)?;
        let native = authority["evidence"]["resources"]["workspace"].get("native_owner");
        #[cfg(target_os = "linux")]
        assert!(native.is_some(), "Linux native proof must be available");
        let identity = Arc::new(IdentityMapper::new(&config.identity)?);
        let running = Arc::new(SyncEngine::new(
            config.clone(),
            Database::open_existing_for_pair_refresh_recovery(root.join("reposync.db"))?,
            SvnClient::new(&config.svn.url, "", ""),
            GitClient::new(root.join("repos/parent/git-repo"))?,
            identity.clone(),
        ));
        let lock = Arc::new(crate::data_dir::acquire(&root)?);
        let service = RefreshService::with_fixture_recovery_client(running.clone(), lock, client)?;
        let request = format!("real-crash-{stage}");
        let prepared = service
            .prepare_recovery(
                "real-engine-session".into(),
                "child".into(),
                request.clone(),
            )
            .await;
        if native.is_none() {
            assert!(prepared.is_err());
            assert_eq!(
                db.get_state(&authority_key)?.as_deref(),
                Some(authority_raw.as_str())
            );
            assert_eq!(db.pair_refresh_operation(id)?, operation);
            assert!(
                crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&db.conn(), None)
                    .is_err()
            );
            assert_eq!(
                serde_json::to_value(db.get_repository("parent")?)?,
                parent_before
            );
            continue;
        }
        let approval = match prepared? {
            RefreshResponse::Approval(value) => value,
            other => anyhow::bail!("unexpected {other:?}"),
        };
        assert_eq!(approval.operation_id, id);
        assert_eq!(approval.kind, "recovery");
        if stage != "prepared" {
            let observed = service
                .act(
                    "real-engine-session".into(),
                    "child",
                    &request,
                    approval.preparation_id,
                    approval.approval_digest.clone(),
                    RefreshAction::Reconcile,
                )
                .await?;
            assert_eq!(
                observed.phase,
                if stage == "svn_intent" {
                    "svn_observed"
                } else {
                    "git_observed"
                }
            );
        }
        let completed = service
            .act(
                "real-engine-session".into(),
                "child",
                &request,
                approval.preparation_id,
                approval.approval_digest.clone(),
                RefreshAction::Execute,
            )
            .await?;
        assert_eq!(completed.phase, "completed");
        assert_eq!(completed.operation_id, id);
        assert_eq!(completed.svn_revision, Some(5));
        assert_eq!(
            completed.git_sha.as_deref(),
            Some(operation.pins.git.prepared_sha.as_str())
        );
        let repeated = service
            .act(
                "real-engine-session".into(),
                "child",
                &request,
                approval.preparation_id,
                approval.approval_digest,
                RefreshAction::Execute,
            )
            .await?;
        assert_eq!(repeated.operation_id, id);
        assert_eq!(
            serde_json::to_value(db.get_repository("parent")?)?,
            parent_before
        );
        let merged = target.find_commit(git2::Oid::from_str(&operation.pins.git.prepared_sha)?)?;
        assert_eq!(merged.parent_count(), 2);
        assert_eq!(merged.parent_id(0)?.to_string(), baseline);
        assert_eq!(merged.parent_id(1)?.to_string(), parent_tip);
        assert_eq!(
            merged.tree_id(),
            target
                .find_commit(git2::Oid::from_str(&parent_tip)?)?
                .tree_id()
        );
        assert_eq!(
            SvnClient::new(format!("{svn_root}/branches/f"), "", "")
                .info()
                .await?
                .latest_rev,
            5
        );
        let counts = std::fs::read_to_string(root.join("requests.jsonl"))?
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count();
        assert_eq!(counts, 1);
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&db.conn(), None)
                .is_ok()
        );
        drop(service);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while db.get_state("writer_fence_v1:lease")?.is_some() {
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(running);
        let (_reopened, _owner) = Database::open_with_exclusive_owner(&root)?;
        let child = engine(
            &config,
            &root,
            &format!("{svn_root}/branches/f"),
            &root.join("repos/child/git-repo"),
            "child",
            &identity,
        )?;
        quiet(&child.run_sync_cycle().await?);
        assert_eq!(
            db.get_repo_watermark("child")?,
            (5, operation.pins.git.prepared_sha)
        );
        assert_eq!(
            std::fs::read_to_string(root.join("requests.jsonl"))?
                .lines()
                .filter(|line| line.contains("/git-receive-pack"))
                .count(),
            counts
        );
        positives += 1;
        drop(temp);
    }
    println!("REPOSYNC_REAL_ENGINE_RESTART_POSITIVE_COUNT {positives}");
    println!("REPOSYNC_REAL_ENGINE_RESTART_CONTROLS_VERIFIED");
    Ok(())
}
