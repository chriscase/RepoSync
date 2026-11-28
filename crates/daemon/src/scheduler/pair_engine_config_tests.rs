//! The configuration a pair's sync engine runs with.
use super::{memory_limit_bytes, pair_engine_config};
use reposync_core::config::AppConfig;
use reposync_core::models::Repository;

fn service_config(auto_merge: bool) -> AppConfig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!(
            "[daemon]\ndata_dir = \"/tmp/unused\"\n[svn]\nurl = \"file:///nonexistent\"\nusername = \"fixture\"\ntrunk_path = \"trunk\"\n[github]\nrepo = \"local/fixture\"\n[sync]\nauto_merge = {auto_merge}\n"
        ),
    )
    .unwrap();
    AppConfig::load_from_file(&path).unwrap()
}

fn pair(auto_merge: bool) -> Repository {
    serde_json::from_value(serde_json::json!({
        "id": "pair", "name": "pair", "svn_url": "file:///nonexistent", "svn_branch": "branches/x",
        "svn_username": "fixture", "git_provider": "gitea", "git_api_url": "", "git_repo": "local/pair",
        "git_branch": "release/x", "sync_mode": "direct", "poll_interval_secs": 60,
        "lfs_threshold_mb": 0, "auto_merge": auto_merge, "enabled": true, "created_by": null,
        "created_at": "", "updated_at": ""
    }))
    .unwrap()
}

#[test]
fn a_pair_with_auto_merge_off_holds_conflicts_for_review() {
    // The engine auto-merges only when its configuration says so, so a pair
    // that turned auto-merge off must not inherit the service's "on".
    assert!(
        !pair_engine_config(&service_config(true), &pair(false))
            .sync
            .auto_merge
    );
    assert!(
        pair_engine_config(&service_config(true), &pair(true))
            .sync
            .auto_merge
    );
    // The service setting is a master switch a pair cannot override.
    assert!(
        !pair_engine_config(&service_config(false), &pair(true))
            .sync
            .auto_merge
    );
    assert!(
        !pair_engine_config(&service_config(false), &pair(false))
            .sync
            .auto_merge
    );
}

#[test]
fn a_pair_engine_uses_the_pair_branch_and_a_baked_svn_path() {
    let config = pair_engine_config(&service_config(true), &pair(true));
    assert_eq!(config.github.default_branch, "release/x");
    assert_eq!(config.svn.trunk_path, "");
    assert_eq!(config.svn.layout, reposync_core::config::SvnLayout::Custom);
}

#[test]
fn an_oversized_memory_limit_saturates_instead_of_wrapping() {
    assert_eq!(memory_limit_bytes(0), 0);
    assert_eq!(memory_limit_bytes(512), 512 * 1024 * 1024);
    // 2^44 MB would wrap to 0 (limit silently disabled) if multiplied unchecked.
    assert_eq!(memory_limit_bytes(1 << 44), u64::MAX);
    assert_eq!(memory_limit_bytes(u64::MAX), u64::MAX);
}
