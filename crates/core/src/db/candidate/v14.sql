CREATE TABLE repo_migration_state (
 repo_id TEXT PRIMARY KEY NOT NULL REFERENCES repositories(id) ON DELETE RESTRICT,
 disposition TEXT NOT NULL CHECK(disposition IN ('qualified','needs_reconciliation','external_effect_unknown','not_qualified')),
 reason_code TEXT NOT NULL, evidence_manifest_sha256 TEXT NOT NULL CHECK(length(evidence_manifest_sha256)=64)
);
CREATE TABLE pair_lineages (
 repo_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE RESTRICT,
 generation INTEGER NOT NULL CHECK(generation>0),
 svn_uuid TEXT NOT NULL CHECK(length(svn_uuid)>0), svn_root_url TEXT NOT NULL CHECK(length(svn_root_url)>0),
 svn_branch_path TEXT NOT NULL, source_svn_uuid TEXT NOT NULL CHECK(length(source_svn_uuid)>0),
 source_svn_path TEXT NOT NULL, source_svn_rev INTEGER NOT NULL CHECK(source_svn_rev>0),
 copy_from_path TEXT, copy_from_rev INTEGER,
 baseline_svn_rev INTEGER NOT NULL CHECK(baseline_svn_rev>0),
 baseline_svn_tree_sha256 TEXT NOT NULL CHECK(length(baseline_svn_tree_sha256)=64 AND baseline_svn_tree_sha256 NOT GLOB '*[^0-9a-f]*'),
 git_provider TEXT NOT NULL CHECK(length(git_provider)>0), git_repo_identity TEXT NOT NULL CHECK(length(git_repo_identity)>0),
 git_ref TEXT NOT NULL CHECK(length(git_ref)>0),
 baseline_git_sha TEXT NOT NULL CHECK(length(baseline_git_sha) IN (40,64) AND baseline_git_sha NOT GLOB '*[^0-9a-f]*'),
 projection_version INTEGER NOT NULL CHECK(projection_version>0),
 projection_json TEXT NOT NULL CHECK(json_valid(projection_json)),
 policy_sha256 TEXT NOT NULL CHECK(length(policy_sha256)=64 AND policy_sha256 NOT GLOB '*[^0-9a-f]*'),
 proof_sha256 TEXT NOT NULL CHECK(length(proof_sha256)=64 AND proof_sha256 NOT GLOB '*[^0-9a-f]*'),
 PRIMARY KEY(repo_id,generation), UNIQUE(repo_id,generation,projection_version,policy_sha256),
 CHECK((copy_from_path IS NULL AND copy_from_rev IS NULL) OR
       (copy_from_path IS NOT NULL AND copy_from_rev IS NOT NULL AND copy_from_rev>0))
);
CREATE TABLE pair_outcomes (
 id TEXT PRIMARY KEY NOT NULL, repo_id TEXT NOT NULL, generation INTEGER NOT NULL,
 direction TEXT NOT NULL CHECK(direction IN ('svn_to_git','git_to_svn')),
 source_key TEXT NOT NULL, predecessor_source_key TEXT NOT NULL,
 source_svn_rev INTEGER, source_git_sha TEXT,
 outcome TEXT NOT NULL CHECK(outcome IN ('applied_verified','filtered_no_target','empty_no_target','semantic_no_delta','pending','locally_published_not_remote','effect_unknown','reconciliation_required')),
 target_git_sha TEXT, target_svn_rev INTEGER,
 projection_version INTEGER NOT NULL CHECK(projection_version>0), policy_sha256 TEXT NOT NULL,
 evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
 FOREIGN KEY(repo_id,generation,projection_version,policy_sha256) REFERENCES pair_lineages(repo_id,generation,projection_version,policy_sha256) ON DELETE RESTRICT,
 UNIQUE(repo_id,generation,direction,source_key),
 UNIQUE(repo_id,generation,direction,source_key,id,projection_version,policy_sha256),
 CHECK((direction='svn_to_git' AND source_svn_rev IS NOT NULL AND source_svn_rev>0 AND source_git_sha IS NULL AND target_svn_rev IS NULL AND source_key='svn:'||source_svn_rev) OR
       (direction='git_to_svn' AND source_git_sha IS NOT NULL AND length(source_git_sha) IN (40,64) AND source_git_sha NOT GLOB '*[^0-9a-f]*' AND source_svn_rev IS NULL AND target_git_sha IS NULL AND source_key='git:'||source_git_sha)),
 CHECK(target_git_sha IS NULL OR (length(target_git_sha) IN (40,64) AND target_git_sha NOT GLOB '*[^0-9a-f]*')),
 CHECK(target_svn_rev IS NULL OR target_svn_rev>0),
 CHECK(outcome NOT IN ('filtered_no_target','empty_no_target','semantic_no_delta') OR (target_git_sha IS NULL AND target_svn_rev IS NULL)),
 CHECK(outcome!='applied_verified' OR (direction='svn_to_git' AND target_git_sha IS NOT NULL) OR (direction='git_to_svn' AND target_svn_rev IS NOT NULL))
);
CREATE TABLE pair_frontiers (
 repo_id TEXT NOT NULL, generation INTEGER NOT NULL,
 direction TEXT NOT NULL CHECK(direction IN ('svn_to_git','git_to_svn')),
 source_key TEXT NOT NULL, handled_svn_rev INTEGER, handled_git_sha TEXT,
 emitted_git_sha TEXT, emitted_svn_rev INTEGER,
 authority_kind TEXT NOT NULL CHECK(authority_kind IN ('baseline','outcome')),
 evidence_outcome_id TEXT, projection_version INTEGER NOT NULL, policy_sha256 TEXT NOT NULL,
 PRIMARY KEY(repo_id,generation,direction),
 FOREIGN KEY(repo_id,generation,projection_version,policy_sha256) REFERENCES pair_lineages(repo_id,generation,projection_version,policy_sha256) ON DELETE RESTRICT,
 FOREIGN KEY(repo_id,generation,direction,source_key,evidence_outcome_id,projection_version,policy_sha256) REFERENCES pair_outcomes(repo_id,generation,direction,source_key,id,projection_version,policy_sha256) ON DELETE RESTRICT,
 CHECK((authority_kind='baseline' AND evidence_outcome_id IS NULL) OR (authority_kind='outcome' AND evidence_outcome_id IS NOT NULL)),
 CHECK((direction='svn_to_git' AND handled_svn_rev IS NOT NULL AND handled_svn_rev>0 AND handled_git_sha IS NULL AND emitted_svn_rev IS NULL AND source_key='svn:'||handled_svn_rev) OR
       (direction='git_to_svn' AND handled_git_sha IS NOT NULL AND length(handled_git_sha) IN (40,64) AND handled_git_sha NOT GLOB '*[^0-9a-f]*' AND handled_svn_rev IS NULL AND emitted_git_sha IS NULL AND source_key='git:'||handled_git_sha)),
 CHECK(emitted_git_sha IS NULL OR (length(emitted_git_sha) IN (40,64) AND emitted_git_sha NOT GLOB '*[^0-9a-f]*')),
 CHECK(emitted_svn_rev IS NULL OR emitted_svn_rev>0)
);
CREATE TABLE legacy_evidence_links (
 repo_id TEXT NOT NULL, generation INTEGER NOT NULL,
 legacy_table TEXT NOT NULL CHECK(legacy_table IN ('commit_map','sync_records','kv_state','watermarks','import_progress')),
 legacy_key TEXT NOT NULL, interpretation TEXT NOT NULL,
 PRIMARY KEY(repo_id,generation,legacy_table,legacy_key), UNIQUE(legacy_table,legacy_key),
 FOREIGN KEY(repo_id,generation) REFERENCES pair_lineages(repo_id,generation) ON DELETE RESTRICT
);
CREATE TRIGGER lineage_immutable BEFORE UPDATE ON pair_lineages BEGIN SELECT RAISE(ABORT,'lineage is immutable'); END;
CREATE TRIGGER frontier_initial BEFORE INSERT ON pair_frontiers BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM pair_frontiers WHERE repo_id=NEW.repo_id AND generation=NEW.generation AND direction=NEW.direction) OR NEW.authority_kind!='baseline' OR NOT EXISTS(
 SELECT 1 FROM pair_lineages l WHERE l.repo_id=NEW.repo_id AND l.generation=NEW.generation AND
 l.projection_version=NEW.projection_version AND l.policy_sha256=NEW.policy_sha256 AND
 ((NEW.direction='svn_to_git' AND NEW.handled_svn_rev=l.baseline_svn_rev AND NEW.emitted_git_sha=l.baseline_git_sha) OR
  (NEW.direction='git_to_svn' AND NEW.handled_git_sha=l.baseline_git_sha AND NEW.emitted_svn_rev IS NULL)))
 THEN RAISE(ABORT,'initial frontier requires proved baseline') END;
