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
}

export interface StartRepoImportRequest {
  import_mode?: RepoImportMode;
  svn_revision?: string;
}

export const DEFAULT_REPO_IMPORT_MODE: RepoImportMode = 'full';

export function normalizeImportMode(raw?: string | null): RepoImportMode {
  return raw === 'snapshot' ? 'snapshot' : 'full';
}

export function importModeLabel(mode: RepoImportMode): string {
  return mode === 'snapshot' ? 'Snapshot at pinned SVN revision' : 'Full SVN history';
}

export function startImportButtonLabel(mode: RepoImportMode): string {
  return mode === 'snapshot' ? 'Start snapshot import' : 'Start full history import';
}

/** User-facing notice; never claims earlier SVN history was imported for snapshots. */
export function importHistoryNotice(fields: ImportBaselineFields): string | null {
  if (fields.import_mode === 'snapshot') {
    if (fields.history_boundary) return fields.history_boundary;
    if (fields.starting_revision != null) {
      return `SVN history before r${fields.starting_revision} was not imported; later revisions remain pending`;
    }
    return 'Earlier SVN history was not imported; sync continues from the verified snapshot baseline.';
  }
  if (fields.import_mode === 'full' && fields.earlier_history_imported === false) {
    return fields.history_boundary ?? null;
  }
  return null;
}

export function buildStartImportBody(
  mode: RepoImportMode,
  svnRevision: string,
): StartRepoImportRequest | Record<string, never> {
  if (mode === 'full') {
    return {};
  }
  const trimmed = svnRevision.trim();
  return {
    import_mode: 'snapshot',
    svn_revision: trimmed.length > 0 ? trimmed : 'HEAD',
  };
}

export function parseSvnRevisionInput(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return 'HEAD';
  if (/^head$/i.test(trimmed)) return 'HEAD';
  const n = Number(trimmed);
  if (!Number.isInteger(n) || n < 1) {
    throw new Error('SVN revision must be HEAD or a positive integer');
  }
  return String(n);
}
