/** Shared types and copy for team SVN import baseline (full history vs snapshot). */

export type RepoImportMode = 'full' | 'snapshot';

export interface SnapshotPin {
  svn_uuid: string;
  canonical_url: string;
  operative_rev: number;
  peg_rev: number;
  copy_from_path?: string | null;
  copy_from_rev?: number | null;
  requested: string;
}

export interface ImportBaselineFields {
  import_mode?: RepoImportMode | string | null;
  starting_revision?: number | null;
  history_boundary?: string | null;
  earlier_history_imported?: boolean | null;
  snapshot_pin?: SnapshotPin | null;
  phase?: string | null;
  lifecycle?: string | null;
  outcome_detail?: string | null;
}

export interface StartRepoImportRequest {
  import_mode?: RepoImportMode;
  svn_revision?: string;
}

export const DEFAULT_REPO_IMPORT_MODE: RepoImportMode = 'full';

const POSITIVE_BASE10 = /^[1-9][0-9]*$/;

export function normalizeImportMode(raw?: string | null): RepoImportMode {
  return raw === 'snapshot' ? 'snapshot' : 'full';
}

export function importModeLabel(mode: RepoImportMode): string {
  return mode === 'snapshot' ? 'Snapshot at pinned SVN revision' : 'Full SVN history';
}

export function startImportButtonLabel(mode: RepoImportMode): string {
  return mode === 'snapshot' ? 'Start snapshot import' : 'Start full history import';
}

/** True only when the server recorded a snapshot pin / boundary (verified baseline). */
export function hasVerifiedSnapshotBaseline(fields: ImportBaselineFields): boolean {
  if (fields.snapshot_pin) return true;
  return fields.starting_revision != null && Boolean(fields.history_boundary);
}

/**
 * Boundary copy from the server only. Returns null when there is no verified pin.
 */
export function importHistoryNotice(fields: ImportBaselineFields): string | null {
  if (!hasVerifiedSnapshotBaseline(fields)) {
    return null;
  }
  if (fields.history_boundary) {
    return fields.history_boundary;
  }
  if (fields.starting_revision != null) {
    return `SVN history before r${fields.starting_revision} was not imported; later revisions remain pending`;
  }
  return null;
}

/** Refused or failed snapshot import with no verified pin — not a baseline claim. */
export function importBaselineFailureNotice(fields: ImportBaselineFields): string | null {
  if (normalizeImportMode(fields.import_mode) !== 'snapshot') {
    return null;
  }
  if (hasVerifiedSnapshotBaseline(fields)) {
    return null;
  }
  const failed =
    fields.lifecycle === 'failed'
    || fields.phase === 'failed'
    || fields.lifecycle === 'cancelled'
    || fields.phase === 'cancelled';
  if (!failed) {
    return null;
  }
  if (fields.outcome_detail?.trim()) {
    return fields.outcome_detail.trim();
  }
  return 'Snapshot import did not complete; no verified SVN baseline was recorded.';
}

export function buildStartImportBody(
  mode: RepoImportMode,
  svnRevision: string,
): StartRepoImportRequest | Record<string, never> {
  if (mode === 'full') {
    return {};
  }
  return {
    import_mode: 'snapshot',
    svn_revision: parseSvnRevisionInput(svnRevision),
  };
}

export function parseSvnRevisionInput(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return 'HEAD';
  if (/^head$/i.test(trimmed)) return 'HEAD';
  if (!POSITIVE_BASE10.test(trimmed)) {
    throw new Error('SVN revision must be HEAD or a positive base-10 integer');
  }
  return trimmed;
}

export function validateSvnRevisionInput(raw: string): string | null {
  try {
    parseSvnRevisionInput(raw);
    return null;
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}
