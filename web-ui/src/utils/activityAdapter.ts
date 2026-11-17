import type { ActivityEvent } from '@appforgeapps/uiforge';
import type { AuditEntry, SyncRecord } from '../api';

/**
 * Human-readable labels for audit action types.
 */
const ACTION_LABELS: Record<string, string> = {
  sync_cycle: 'Sync Cycle',
  conflict_detected: 'Conflict Detected',
  conflict_resolved: 'Conflict Resolved',
  sync_error: 'Sync Error',
  webhook_received: 'Webhook Received',
  daemon_started: 'Daemon Started',
  auth_login: 'Auth Login',
  config_updated: 'Config Updated',
};

export interface AuditActionOption { value: string; label: string }
export interface AuditActionGroup { label: string; options: readonly AuditActionOption[] }

/**
 * The action filter on /audit (#264, matrix row G07.1). These are the actions the server
 * writes to the audit log that /api/audit reads, taken from the Rust source
 * (crates/core sync_engine, import, managed_remove and queries; crates/daemon scheduler;
 * crates/web api/repos). The demonstration seed's actions (sync_error, webhook_received,
 * daemon_started, auth_login, config_updated, conflict_detected, conflict_resolved) are
 * not written by any shipped code path and are not listed. An action in the address that
 * is not listed here is still shown and still filters.
 */
export const AUDIT_ACTION_GROUPS: readonly AuditActionGroup[] = [
  { label: 'Sync', options: [
    { value: 'sync_cycle', label: 'Sync Cycles' },
    { value: 'sync_trigger', label: 'Sync Requests' },
    { value: 'sync_refused', label: 'Sync Refused' },
    { value: 'svn_to_git_no_target', label: 'SVN Revisions With Nothing to Apply' },
    { value: 'svn_to_git_apply_failed', label: 'SVN Revisions That Failed to Apply' },
    { value: 'skip_commit', label: 'Commits Skipped' },
  ] },
  { label: 'Path policy', options: [
    { value: 'path_violation_filtered', label: 'Path Violations Filtered' },
    { value: 'path_violation_skipped', label: 'Path Violations Skipped' },
  ] },
  { label: 'Import', options: [
    { value: 'import_full', label: 'Full History Imports' },
    { value: 'import_snapshot', label: 'Snapshot Imports' },
    { value: 'file_policy_skip', label: 'Files Skipped by Policy' },
    { value: 'reserved_metadata_exclude', label: 'Reserved Metadata Excluded' },
    { value: 'unsupported_hardlink_reject', label: 'Hard Links Rejected' },
  ] },
  { label: 'Automatic reconcile', options: [
    { value: 'auto_reconcile_finalized', label: 'Auto Reconcile Finalized' },
    { value: 'auto_reconcile_resume_authorized', label: 'Auto Reconcile Resume Authorized' },
    { value: 'auto_reconcile_still_held', label: 'Auto Reconcile Still Held' },
    { value: 'auto_reconcile_skipped', label: 'Auto Reconcile Skipped' },
    { value: 'auto_reconcile_failed', label: 'Auto Reconcile Failed' },
  ] },
  { label: 'Managed removal', options: [
    { value: 'managed_remove_accepted', label: 'Managed Removal Accepted' },
    { value: 'reviewed_managed_remove_accepted', label: 'Reviewed Removal Accepted' },
    { value: 'managed_remove_completed', label: 'Managed Removal Completed' },
    { value: 'managed_remove_failed', label: 'Managed Removal Failed' },
    { value: 'managed_remove_restored', label: 'Registration Restored' },
  ] },
  { label: 'Errors', options: [
    { value: 'errors_acknowledged', label: 'Errors Acknowledged' },
  ] },
];

/** True when the action filter lists this action; an unlisted one gets its own option. */
export function isListedAuditAction(action: string): boolean {
  return AUDIT_ACTION_GROUPS.some(group => group.options.some(option => option.value === action));
}

/** Outcome of an audit entry as a word; the page pairs it with a mark. */
export function auditOutcomeWord(success: boolean): 'Success' | 'Failed' {
  return success ? 'Success' : 'Failed';
}

