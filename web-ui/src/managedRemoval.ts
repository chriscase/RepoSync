import { accountStorageKey, clientSession } from './session.ts';
/** Managed root-repository removal (#65) — distinct from pause/disable. */

export interface ManagedRemovalRecovery {
  operation_id: string;
  name: string;
  last_svn_rev: number;
  last_git_sha: string;
  commit_map_count: number;
  remote_git: string;
  remote_svn: string;
  restore_supported: boolean;
  retention: string;
}

export interface ManagedRemovalPartialCleanup {
  outcome_detail?: string | null;
  registration_listed: boolean;
  remote_git: string;
  remote_svn: string;
  retry_is_local_cleanup_only: boolean;
}

export interface ManagedRemovalStatus {
  ok: boolean;
  action: 'managed_remove';
  state: string;
  operation_id: string;
  updated_at?: string;
  message: string;
  warnings?: string[];
  remote_git: string;
  remote_svn: string;
  restore_supported: boolean;
  retryable: boolean;
  registration_listed: boolean;
  recovery?: ManagedRemovalRecovery | null;
  partial_cleanup?: ManagedRemovalPartialCleanup | null;
  protocol?: number;
  request_id?: string | null;
  review_pin?: string | null;
  reviewed?: {
    git: { phase: string; errors: string[] };
    svn: { phase: string; errors: string[] };
    cleanup_started: boolean;
    cleanup_completed: boolean;
    cleanup_errors?: string[];
  } | null;
}

export interface ReviewedRemovalPreview {
  protocol: 2;
  repo_id: string;
  review_pin: string;
  delete_git: boolean;
  delete_svn: boolean;
  git: { remote_url: string; reference: string; expected_sha: string | null } | null;
  svn: { root_url: string; uuid: string; url: string; base_revision: number; expected_present: boolean } | null;
  dependency_preview: RemovalDependencyPreview;
  retention: string;
}

/** The confirmation and send use the same visible target and choices. */
export function reviewedRemovalReady(review: ReviewedRemovalPreview | undefined, repoId: string | undefined, options: { delete_git: boolean; delete_svn: boolean }, unavailable: boolean): review is ReviewedRemovalPreview {
  return !unavailable && review?.protocol === 2 && review.repo_id === repoId
    && /^[a-f0-9]{64}$/.test(review.review_pin)
    && typeof review.delete_git === 'boolean' && typeof review.delete_svn === 'boolean'
    && review.delete_git === options.delete_git && review.delete_svn === options.delete_svn
    && review.dependency_preview?.parent_removal_blocked === false;
}

const PENDING_REVIEW_PREFIX = 'reposync.pendingReviewedRemoval.';
/** Save the original approved request before the destructive send. A lost
 * response must not silently turn a new preview into a second approval. */
export function retainPendingRemovalReview(repoId: string, review: ReviewedRemovalPreview): void {
  if (review.repo_id !== repoId) throw new Error('Removal review belongs to another repository');
  const key = `${accountStorageKey(PENDING_REVIEW_PREFIX)}${repoId}`;
  const existing = readPendingRemovalReview(repoId);
  if (existing && existing.review_pin !== review.review_pin) throw new Error('Resolve the retained removal request before approving different targets');
  localStorage.setItem(key, JSON.stringify(review));
}

export function readPendingRemovalReview(repoId: string): ReviewedRemovalPreview | null {
  const key = `${accountStorageKey(PENDING_REVIEW_PREFIX)}${repoId}`;
  const raw = localStorage.getItem(key);
  if (!raw) return null;
  const parsed = JSON.parse(raw) as ReviewedRemovalPreview;
  if (!reviewedRemovalReady(parsed, repoId, { delete_git: parsed.delete_git, delete_svn: parsed.delete_svn }, false)) throw new Error('Retained removal review is malformed; do not authorize another request');
  return parsed;
}

export function clearPendingRemovalReview(repoId: string, pin: string): void {
  if (readPendingRemovalReview(repoId)?.review_pin !== pin) return;
  localStorage.removeItem(`${accountStorageKey(PENDING_REVIEW_PREFIX)}${repoId}`);
}

export type ManagedRemovalVisualState =
  | 'cancelling'
  | 'queued'
  | 'running'
  | 'reconciliation_required'
  | 'failed'
  | 'completed';

