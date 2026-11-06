//! Actual-engine restart after a durable SVN receipt and after a terminal checkpoint.
use super::*;
use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};

#[test]
fn qualified_real_engine_receipt_restart_driver() {
    let temp = tempfile::tempdir().unwrap();
    let trust = temp.path().join("pair-refresh-probe-git-config");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!(
                "{}::qualified_real_engine_receipt_restart_worker",
                module_path!().split_once("::").unwrap().1
            ),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("REPOSYNC_REAL_ENGINE_RECEIPT_RESTART_FIXTURE", "1")
        .env("REPOSYNC_FENCE_HOST", "disposable-receipt-restart-host")
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
    assert!(stdout.contains("REPOSYNC_REAL_ENGINE_RECEIPT_RESTART_CONTROLS_VERIFIED"));
    #[cfg(target_os = "linux")]
    assert!(stdout.contains("REPOSYNC_REAL_ENGINE_RECEIPT_RESTART_POSITIVE_COUNT 2"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qualified_real_engine_receipt_crash_worker() -> Result<()> {
    let Ok(root) = std::env::var("REPOSYNC_REAL_ENGINE_RECEIPT_CRASH_ROOT") else {
        return Ok(());
    };
    let root = PathBuf::from(root);
    assert_eq!(root.canonicalize()?, root);
    let stage = std::env::var("REPOSYNC_REAL_ENGINE_RECEIPT_CRASH_STAGE")?;
    assert!(matches!(stage.as_str(), "svn_observed" | "completed"));
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
    if stage == "svn_observed" {
        running.db().conn().execute_batch(
            "CREATE TRIGGER stop_real_next_intent BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='git_intent' BEGIN SELECT RAISE(ABORT,'real engine stop before Git intent'); END;"
        )?;
    }
    let result = service
        .act(
            "real-engine-session".into(),
            "child",
            &request,
            approval.preparation_id,
            approval.approval_digest.clone(),
            RefreshAction::Execute,
        )
        .await;
    if stage == "svn_observed" {
        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("real engine stop before Git intent"),
            "{error:#}"
        );
        running
            .db()
            .conn()
            .execute_batch("DROP TRIGGER stop_real_next_intent;")?;
    } else {
        let result = result?;
        assert_eq!(result.phase, "completed");
        assert_eq!(result.svn_revision, Some(5));
    }
    // Save only the original approval the caller already possessed. The caller
    // receives no completion response before the process exits.
    std::fs::write(
        root.join("original-approval.json"),
        serde_json::to_vec(&approval)?,
    )?;
    let id = approval.operation_id;
    std::mem::forget(service);
    let operation = running.db().pair_refresh_operation(id)?;
    assert_eq!(serde_json::to_value(operation.phase)?, stage);
    println!(
        "REPOSYNC_REAL_ENGINE_RECEIPT_CRASH_READY {}",
        serde_json::json!({"id":id,"pid":std::process::id(),"stage":stage})
    );
    std::io::Write::flush(&mut std::io::stdout())?;
    std::process::exit(0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qualified_real_engine_receipt_restart_worker() -> Result<()> {
    if std::env::var("REPOSYNC_REAL_ENGINE_RECEIPT_RESTART_FIXTURE")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Ok(());
    }
    let mut positives = 0;
    for stage in ["svn_observed", "completed"] {
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
                    "{}::qualified_real_engine_receipt_crash_worker",
                    module_path!().split_once("::").unwrap().1
                ),
                "--nocapture",
                "--test-threads=1",
            ])
            .env("REPOSYNC_REAL_ENGINE_RECEIPT_CRASH_ROOT", &root)
            .env("REPOSYNC_REAL_ENGINE_RECEIPT_CRASH_STAGE", stage));
        let ready: serde_json::Value = serde_json::from_str(
            output
                .lines()
                .find_map(|line| {
                    line.split_once("REPOSYNC_REAL_ENGINE_RECEIPT_CRASH_READY ")
                        .map(|(_, value)| value)
                })
                .context("crash marker")?,
        )?;
        let id = uuid::Uuid::parse_str(ready["id"].as_str().context("operation id")?)?;
        let operation = db.pair_refresh_operation(id)?;
        assert_eq!(
            operation
                .svn_receipt
                .as_ref()
                .context("durable SVN receipt")?
                .revision,
            5
        );
        assert_eq!(
            SvnClient::new(format!("{svn_root}/branches/f"), "", "")
                .info()
                .await?
                .latest_rev,
            5
        );
        assert_eq!(
            std::fs::read_to_string(root.join("requests.jsonl"))?
                .lines()
                .filter(|line| line.contains("/git-receive-pack"))
                .count(),
            usize::from(stage == "completed")
        );
        assert_eq!(
            db.get_repo_watermark("child")?,
            if stage == "completed" {
                (5, operation.pins.git.prepared_sha.clone())
            } else {
                (3, baseline.clone())
            }
        );
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
        if native.is_none() && stage != "completed" {
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
        let approval = if stage == "completed" {
            let receipt = match prepared? {
                RefreshResponse::Receipt(value) => value,
                other => anyhow::bail!("unexpected {other:?}"),
            };
            assert_eq!(receipt.operation_id, id);
            assert_eq!(receipt.phase, "completed");
            assert_eq!(receipt.svn_revision, Some(5));
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("original-approval.json"))?)?;
            crate::pair_refresh::service::RefreshApprovalView {
                version: 1,
                expires_at: saved["expires_at"]
                    .as_str()
                    .context("original expiry")?
                    .into(),
                preparation_id: uuid::Uuid::parse_str(
                    saved["preparation_id"].as_str().context("preparation id")?,
                )?,
                operation_id: id,
                request_id: request.clone(),
                child_id: "child".into(),
                parent_id: "parent".into(),
                approval_digest: saved["approval_digest"]
                    .as_str()
                    .context("original approval digest")?
                    .into(),
                kind: "integration".into(),
            }
        } else {
            let approval = match prepared? {
                RefreshResponse::Approval(value) => value,
                other => anyhow::bail!("unexpected {other:?}"),
            };
            assert_eq!(approval.operation_id, id);
            assert_eq!(approval.kind, "recovery");
            approval
        };
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
        while stage != "completed" && db.get_state("writer_fence_v1:lease")?.is_some() {
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
            (5, operation.pins.git.prepared_sha.clone())
        );
        assert_eq!(db.get_state("last_svn_rev_child")?, Some("5".into()));
        assert_eq!(
            db.get_state(&db.scoped_last_git_sha_kv_key_for_repo("child")?)?,
            Some(operation.pins.git.prepared_sha)
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
    println!("REPOSYNC_REAL_ENGINE_RECEIPT_RESTART_POSITIVE_COUNT {positives}");
    println!("REPOSYNC_REAL_ENGINE_RECEIPT_RESTART_CONTROLS_VERIFIED");
    Ok(())
}
