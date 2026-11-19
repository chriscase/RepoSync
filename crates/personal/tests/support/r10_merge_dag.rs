//! R10. Resume a partially replayed merged PR from its confirmed Personal mappings.
use super::*;
use reposync_core::config::GitProvider;
use reposync_core::git::github::GitHubClient;
use reposync_core::history_inspect::resolve_personal_checkpoint;
use reposync_personal::engine::PersonalSyncEngine;
use reposync_personal::initial_import::{ImportMode, InitialImport};
use serde_json::json;
use std::collections::BTreeMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

type Tree = BTreeMap<String, Vec<u8>>;

fn git_tree(bare: &Path, sha: &str) -> Tree {
    let repo = git2::Repository::open_bare(bare).unwrap();
    let tree = repo
        .find_commit(sha.parse().unwrap())
        .unwrap()
        .tree()
        .unwrap();
    tree.iter()
        .map(|entry| {
            let blob = repo
                .find_blob(entry.id())
                .expect("flat fixture contains only blobs");
            (entry.name().unwrap().to_owned(), blob.content().to_vec())
        })
        .collect()
}

async fn svn_tree(svn: &SvnClient, rev: i64) -> Tree {
    let export = TempDir::new().unwrap();
    svn.export("", rev, export.path()).await.unwrap();
    std::fs::read_dir(export.path())
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

fn add_file(git: &Path, name: &str, data: &str, subject: &str) -> String {
    std::fs::write(git.join(name), data).unwrap();
    git_cmd(git, &["add", "--", name]);
    git_cmd(git, &["commit", "-m", subject]);
    git_sha(git)
}

// Read the request target without retaining any headers.
async fn request_path(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let path = line
        .strip_prefix("GET ")
        .unwrap()
        .split(['?', ' '])
        .next()
        .unwrap()
        .to_owned();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
            return path;
        }
    }
}

// Only the PR metadata is synthetic. The SHAs, parents, subjects and blobs come
// from actual Git objects; the same closed-list response is used across restart.
async fn pr_provider(
    git: &Path,
    prs: &[(&str, &str, &[&str])],
) -> (String, tokio::task::JoinHandle<()>) {
    let repo = git2::Repository::open(git).unwrap();
    let metadata = |sha: &str| {
        let c = repo.find_commit(sha.parse().unwrap()).unwrap();
        serde_json::to_value(github_commit(sha.to_owned(), c.message().unwrap())).unwrap()
    };
    let mut routes = BTreeMap::new();
    let mut pulls = Vec::new();
    for (index, (branch, merge, commits)) in prs.iter().enumerate() {
        let number = index + 1;
        let commit = repo.find_commit(merge.parse().unwrap()).unwrap();
        let parents: Vec<_> = commit.parent_ids().map(|id| id.to_string()).collect();
        let mut detail = metadata(merge);
        detail["parents"] = json!(parents
            .iter()
            .map(|sha| json!({"sha":sha}))
            .collect::<Vec<_>>());
        pulls.push(json!({
            "number":number, "title":branch, "html_url":format!("http://127.0.0.1/pull/{number}"),
            "state":"closed", "head":{"ref":branch,"sha":parents[1]}, "base":{"ref":"main","sha":parents[0]},
            "merge_commit_sha":merge,"merged_at":format!("2025-01-{number:02}T00:00:00Z")
        }));
        routes.insert(format!("commits/{merge}"), detail.to_string());
        routes.insert(
            format!("pulls/{number}/commits"),
            json!(commits.iter().map(|sha| metadata(sha)).collect::<Vec<_>>()).to_string(),
        );
    }
    // A closed, unmerged PR can have a test-merge SHA. It has no commit-list route.
    let mut unmerged = pulls[0].clone();
    unmerged["number"] = json!(99);
    unmerged["merged_at"] = serde_json::Value::Null;
    let mut denied = unmerged.clone();
    denied["number"] = json!(98);
    denied["merged"] = json!(false);
    denied["merged_at"] = json!("2025-01-01T00:00:00Z");
    pulls.insert(0, denied);
    pulls.insert(0, unmerged);
    routes.insert("pulls".to_owned(), json!(pulls).to_string());
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let path = request_path(&mut stream).await;
            let route = path.strip_prefix("/repos/test/test-repo/").unwrap();
            let body = routes.get(route).expect("unexpected test-provider route");
            stream
                .write_all(github_stub_http_response(body).as_bytes())
                .await
                .unwrap();
        }
    });
    (url, task)
}

