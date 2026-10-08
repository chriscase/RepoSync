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
  message: string;
  remote_git: string;
  remote_svn: string;
  restore_supported: boolean;
  retryable: boolean;
  registration_listed: boolean;
  recovery?: ManagedRemovalRecovery | null;
  partial_cleanup?: ManagedRemovalPartialCleanup | null;
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

export function managedRemovalStateLabel(state: string | undefined): string {
  switch (`${state ?? ''}`.toLowerCase()) {
    case 'cancelling':
      return 'Cancelling in-flight work';
    case 'queued':
      return 'Removal queued';
    case 'running':
      return 'Cleaning owned local data';
    case 'reconciliation_required':
      return 'Blocked by unresolved external effect';
    case 'failed':
      return 'Local cleanup failed';
    case 'completed':
      return 'Removed from RepoSync';
    default:
      return state ?? '';
  }
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
}

export function managedRemovalReceiptFromStatus(
  repoId: string,
  status: ManagedRemovalStatus,
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
    updated_at: new Date().toISOString(),
  };
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
  };
}

function receiptStorageKey(repoId: string): string {
  return `${RECEIPT_PREFIX}${repoId}`;
}

function migrateLegacyReceipt(): void {
  try {
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

export function persistManagedRemovalReceipt(receipt: ManagedRemovalReceipt): void {
  try {
    const payload: ManagedRemovalReceipt = {
      ...receipt,
      updated_at: receipt.updated_at || new Date().toISOString(),
    };
    localStorage.setItem(receiptStorageKey(receipt.repoId), JSON.stringify(payload));
  } catch {
    /* ignore quota */
  }
}

export function readManagedRemovalReceipt(repoId?: string): ManagedRemovalReceipt | null {
  migrateLegacyReceipt();
  try {
    if (repoId) {
      const raw = localStorage.getItem(receiptStorageKey(repoId));
      if (!raw) return null;
      const parsed = JSON.parse(raw) as ManagedRemovalReceipt;
      if (!parsed?.repoId || !parsed?.operationId) return null;
      return parsed;
    }
    let latest: ManagedRemovalReceipt | null = null;
    for (let i = 0; i < localStorage.length; i += 1) {
      const key = localStorage.key(i);
      if (!key?.startsWith(RECEIPT_PREFIX)) continue;
      const raw = localStorage.getItem(key);
      if (!raw) continue;
      const parsed = JSON.parse(raw) as ManagedRemovalReceipt;
      if (!parsed?.repoId || !parsed?.operationId) continue;
      if (
        !latest
        || (parsed.updated_at && (!latest.updated_at || parsed.updated_at > latest.updated_at))
      ) {
        latest = parsed;
      }
    }
    return latest;
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
      if (key?.startsWith(RECEIPT_PREFIX)) {
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
