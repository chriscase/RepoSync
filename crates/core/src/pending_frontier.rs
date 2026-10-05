//! Deterministic pending-commit selection via ancestry hide/push.
//!
//! The pending set from handled checkpoint `P` to tip `R` is commits reachable
//! from `R` and not from `P` (`P..R`). A revwalk that stops when `P` is first
//! visited can omit older pending commits on other merge parents. Unqualified
//! merge DAGs fail closed. Linear backlogs over the reviewed cap are admitted
//! at inspect time and replayed in explicit oldest-first batches of at most
//! [`DEFAULT_PENDING_COMMIT_CAP`] commits per cycle.

use git2::{Oid, Repository, Sort};

use crate::errors::GitError;

/// Reviewed pending-commit batch size for replay continuation.
pub const DEFAULT_PENDING_COMMIT_CAP: usize = 1000;

/// Replay batch cap, overridable in debug builds for fixture tests.
pub fn effective_pending_commit_cap() -> usize {
    #[cfg(debug_assertions)]
    if let Some(cap) = test_pending_commit_cap_override() {
        return cap;
    }
    DEFAULT_PENDING_COMMIT_CAP
}

#[cfg(debug_assertions)]
std::thread_local! {
    static TEST_PENDING_COMMIT_CAP: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// Install a per-thread replay cap for fixture tests.
#[cfg(debug_assertions)]
pub fn set_test_pending_commit_cap(cap: Option<usize>) {
    TEST_PENDING_COMMIT_CAP.set(cap);
}

#[cfg(debug_assertions)]
fn test_pending_commit_cap_override() -> Option<usize> {
    TEST_PENDING_COMMIT_CAP.get()
}

pub const REASON_MERGE_DAG: &str = "unsupported_merge_dag";
pub const REASON_BACKLOG: &str = "unsupported_backlog";
pub const REASON_UNPROVEN_RANGE: &str = "unproven_pending_range";

pub const DETAIL_MERGE_DAG: &str = "pending Git history contains a merge commit";
pub const DETAIL_BACKLOG: &str =
    "more than 1000 pending Git commits require a reviewed continuation algorithm";

/// Result of a capped pending-commit batch selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBatch {
    /// Oldest-first pending commits, at most `cap` entries.
    pub commits: Vec<Oid>,
    /// Total pending commits on the P→R frontier (may exceed `commits.len()`).
    pub total: usize,
    /// True when `total > commits.len()` and another batch is required.
    pub has_more: bool,
}

fn unsupported(reason: &str, detail: impl Into<String>) -> GitError {
    GitError::UnsupportedHistory {
        reason: reason.to_string(),
        detail: detail.into(),
    }
}

fn parse_commit(repo: &Repository, sha: &str) -> Result<Oid, GitError> {
    let oid = Oid::from_str(sha).map_err(GitError::Git2Error)?;
    repo.find_commit(oid).map_err(GitError::from)?;
    Ok(oid)
}

fn ensure_descendant(
    repo: &Repository,
    since: Oid,
    tip: Oid,
    since_sha: &str,
    tip_sha: &str,
) -> Result<(), GitError> {
    if since == tip {
        return Ok(());
    }
    match repo.graph_descendant_of(tip, since) {
        Ok(true) => Ok(()),
        Ok(false) => Err(unsupported(
            REASON_UNPROVEN_RANGE,
            format!("{since_sha} is not an ancestor of {tip_sha}"),
        )),
        Err(error) => Err(unsupported(
            REASON_UNPROVEN_RANGE,
            format!("pending range ancestry could not be established: {error}"),
        )),
    }
}

