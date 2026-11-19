//! R10. Resume a partially replayed merged PR from its confirmed Personal mappings.
use super::*;
use reposync_core::config::GitProvider;
use reposync_core::git::github::GitHubClient;
use reposync_core::history_inspect::resolve_personal_checkpoint;
use reposync_personal::engine::PersonalSyncEngine;
use reposync_personal::initial_import::{ImportMode, InitialImport};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
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
        .split(' ')
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
#[derive(Clone, Copy)]
enum ProviderView {
    Complete,
    IncompleteDag,
    PendingBeyondCap,
    PendingMissing,
    EmptyDag,
    ShortDag,
    ReorderedDag,
}

async fn pr_provider(
    git: &Path,
    prs: &[(&str, &str, &[&str])],
    view: Arc<Mutex<ProviderView>>,
    pages_served: Arc<AtomicUsize>,
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
            "merge_commit_sha":merge,"merged_at":"2025-01-01T00:00:00Z"
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
    pulls.reverse(); // Newest update first; all merge timestamps deliberately tie.
    routes.insert("pulls".to_owned(), json!(pulls).to_string());
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server_url = url.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let path = request_path(&mut stream).await;
            let route = path.strip_prefix("/repos/test/test-repo/").unwrap();
            let (route, query) = route.split_once('?').unwrap_or((route, ""));
            let mut body = routes
                .get(route)
                .expect("unexpected test-provider route")
                .clone();
            let mut continuation = None;
            let current = *view.lock().unwrap();
            match (current, route) {
                (ProviderView::IncompleteDag, "pulls/3/commits") => {
                    let commits: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
                    body = json!([commits[0]]).to_string();
                    continuation = Some(format!(
                        "{server_url}/repos/test/test-repo/pulls/3/commits?page=2"
                    ));
                }
                (ProviderView::EmptyDag, "pulls/3/commits") => body = "[]".into(),
                (ProviderView::ShortDag, "pulls/3/commits") => {
                    let commits: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
                    body = json!([commits[0]]).to_string();
                }
                (ProviderView::ReorderedDag, "pulls/3/commits") => {
                    let mut commits: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
                    commits.swap(0, 1);
                    body = json!(commits).to_string();
                }
                (ProviderView::PendingBeyondCap, "pulls") => {
                    let page: usize = query
                        .split('&')
                        .find_map(|pair| pair.strip_prefix("page="))
                        .unwrap_or("1")
                        .parse()
                        .unwrap();
                    pages_served.fetch_add(1, Ordering::SeqCst);
                    if page <= 10 {
                        let mut rows = Vec::new();
                        for i in 0..100 {
                            let mut item = pulls
                                .iter()
                                .find(|row| row["number"] == 99)
                                .unwrap()
                                .clone();
                            item["number"] = json!(1000 + 100 * page + i);
                            rows.push(item);
                        }
                        if page == 1 {
                            rows[0] = pulls.iter().find(|row| row["number"] == 4).unwrap().clone();
                        }
                        body = json!(rows).to_string();
                        continuation = Some(format!(
                            "{server_url}/repositories/123/pulls?page={}",
                            page + 1
                        ));
                    } else {
                        body = json!(pulls
                            .iter()
                            .filter(|row| matches!(row["number"].as_u64(), Some(1..=3)))
                            .collect::<Vec<_>>())
                        .to_string();
                    }
                }
                (ProviderView::PendingMissing, "pulls") => {
                    body =
                        json!([pulls.iter().find(|row| row["number"] == 4).unwrap()]).to_string();
                }
                _ => {}
            }
            let mut response = github_stub_http_response(&body);
            if let Some(next) = continuation {
                response = response.replacen(
                    "Content-Type:",
                    &format!("Link: <{next}>; rel=\"next\"\r\nContent-Type:"),
                    1,
                );
            }
            stream.write_all(response.as_bytes()).await.unwrap();
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
            request_path(&mut stream).await.split('?').next().unwrap(),
            "/repos/test/test-repo/pulls/7/commits"
        );
        let body = json!([github_commit("a".repeat(40), "first page")]).to_string();
        let reply = github_stub_http_response(&body).replacen(
            "Content-Type:",
            &format!("Link: <{path}>; rel=last\r\nLink: <{path}>; rel=next\r\nContent-Type:"),
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
    let (historic, historical_merge);
    {
        git2::Repository::init_bare(&bare).unwrap();
        let client = Arc::new(Mutex::new(GitClient::init(&git).unwrap()));
        git_cmd(&git, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_cmd(&git, &["remote", "add", "origin", bare.to_str().unwrap()]);
        add_file(&git, "anchor.txt", "original\n", "Original tree");
        git_cmd(&git, &["checkout", "-b", "historical"]);
        historic = add_file(&git, "anchor.txt", "stale\n", "Historical PR value");
        git_cmd(&git, &["checkout", "main"]);
        git_cmd(
            &git,
            &[
                "merge",
                "--no-ff",
                "historical",
                "-m",
                "Merge historical PR",
            ],
        );
        historical_merge = git_sha(&git);
        git_cmd(&git, &["push", "origin", "main"]);
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
    git_cmd(&git, &["checkout", "-b", "later"]);
    std::fs::write(git.join("earlier.txt"), "updated by later PR\n").unwrap();
    git_cmd(&git, &["add", "--", "earlier.txt"]);
    let later = add_file(&git, "later.txt", "later\n", "Later PR");
    git_cmd(&git, &["checkout", "main"]);
    git_cmd(&git, &["merge", "--no-ff", "later", "-m", "Merge later PR"]);
    let tip = git_sha(&git);
    git_cmd(&git, &["push", "origin", "main"]);
    for (child, parents) in [
        (&imported, vec![&historical_merge]),
        (&later, vec![&merge]),
        (&tip, vec![&merge, &later]),
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
    let view = Arc::new(Mutex::new(ProviderView::IncompleteDag));
    let pages = Arc::new(AtomicUsize::new(0));
    let (api, provider) = pr_provider(
        &git,
        &[
            ("historical", &historical_merge, &[&historic]),
            ("earlier", &base, &[&earlier]),
            ("feature", &merge, &[&left, &right, &resolution]),
            ("later", &tip, &[&later]),
        ],
        view.clone(),
        pages.clone(),
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

    let first = [
        (baseline + 1, earlier.as_str()),
        (baseline + 2, left.as_str()),
    ];
    {
        let engine = open();
        let earlier_only = engine.run_cycle().await.unwrap();
        assert_eq!(
            (earlier_only.prs_processed, earlier_only.git_to_svn_count),
            (1, 1),
            "an incomplete middle PR must allow earlier work and block later work"
        );
        assert_eq!(svn_youngest(&url), baseline + 1);
        let db = Database::new(&path).unwrap();
        assert_mappings(&db, "git_to_svn", &first[..1]);
        assert!(!db.is_personal_pr_synced(&historical_merge).unwrap());
        assert!(!db.is_personal_pr_synced(&tip).unwrap());
        let pr = db.list_pr_syncs(2).unwrap().remove(0);
        assert_eq!(
            (
                pr.pr_number,
                pr.status.as_str(),
                pr.commit_count,
                pr.svn_rev_start
            ),
            (3, "pending", 0, None)
        );
    }
    {
        let engine = open();
        let db = Database::new(&path).unwrap();
        *view.lock().unwrap() = ProviderView::PendingMissing;
        let absent = engine.run_cycle().await.unwrap();
        assert_eq!((absent.prs_processed, absent.git_to_svn_count), (0, 0));
        assert_eq!(svn_youngest(&url), baseline + 1);
        assert_mappings(&db, "git_to_svn", &first[..1]);
        *view.lock().unwrap() = ProviderView::ShortDag;
        let unproven = engine.run_cycle().await.unwrap();
        assert_eq!(
            (unproven.prs_processed, unproven.git_to_svn_count),
            (0, 0),
            "unadvertised first-page prefix must not complete a real merge"
        );
        assert_eq!(svn_youngest(&url), baseline + 1);
        assert_mappings(&db, "git_to_svn", &first[..1]);
        *view.lock().unwrap() = ProviderView::Complete;
        std::fs::create_dir(&obstructed).unwrap();
        let stats = engine.run_cycle().await.unwrap();
        assert_eq!(
            stats.prs_processed, 0,
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
    *view.lock().unwrap() = ProviderView::PendingBeyondCap;
    let truncated = engine.run_cycle().await.unwrap();
    assert_eq!(
        (truncated.git_to_svn_count, truncated.prs_processed),
        (0, 0)
    );
    assert_eq!(pages.load(Ordering::SeqCst), 10);
    assert_eq!(svn_youngest(&url), baseline + 2);
    assert_mappings(&db, "git_to_svn", &first);
    assert!(!db.is_personal_pr_synced(&tip).unwrap());
    *view.lock().unwrap() = ProviderView::PendingMissing;
    let missing = engine.run_cycle().await.unwrap();
    assert_eq!((missing.git_to_svn_count, missing.prs_processed), (0, 0));
    assert_eq!(svn_youngest(&url), baseline + 2);
    assert_mappings(&db, "git_to_svn", &first);
    for changed in [
        ProviderView::EmptyDag,
        ProviderView::ShortDag,
        ProviderView::ReorderedDag,
    ] {
        *view.lock().unwrap() = changed;
        let rejected = engine.run_cycle().await.unwrap();
        assert_eq!((rejected.prs_processed, rejected.git_to_svn_count), (0, 0));
        assert_eq!(svn_youngest(&url), baseline + 2);
        assert_mappings(&db, "git_to_svn", &first);
        assert!(!db.is_personal_pr_synced(&merge).unwrap());
        assert!(!db.is_personal_pr_synced(&tip).unwrap());
    }
    *view.lock().unwrap() = ProviderView::Complete;
    let stats = engine.run_cycle().await.unwrap();
    assert_eq!(
        (
            stats.svn_to_git_count,
            stats.prs_processed,
            stats.git_to_svn_count
        ),
        (0, 2, 3)
    );
    let expected = [
        first[0],
        first[1],
        (baseline + 3, right.as_str()),
        (baseline + 4, resolution.as_str()),
        (baseline + 5, later.as_str()),
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
    assert_eq!(svn_tree(&svn, baseline + 5).await, git_tree(&bare, &tip));
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
                Some((baseline + 5).to_string())
            );
        }
        assert_eq!(svn_youngest(&url), baseline + 5);
        assert_mappings(&db, "git_to_svn", &expected);
        assert_mappings(&db, "svn_to_git", &[(baseline, imported.as_str())]);
        assert_eq!(
            resolve_personal_checkpoint(&db, &git).unwrap().as_ref(),
            Some(&later)
        );
        assert_eq!(git_sha_at(&bare, "refs/heads/main"), tip);
        assert_eq!(git_sha(&git), tip);
        assert!(db.active_personal_svn_commit_operation().unwrap().is_none());
        let prs = db.list_pr_syncs(10).unwrap();
        assert_eq!(prs.len(), 3);
        for (pr, sha, start, end, count) in [
            (&prs[0], &tip, baseline + 5, baseline + 5, 1),
            (&prs[1], &merge, baseline + 2, baseline + 4, 3),
            (&prs[2], &base, baseline + 1, baseline + 1, 1),
        ] {
            assert_eq!(
                (
                    &pr.merge_sha,
                    pr.status.as_str(),
                    pr.svn_rev_start,
                    pr.svn_rev_end,
                    pr.commit_count
                ),
                (sha, "completed", Some(start), Some(end), count)
            );
        }
        let conn = db.conn();
        let mut stmt = conn
            .prepare("SELECT action,details FROM audit_log WHERE success=0 ORDER BY id")
            .unwrap();
        let errors = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(errors.len(), 9, "unexpected failed phases: {errors:?}");
        assert_eq!(errors[0].0, "git_to_svn_incomplete_pr");
        assert!(errors[0].1.contains("incomplete commit list for PR #3"));
        assert!(errors[1].1.contains("pending Personal PR #3 is missing"));
        assert_eq!(errors[2].0, "git_to_svn_pr_membership");
        assert_eq!(errors[3].0, "git_to_svn_error");
        assert!(errors[4].1.contains("incomplete pull-request discovery"));
        assert!(errors[5].1.contains("pending Personal PR #3 is missing"));
        assert!(errors[6..]
            .iter()
            .all(|(action, _)| action == "git_to_svn_pr_membership"));
    }
    let mut checked_trees = BTreeMap::new();
    for rev in baseline..=baseline + 5 {
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
        "mode":"personal","import":imported,"earlier_pr_merge":base,"pr_merge":merge,"tip":tip,"historical_merge":historical_merge,"discovery_pages":pages.load(Ordering::SeqCst),"earlier_before_incomplete":1,"first_unadvertised_prefix_refused":true,"missing_pending_zero_writes":true,"changed_lists_refused":3,"mappings":expected,"restart_after":first,
        "reopened_commits":stats.git_to_svn_count,"svn_trees":checked_trees,
        "pr_range":[baseline+2,baseline+4],"quiet_cycles":2})
    );
}
