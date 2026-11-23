//! #60 SC12: the legacy single-repository path must keep its unrestricted behavior.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use reposync_core::config::{AppConfig, IdentityConfig, SvnLayout};
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;

fn output(command: &mut Command) -> Vec<u8> {
    let result = command.output().expect("start a native fixture command");
    assert!(
        result.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    output(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fixture Author")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture Author")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
    )
}

fn git_text(repo: &Path, args: &[&str]) -> String {
    String::from_utf8(git(repo, args)).unwrap().trim().into()
}

/// Independently verify both the complete path set and every byte of an SVN
/// revision as it appears in a published Git tree. SVN names are trunk-relative.
fn assert_same_tree(svn_repo: &Path, revision: &str, bare: &Path, git_ref: &str) {
    let svn = output(
        Command::new("svnlook")
            .args(["tree", "--full-paths", "-r", revision])
            .arg(svn_repo),
    );
    let mut svn_paths: Vec<_> = std::str::from_utf8(&svn)
        .unwrap()
        .lines()
        .filter(|path| !path.ends_with('/'))
        .map(|path| path.strip_prefix("trunk/").unwrap())
        .collect();
    svn_paths.sort_unstable();
    assert_eq!(svn_paths.len(), 5, "fixture must contain all five files");

    let git_tree = git_text(bare, &["ls-tree", "-r", "--name-only", git_ref]);
    let mut git_paths: Vec<_> = git_tree.lines().collect();
    git_paths.sort_unstable();
    assert_eq!(git_paths, svn_paths, "Git tree must be unrestricted");

    for path in git_paths {
        let expected = output(
            Command::new("svnlook")
                .args(["cat", "-r", revision])
                .arg(svn_repo)
                .arg(format!("trunk/{path}")),
        );
        let actual = git(bare, &["show", &format!("{git_ref}:{path}")]);
        assert_eq!(actual, expected, "SVN r{revision} versus Git {git_ref}:{path}");
    }
}

fn open_engine(root: &Path, bridge: &Path, svn_url: &str) -> SyncEngine {
    let mut config: AppConfig = toml::from_str(
        r#"
[daemon]
[svn]
url = "file:///unused"
username = ""
password_env = "REPOSYNC_FIXTURE_UNUSED"
[github]
repo = "fixture/legacy"
token_env = "REPOSYNC_FIXTURE_UNUSED"
"#,
    )
    .unwrap();
    config.daemon.data_dir = root.to_path_buf();
    config.svn.url = svn_url.into();
    config.svn.layout = SvnLayout::Custom;
    config.svn.password = Some(String::new());
    config.github.token = Some(String::new());

    let db = Database::new(&root.join("sync.db")).unwrap();
    db.initialize().unwrap();
    SyncEngine::new(
        config,
        db,
        SvnClient::new(svn_url, "", ""),
        GitClient::new(bridge).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    )
}

fn applied(db: &Database) -> Vec<(String, i64, String)> {
    let conn = db.conn();
    let mut query = conn
        .prepare(
            "SELECT direction, svn_rev, git_sha FROM sync_records
             WHERE repo_id IS NULL AND status = 'applied' ORDER BY svn_rev, direction",
        )
        .unwrap();
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_single_repo_replays_all_teams_and_remains_quiet_on_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/monorepo");
    let svn_repo = PathBuf::from(
        String::from_utf8(output(
            Command::new("python3")
                .arg("-B")
                .arg(fixture.join("seed.py"))
                .arg(root.join("monorepo")),
        ))
        .unwrap()
        .trim(),
    );
    let svn_root = reqwest::Url::from_file_path(&svn_repo).unwrap();
    let trunk =
        reposync_core::svn::operand::configured_branch_url(svn_root.as_str(), "trunk").unwrap();

    // Establish the pre-existing r1 Git baseline used by legacy single-repo
    // callers. This setup does not exercise the managed snapshot importer.
    let bridge = root.join("bridge");
    SvnClient::new(&trunk, "", "")
        .export("", 1, &bridge)
        .await
        .unwrap();
    let bare = root.join("origin.git");
    git2::Repository::init_bare(&bare).unwrap();
    git(&bridge, &["init", "-b", "main"]);
    git(&bridge, &["add", "--", "."]);
    git(&bridge, &["commit", "-m", "Existing SVN r1 baseline"]);
    git(&bridge, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&bridge, &["push", "origin", "main"]);
    let initial = git_text(&bare, &["rev-parse", "main"]);
    assert_same_tree(&svn_repo, "1", &bare, "main");

    let engine = open_engine(root, &bridge, &trunk);
    engine.db().set_state("last_svn_rev", "1").unwrap();
    engine.db().set_state("last_git_hash", &initial).unwrap();
    let inbound = engine.run_sync_cycle().await.unwrap();
    assert_eq!((inbound.svn_to_git_count, inbound.git_to_svn_count), (2, 0));
    let r2 = git_text(&bare, &["rev-parse", "main^"]);
    let r3 = git_text(&bare, &["rev-parse", "main"]);
    assert_eq!(git_text(&bare, &["rev-parse", "main^^"]), initial);
    assert_same_tree(&svn_repo, "2", &bare, &r2);
    assert_same_tree(&svn_repo, "3", &bare, &r3);
    let youngest = || {
        String::from_utf8(output(Command::new("svnlook").arg("youngest").arg(&svn_repo)))
            .unwrap()
    };
    assert_eq!(youngest().trim(), "3");

    let developer = root.join("developer");
    git(
        root,
        &[
            "clone",
            "-b",
            "main",
            bare.to_str().unwrap(),
            developer.to_str().unwrap(),
        ],
    );
    std::fs::write(developer.join("docs/index.txt"), b"edited shared docs in Git\n").unwrap();
    std::fs::write(developer.join("team-c/readme.txt"), b"edited team c in Git\n").unwrap();
    git(&developer, &["add", "--", "docs/index.txt", "team-c/readme.txt"]);
    git(&developer, &["commit", "-m", "Edit docs and team-c on Git"]);
    git(&developer, &["push", "origin", "main"]);
    let git_tip = git_text(&bare, &["rev-parse", "main"]);

    let outbound = engine.run_sync_cycle().await.unwrap();
    assert_eq!((outbound.svn_to_git_count, outbound.git_to_svn_count), (0, 1));
    assert_eq!(youngest().trim(), "4");
    assert_same_tree(&svn_repo, "4", &bare, "main");
    let records = vec![
        ("svn_to_git".into(), 2, r2),
        ("svn_to_git".into(), 3, r3),
        ("git_to_svn".into(), 4, git_tip.clone()),
    ];
    assert_eq!(applied(engine.db()), records);
    drop(engine);

    let reopened = open_engine(root, &bridge, &trunk);
    for _ in 0..2 {
        let quiet = reopened.run_sync_cycle().await.unwrap();
        assert_eq!((quiet.svn_to_git_count, quiet.git_to_svn_count), (0, 0));
        assert_eq!(youngest().trim(), "4");
        assert_eq!(git_text(&bare, &["rev-parse", "main"]), git_tip);
        assert_eq!(applied(reopened.db()), records);
    }
}