/** Shown in the repository column when the entry records no repository. */
export const AUDIT_REPOSITORY_NOT_RECORDED = 'Not recorded';

/**
 * The repository an audit entry records, named from the repository list when it is known.
 * An id the list does not name (removed, or the list is unavailable) is shown as the id.
 * An entry without a repository returns undefined, except an error acknowledgement whose
 * own details say it covered all repositories (the server writes no repository for it).
 */
export function auditRepositoryName(
  entry: Pick<AuditEntry, 'repo_id' | 'action' | 'details'>,
  repos?: readonly { id: string; name: string }[],
): string | undefined {
  if (entry.repo_id) return repos?.find(repo => repo.id === entry.repo_id)?.name ?? `Repository ${entry.repo_id}`;
  if (entry.action !== 'errors_acknowledged' || !entry.details) return undefined;
  try {
    const details = JSON.parse(entry.details) as { schema?: unknown; scope?: unknown } | null;
    return details?.schema === 'reposync.error-ack.v1' && details.scope === 'all' ? 'All repositories' : undefined;
  } catch {
    return undefined;
  }
}

export type AuditListState = 'invalid' | 'failed' | 'stale' | 'loading' | 'no-matches' | 'nothing-on-page' | 'ready';

/**
 * Which state the audit list is in. A failed refresh that still holds the last page for
 * the same filters is 'stale': the earlier entries stay, marked as such, beside the error.
 */
export function auditListState(read: {
  filterError?: string;
  isError: boolean;
  isPending: boolean;
  page?: { total: number; entries: readonly unknown[] };
}): AuditListState {
  if (read.filterError) return 'invalid';
  if (read.isError) return read.page?.entries.length ? 'stale' : 'failed';
  if (read.isPending || !read.page) return 'loading';
  if (!read.page.entries.length) return read.page.total === 0 ? 'no-matches' : 'nothing-on-page';
  return 'ready';
}

/**
 * Convert an AuditEntry to an ActivityEvent for UIForgeActivityStream.
 *
 * The `type` field encodes both action and success so that UIForge's grouping
 * only groups entries with the same (action, success) pair — matching the
 * existing groupEntries() behavior.
 */
export function auditEntryToActivityEvent(
  entry: AuditEntry,
  repoName?: string,
): ActivityEvent {
  const baseAction = entry.action;
  const type = entry.success ? baseAction : `${baseAction}_failed`;
  const title = ACTION_LABELS[baseAction] ?? baseAction.replace(/_/g, ' ');

  return {
    id: entry.id,
    type,
    title: entry.success ? title : `${title} (Failed)`,
    description: entry.details ?? undefined,
    timestamp: entry.created_at,
    metadata: {
      repository: repoName ?? 'default',
      action: baseAction,
      success: entry.success,
      direction: entry.direction,
      svn_rev: entry.svn_rev,
      git_sha: entry.git_sha,
      author: entry.author,
    },
  };
}

/**
 * Convert a SyncRecord to an ActivityEvent for UIForgeActivityStream.
 *
 * Uses direction as the type so consecutive SVN→Git or Git→SVN syncs
 * are grouped together (e.g., batch imports).
 */
export function syncRecordToActivityEvent(
  record: SyncRecord,
  repoName?: string,
): ActivityEvent {
  const type = `sync_${record.direction}`;
  const truncatedMsg =
    record.message.length > 200
      ? record.message.slice(0, 197) + '...'
      : record.message;

  return {
    id: record.id,
    type,
    title: truncatedMsg || `${record.direction === 'svn_to_git' ? 'SVN \u2192 Git' : 'Git \u2192 SVN'} sync`,
    description: record.message,
    timestamp: record.synced_at,
    metadata: {
      repository: repoName ?? 'default',
      direction: record.direction,
      svn_rev: record.svn_rev,
      git_sha: record.git_sha,
      author: record.author,
      status: record.status,
      committed_at: record.timestamp,
    },
  };
}
