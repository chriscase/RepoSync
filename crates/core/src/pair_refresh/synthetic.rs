//! Fixture-only pair refresh. No SyncEngine, network, or live Git or SVN process.
//!
//! Default refresh appends an integration and leaves published commits and
//! revisions in place. Rewrite containment refuses a rebased mapped commit.
//! Re-anchor opens a new generation, keeps the old ref and path, and parks
//! displaced work. Recovery is journaled before each side effect.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use sha2::{Digest, Sha256};

const POLICY: &str = "pair_refresh_synthetic_v1";
const UUID: &str = "synthetic-svn-uuid";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Node {
    parents: Vec<String>,
    tree: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rev {
    path: String,
    tree: BTreeMap<String, String>,
    mergeinfo: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Generation {
    id: u64,
    git_branch: String,
    svn_path: String,
    git_tip: String,
    svn_revision: i64,
    parent_baseline_svn: i64,
    baseline_sha: String,
    retained_ref: String,
    mapping: BTreeMap<String, i64>,
    retired: bool,
    displaced: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Job {
    request_id: String,
    digest: String,
    operation: String,
    phase: String,
    same_name: bool,
    retire: bool,
    parent_tip: String,
    result_tree: BTreeMap<String, String>,
    svn_before: i64,
    git_before: String,
    new_svn: Option<i64>,
    new_git: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntheticPlan {
    pub digest: String,
    pub operation: String,
    pub pair_git_tip: String,
    pub parent_git_tip: String,
    pub svn_uuid: String,
    pub pair_path: String,
    pub parent_path: String,
    pub pair_svn: i64,
    pub parent_svn: i64,
    pub generation: u64,
    pub policy: String,
    pub pending_pair_git: Vec<String>,
    pub pending_parent_git: Vec<String>,
    pub pending_pair_svn: usize,
    pub pending_parent_svn: usize,
    pub conflicts: Vec<String>,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntheticReceipt {
    pub request_id: String,
    pub digest: String,
    pub operation: String,
    pub noop: bool,
    pub generation: u64,
    pub git_tip: String,
    pub svn_revision: i64,
}

/// In-memory pair used by the #69 synthetic tests.
#[derive(Clone, Debug)]
pub struct SyntheticPairRefresh {
    generations: Vec<Generation>,
    active: usize,
    commits: BTreeMap<String, Node>,
    revisions: BTreeMap<i64, Rev>,
    next_rev: i64,
    parent_branch: String,
    parent_path: String,
    parent_tip: String,
    parent_svn: i64,
    nested: Vec<String>,
    git_protected: bool,
    svn_denied: bool,
    crash_after: Option<String>,
    resolution: BTreeMap<String, String>,
    jobs: Vec<Job>,
    receipts: Vec<SyntheticReceipt>,
    cancelled: BTreeSet<String>,
}

pub fn runbook() -> &'static str {
    "Default refresh integrates the parent as a new Git commit and a new SVN revision and leaves published history in place. Rewrite containment refuses a mapped commit that is no longer an ancestor. Re-anchor opens another generation from an SVN-origin baseline, retains the old ref and path, and does not delete displaced work. Retirement only marks the old generation; it is a separate approval from refresh."
}

impl SyntheticPairRefresh {
    pub fn origin() -> Self {
        let tree = BTreeMap::from([("readme".into(), "origin".into())]);
        let sha = hash(&["commit", "origin"]);
        let mut commits = BTreeMap::new();
        commits.insert(
            sha.clone(),
            Node {
                parents: Vec::new(),
                tree: tree.clone(),
            },
        );
        let mut revisions = BTreeMap::new();
        revisions.insert(
            1,
            Rev {
                path: "branches/feature".into(),
                tree: tree.clone(),
                mergeinfo: None,
            },
        );
        revisions.insert(
            1,
            Rev {
                path: "trunk".into(),
                tree,
                mergeinfo: None,
            },
        );
        // One repository revision is visible on both paths because the branch
        // was copied from trunk at r1. The map keeps a single revision record;
        // path membership for r1 is both paths via the copy.
        let _ = revisions.insert(
            1,
            Rev {
                path: "trunk".into(),
                tree: BTreeMap::from([("readme".into(), "origin".into())]),
                mergeinfo: Some("copied-to:branches/feature".into()),
            },
        );
        let mut mapping = BTreeMap::new();
        mapping.insert(sha.clone(), 1);
        let generation = Generation {
            id: 1,
            git_branch: "feature".into(),
            svn_path: "branches/feature".into(),
            git_tip: sha.clone(),
            svn_revision: 1,
            parent_baseline_svn: 1,
            baseline_sha: sha.clone(),
            retained_ref: "refs/reposync/pair-refresh/generation/1".into(),
            mapping,
            retired: false,
            displaced: Vec::new(),
        };
        Self {
            generations: vec![generation],
            active: 0,
            commits,
            revisions,
            next_rev: 2,
            parent_branch: "main".into(),
            parent_path: "trunk".into(),
            parent_tip: sha,
            parent_svn: 1,
            nested: Vec::new(),
            git_protected: false,
            svn_denied: false,
            crash_after: None,
            resolution: BTreeMap::new(),
            jobs: Vec::new(),
            receipts: Vec::new(),
            cancelled: BTreeSet::new(),
        }
    }

    pub fn preview(&self) -> SyntheticPlan {
        self.plan("update", false, false)
    }

    pub fn preview_reanchor(&self, same_name: bool, retire: bool) -> SyntheticPlan {
        self.plan("reanchor", same_name, retire)
    }

    pub fn set_crash_after(&mut self, phase: Option<&str>) {
        self.crash_after = phase.map(str::to_string);
    }

    pub fn set_git_protected(&mut self, protected: bool) {
        self.git_protected = protected;
    }

    pub fn set_svn_denied(&mut self, denied: bool) {
        self.svn_denied = denied;
    }

    pub fn set_nested(&mut self, name: &str) {
        self.nested.push(name.to_string());
    }

    pub fn resolve(&mut self, path: &str, choice: &str) {
        self.resolution.insert(path.to_string(), choice.to_string());
    }

    pub fn advance_parent(&mut self, path: &str, content: &str) {
        let mut tree = self.tree_of(&self.parent_tip);
        tree.insert(path.to_string(), content.to_string());
        let sha = hash(&[
            "parent",
            &self.parent_tip,
            path,
            content,
            &self.next_rev.to_string(),
        ]);
        self.commits.insert(
            sha.clone(),
            Node {
                parents: vec![self.parent_tip.clone()],
                tree: tree.clone(),
            },
        );
        let number = self.next_rev;
        self.next_rev += 1;
        self.revisions.insert(
            number,
            Rev {
                path: self.parent_path.clone(),
                tree,
                mergeinfo: None,
            },
        );
        self.parent_tip = sha;
        self.parent_svn = number;
    }

    pub fn advance_pair(&mut self, path: &str, content: &str) {
        let tip = self.active_gen().git_tip.clone();
        let mut tree = self.tree_of(&tip);
        tree.insert(path.to_string(), content.to_string());
        let sha = hash(&["pair", &tip, path, content, &self.next_rev.to_string()]);
        self.commits.insert(
            sha.clone(),
            Node {
                parents: vec![tip],
                tree: tree.clone(),
            },
        );
        let number = self.next_rev;
        self.next_rev += 1;
        let path_name = self.active_gen().svn_path.clone();
        self.revisions.insert(
            number,
            Rev {
                path: path_name,
                tree,
                mergeinfo: None,
            },
        );
        let gen = self.active_gen_mut();
        gen.git_tip = sha;
        gen.svn_revision = number;
    }

    pub fn append_pair_svn(&mut self, path: &str, content: &str) {
        let tip = self.active_gen().git_tip.clone();
        let mut tree = self.tree_of(&tip);
        tree.insert(path.to_string(), content.to_string());
        let number = self.next_rev;
        self.next_rev += 1;
        let svn_path = self.active_gen().svn_path.clone();
        self.revisions.insert(
            number,
            Rev {
                path: svn_path,
                tree,
                mergeinfo: None,
            },
        );
        self.active_gen_mut().svn_revision = number;
    }

    pub fn rebase_pair_off_mapping(&mut self) {
        let tree = self.tree_of(&self.active_gen().git_tip);
        let sha = hash(&["rebased-root", &self.active_gen().git_tip]);
        self.commits.insert(
            sha.clone(),
            Node {
                parents: Vec::new(),
                tree,
            },
        );
        self.active_gen_mut().git_tip = sha;
    }

    pub fn execute(&mut self, request: &str, digest: &str) -> Result<SyntheticReceipt, String> {
        if let Some(receipt) = self.receipts.iter().find(|item| item.request_id == request) {
            return Ok(receipt.clone());
        }
        if self.jobs.iter().any(|job| job.request_id == request) {
            return self.resume(request);
        }
        if self.cancelled.contains(request) {
            return Err("cancelled request cannot execute".into());
        }
        let plan = self.preview();
        if plan.digest != digest {
            return Err("stale preview: approval digest does not match the current pins".into());
        }
        if !plan.executable {
            return Err(format!("refresh refused: {}", plan.conflicts.join(", ")));
        }
        if plan.pending_parent_git.is_empty() && plan.pending_parent_svn == 0 {
            return self.finish_noop(request, digest);
        }
        let gen = self.active_gen();
        let result_tree = self.merge_result(&gen.git_tip, &self.parent_tip)?;
        self.jobs.push(Job {
            request_id: request.to_string(),
            digest: digest.to_string(),
            operation: "update".into(),
            phase: "admitted".into(),
            same_name: false,
            retire: false,
            parent_tip: self.parent_tip.clone(),
            result_tree,
            svn_before: gen.svn_revision,
            git_before: gen.git_tip.clone(),
            new_svn: None,
            new_git: None,
            error: None,
        });
        self.drive(request)
    }

    pub fn resume(&mut self, request: &str) -> Result<SyntheticReceipt, String> {
        if let Some(receipt) = self.receipts.iter().find(|item| item.request_id == request) {
            return Ok(receipt.clone());
        }
        if !self.jobs.iter().any(|job| job.request_id == request) {
            return Err("no recoverable refresh job".into());
        }
        self.drive(request)
    }

    pub fn cancel(&mut self, request: &str) -> Result<(), String> {
        if self.receipts.iter().any(|item| item.request_id == request) {
            return Err("completed refresh cannot be cancelled".into());
        }
        if let Some(job) = self.jobs.iter().find(|job| job.request_id == request) {
            if job.phase != "admitted" {
                return Err("cancellation is refused after a publication intent".into());
            }
        }
        self.cancelled.insert(request.to_string());
        Ok(())
    }

    pub fn reanchor(&mut self, request: &str, digest: &str) -> Result<SyntheticReceipt, String> {
        if let Some(receipt) = self
            .receipts
            .iter()
            .find(|item| item.request_id == request && item.operation == "reanchor")
        {
            return Ok(receipt.clone());
        }
        let mut matched = None;
        for same_name in [false, true] {
            for retire in [false, true] {
                let plan = self.preview_reanchor(same_name, retire);
                if plan.digest == digest {
                    matched = Some((same_name, retire, plan));
                }
            }
        }
        let Some((same_name, retire, plan)) = matched else {
            return Err("re-anchor approval does not match a current generation plan".into());
        };
        if !plan.conflicts.is_empty() {
            return Err(format!("re-anchor refused: {}", plan.conflicts.join(", ")));
        }
        let old_index = self.active;
        let old = self.generations[old_index].clone();
        let displaced = plan.pending_pair_git.clone();
        self.generations[old_index].displaced = displaced;
        self.generations[old_index].retired = retire;
        let mut mapping = BTreeMap::new();
        mapping.insert(self.parent_tip.clone(), self.parent_svn);
        let id = old.id + 1;
        let svn_path = if same_name {
            old.svn_path.clone()
        } else {
            format!("{}-g{id}", old.svn_path)
        };
        let number = self.next_rev;
        self.next_rev += 1;
        self.revisions.insert(
            number,
            Rev {
                path: svn_path.clone(),
                tree: self.tree_of(&self.parent_tip),
                mergeinfo: Some(format!(
                    "svn-origin:{}@{}",
                    self.parent_path, self.parent_svn
                )),
            },
        );
        self.generations.push(Generation {
            id,
            git_branch: old.git_branch.clone(),
            svn_path,
            git_tip: self.parent_tip.clone(),
            svn_revision: number,
            parent_baseline_svn: self.parent_svn,
            baseline_sha: self.parent_tip.clone(),
            retained_ref: format!("refs/reposync/pair-refresh/generation/{id}"),
            mapping,
            retired: false,
            displaced: Vec::new(),
        });
        self.active = self.generations.len() - 1;
        let gen = self.active_gen();
        let receipt = SyntheticReceipt {
            request_id: request.to_string(),
            digest: digest.to_string(),
            operation: "reanchor".into(),
            noop: false,
            generation: gen.id,
            git_tip: gen.git_tip.clone(),
            svn_revision: gen.svn_revision,
        };
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    pub fn operation_state(&self, request: &str) -> String {
        if let Some(receipt) = self.receipts.iter().find(|item| item.request_id == request) {
            return format!("completed:{}", receipt.operation);
        }
        if self.cancelled.contains(request) {
            return "cancelled".into();
        }
        self.jobs
            .iter()
            .find(|job| job.request_id == request)
            .map(|job| job.phase.clone())
            .unwrap_or_else(|| "idle".into())
    }

    pub fn commit_exists(&self, sha: &str) -> bool {
        self.commits.contains_key(sha)
    }

    pub fn revision_exists(&self, number: i64) -> bool {
        self.revisions.contains_key(&number)
    }

    pub fn revisions_on(&self, path: &str) -> Vec<i64> {
        let mut numbers: Vec<i64> = self
            .revisions
            .iter()
            .filter(|(_, rev)| rev.path == path)
            .map(|(number, _)| *number)
            .collect();
        if path != "trunk" {
            if let Some(rev) = self.revisions.get(&1) {
                if rev.mergeinfo.as_deref() == Some("copied-to:branches/feature")
                    && !numbers.contains(&1)
                {
                    numbers.push(1);
                }
            }
        }
        numbers.sort_unstable();
        numbers.dedup();
        numbers
    }

    pub fn git_tip(&self) -> &str {
        &self.active_gen().git_tip
    }

    pub fn parent_tip(&self) -> &str {
        &self.parent_tip
    }

    pub fn generation_id(&self) -> u64 {
        self.active_gen().id
    }

    pub fn generation_count(&self) -> usize {
        self.generations.len()
    }

    pub fn retained_ref(&self, id: u64) -> &str {
        &self.generation(id).retained_ref
    }

    pub fn displaced(&self, id: u64) -> &[String] {
        &self.generation(id).displaced
    }

    pub fn retired(&self, id: u64) -> bool {
        self.generation(id).retired
    }

    pub fn svn_path_of(&self, id: u64) -> &str {
        &self.generation(id).svn_path
    }

    pub fn commit_count(&self) -> usize {
        self.commits.len()
    }

    pub fn revision_count(&self) -> usize {
        self.revisions.len()
    }

    pub fn tree_file(&self, sha: &str, path: &str) -> Option<&str> {
        self.commits
            .get(sha)
            .and_then(|node| node.tree.get(path).map(String::as_str))
    }

    pub fn parents_of(&self, sha: &str) -> Vec<String> {
        self.commits
            .get(sha)
            .map(|node| node.parents.clone())
            .unwrap_or_default()
    }

    fn drive(&mut self, request: &str) -> Result<SyntheticReceipt, String> {
        self.mark(request, "svn_intent")?;
        if self.crash_now("svn_intent") {
            return Err("crashed after svn_intent".into());
        }
        if self.svn_denied {
            let message = "svn permission denied".to_string();
            self.fail(request, &message);
            return Err(message);
        }
        if self.job(request).new_svn.is_none() {
            let number = self.next_rev;
            self.next_rev += 1;
            let path = self.active_gen().svn_path.clone();
            let tree = self.job(request).result_tree.clone();
            let parent_base = self.active_gen().parent_baseline_svn;
            self.revisions.insert(
                number,
                Rev {
                    path,
                    tree,
                    mergeinfo: Some(format!(
                        "{}:{}:{}",
                        self.parent_path, parent_base, self.parent_svn
                    )),
                },
            );
            self.job_mut(request).new_svn = Some(number);
            self.active_gen_mut().svn_revision = number;
        }
        self.mark(request, "svn_observed")?;
        if self.crash_now("svn_observed") {
            return Err("crashed after svn_observed".into());
        }
        if self.git_protected {
            let message = "protected ref rejected the integration update".to_string();
            self.fail(request, &message);
            return Err(message);
        }
        self.mark(request, "git_intent")?;
        if self.crash_now("git_intent") {
            return Err("crashed after git_intent".into());
        }
        if self.job(request).new_git.is_none() {
            let git_before = self.job(request).git_before.clone();
            let parent_tip = self.job(request).parent_tip.clone();
            let tree = self.job(request).result_tree.clone();
            let sha = hash(&["integration", &git_before, &parent_tip, &tree_key(&tree)]);
            self.commits.insert(
                sha.clone(),
                Node {
                    parents: vec![git_before, parent_tip],
                    tree,
                },
            );
            self.job_mut(request).new_git = Some(sha.clone());
            self.active_gen_mut().git_tip = sha;
        }
        self.mark(request, "git_observed")?;
        if self.crash_now("git_observed") {
            return Err("crashed after git_observed".into());
        }
        let new_git = self.job(request).new_git.clone().expect("git receipt");
        let new_svn = self.job(request).new_svn.expect("svn receipt");
        let parent_svn = self.parent_svn;
        let gen = self.active_gen_mut();
        gen.mapping.insert(new_git.clone(), new_svn);
        gen.baseline_sha = new_git;
        gen.parent_baseline_svn = parent_svn;
        self.mark(request, "checkpoint")?;
        let job = self.job(request).clone();
        let gen = self.active_gen();
        let receipt = SyntheticReceipt {
            request_id: request.to_string(),
            digest: job.digest,
            operation: "update".into(),
            noop: false,
            generation: gen.id,
            git_tip: gen.git_tip.clone(),
            svn_revision: gen.svn_revision,
        };
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    fn finish_noop(&mut self, request: &str, digest: &str) -> Result<SyntheticReceipt, String> {
        let gen = self.active_gen();
        let receipt = SyntheticReceipt {
            request_id: request.to_string(),
            digest: digest.to_string(),
            operation: "update".into(),
            noop: true,
            generation: gen.id,
            git_tip: gen.git_tip.clone(),
            svn_revision: gen.svn_revision,
        };
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    fn plan(&self, operation: &str, same_name: bool, retire: bool) -> SyntheticPlan {
        let gen = self.active_gen();
        let rewritten = !self.ancestors(&gen.git_tip).contains(&gen.baseline_sha);
        let mut conflicts = Vec::new();
        if rewritten {
            conflicts.push("rewritten_lineage".into());
        }
        if !self.nested.is_empty() {
            conflicts.push("nested_descendant".into());
        }
        let pending_parent_git = if rewritten {
            Vec::new()
        } else {
            self.pending_commits(&self.parent_tip, &gen.git_tip, &gen.mapping)
        };
        let pending_pair_git = if rewritten {
            Vec::new()
        } else {
            self.pending_commits(&gen.git_tip, &self.parent_tip, &gen.mapping)
        };
        let pending_parent_svn = self
            .revisions
            .iter()
            .filter(|(number, rev)| {
                **number > gen.parent_baseline_svn && rev.path == self.parent_path
            })
            .count();
        let pending_pair_svn = self
            .revisions
            .iter()
            .filter(|(number, rev)| **number > gen.svn_revision && rev.path == gen.svn_path)
            .count();
        let overlap = self.overlapping_conflicts(&gen.git_tip, &self.parent_tip);
        for path in &overlap {
            if self.resolution.get(path).map(String::as_str) != Some("child")
                && self.resolution.get(path).map(String::as_str) != Some("parent")
            {
                conflicts.push(format!("conflict:{path}"));
            }
        }
        let executable = operation == "update" && conflicts.is_empty() && !rewritten;
        let mut material = vec![
            operation.to_string(),
            POLICY.into(),
            UUID.into(),
            gen.id.to_string(),
            gen.git_tip.clone(),
            self.parent_tip.clone(),
            gen.svn_path.clone(),
            self.parent_branch.clone(),
            self.parent_path.clone(),
            gen.svn_revision.to_string(),
            self.parent_svn.to_string(),
            same_name.to_string(),
            retire.to_string(),
        ];
        material.extend(pending_pair_git.iter().cloned());
        material.extend(pending_parent_git.iter().cloned());
        material.extend(conflicts.iter().cloned());
        material.extend(
            self.resolution
                .iter()
                .map(|(path, choice)| format!("{path}={choice}")),
        );
        SyntheticPlan {
            digest: hash(&material.iter().map(String::as_str).collect::<Vec<_>>()),
            operation: operation.into(),
            pair_git_tip: gen.git_tip.clone(),
            parent_git_tip: self.parent_tip.clone(),
            svn_uuid: UUID.into(),
            pair_path: gen.svn_path.clone(),
            parent_path: self.parent_path.clone(),
            pair_svn: gen.svn_revision,
            parent_svn: self.parent_svn,
            generation: gen.id,
            policy: POLICY.into(),
            pending_pair_git,
            pending_parent_git,
            pending_pair_svn,
            pending_parent_svn,
            conflicts,
            executable,
        }
    }

    fn overlapping_conflicts(&self, pair_tip: &str, parent_tip: &str) -> Vec<String> {
        let base = self.lowest_common(pair_tip, parent_tip);
        let base_tree = self.tree_of(&base);
        let pair_tree = self.tree_of(pair_tip);
        let parent_tree = self.tree_of(parent_tip);
        let mut paths = BTreeSet::new();
        paths.extend(base_tree.keys().cloned());
        paths.extend(pair_tree.keys().cloned());
        paths.extend(parent_tree.keys().cloned());
        let mut conflicts = Vec::new();
        for path in paths {
            let base = base_tree.get(&path).map(String::as_str);
            let left = pair_tree.get(&path).map(String::as_str);
            let right = parent_tree.get(&path).map(String::as_str);
            if left != right && left != base && right != base {
                conflicts.push(path);
            }
        }
        conflicts
    }

    fn merge_result(
        &self,
        pair_tip: &str,
        parent_tip: &str,
    ) -> Result<BTreeMap<String, String>, String> {
        let base = self.lowest_common(pair_tip, parent_tip);
        let base_tree = self.tree_of(&base);
        let pair_tree = self.tree_of(pair_tip);
        let parent_tree = self.tree_of(parent_tip);
        let mut paths = BTreeSet::new();
        paths.extend(base_tree.keys().cloned());
        paths.extend(pair_tree.keys().cloned());
        paths.extend(parent_tree.keys().cloned());
        let mut result = BTreeMap::new();
        for path in paths {
            let base = base_tree.get(&path).cloned();
            let left = pair_tree.get(&path).cloned();
            let right = parent_tree.get(&path).cloned();
            if left == right {
                if let Some(value) = left {
                    result.insert(path, value);
                }
            } else if left.as_deref() == base.as_deref() {
                if let Some(value) = right {
                    result.insert(path, value);
                }
            } else if right.as_deref() == base.as_deref() {
                if let Some(value) = left {
                    result.insert(path, value);
                }
            } else {
                match self.resolution.get(&path).map(String::as_str) {
                    Some("child") => {
                        if let Some(value) = left {
                            result.insert(path, value);
                        }
                    }
                    Some("parent") => {
                        if let Some(value) = right {
                            result.insert(path, value);
                        }
                    }
                    _ => return Err(format!("resolution required for {path}")),
                }
            }
        }
        Ok(result)
    }

    fn pending_commits(
        &self,
        tip: &str,
        other: &str,
        mapping: &BTreeMap<String, i64>,
    ) -> Vec<String> {
        let stop = self.ancestors(other);
        let mut pending = Vec::new();
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([tip.to_string()]);
        while let Some(sha) = queue.pop_front() {
            if !seen.insert(sha.clone()) || stop.contains(&sha) || mapping.contains_key(&sha) {
                continue;
            }
            pending.push(sha.clone());
            if let Some(node) = self.commits.get(&sha) {
                queue.extend(node.parents.clone());
            }
        }
        pending
    }

    fn ancestors(&self, tip: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::from([tip.to_string()]);
        while let Some(sha) = queue.pop_front() {
            if !out.insert(sha.clone()) {
                continue;
            }
            if let Some(node) = self.commits.get(&sha) {
                queue.extend(node.parents.clone());
            }
        }
        out
    }

    fn lowest_common(&self, left: &str, right: &str) -> String {
        let right_ancestors = self.ancestors(right);
        let mut queue = VecDeque::from([left.to_string()]);
        let mut seen = BTreeSet::new();
        while let Some(sha) = queue.pop_front() {
            if !seen.insert(sha.clone()) {
                continue;
            }
            if right_ancestors.contains(&sha) {
                return sha;
            }
            if let Some(node) = self.commits.get(&sha) {
                queue.extend(node.parents.clone());
            }
        }
        left.to_string()
    }

    fn tree_of(&self, sha: &str) -> BTreeMap<String, String> {
        self.commits
            .get(sha)
            .map(|node| node.tree.clone())
            .unwrap_or_default()
    }

    fn mark(&mut self, request: &str, phase: &str) -> Result<(), String> {
        let job = self.job_mut(request);
        if matches!(job.phase.as_str(), "svn_denied" | "reconciliation_required") {
            return Err(job.error.clone().unwrap_or_else(|| job.phase.clone()));
        }
        let order = [
            "admitted",
            "svn_intent",
            "svn_observed",
            "git_intent",
            "git_observed",
            "checkpoint",
        ];
        let current = order
            .iter()
            .position(|item| *item == job.phase)
            .unwrap_or(0);
        let next = order
            .iter()
            .position(|item| *item == phase)
            .unwrap_or(current);
        if next > current {
            job.phase = phase.to_string();
        }
        Ok(())
    }

    fn fail(&mut self, request: &str, message: &str) {
        let protected = self.git_protected;
        let published = self.job(request).new_svn.is_some();
        let job = self.job_mut(request);
        job.error = Some(message.to_string());
        job.phase = if protected && published {
            "reconciliation_required".into()
        } else {
            "svn_denied".into()
        };
    }

    fn crash_now(&self, phase: &str) -> bool {
        self.crash_after.as_deref() == Some(phase)
    }

    fn job(&self, request: &str) -> &Job {
        self.jobs
            .iter()
            .find(|job| job.request_id == request)
            .expect("job")
    }

    fn job_mut(&mut self, request: &str) -> &mut Job {
        self.jobs
            .iter_mut()
            .find(|job| job.request_id == request)
            .expect("job")
    }

    fn active_gen(&self) -> &Generation {
        &self.generations[self.active]
    }

    fn active_gen_mut(&mut self) -> &mut Generation {
        &mut self.generations[self.active]
    }

    fn generation(&self, id: u64) -> &Generation {
        self.generations
            .iter()
            .find(|gen| gen.id == id)
            .expect("generation")
    }
}

fn tree_key(tree: &BTreeMap<String, String>) -> String {
    tree.iter()
        .map(|(path, content)| format!("{path}={content}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(pair: &SyntheticPairRefresh) -> (usize, usize) {
        (pair.commit_count(), pair.revision_count())
    }

    #[test]
    fn preview_pins_digest_to_exact_inputs() {
        let mut pair = SyntheticPairRefresh::origin();
        let plan = pair.preview();
        assert_eq!(plan.policy, POLICY);
        assert_eq!(plan.svn_uuid, UUID);
        assert_eq!(plan.generation, 1);
        assert_eq!(plan.pair_git_tip, pair.git_tip());
        assert_eq!(plan.parent_git_tip, pair.parent_tip());
        assert_eq!(plan.pair_svn, 1);
        assert_eq!(plan.parent_svn, 1);
        assert_eq!(pair.preview().digest, plan.digest);
        pair.advance_parent("readme", "parent");
        assert_ne!(pair.preview().digest, plan.digest);
        assert!(!plan.executable || plan.conflicts.is_empty());
    }

    #[test]
    fn both_sides_unsynced_are_preserved() {
        let mut pair = SyntheticPairRefresh::origin();
        let origin = pair.git_tip().to_string();
        pair.advance_pair("pair.txt", "pair");
        pair.advance_parent("parent.txt", "parent");
        let pair_tip = pair.git_tip().to_string();
        let plan = pair.preview();
        assert!(!plan.pending_pair_git.is_empty());
        assert!(!plan.pending_parent_git.is_empty());
        assert!(plan.executable);
        let digest = plan.digest;
        pair.execute("both", &digest).unwrap();
        assert!(pair.commit_exists(&origin));
        assert!(pair.commit_exists(&pair_tip));
        assert!(pair.revision_exists(1));
    }

    #[test]
    fn inherited_commits_are_not_new_work() {
        let mut pair = SyntheticPairRefresh::origin();
        let origin = pair.git_tip().to_string();
        pair.advance_pair("pair.txt", "kept");
        let plan = pair.preview();
        assert!(!plan.pending_parent_git.contains(&origin));
        assert_eq!(plan.pending_parent_git.len(), 0);
        pair.advance_parent("parent.txt", "new");
        let plan = pair.preview();
        assert_eq!(plan.pending_parent_git.len(), 1);
        assert!(!plan.pending_parent_git.contains(&origin));
    }

    #[test]
    fn rewritten_lineage_is_rejected() {
        let mut pair = SyntheticPairRefresh::origin();
        let mapped = pair.git_tip().to_string();
        pair.rebase_pair_off_mapping();
        let plan = pair.preview();
        assert!(plan
            .conflicts
            .iter()
            .any(|item| item == "rewritten_lineage"));
        assert!(!plan.executable);
        let error = pair.execute("rewrite", &plan.digest).unwrap_err();
        assert!(error.contains("rewritten_lineage"));
        assert!(pair.commit_exists(&mapped));
        assert_eq!(pair.revision_count(), 1);
    }

    #[test]
    fn stale_preview_is_rejected() {
        let mut pair = SyntheticPairRefresh::origin();
        let digest = pair.preview().digest;
        let before = counts(&pair);
        pair.advance_parent("readme", "moved");
        let error = pair.execute("stale", &digest).unwrap_err();
        assert!(error.contains("stale preview"));
        assert_eq!(counts(&pair), (before.0 + 1, before.1 + 1));
    }

    #[test]
    fn parent_advanced_appends_integration() {
        let mut pair = SyntheticPairRefresh::origin();
        let old_tip = pair.git_tip().to_string();
        pair.advance_parent("readme", "parent");
        let parent_tip = pair.parent_tip().to_string();
        let before_revs = pair.revision_count();
        let receipt = pair.execute("parent", &pair.preview().digest).unwrap();
        assert!(!receipt.noop);
        assert!(pair.commit_exists(&old_tip));
        assert!(pair.revision_exists(1));
        assert_eq!(pair.revision_count(), before_revs + 1);
        let parents = pair.parents_of(pair.git_tip());
        assert!(parents.contains(&old_tip));
        assert!(parents.contains(&parent_tip));
    }

    #[test]
    fn pair_advanced_is_preserved() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.advance_pair("pair.txt", "only-pair");
        let tip = pair.git_tip().to_string();
        let revs = pair.revision_count();
        let receipt = pair.execute("pair-only", &pair.preview().digest).unwrap();
        assert!(receipt.noop);
        assert_eq!(pair.git_tip(), tip);
        assert!(pair.commit_exists(&tip));
        assert_eq!(pair.revision_count(), revs);
    }

    #[test]
    fn both_advanced_cleanly_after_resolution() {
        let mut pair = SyntheticPairRefresh::origin();
        let origin = pair.git_tip().to_string();
        pair.advance_pair("shared.txt", "pair-side");
        pair.advance_pair("pair-only.txt", "kept");
        let pair_tip = pair.git_tip().to_string();
        pair.advance_parent("shared.txt", "parent-side");
        pair.resolve("shared.txt", "parent");
        let receipt = pair.execute("clean", &pair.preview().digest).unwrap();
        assert!(!receipt.noop);
        assert!(pair.commit_exists(&origin));
        assert!(pair.commit_exists(&pair_tip));
        assert_eq!(
            pair.tree_file(pair.git_tip(), "shared.txt"),
            Some("parent-side")
        );
        assert_eq!(
            pair.tree_file(pair.git_tip(), "pair-only.txt"),
            Some("kept")
        );
    }

    #[test]
    fn conflicting_edits_require_resolution() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.advance_pair("readme", "pair");
        pair.advance_parent("readme", "parent");
        let before = counts(&pair);
        let plan = pair.preview();
        assert!(plan.conflicts.iter().any(|item| item.contains("readme")));
        assert!(pair.execute("conflict", &plan.digest).is_err());
        assert_eq!(counts(&pair), before);
        pair.resolve("readme", "child");
        pair.execute("conflict-resolved", &pair.preview().digest)
            .unwrap();
        assert_eq!(pair.tree_file(pair.git_tip(), "readme"), Some("pair"));
        assert!(pair.revision_exists(1));
    }

    #[test]
    fn pending_svn_only_work_is_preserved() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.append_pair_svn("notes.txt", "svn-only");
        let svn_rev = pair.active_gen().svn_revision;
        pair.advance_parent("readme", "parent");
        let before = pair.revision_count();
        pair.execute("svn-only", &pair.preview().digest).unwrap();
        assert!(pair.revision_exists(svn_rev));
        assert_eq!(pair.revision_count(), before + 1);
    }

    #[test]
    fn previously_synced_git_rebased_is_rejected() {
        let mut pair = SyntheticPairRefresh::origin();
        let mapped = pair.git_tip().to_string();
        pair.rebase_pair_off_mapping();
        let error = pair.execute("rebased", &pair.preview().digest).unwrap_err();
        assert!(error.contains("rewritten_lineage"));
        assert!(pair.commit_exists(&mapped));
    }

    #[test]
    fn nested_descendant_is_blocked() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.set_nested("branches/feature-child");
        pair.advance_parent("readme", "parent");
        let plan = pair.preview();
        assert!(plan
            .conflicts
            .iter()
            .any(|item| item == "nested_descendant"));
        let error = pair.execute("nested", &plan.digest).unwrap_err();
        assert!(error.contains("nested_descendant"));
        assert_eq!(pair.commit_count(), 2);
    }

    #[test]
    fn repeated_unchanged_request_is_noop() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.advance_parent("readme", "parent");
        let first = pair.execute("once", &pair.preview().digest).unwrap();
        let again = pair.execute("once", &first.digest).unwrap();
        assert_eq!(first, again);
        let after = counts(&pair);
        let second = pair.execute("twice", &pair.preview().digest).unwrap();
        assert!(second.noop);
        assert_eq!(counts(&pair), after);
    }

    #[test]
    fn restart_after_svn_before_git_does_not_replay() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.advance_parent("readme", "parent");
        let digest = pair.preview().digest;
        let revs_before = pair.revision_count();
        pair.set_crash_after(Some("svn_observed"));
        let error = pair.execute("restart-svn", &digest).unwrap_err();
        assert!(error.contains("svn_observed"));
        assert_eq!(pair.revision_count(), revs_before + 1);
        let commits = pair.commit_count();
        pair.set_crash_after(None);
        pair.resume("restart-svn").unwrap();
        assert_eq!(pair.revision_count(), revs_before + 1);
        assert_eq!(pair.commit_count(), commits + 1);
    }

    #[test]
    fn restart_after_git_before_checkpoint_does_not_replay() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.advance_parent("readme", "parent");
        let digest = pair.preview().digest;
        pair.set_crash_after(Some("git_observed"));
        assert!(pair
            .execute("restart-git", &digest)
            .unwrap_err()
            .contains("git_observed"));
        let commits = pair.commit_count();
        let revs = pair.revision_count();
        let tip = pair.git_tip().to_string();
        pair.set_crash_after(None);
        let receipt = pair.resume("restart-git").unwrap();
        assert_eq!(receipt.git_tip, tip);
        assert_eq!(pair.commit_count(), commits);
        assert_eq!(pair.revision_count(), revs);
        assert_eq!(pair.operation_state("restart-git"), "completed:update");
    }

    #[test]
    fn cancellation_before_intent_leaves_history() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.advance_parent("readme", "parent");
        let before = counts(&pair);
        pair.cancel("stop").unwrap();
        assert_eq!(pair.operation_state("stop"), "cancelled");
        assert!(pair.execute("stop", &pair.preview().digest).is_err());
        assert_eq!(counts(&pair), before);
        assert!(runbook().contains("Re-anchor"));
    }

    #[test]
    fn protected_git_ref_rejection_is_visible() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.advance_parent("readme", "parent");
        pair.set_git_protected(true);
        let before_commits = pair.commit_count();
        let before_revs = pair.revision_count();
        let error = pair
            .execute("protected", &pair.preview().digest)
            .unwrap_err();
        assert!(error.contains("protected ref rejected"));
        assert_eq!(pair.commit_count(), before_commits);
        assert_eq!(pair.revision_count(), before_revs + 1);
        let again = pair.resume("protected").unwrap_err();
        assert!(again.contains("protected ref rejected"));
        assert_eq!(pair.revision_count(), before_revs + 1);
        assert_eq!(pair.operation_state("protected"), "reconciliation_required");
    }

    #[test]
    fn svn_permission_failure_is_visible() {
        let mut pair = SyntheticPairRefresh::origin();
        pair.advance_parent("readme", "parent");
        pair.set_svn_denied(true);
        let before = counts(&pair);
        let error = pair.execute("denied", &pair.preview().digest).unwrap_err();
        assert!(error.contains("permission"));
        assert_eq!(counts(&pair), before);
        let again = pair.resume("denied").unwrap_err();
        assert!(again.contains("permission"));
        assert_eq!(counts(&pair), before);
    }

    #[test]
    fn reanchor_retains_old_generation_and_displaced_work() {
        let mut pair = SyntheticPairRefresh::origin();
        let old_tip = {
            pair.advance_pair("wip.txt", "unsynced");
            pair.git_tip().to_string()
        };
        let old_path = pair.svn_path_of(1).to_string();
        let old_revs = pair.revisions_on(&old_path);
        let plan = pair.preview_reanchor(false, false);
        let receipt = pair.reanchor("reanchor", &plan.digest).unwrap();
        assert_eq!(receipt.generation, 2);
        assert_eq!(pair.generation_count(), 2);
        assert!(pair.commit_exists(&old_tip));
        assert_eq!(pair.displaced(1), &[old_tip]);
        assert_eq!(
            pair.retained_ref(1),
            "refs/reposync/pair-refresh/generation/1"
        );
        assert_eq!(pair.revisions_on(&old_path), old_revs);
        assert_ne!(pair.svn_path_of(2), old_path);
        assert!(!pair.retired(1));
        assert!(runbook().contains("does not delete displaced work"));
    }

    #[test]
    fn same_name_reuse_requires_approval() {
        let mut pair = SyntheticPairRefresh::origin();
        let old_path = pair.svn_path_of(1).to_string();
        assert!(pair
            .reanchor("update-digest", &pair.preview().digest)
            .is_err());
        assert_eq!(pair.generation_count(), 1);
        let different = pair.preview_reanchor(false, false).digest;
        assert_ne!(different, pair.preview_reanchor(true, false).digest);
        pair.reanchor("new-path", &different).unwrap();
        assert_eq!(pair.svn_path_of(1), old_path);
        assert_ne!(pair.svn_path_of(2), old_path);
        assert!(pair.revision_exists(1));

        let mut pair = SyntheticPairRefresh::origin();
        let old_path = pair.svn_path_of(1).to_string();
        let digest = pair.preview_reanchor(true, false).digest;
        pair.reanchor("same-name", &digest).unwrap();
        assert_eq!(pair.svn_path_of(2), old_path);
        assert!(pair.revision_exists(1));
        assert_eq!(pair.generation_id(), 2);
    }

    #[test]
    fn explicit_retirement_keeps_old_path() {
        let mut pair = SyntheticPairRefresh::origin();
        let path = pair.svn_path_of(1).to_string();
        let revs = pair.revisions_on(&path);
        let digest = pair.preview_reanchor(false, true).digest;
        pair.reanchor("retire", &digest).unwrap();
        assert!(pair.retired(1));
        assert_eq!(pair.revisions_on(&path), revs);
        assert_eq!(
            pair.retained_ref(1),
            "refs/reposync/pair-refresh/generation/1"
        );
        assert!(pair.revision_exists(1));
    }
}