const TERMINAL = new Set(['completed', 'failed', 'reconciliation_required']);

export function managedRemovalIsTerminal(state: string | undefined): boolean {
  return TERMINAL.has(`${state || ''}`.toLowerCase());
}

export function managedRemovalNeedsPoll(state: string | undefined): boolean {
  return !!state && !managedRemovalIsTerminal(state);
}

/** Failed or unavailable dependency evidence never authorizes removal. */
export function removalDependencyPreviewConfirmReady(args: {
  isLoading: boolean;
  isFetching: boolean;
  isError: boolean;
  dependencyPreview?: RemovalDependencyPreview | null;
}): boolean {
  if (args.isLoading || args.isFetching) {
    return false;
  }
  if (args.dependencyPreview) {
    return true;
  }
  return false;
}

export function managedRemovalStateLabel(state: string | undefined, protocol?: number): string {
  switch (`${state ?? ''}`.toLowerCase()) {
    case 'cancelling':
      return 'Cancelling in-flight work';
    case 'queued':
      return 'Removal queued';
    case 'running':
      return protocol === 2 ? 'Removal running; inspect remote and cleanup outcomes' : 'Cleaning owned local data';
    case 'reconciliation_required':
      return 'Blocked by unresolved external effect';
    case 'failed':
      return protocol === 2 ? 'Removal failed; evidence retained' : 'Local cleanup failed';
    case 'completed':
      return protocol === 2 ? 'Removal completion reported' : 'Removed from RepoSync';
    default:
      return state ?? '';
  }
}

const DELETION_LABELS: Record<string, string> = {
  not_requested: 'Not requested; preserved',
  prepared: 'Approved target; deletion not started',
  intent_recorded: 'Intent recorded; collected result not recorded',
  verified_deleted: 'Deletion verified',
  verified_already_absent: 'Already absent at the approved target',
  failed_before_effect: 'Failed before an external effect',
  reconciliation_required: 'Held; effect or collection evidence is unresolved',
};
const DELETION_SUMMARIES: Record<string, string> = { not_requested: 'untouched', prepared: 'pending', intent_recorded: 'reconciliation_required', verified_deleted: 'deleted', verified_already_absent: 'already_absent', failed_before_effect: 'failed', reconciliation_required: 'reconciliation_required' };
export function reviewedRemovalDisplay(status: ManagedRemovalStatus) {
  const p = status.reviewed;
  const unknown = { known: false, remoteDone: false, git: 'Unknown; do not infer deletion', svn: 'Unknown; do not infer deletion', cleanup: 'Unknown', errors: ['Removal evidence is unavailable or contradictory. Do not infer completion or authorize different work.'] };
  if (status.protocol !== 2 || !p || typeof p.cleanup_started !== 'boolean' || typeof p.cleanup_completed !== 'boolean'
    || !Array.isArray(p.cleanup_errors) || !p.cleanup_errors.every(e => typeof e === 'string')
    || ![p.git, p.svn].every(r => r && Object.prototype.hasOwnProperty.call(DELETION_LABELS, r.phase) && Array.isArray(r.errors) && r.errors.every(e => typeof e === 'string'))
    || status.remote_git !== DELETION_SUMMARIES[p.git.phase] || status.remote_svn !== DELETION_SUMMARIES[p.svn.phase]) return unknown;
  const remoteDone = [p.git, p.svn].every(r => ['not_requested', 'verified_deleted', 'verified_already_absent'].includes(r.phase));
  if ((p.cleanup_completed && (!p.cleanup_started || !remoteDone || status.state !== 'completed' || !status.ok))
    || (status.state === 'completed' && (!p.cleanup_completed || !remoteDone))) return unknown;
  return {
    known: true,
    remoteDone,
    git: DELETION_LABELS[p.git.phase],
    svn: DELETION_LABELS[p.svn.phase],
    cleanup: p.cleanup_completed ? 'Completed' : p.cleanup_started ? 'Started; completion not recorded' : 'Not started',
    errors: [...p.git.errors.map(e => `Git: ${e}`), ...p.svn.errors.map(e => `SVN: ${e}`), ...p.cleanup_errors.map(e => `Local admission/cleanup: ${e}`)],
  };
}