END;
CREATE TRIGGER frontier_advance BEFORE UPDATE ON pair_frontiers BEGIN
 SELECT CASE WHEN NEW.authority_kind!='outcome' OR NEW.repo_id!=OLD.repo_id OR NEW.generation!=OLD.generation OR NEW.direction!=OLD.direction OR
 NOT EXISTS(SELECT 1 FROM pair_outcomes o WHERE o.id=NEW.evidence_outcome_id AND o.repo_id=NEW.repo_id AND o.generation=NEW.generation AND o.direction=NEW.direction AND o.source_key=NEW.source_key AND o.predecessor_source_key=OLD.source_key AND o.projection_version=NEW.projection_version AND o.policy_sha256=NEW.policy_sha256 AND o.outcome IN ('applied_verified','filtered_no_target','empty_no_target','semantic_no_delta') AND o.target_git_sha IS NEW.emitted_git_sha AND o.target_svn_rev IS NEW.emitted_svn_rev)
 OR (NEW.direction='svn_to_git' AND NEW.handled_svn_rev<=OLD.handled_svn_rev)
 THEN RAISE(ABORT,'frontier requires matching resolved outcome transition') END;
END;
CREATE TRIGGER frontier_no_delete BEFORE DELETE ON pair_frontiers BEGIN SELECT RAISE(ABORT,'frontier cannot be deleted'); END;
CREATE TRIGGER outcome_cited_immutable BEFORE UPDATE ON pair_outcomes WHEN EXISTS(SELECT 1 FROM pair_frontiers WHERE evidence_outcome_id=OLD.id) BEGIN SELECT RAISE(ABORT,'cited outcome is immutable'); END;

CREATE TRIGGER lineage_no_replace BEFORE INSERT ON pair_lineages WHEN EXISTS(SELECT 1 FROM pair_lineages WHERE repo_id=NEW.repo_id AND generation=NEW.generation) BEGIN SELECT RAISE(ABORT,'lineage cannot be replaced'); END;
CREATE TRIGGER outcome_cited_no_replace BEFORE INSERT ON pair_outcomes WHEN EXISTS(SELECT 1 FROM pair_frontiers WHERE evidence_outcome_id=NEW.id) BEGIN SELECT RAISE(ABORT,'cited outcome cannot be replaced'); END;
