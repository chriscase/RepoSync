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

export function managedRemovalStateLabel(state: string): string {
  switch (state.toLowerCase()) {
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
      return state;
  }
}

const RECEIPT_KEY = 'reposync.managedRemovalReceipt';

export interface ManagedRemovalReceipt {
  repoId: string;
  operationId: string;
  state: string;
  message: string;
  recovery?: ManagedRemovalRecovery | null;
}

export function persistManagedRemovalReceipt(receipt: ManagedRemovalReceipt): void {
  try {
    sessionStorage.setItem(RECEIPT_KEY, JSON.stringify(receipt));
  } catch {
    /* ignore quota */
  }
}

export function readManagedRemovalReceipt(): ManagedRemovalReceipt | null {
  try {
    const raw = sessionStorage.getItem(RECEIPT_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as ManagedRemovalReceipt;
    if (!parsed?.repoId || !parsed?.operationId) return null;
    return parsed;
  } catch {
    return null;
  }
}

export function clearManagedRemovalReceipt(): void {
  try {
    sessionStorage.removeItem(RECEIPT_KEY);
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
