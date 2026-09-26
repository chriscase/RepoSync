//! Candidate authority implementation. Compiled only for explicit reliability
//! fixture qualification; ordinary initialization never calls this module.
use anyhow::{bail, ensure, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const V14_SQL: &str = include_str!("candidate/v14.sql");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lineage {
    pub repo_id: String,
    pub generation: i64,
    pub svn_uuid: String,
    pub svn_root_url: String,
    pub svn_branch_path: String,
    pub source_svn_uuid: String,
    pub source_svn_path: String,
    pub source_svn_rev: i64,
    pub copy_from_path: Option<String>,
    pub copy_from_rev: Option<i64>,
    pub baseline_svn_rev: i64,
    pub baseline_svn_tree_sha256: String,
    pub git_provider: String,
    pub git_repo_identity: String,
    pub git_ref: String,
    pub baseline_git_sha: String,
    pub projection_version: i64,
    pub projection_json: String,
    pub proof_sha256: String,
}
impl Lineage {
    pub fn policy_hash(&self) -> String {
        hex::encode(Sha256::digest(self.projection_json.as_bytes()))
    }
}

pub(crate) fn insert_baseline(c: &Connection, l: &Lineage) -> Result<()> {
    let policy = l.policy_hash();
    c.execute("INSERT INTO pair_lineages VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
        params![l.repo_id,l.generation,l.svn_uuid,l.svn_root_url,l.svn_branch_path,l.source_svn_uuid,l.source_svn_path,l.source_svn_rev,l.copy_from_path,l.copy_from_rev,l.baseline_svn_rev,l.baseline_svn_tree_sha256,l.git_provider,l.git_repo_identity,l.git_ref,l.baseline_git_sha,l.projection_version,l.projection_json,policy,l.proof_sha256])?;
    c.execute("INSERT INTO pair_frontiers VALUES (?1,?2,'svn_to_git',?3,?4,NULL,?5,NULL,'baseline',NULL,?6,?7)",
        params![l.repo_id,l.generation,format!("svn:{}",l.baseline_svn_rev),l.baseline_svn_rev,l.baseline_git_sha,l.projection_version,policy])?;
    // An SVN import proves this initial handled Git baseline; it does not
    // invent an outbound SVN remote effect.
    c.execute("INSERT INTO pair_frontiers VALUES (?1,?2,'git_to_svn',?3,NULL,?4,NULL,NULL,'baseline',NULL,?5,?6)",
        params![l.repo_id,l.generation,format!("git:{}",l.baseline_git_sha),l.baseline_git_sha,l.projection_version,policy])?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ResolvedTransition {
    pub id: String,
    pub repo_id: String,
    pub generation: i64,
    pub direction: String,
    pub predecessor_source_key: String,
    pub source_svn_rev: Option<i64>,
    pub source_git_sha: Option<String>,
    pub outcome: String,
    pub target_git_sha: Option<String>,
    pub target_svn_rev: Option<i64>,
    pub projection_version: i64,
    pub policy_sha256: String,
    pub evidence_json: String,
}
impl ResolvedTransition {
    pub fn source_key(&self) -> Result<String> {
        match self.direction.as_str() {
            "svn_to_git" => Ok(format!(
                "svn:{}",
                self.source_svn_rev
                    .ok_or_else(|| anyhow::anyhow!("missing SVN source"))?
            )),
            "git_to_svn" => Ok(format!(
                "git:{}",
                self.source_git_sha
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("missing Git source"))?
            )),
            _ => bail!("invalid direction"),
        }
    }
}

/// Single checked writer for a later frontier. This offline component does not
/// qualify external effects or select an active generation. Callers must name
/// the exact generation and supply separately verified, pinned evidence.
pub fn advance_frontier(c: &mut Connection, r: &ResolvedTransition) -> Result<()> {
    ensure!(
        c.pragma_query_value::<i64, _>(None, "foreign_keys", |row| row.get(0))? == 1,
        "foreign keys disabled"
    );
    ensure!(
        [
            "applied_verified",
            "filtered_no_target",
            "empty_no_target",
            "semantic_no_delta"
        ]
        .contains(&r.outcome.as_str()),
        "unresolved outcome"
    );
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: String = tx.query_row(
        "SELECT source_key FROM pair_frontiers WHERE repo_id=?1 AND generation=?2 AND direction=?3",
        params![r.repo_id, r.generation, r.direction],
        |row| row.get(0),
    )?;
    ensure!(current == r.predecessor_source_key, "stale predecessor");
    let source = r.source_key()?;
    ensure!(source != current, "source already handled");
    tx.execute(
        "INSERT INTO pair_outcomes VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            r.id,
            r.repo_id,
            r.generation,
            r.direction,
            source,
            r.predecessor_source_key,
            r.source_svn_rev,
            r.source_git_sha,
            r.outcome,
            r.target_git_sha,
            r.target_svn_rev,
            r.projection_version,
            r.policy_sha256,
            r.evidence_json
        ],
    )?;
    ensure!(tx.execute("UPDATE pair_frontiers SET source_key=?4,handled_svn_rev=?5,handled_git_sha=?6,emitted_git_sha=?7,emitted_svn_rev=?8,authority_kind='outcome',evidence_outcome_id=?9,projection_version=?10,policy_sha256=?11 WHERE repo_id=?1 AND generation=?2 AND direction=?3",params![r.repo_id,r.generation,r.direction,source,r.source_svn_rev,r.source_git_sha,r.target_git_sha,r.target_svn_rev,r.id,r.projection_version,r.policy_sha256])?==1,"missing frontier");
    ensure!(
        tx.prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_none(),
        "invalid foreign key"
    );
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn lineage(repo: &str, generation: i64) -> Lineage {
        Lineage {
            repo_id: repo.into(),
            generation,
            svn_uuid: "fixture-uuid".into(),
            svn_root_url: "file:///fixture".into(),
            svn_branch_path: "trunk".into(),
            source_svn_uuid: "fixture-uuid".into(),
            source_svn_path: "trunk".into(),
            source_svn_rev: 2,
            copy_from_path: None,
            copy_from_rev: None,
            baseline_svn_rev: 2,
            baseline_svn_tree_sha256: "a".repeat(64),
            git_provider: "fixture".into(),
            git_repo_identity: "fixture.git".into(),
            git_ref: "refs/heads/main".into(),
            baseline_git_sha: "b".repeat(40),
            projection_version: 1,
            projection_json: "{}".into(),
            proof_sha256: "c".repeat(64),
        }
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        super::super::schema::run_migrations(&c).unwrap();
        for repo in ["one", "two"] {
            c.execute("INSERT INTO repositories(id,name,svn_url,created_at,updated_at) VALUES(?1,?1,'file:///fixture','t','t')",[repo]).unwrap();
        }
        c.execute_batch(V14_SQL).unwrap();
        for (repo, generation) in [("one", 1), ("one", 2), ("two", 1)] {
            insert_baseline(&c, &lineage(repo, generation)).unwrap();
        }
        c
    }
    fn transition() -> ResolvedTransition {
        ResolvedTransition {
            id: "effect".into(),
            repo_id: "one".into(),
            generation: 1,
            direction: "svn_to_git".into(),
            predecessor_source_key: "svn:2".into(),
            source_svn_rev: Some(3),
            source_git_sha: None,
            outcome: "applied_verified".into(),
            target_git_sha: Some("d".repeat(40)),
            target_svn_rev: None,
            projection_version: 1,
            policy_sha256: lineage("one", 1).policy_hash(),
            evidence_json: "{}".into(),
        }
    }
    fn snapshot(c: &Connection) -> String {
        let mut q=c.prepare("SELECT repo_id,generation,direction,source_key,authority_kind,ifnull(evidence_outcome_id,'NULL') FROM pair_frontiers ORDER BY 1,2,3").unwrap();
        format!(
            "{:?}",
            q.query_map([], |r| Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?
            )))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
        )
    }
    #[test]
    fn valid_baseline_and_resolved_frontiers() {
        let mut c = db();
        assert_eq!(c.query_row("SELECT count(*) FROM pair_frontiers WHERE authority_kind='baseline' AND evidence_outcome_id IS NULL",[],|r|r.get::<_,i64>(0)).unwrap(),6);
        assert_eq!(c.query_row("SELECT emitted_svn_rev FROM pair_frontiers WHERE repo_id='one' AND generation=1 AND direction='git_to_svn'",[],|r|r.get::<_,Option<i64>>(0)).unwrap(),None);
        advance_frontier(&mut c, &transition()).unwrap();
        let mut t = transition();
        t.id = "no-target".into();
        t.predecessor_source_key = "svn:3".into();
        t.source_svn_rev = Some(4);
        t.outcome = "empty_no_target".into();
        t.target_git_sha = None;
        advance_frontier(&mut c, &t).unwrap();
        t.id = "outbound".into();
        t.direction = "git_to_svn".into();
        t.predecessor_source_key = format!("git:{}", "b".repeat(40));
        t.source_svn_rev = None;
        t.source_git_sha = Some("e".repeat(40));
        t.outcome = "applied_verified".into();
        t.target_svn_rev = Some(5);
        advance_frontier(&mut c, &t).unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM pair_outcomes", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(c.query_row("SELECT source_key FROM pair_frontiers WHERE repo_id='one' AND generation=2 AND direction='svn_to_git'",[],|r|r.get::<_,String>(0)).unwrap(),"svn:2");
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"K01_VALID","baselines":6,"resolved_transitions":3,"explicit_generation":true,"fk_check":"empty"})
        );
    }
    #[test]
    fn invalid_frontier_evidence_matrix() {
        let mut cases = Vec::new();
        for failure in [
            "cross_repo",
            "cross_generation",
            "wrong_direction",
            "wrong_source",
            "policy",
            "projection",
            "pending",
            "effect_unknown",
            "reconciliation_required",
            "nonexistent",
            "null",
            "baseline_later",
            "predecessor",
        ] {
            let c = db();
            let before = snapshot(&c);
            let mut t = transition();
            match failure {
                "cross_repo" => t.repo_id = "two".into(),
                "cross_generation" => t.generation = 2,
                "wrong_direction" => {
                    t.direction = "git_to_svn".into();
                    t.source_svn_rev = None;
                    t.source_git_sha = Some("e".repeat(40));
                    t.target_git_sha = None;
                    t.target_svn_rev = Some(3);
                }
                "wrong_source" => t.source_svn_rev = Some(4),
                "policy" => t.policy_sha256 = "f".repeat(64),
                "projection" => t.projection_version = 2,
                "pending" | "effect_unknown" | "reconciliation_required" => {
                    t.outcome = failure.into();
                    t.target_git_sha = None;
                }
                _ => {}
            }
            let source = t.source_key().unwrap();
            let inserted = c.execute(
                "INSERT INTO pair_outcomes VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    t.id,
                    t.repo_id,
                    t.generation,
                    t.direction,
                    source,
                    t.predecessor_source_key,
                    t.source_svn_rev,
                    t.source_git_sha,
                    t.outcome,
                    t.target_git_sha,
                    t.target_svn_rev,
                    t.projection_version,
                    t.policy_sha256,
                    t.evidence_json
                ],
            );
            let id = if failure == "nonexistent" {
                "absent"
            } else {
                "effect"
            };
            let evidence = if failure == "null" { None } else { Some(id) };
            let authority = if failure == "baseline_later" {
                "baseline"
            } else {
                "outcome"
            };
            if failure == "predecessor" {
                c.execute(
                    "UPDATE pair_outcomes SET predecessor_source_key='svn:1'",
                    [],
                )
                .unwrap();
            }
            let rejected=c.execute("UPDATE pair_frontiers SET source_key='svn:3',handled_svn_rev=3,emitted_git_sha=?1,authority_kind=?2,evidence_outcome_id=?3 WHERE repo_id='one' AND generation=1 AND direction='svn_to_git'",params!["d".repeat(40),authority,evidence]).is_err();
            assert!(rejected, "{failure}; inserted={inserted:?}");
            assert_eq!(snapshot(&c), before, "{failure}");
            // Also prove the single writer rejects unresolved/mismatched semantics atomically.
            if [
                "pending",
                "effect_unknown",
                "reconciliation_required",
                "policy",
                "projection",
            ]
            .contains(&failure)
            {
                let mut fresh = db();
                let s = snapshot(&fresh);
                assert!(advance_frontier(&mut fresh, &t).is_err());
                assert_eq!(snapshot(&fresh), s);
                assert_eq!(
                    fresh
                        .query_row("SELECT count(*) FROM pair_outcomes", [], |r| r
                            .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
            }
            cases.push(failure);
        }
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"K01_REJECTION","rejected":cases,"frontiers_unchanged":true})
        );
    }
    #[test]
    fn copy_origin_null_matrix() {
        let mut results = Vec::new();
        for (path, rev, valid) in [
            (None, None, true),
            (Some("trunk"), None, false),
            (None, Some(1), false),
            (Some("trunk"), Some(0), false),
            (Some("trunk"), Some(-1), false),
            (Some("trunk"), Some(1), true),
            (Some(""), Some(1), true),
        ] {
            let c = db();
            let mut l = lineage("one", 3);
            l.copy_from_path = path.map(String::from);
            l.copy_from_rev = rev;
            assert_eq!(insert_baseline(&c, &l).is_ok(), valid, "{path:?}/{rev:?}");
            results.push((path, rev, valid));
        }
        // Mandatory directional values cannot satisfy CHECK with NULL either.
        let c = db();
        assert!(c.execute("INSERT INTO pair_outcomes VALUES('null','one',1,'svn_to_git','svn:3','svn:2',NULL,NULL,'applied_verified',NULL,NULL,1,?1,'{}')",[lineage("one",1).policy_hash()]).is_err());
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"K03_ORIGIN","matrix":results,"mandatory_null_rejected":true})
        );
    }
}
