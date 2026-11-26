//! Durable Team path projection loading. The existing allow/block lists and
//! rewrite rule are read in one SQLite statement to avoid mixed reloads.
use rusqlite::OptionalExtension;

use crate::{errors::DatabaseError, path_projection::TeamPrefixRewrite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRewritePolicy {
    pub rule: TeamPrefixRewrite,
    pub allowed: Vec<String>,
    pub blocked: Vec<String>,
}

impl TeamRewritePolicy {
    pub fn parse(
        raw_rules: &str,
        raw_allow: Option<&str>,
        raw_block: Option<&str>,
    ) -> Result<Option<Self>, DatabaseError> {
        let Some(rule) = TeamPrefixRewrite::from_json(Some(raw_rules))
            .map_err(|e| DatabaseError::Other(e.into()))?
        else {
            return Ok(None);
        };
        let parse_scope = |raw: Option<&str>, name: &str| -> Result<Vec<String>, DatabaseError> {
            serde_json::from_str(raw.unwrap_or("[]")).map_err(|_| {
                DatabaseError::Other(format!("invalid {name} for active rewrite rule"))
            })
        };
        let allowed = parse_scope(raw_allow, "allow list")?;
        let blocked = parse_scope(raw_block, "block list")?;
        rule.validate_scope(&allowed)
            .map_err(|e| DatabaseError::Other(e.into()))?;
        Ok(Some(Self {
            rule,
            allowed,
            blocked,
        }))
    }
}

fn marker_key(repo: &str) -> String {
    format!("team_rewrite_verified_snapshot_v1:{repo}")
}

pub fn policy_snapshot(allowed: &[String], blocked: &[String], prefix: Option<&str>) -> String {
    match prefix {
        Some(prefix) => serde_json::json!({"allowed_paths": allowed, "blocked_patterns": blocked, "rewrite_prefix": prefix}),
        None => serde_json::json!({"allowed_paths": allowed, "blocked_patterns": blocked}),
    }.to_string()
}

impl TeamRewritePolicy {
    pub fn snapshot(&self) -> String {
        policy_snapshot(&self.allowed, &self.blocked, Some(self.rule.prefix()))
    }
}

impl super::Database {
    pub(crate) fn stage_verified_team_rewrite_snapshot(
        &self,
        repo: &str,
        policy: &TeamRewritePolicy,
        svn_rev: i64,
        git_sha: &str,
    ) -> Result<(), DatabaseError> {
        if self.team_path_rewrite(repo)?.as_ref() != Some(policy) {
            return Err(DatabaseError::Other(
                "rewrite settings changed during projected import".into(),
            ));
        }
        let marker = serde_json::json!([policy.snapshot(), svn_rev, git_sha]);
        self.set_state(&marker_key(repo), &marker.to_string())
    }

    pub fn require_team_rewrite_baseline(
        &self,
        repo: &str,
        policy: &TeamRewritePolicy,
    ) -> Result<(), DatabaseError> {
        use super::import_operations::{snapshot_origin_evidence, SnapshotOriginEvidence};
        let refusal = || {
            DatabaseError::Other("Team path rewriting needs a completed matching projected snapshot in a fresh destination".into())
        };
        let marker = self.get_state(&marker_key(repo))?.ok_or_else(refusal)?;
        let (stored_policy, rev, sha): (String, i64, String) =
            serde_json::from_str(&marker).map_err(|_| refusal())?;
        if stored_policy != policy.snapshot() {
            return Err(refusal());
        }
        match snapshot_origin_evidence(&self.conn(), repo, &sha)? {
            SnapshotOriginEvidence::Verified(origin) if origin.svn_rev == rev => Ok(()),
            _ => Err(refusal()),
        }
    }

    pub fn team_path_rewrite(
        &self,
        repo: &str,
    ) -> Result<Option<TeamRewritePolicy>, DatabaseError> {
        let raw: Option<(Option<String>, Option<String>, Option<String>)> = self.conn().query_row(
            "SELECT path_rewrite_rules, allowed_paths, blocked_patterns FROM repositories WHERE id=?1",
            [repo],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?;
        let Some((rules, allowed, blocked)) = raw else {
            return Err(DatabaseError::Other(
                "rewrite policy registration not found".into(),
            ));
        };
        TeamRewritePolicy::parse(
            rules.as_deref().unwrap_or("[]"),
            allowed.as_deref(),
            blocked.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_config_refuses_names_outside_its_injective_domain() {
        let rule = r#"[{"strip_prefix":"src/foo/"}]"#;
        assert!(TeamRewritePolicy::parse(rule, Some(r#"["src/foo","src/foobar"]"#), None).is_err());
        assert!(TeamRewritePolicy::parse(rule, None, None).is_err());
        let cfg = TeamRewritePolicy::parse(rule, Some(r#"["src/foo"]"#), Some(r#"["*.tmp"]"#))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.rule.to_svn("x").unwrap(), "src/foo/x");
        assert_eq!(cfg.blocked, ["*.tmp"]);
        assert!(TeamRewritePolicy::parse("[]", None, None)
            .unwrap()
            .is_none());
    }
}
