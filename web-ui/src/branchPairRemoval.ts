/** UI-only classification for an existing branch-pair DELETE response. */

export interface BranchPairRemovalResult {
  ok?: boolean;
  message?: string;
  warnings?: string[];
  state?: string;
  lifecycle?: string;
  operation_id?: string;
  registration_listed?: boolean;
}

export type BranchPairRemovalOutcome = 'completed' | 'in_progress' | 'rejected';

export interface BranchPairRemovalNotice {
  outcome: 'completed' | 'in_progress';
  message: string;
  warnings: string[];
  operationId?: string;
  name?: string;
}

const IN_PROGRESS_STATES = new Set([
  'queued',
  'running',
  'cancelling',
  'canceling',
  'accepted',
  'pending',
  'in_progress',
]);

export function isNotFoundError(error: unknown): boolean {
  if (!error) return false;
  const message = error instanceof Error ? error.message : String(error);
  return /\b404\b/.test(message) || /repository not found/i.test(message);
}

/**
 * Completed removal is `ok: true` without an in-progress lifecycle.
 * A queued or accepted operation is never treated as completion, even when
 * the HTTP call itself succeeded.
 */
export function branchPairRemovalOutcome(result: BranchPairRemovalResult): BranchPairRemovalOutcome {
  const state = `${result.state || result.lifecycle || ''}`.trim().toLowerCase();
  if (IN_PROGRESS_STATES.has(state)) return 'in_progress';
  if (result.ok === true) return 'completed';
  if (result.operation_id) return 'in_progress';
  return 'rejected';
}

/**
 * Destination captured for the pair that is on screen.
 * `null` means the current page (a parent deleting a child) should stay.
 * A missing parent falls back to the repository list.
 */
export function removalDestination(args: {
  viewedId: string;
  targetId: string;
  parentId: string | null;
  parentExists: boolean | null;
}): string | null {
  if (args.targetId !== args.viewedId) return null;
  if (!args.parentId || args.parentExists === false) return '/repos';
  return `/repos/${args.parentId}`;
}

export function repoDetailQueryKeys(repoId: string): unknown[][] {
  return [
    ['repo', repoId],
    ['status', repoId],
    ['sync-records', repoId],
    ['commit-map', repoId],
    ['audit', repoId],
    ['repo-credentials', repoId],
    ['branch-pairs', repoId],
    ['import-status', repoId],
  ];
}

export function readBranchPairRemovalNotice(state: unknown): BranchPairRemovalNotice | null {
  if (!state || typeof state !== 'object') return null;
  const notice = (state as { branchPairRemoval?: BranchPairRemovalNotice }).branchPairRemoval;
  if (!notice || (notice.outcome !== 'completed' && notice.outcome !== 'in_progress')) return null;
  return normalizeBranchPairRemovalNotice(notice);
}

const BRANCH_PAIR_RECEIPT_KEY = 'reposync.branchPairRemovalReceipt';

function normalizeBranchPairRemovalNotice(notice: BranchPairRemovalNotice): BranchPairRemovalNotice {
  return {
    outcome: notice.outcome,
    message: notice.message ?? '',
    warnings: Array.isArray(notice.warnings) ? notice.warnings : [],
    operationId: notice.operationId,
    name: notice.name,
  };
}

export function persistBranchPairRemovalNotice(notice: BranchPairRemovalNotice): void {
  try {
    sessionStorage.setItem(BRANCH_PAIR_RECEIPT_KEY, JSON.stringify(normalizeBranchPairRemovalNotice(notice)));
  } catch {
    /* ignore */
  }
}

export function readPersistedBranchPairRemovalNotice(): BranchPairRemovalNotice | null {
  try {
    const raw = sessionStorage.getItem(BRANCH_PAIR_RECEIPT_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as BranchPairRemovalNotice;
    if (parsed.outcome !== 'completed' && parsed.outcome !== 'in_progress') return null;
    return normalizeBranchPairRemovalNotice(parsed);
  } catch {
    return null;
  }
}

export function clearPersistedBranchPairRemovalNotice(): void {
  try {
    sessionStorage.removeItem(BRANCH_PAIR_RECEIPT_KEY);
  } catch {
    /* ignore */
  }
}