// Never offer a partial PR to the replay loop as though the last SHA were final.
#[tokio::test]
async fn incomplete_pr_commit_page_is_refused() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let path = format!("{url}/repos/test/test-repo/pulls/7/commits?page=2");
    let provider = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert_eq!(
            request_path(&mut stream).await,
            "/repos/test/test-repo/pulls/7/commits"
        );
        let body = json!([github_commit("a".repeat(40), "first page")]).to_string();
        let reply = github_stub_http_response(&body).replacen(
            "Content-Type:",
            &format!("Link: <{path}>; rel=\"next\"\r\nContent-Type:"),
            1,
        );
        stream.write_all(reply.as_bytes()).await.unwrap();
    });
    let api = GitHubClient::new(url, "synthetic", GitProvider::GitHub);
    let result = api.get_pr_commits("test/test-repo", 7).await;
    provider.await.unwrap();
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("incomplete commit list for PR #7"));
}

fn assert_mappings(db: &Database, direction: &str, expected: &[(i64, &str)]) {
    let conn = db.conn();
    for table in ["commit_map", "sync_records"] {
        let sql = format!(
            "SELECT svn_rev,git_sha,repo_id {} FROM {table}
            WHERE direction=?1 ORDER BY svn_rev,git_sha",
            if table == "sync_records" {
                ",status"
            } else {
                ""
            }
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let actual: Vec<(i64, String)> = stmt
            .query_map([direction], |row| {
                assert_eq!(row.get::<_, String>(2)?, PERSONAL_SCOPE_KEY);
                if table == "sync_records" {
                    assert_eq!(row.get::<_, String>(3)?, "applied");
                }
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            actual,
            expected
                .iter()
                .map(|(r, s)| (*r, s.to_string()))
                .collect::<Vec<_>>(),
            "{table}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r10_personal_merge_dag_replays_across_restart() {
    assert!(
        svn_available(),
        "this qualification requires local SVN tooling"
    );
    let root = TempDir::new().unwrap();
    let url = create_svn_repo(root.path());
    let svn = SvnClient::new(&url, "", "");
    let wc = root.path().join("svn-wc");
    svn_checkout(&url, &wc);
    let baseline = svn_commit_file(&wc, "anchor.txt", "base\n", "SVN baseline");
    let git = root.path().join("git");
    let bare = root.path().join("origin.git");
    let path = root.path().join("personal.db");
    let mut config = make_test_config(&url, root.path());
    let github = |api: &str| GitHubClient::new(api, "synthetic", GitProvider::GitHub);
    {
        git2::Repository::init_bare(&bare).unwrap();
        let client = Arc::new(Mutex::new(GitClient::init(&git).unwrap()));
        git_cmd(&git, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_cmd(&git, &["remote", "add", "origin", bare.to_str().unwrap()]);
        let (api, provider) = spawn_github_exists_stub();
        config.github.api_url = api.clone();
        let db = setup_db(&path);
        assert_eq!(
            InitialImport {
                svn_client: &svn,
                git_client: &client,
                github_client: &github(&api),
                db: &db,
                config: &config,
                formatter: &CommitFormatter::new(&config.commit_format),
            }
            .import(ImportMode::Snapshot)
            .await
            .unwrap(),
            1
        );
        provider.join().unwrap();
    }
    let imported = git_sha(&git);
    git_cmd(&git, &["checkout", "-b", "earlier"]);
    let earlier = add_file(&git, "earlier.txt", "earlier\n", "Earlier PR");
    git_cmd(&git, &["checkout", "main"]);
    git_cmd(
        &git,
        &["merge", "--no-ff", "earlier", "-m", "Merge earlier PR"],
    );
    let base = git_sha(&git);
    git_cmd(&git, &["checkout", "-b", "feature"]);
    let left = add_file(&git, "left.txt", "left\n", "Left change");
    git_cmd(&git, &["checkout", "-b", "side", &base]);
    let right = add_file(&git, "right.txt", "right\n", "Right change");
    git_cmd(&git, &["checkout", "feature"]);
    git_cmd(&git, &["merge", "--no-ff", "--no-commit", "side"]);
    let resolution = add_file(
        &git,
        "resolution.txt",
        "resolved\n",
        "Resolve internal merge",
    );
    git_cmd(&git, &["checkout", "main"]);
    git_cmd(&git, &["merge", "--no-ff", "feature", "-m", "Merge PR"]);
    let merge = git_sha(&git);
    git_cmd(&git, &["push", "origin", "main"]);
    for (child, parents) in [
        (&earlier, vec![&imported]),
        (&base, vec![&imported, &earlier]),
        (&left, vec![&base]),
        (&right, vec![&base]),
        (&resolution, vec![&left, &right]),
        (&merge, vec![&base, &resolution]),
    ] {
        let repo = git2::Repository::open_bare(&bare).unwrap();
        let commit = repo.find_commit(child.parse().unwrap()).unwrap();
        assert_eq!(
            commit
                .parent_ids()
                .map(|id| id.to_string())
                .collect::<Vec<_>>(),
            parents.into_iter().map(String::as_str).collect::<Vec<_>>()
        );
    }
    assert_eq!(svn_tree(&svn, baseline).await, git_tree(&bare, &imported));
    let (api, provider) = pr_provider(
        &git,
        &[
            ("earlier", &base, &[&earlier]),
            ("feature", &merge, &[&left, &right, &resolution]),
        ],
    )
    .await;
    config.github.api_url = api.clone();
    let open = || {
        PersonalSyncEngine::new(
            config.clone(),
            Database::new(&path).unwrap(),
            svn.clone(),
            GitClient::new(&git).unwrap(),
            github(&api),
        )
    };
    let obstructed = wc.join("right.txt");
    std::fs::create_dir(&obstructed).unwrap();
    let first = [
        (baseline + 1, earlier.as_str()),
        (baseline + 2, left.as_str()),
    ];
    {
        let engine = open();
        let stats = engine.run_cycle().await.unwrap();
        assert_eq!(
            stats.prs_processed, 1,
            "the interrupted PR cannot be complete"
        );
        assert_eq!(
            svn_youngest(&url),
            baseline + 2,
            "left must be durable before right fails"
        );
        let db = Database::new(&path).unwrap();
        assert_mappings(&db, "git_to_svn", &first);
        assert_eq!(
            resolve_personal_checkpoint(&db, &git).unwrap().as_ref(),
            Some(&left)
        );
        assert_eq!(
            svn_tree(&svn, baseline + 1).await,
            git_tree(&bare, &earlier)
        );
        assert!(db.is_personal_pr_synced(&base).unwrap());
        let pending = db
            .list_pr_syncs(2)
            .unwrap()
            .into_iter()
            .find(|pr| pr.merge_sha == merge)
            .unwrap();
        assert_eq!(
            (
                pending.status.as_str(),
                pending.svn_rev_start,
                pending.svn_rev_end
            ),
            ("pending", Some(baseline + 2), Some(baseline + 2))
        );
        assert!(!db.is_personal_pr_synced(&merge).unwrap());
        let failed: String = db
            .conn()
            .query_row(
                "SELECT git_sha FROM audit_log WHERE action='git_to_svn_error'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failed, right);
        assert_eq!(svn_tree(&svn, baseline + 2).await, git_tree(&bare, &left));
    } // Drop the engine and independent SQLite observer before reopening.
    std::fs::remove_dir(&obstructed).unwrap();
    let engine = open();
    let db = Database::new(&path).unwrap();
    assert_mappings(&db, "git_to_svn", &first);
    assert_eq!(
        resolve_personal_checkpoint(&db, &git).unwrap().as_ref(),
        Some(&left)
    );
    let stats = engine.run_cycle().await.unwrap();
    assert_eq!(
        (
            stats.svn_to_git_count,
            stats.prs_processed,
            stats.git_to_svn_count
        ),
        (0, 1, 2)
    );
    let expected = [
        first[0],
        first[1],
        (baseline + 3, right.as_str()),
        (baseline + 4, resolution.as_str()),
    ];
    let mut joined = git_tree(&bare, &left);
    joined.insert(
        "right.txt".into(),
        git_tree(&bare, &right).remove("right.txt").unwrap(),
    );
    assert_eq!(svn_tree(&svn, baseline + 3).await, joined);
    assert_eq!(
        svn_tree(&svn, baseline + 4).await,
        git_tree(&bare, &resolution)
    );
    assert_eq!(git_tree(&bare, &resolution), git_tree(&bare, &merge));
    for cycle in 0..=2 {
        if cycle > 0 {
            let quiet = engine.run_cycle().await.unwrap();
            assert_eq!(
                (
                    quiet.svn_to_git_count,
                    quiet.prs_processed,
                    quiet.git_to_svn_count
                ),
                (0, 0, 0)
            );
            assert_eq!(
                db.get_watermark("svn_rev").unwrap(),
                Some((baseline + 4).to_string())
            );
        }
        assert_eq!(svn_youngest(&url), baseline + 4);
        assert_mappings(&db, "git_to_svn", &expected);
        assert_mappings(&db, "svn_to_git", &[(baseline, imported.as_str())]);
        assert_eq!(
            resolve_personal_checkpoint(&db, &git).unwrap().as_ref(),
            Some(&resolution)
        );
        assert_eq!(git_sha_at(&bare, "refs/heads/main"), merge);
        assert_eq!(git_sha(&git), merge);
        assert!(db.active_personal_svn_commit_operation().unwrap().is_none());
        let prs = db.list_pr_syncs(10).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(
            (&prs[1].merge_sha, prs[1].svn_rev_start, prs[1].svn_rev_end),
            (&base, Some(baseline + 1), Some(baseline + 1))
        );
        assert_eq!(
            (prs[0].svn_rev_start, prs[0].svn_rev_end),
            (Some(baseline + 2), Some(baseline + 4)),
            "completed PR range must include the revision created before restart"
        );
        assert_eq!(
            (
                &prs[0].merge_sha,
                prs[0].status.as_str(),
                prs[0].commit_count
            ),
            (&merge, "completed", 3)
        );
        assert_eq!(
            db.conn()
                .query_row(
                    "SELECT count(*) FROM audit_log WHERE success=0",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
    let mut checked_trees = BTreeMap::new();
    for rev in baseline..=baseline + 4 {
        let files: BTreeMap<_, _> = svn_tree(&svn, rev)
            .await
            .into_iter()
            .map(|(name, bytes)| (name, String::from_utf8(bytes).unwrap()))
            .collect();
        checked_trees.insert(rev, files);
    }
    provider.abort();
    assert!(provider.await.unwrap_err().is_cancelled());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        json!({"case":"R10_PERSONAL_MERGE_DAG_RESTART","status":"PASS",
        "mode":"personal","import":imported,"earlier_pr_merge":base,"pr_merge":merge,"mappings":expected,"restart_after":first,
        "reopened_commits":stats.git_to_svn_count,"svn_trees":checked_trees,
        "pr_range":[baseline+2,baseline+4],"quiet_cycles":2})
    );
}