/// Walk the full P→R frontier and verify it is a linear pending range.
///
/// Merge commits fail closed. The total pending count may exceed the replay cap;
/// inspect uses this to admit qualified linear histories before batch replay.
pub fn verify_linear_pending_range(
    repo: &Repository,
    since_sha: &str,
    tip_sha: &str,
) -> Result<usize, GitError> {
    let since = parse_commit(repo, since_sha)?;
    let tip = parse_commit(repo, tip_sha)?;
    ensure_descendant(repo, since, tip, since_sha, tip_sha)?;
    if since == tip {
        return Ok(0);
    }

    let mut revwalk = repo.revwalk().map_err(GitError::from)?;
    revwalk
        .set_sorting(Sort::TOPOLOGICAL | Sort::TIME)
        .map_err(GitError::from)?;
    revwalk.push(tip).map_err(GitError::from)?;
    revwalk.hide(since).map_err(GitError::from)?;

    let mut total = 0usize;
    let mut merge_sha = None;
    for oid in revwalk {
        let oid = oid.map_err(GitError::from)?;
        let commit = repo.find_commit(oid).map_err(GitError::from)?;
        if commit.parent_count() >= 2 && merge_sha.is_none() {
            merge_sha = Some(oid);
        }
        total += 1;
    }
    if let Some(merge) = merge_sha {
        return Err(unsupported(
            REASON_MERGE_DAG,
            format!("{DETAIL_MERGE_DAG}: {merge}"),
        ));
    }
    Ok(total)
}

/// Select the oldest-first pending batch from P→R, at most `cap` commits.
///
/// `since_sha` is hidden (the commit and its ancestors), not used as a
/// visited-order stop. Merge commits fail closed. When the frontier exceeds
/// `cap`, the first batch is returned with `has_more = true` instead of
/// silently truncating without signaling continuation.
pub fn select_pending_batch(
    repo: &Repository,
    since_sha: &str,
    tip_sha: &str,
    cap: usize,
) -> Result<PendingBatch, GitError> {
    let since = parse_commit(repo, since_sha)?;
    let tip = parse_commit(repo, tip_sha)?;
    ensure_descendant(repo, since, tip, since_sha, tip_sha)?;
    if since == tip {
        return Ok(PendingBatch {
            commits: Vec::new(),
            total: 0,
            has_more: false,
        });
    }

    let mut revwalk = repo.revwalk().map_err(GitError::from)?;
    revwalk
        .set_sorting(Sort::TOPOLOGICAL | Sort::TIME)
        .map_err(GitError::from)?;
    revwalk.push(tip).map_err(GitError::from)?;
    revwalk.hide(since).map_err(GitError::from)?;

    let mut frontier = Vec::new();
    let mut merge_sha = None;
    for oid in revwalk {
        let oid = oid.map_err(GitError::from)?;
        let commit = repo.find_commit(oid).map_err(GitError::from)?;
        if commit.parent_count() >= 2 && merge_sha.is_none() {
            merge_sha = Some(oid);
        }
        frontier.push(oid);
    }
    if let Some(merge) = merge_sha {
        return Err(unsupported(
            REASON_MERGE_DAG,
            format!("{DETAIL_MERGE_DAG}: {merge}"),
        ));
    }
    let total = frontier.len();
    frontier.reverse();
    let batch_len = total.min(cap);
    let commits = frontier[..batch_len].to_vec();
    Ok(PendingBatch {
        commits,
        total,
        has_more: total > cap,
    })
}

