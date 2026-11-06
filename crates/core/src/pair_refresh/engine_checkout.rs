//! A completed authenticated refresh can leave the ordinary bridge at the
//! exact old child checkpoint. The normal history inspector still checks the
//! worktree, complete graph, fresh remote and all other ancestry constraints.

use crate::db::Database;
use crate::errors::DatabaseError;

pub(crate) struct CompletedRefreshCheckout {
    branch: String,
    old_checkpoint: String,
    new_checkpoint: String,
}

impl CompletedRefreshCheckout {
    pub(crate) fn capture(
        db: &Database,
        repo_id: &str,
        branch: &str,
        checkpoint: &str,
    ) -> Result<Option<Self>, DatabaseError> {
        let Some(repo) = db.get_repository(repo_id)? else {
            return Ok(None);
        };
        if repo.parent_id.is_none() || repo.git_branch != branch {
            return Ok(None);
        }
        let Some(operation) = db.completed_refresh_for_engine_checkout(repo_id)? else {
            return Ok(None);
        };
        if operation.version != 3 || operation.pins.git.prepared_sha != checkpoint {
            return Ok(None);
        }
        Ok(Some(Self {
            branch: branch.to_owned(),
            old_checkpoint: operation.pins.child.last_git_sha,
            new_checkpoint: operation.pins.git.prepared_sha,
        }))
    }

    pub(crate) fn permits(&self, branch: &str, checkpoint: &str, local: &str) -> bool {
        self.branch == branch && self.new_checkpoint == checkpoint && self.old_checkpoint == local
    }
}