export function managedRemovalRetryLabel(status: ManagedRemovalStatus, pending: boolean): string {
  if (status.protocol !== 2) return pending ? 'Retrying local cleanup…' : 'Retry local cleanup';
  if (reviewedRemovalDisplay(status).remoteDone) return pending ? 'Retrying owned local cleanup…' : 'Retry owned local cleanup';
  return pending ? 'Checking original reviewed operation…' : 'Continue original reviewed operation';
}

export function managedRemovalPanelHeading(status: ManagedRemovalStatus): string {
  if (status.protocol !== 2) return managedRemovalStateLabel(status.state);
  const view = reviewedRemovalDisplay(status);
  if (!view.known) return 'Removal evidence unavailable';
  if (status.state === 'completed') return 'Removal completed; verified cleanup recorded';
  return managedRemovalStateLabel(status.state, 2);
}

const RECEIPT_PREFIX = 'reposync.managedRemovalReceipt.';
const LEGACY_RECEIPT_KEY = 'reposync.managedRemovalReceipt';

export interface ManagedRemovalReceipt {
  repoId: string;
  operationId: string;
  state: string;
  message: string;
  remote_git: string;
  remote_svn: string;
  restore_supported: boolean;
  retryable: boolean;
  registration_listed: boolean;
  recovery?: ManagedRemovalRecovery | null;
  partial_cleanup?: ManagedRemovalPartialCleanup | null;
  updated_at: string;
  protocol?: number;
  reviewed?: ManagedRemovalStatus['reviewed'];
}

export function managedRemovalReceiptFromStatus(
  repoId: string,
  status: ManagedRemovalStatus,
  updatedAt?: string,
): ManagedRemovalReceipt {
  return {
    repoId,
    operationId: status.operation_id,
    state: status.state,
    message: status.message,
    remote_git: status.remote_git,
    remote_svn: status.remote_svn,
    restore_supported: status.restore_supported,
    retryable: status.retryable,
    registration_listed: status.registration_listed,
    recovery: status.recovery,
    partial_cleanup: status.partial_cleanup,
    updated_at: updatedAt ?? status.updated_at ?? new Date().toISOString(),
    protocol: status.protocol,
    reviewed: status.reviewed,
  };
}

/** Poll responses must not resurrect a cleared receipt or overwrite a newer POST receipt. */
export function shouldPersistPolledManagedRemovalReceipt(
  stored: ManagedRemovalReceipt | null,
  polled: ManagedRemovalStatus,
): boolean {
  if (!stored) {
    return false;
  }
  if (stored.operationId !== polled.operation_id) {
    return false;
  }
  const polledAt = polled.updated_at;
  if (polledAt && stored.updated_at && polledAt < stored.updated_at) {
    return false;
  }
  return true;
}

export function managedRemovalStatusFromReceipt(
  receipt: ManagedRemovalReceipt,
): ManagedRemovalStatus {
  return {
    ok: receipt.state.toLowerCase() === 'completed',
    action: 'managed_remove',
    state: receipt.state,
    operation_id: receipt.operationId,
    message: receipt.message,
    remote_git: receipt.remote_git,
    remote_svn: receipt.remote_svn,
    restore_supported: receipt.restore_supported,
    retryable: receipt.retryable,
    registration_listed: receipt.registration_listed,
    recovery: receipt.recovery,
    partial_cleanup: receipt.partial_cleanup,
    protocol: receipt.protocol,
    reviewed: receipt.reviewed,
  };
}

function receiptStorageKey(repoId: string): string {
  return `${accountStorageKey(RECEIPT_PREFIX)}${repoId}`;
}

function migrateLegacyReceipt(): void {
  try {
    // Unscoped legacy evidence has no authenticated owner; retain it without adopting it.
    if (typeof localStorage !== 'undefined' && clientSession().snapshot().token) return;
    const raw = sessionStorage.getItem(LEGACY_RECEIPT_KEY);
    if (!raw) return;
    const parsed = JSON.parse(raw) as ManagedRemovalReceipt;
    if (parsed?.repoId && parsed?.operationId) {
      persistManagedRemovalReceipt({
        ...parsed,
        remote_git: parsed.remote_git ?? 'untouched',
        remote_svn: parsed.remote_svn ?? 'untouched',
        restore_supported: parsed.restore_supported ?? false,
        retryable: parsed.retryable ?? parsed.state !== 'completed',
        registration_listed: parsed.registration_listed ?? parsed.state !== 'completed',
        updated_at: parsed.updated_at ?? new Date().toISOString(),
      });
    }
    sessionStorage.removeItem(LEGACY_RECEIPT_KEY);
  } catch {
    /* ignore */
  }
}