/// Commits reachable from `tip_sha` and not from `since_sha`, oldest first.
///
/// `since_sha` is hidden (the commit and its ancestors), not used as a
/// visited-order stop. Merge commits fail closed. Overflow beyond `cap`
/// fails closed for callers that require a complete pending set in one pass.
pub fn select_pending_oids(
    repo: &Repository,
    since_sha: &str,
    tip_sha: &str,
    cap: usize,
) -> Result<Vec<Oid>, GitError> {
    let batch = select_pending_batch(repo, since_sha, tip_sha, cap)?;
    if batch.has_more {
        return Err(unsupported(REASON_BACKLOG, DETAIL_BACKLOG));
    }
    Ok(batch.commits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Commit, Signature, Time};
    use std::path::Path;
    use tempfile::tempdir;

    struct Dag {
        repo: Repository,
        p: Oid,
        z: Oid,
        b: Oid,
        y: Oid,
        m: Oid,
        _dir: tempfile::TempDir,
    }

    struct Linear {
        repo: Repository,
        oids: Vec<Oid>,
        _dir: tempfile::TempDir,
    }

    fn sig(seconds: i64) -> Signature<'static> {
        Signature::new("T", "t@example.invalid", &Time::new(seconds, 0)).unwrap()
    }

    fn commit<'a>(
        repo: &'a Repository,
        message: &str,
        parents: &[&Commit<'a>],
        seconds: i64,
        filename: &str,
        content: &str,
    ) -> Oid {
        let workdir = repo.workdir().unwrap();
        std::fs::write(workdir.join(filename), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(filename)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = sig(seconds);
        repo.commit(None, &signature, &signature, message, &tree, parents)
            .unwrap()
    }

    /// X→A(P)→B and X→Z→Y merged at M. Z is older than A so a TIME+TOPO
    /// walk from M that stops when A is visited can skip Z.
    fn older_side_merge_dag() -> Dag {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let (p, z, b, y, m) = {
            let x = commit(&repo, "X", &[], 1, "root.txt", "x\n");
            let x_commit = repo.find_commit(x).unwrap();
            let p = commit(&repo, "A checkpoint", &[&x_commit], 10, "a.txt", "a\n");
            let z = commit(&repo, "Z old side", &[&x_commit], 2, "z.txt", "z\n");
            drop(x_commit);
            let p_commit = repo.find_commit(p).unwrap();
            let z_commit = repo.find_commit(z).unwrap();
            let b = commit(&repo, "B", &[&p_commit], 11, "b.txt", "b\n");
            let y = commit(&repo, "Y", &[&z_commit], 3, "y.txt", "y\n");
            drop(p_commit);
            drop(z_commit);
            let b_commit = repo.find_commit(b).unwrap();
            let y_commit = repo.find_commit(y).unwrap();
            let m = commit(
                &repo,
                "M merge",
                &[&b_commit, &y_commit],
                12,
                "m.txt",
                "m\n",
            );
            (p, z, b, y, m)
        };
        Dag {
            repo,
            p,
            z,
            b,
            y,
            m,
            _dir: dir,
        }
    }

    fn walk_until_visited(repo: &Repository, since: Oid, tip: Oid) -> Vec<Oid> {
        let mut revwalk = repo.revwalk().unwrap();
        revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).unwrap();
        revwalk.push(tip).unwrap();
        let mut commits = Vec::new();
        for oid in revwalk {
            let oid = oid.unwrap();
            if oid == since {
                break;
            }
            commits.push(oid);
        }
        commits
    }

    fn linear_repo(count: usize) -> Linear {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let mut oids = Vec::new();
        for index in 0..count {
            let parents: Vec<Commit<'_>> = oids
                .last()
                .copied()
                .map(|oid| repo.find_commit(oid).unwrap())
                .into_iter()
                .collect();
            let parent_refs: Vec<&Commit<'_>> = parents.iter().collect();
            let oid = commit(
                &repo,
                &format!("c{index}"),
                &parent_refs,
                (index as i64) + 1,
                "f.txt",
                &format!("{index}\n"),
            );
            oids.push(oid);
        }
        Linear {
            repo,
            oids,
            _dir: dir,
        }
    }

    #[test]
    fn visited_order_truncation_skips_older_pending_side() {
        let dag = older_side_merge_dag();
        let visited = walk_until_visited(&dag.repo, dag.p, dag.m);
        assert!(
            visited.contains(&dag.m),
            "walk starts at the merge tip: {visited:?}"
        );
        assert!(
            !visited.contains(&dag.z),
            "visited-order stop at P skipped older pending {z}; got {visited:?}",
            z = dag.z
        );
        assert!(
            !visited.contains(&dag.y),
            "TIME+TOPO visit of newer parent B reaches P before older side Y; got {visited:?}"
        );
    }

    #[test]
    fn hide_push_frontier_includes_older_pending_side() {
        let dag = older_side_merge_dag();
        let error = select_pending_oids(
            &dag.repo,
            &dag.p.to_string(),
            &dag.m.to_string(),
            DEFAULT_PENDING_COMMIT_CAP,
        )
        .unwrap_err();
        match error {
            GitError::UnsupportedHistory { reason, detail } => {
                assert_eq!(reason, REASON_MERGE_DAG);
                assert!(detail.contains(&dag.m.to_string()), "{detail}");
            }
            other => panic!("expected merge-dag reject, got {other:?}"),
        }
        // The hide/push set (without the merge fail-closed) must include Z.
        let mut revwalk = dag.repo.revwalk().unwrap();
        revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).unwrap();
        revwalk.push(dag.m).unwrap();
        revwalk.hide(dag.p).unwrap();
        let mut frontier = Vec::new();
        for oid in revwalk {
            frontier.push(oid.unwrap());
        }
        assert!(frontier.contains(&dag.z), "frontier missing older side Z");
        assert!(frontier.contains(&dag.y));
        assert!(frontier.contains(&dag.b));
        assert!(frontier.contains(&dag.m));
        assert!(!frontier.contains(&dag.p));
    }

    #[test]
    fn linear_pending_is_oldest_first_and_excludes_checkpoint() {
        let linear = linear_repo(4);
        let pending = select_pending_oids(
            &linear.repo,
            &linear.oids[0].to_string(),
            &linear.oids[3].to_string(),
            DEFAULT_PENDING_COMMIT_CAP,
        )
        .unwrap();
        assert_eq!(
            pending,
            vec![linear.oids[1], linear.oids[2], linear.oids[3]]
        );
    }

    #[test]
    fn equal_tips_yield_empty_pending() {
        let linear = linear_repo(2);
        let pending = select_pending_oids(
            &linear.repo,
            &linear.oids[1].to_string(),
            &linear.oids[1].to_string(),
            DEFAULT_PENDING_COMMIT_CAP,
        )
        .unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn overflow_fails_closed_instead_of_truncating() {
        let linear = linear_repo(4);
        let error = select_pending_oids(
            &linear.repo,
            &linear.oids[0].to_string(),
            &linear.oids[3].to_string(),
            2,
        )
        .unwrap_err();
        match error {
            GitError::UnsupportedHistory { reason, .. } => {
                assert_eq!(reason, REASON_BACKLOG);
            }
            other => panic!("expected backlog reject, got {other:?}"),
        }
    }

    #[test]
    fn batch_selection_signals_continuation_instead_of_silent_truncation() {
        let linear = linear_repo(4);
        let batch = select_pending_batch(
            &linear.repo,
            &linear.oids[0].to_string(),
            &linear.oids[3].to_string(),
            2,
        )
        .unwrap();
        assert_eq!(batch.total, 3);
        assert!(batch.has_more);
        assert_eq!(batch.commits, vec![linear.oids[1], linear.oids[2]]);
    }

    #[test]
    fn verify_linear_pending_range_counts_full_frontier() {
        let linear = linear_repo(4);
        let total = verify_linear_pending_range(
            &linear.repo,
            &linear.oids[0].to_string(),
            &linear.oids[3].to_string(),
        )
        .unwrap();
        assert_eq!(total, 3);
    }

    #[test]
    fn non_ancestor_tip_fails_closed() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "A", &[], 1, "a.txt", "a\n");
        let a_commit = repo.find_commit(a).unwrap();
        let b = commit(&repo, "B", &[&a_commit], 2, "b.txt", "b\n");
        // Unrelated second root.
        let other = commit(&repo, "other", &[], 3, "c.txt", "c\n");
        let error = select_pending_oids(&repo, &b.to_string(), &other.to_string(), 10).unwrap_err();
        match error {
            GitError::UnsupportedHistory { reason, .. } => {
                assert_eq!(reason, REASON_UNPROVEN_RANGE);
            }
            other => panic!("expected unproven range, got {other:?}"),
        }
    }
}