export function persistManagedRemovalReceipt(receipt: ManagedRemovalReceipt, required = false): void {
  try {
    const payload: ManagedRemovalReceipt = {
      ...receipt,
      updated_at: receipt.updated_at || new Date().toISOString(),
    };
    localStorage.setItem(receiptStorageKey(receipt.repoId), JSON.stringify(payload));
  } catch (error) {
    if (required) throw error;
    /* Legacy best-effort notice persistence remains unchanged. */
  }
}

function parseStoredReceipt(raw: string): ManagedRemovalReceipt | null {
  const parsed = JSON.parse(raw) as ManagedRemovalReceipt;
  if (!parsed?.repoId || !parsed?.operationId) return null;
  return parsed;
}

export function readAllManagedRemovalReceipts(): ManagedRemovalReceipt[] {
  migrateLegacyReceipt();
  const receipts: ManagedRemovalReceipt[] = [];
  try {
    for (let i = 0; i < localStorage.length; i += 1) {
      const key = localStorage.key(i);
      if (!key?.startsWith(accountStorageKey(RECEIPT_PREFIX))) continue;
      const raw = localStorage.getItem(key);
      if (!raw) continue;
      const parsed = parseStoredReceipt(raw);
      if (parsed) receipts.push(parsed);
    }
    receipts.sort((a, b) => {
      const at = a.updated_at ?? '';
      const bt = b.updated_at ?? '';
      return bt.localeCompare(at);
    });
    return receipts;
  } catch {
    return [];
  }
}

export function readManagedRemovalReceipt(repoId?: string): ManagedRemovalReceipt | null {
  migrateLegacyReceipt();
  try {
    if (repoId) {
      const raw = localStorage.getItem(receiptStorageKey(repoId));
      if (!raw) return null;
      return parseStoredReceipt(raw);
    }
    const all = readAllManagedRemovalReceipts();
    return all[0] ?? null;
  } catch {
    return null;
  }
}

export function clearManagedRemovalReceipt(repoId?: string): void {
  try {
    if (repoId) {
      localStorage.removeItem(receiptStorageKey(repoId));
      return;
    }
    for (let i = localStorage.length - 1; i >= 0; i -= 1) {
      const key = localStorage.key(i);
      if (key?.startsWith(accountStorageKey(RECEIPT_PREFIX))) {
        localStorage.removeItem(key);
      }
    }
    sessionStorage.removeItem(LEGACY_RECEIPT_KEY);
  } catch {
    /* ignore */
  }
}

/** Gate for list views: invalid or partial receipts must not render a notice. */
export function shouldDisplayManagedRemovalReceipt(
  receipt: ManagedRemovalReceipt | null,
): receipt is ManagedRemovalReceipt {
  return (
    receipt != null
    && typeof receipt.repoId === 'string'
    && receipt.repoId.length > 0
    && typeof receipt.operationId === 'string'
    && receipt.operationId.length > 0
  );
}

export interface RemovalChildRef {
  id: string;
  name: string;
  git_branch: string;
  svn_branch: string;
  enabled: boolean;
}

export interface RemovalDependencyPreview {
  repo_id: string;
  repo_name: string;
  parent: { id: string; name: string } | null;
  children: RemovalChildRef[];
  parent_removal_blocked: boolean;
  block_reason: string | null;
  credentials: Array<{
    key: string;
    action: string;
    retained_for_repo_ids: string[];
    inheriting_repo_ids: string[];
  }>;
  managed_local_path: string;
  sibling_local_paths_preserved: string[];
  shared_git_registrations: Array<{
    id: string;
    name: string;
    git_branch: string;
    svn_branch: string;
    relationship: string;
  }>;
}

export interface RemovalPreviewResponse {
  ok: boolean;
  action: 'removal_preview';
  dependency_preview: RemovalDependencyPreview;
  active_removal?: ManagedRemovalStatus | null;
}
