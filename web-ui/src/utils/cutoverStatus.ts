import type { Repository } from '../api';

/**
 * Outcome of computing the catch-up status of a duplicated repository
 * (a "clone": a row whose `cloned_from_id` is set) relative to its source.
 *
 * The intent is to give the operator a one-glance answer to the question
 * "is it safe to disable the source pair yet?" after triggering a
 * "Duplicate to new destination" migration.
 */
export type CutoverStatus = {
  /**
   * Machine-readable state. `caught-up` means the clone's processed
   * SVN revision is at least the source's, the clone is idle, and it
   * has no unresolved errors — i.e. the operator can safely disable
   * the source pair now. The other states surface why the clone is
   * not (yet) ready.
   */
  state:
    | 'caught-up'
    | 'catching-up'
    | 'errors'
    | 'source-missing'
    | 'pre-import';
  /** Short label suitable for a badge. */
  label: string;
  /** Optional secondary line with the underlying numbers. */
  detail?: string;
  /** How many SVN revisions the clone is behind the source. Only set
   * when `state === 'catching-up'`. */
  revsBehind?: number;
};

/**
 * Pure computation over fields already returned by `GET /api/repos` and
 * `GET /api/repos/:id` — no new API calls. Returns `null` when the
 * input is not a duplicate (i.e. `clone.cloned_from_id` is unset), so
 * callers can branch on the return value to decide whether to render
 * the indicator at all.
 *
 * Decision order (highest priority first):
 *
 *   1. Source row missing from the repository list (deleted, or fetch
 *      hasn't returned yet) → `source-missing`.
 *   2. Clone has unresolved consecutive errors → `errors` ("review
 *      before cutover"). We use `consecutive_errors` rather than the
 *      lifetime `total_errors` counter so historical transient errors
 *      don't permanently red-flag a healthy clone.
 *   3. Source itself has no processed revisions yet (rare) →
 *      `pre-import`. The comparison wouldn't be meaningful here.
 *   4. Clone's `last_svn_rev >= source.last_svn_rev` AND status is idle
 *      → `caught-up`.
 *   5. Otherwise → `catching-up`, with `revsBehind` computed.
 */
export function computeCutoverStatus(
  clone: Repository,
  source: Repository | null | undefined,
): CutoverStatus | null {
  if (!clone.cloned_from_id) return null;

  if (!source) {
    return {
      state: 'source-missing',
      label: 'Source unavailable',
      detail: `cloned_from_id ${clone.cloned_from_id} could not be resolved.`,
    };
  }

  const consecutive = clone.consecutive_errors ?? 0;
  if (consecutive > 0) {
    return {
      state: 'errors',
      label: 'Errors — review before cutover',
      detail: `${consecutive} consecutive error${consecutive === 1 ? '' : 's'} on the clone.`,
    };
  }

  const cloneRev = clone.last_svn_rev ?? 0;
  const sourceRev = source.last_svn_rev ?? 0;

  if (sourceRev <= 0) {
    return {
      state: 'pre-import',
      label: 'Waiting for source data',
      detail: 'The source pair has no processed revisions yet.',
    };
  }

  const idle =
    clone.sync_status === 'idle' || clone.sync_status === '' || clone.sync_status == null;
  if (cloneRev >= sourceRev && idle) {
    return {
      state: 'caught-up',
      label: 'Caught up — safe to disable source',
      detail: `Clone at r${cloneRev}, matching source.`,
    };
  }

  const behind = Math.max(sourceRev - cloneRev, 0);
  return {
    state: 'catching-up',
    label: behind > 0 ? `Catching up (${behind} rev${behind === 1 ? '' : 's'} behind)` : 'Catching up',
    detail: `Clone at r${cloneRev}, source at r${sourceRev}.`,
    revsBehind: behind,
  };
}

/**
 * Tailwind class fragments for rendering a `CutoverStatus` badge.
 * Kept here so the colour vocabulary stays consistent everywhere the
 * indicator appears (detail page, repository list, future surfaces).
 */
export function cutoverBadgeClasses(state: CutoverStatus['state']): {
  container: string;
  dot: string;
} {
  switch (state) {
    case 'caught-up':
      return {
        container: 'bg-green-900/40 border-green-700 text-green-300',
        dot: 'bg-green-400',
      };
    case 'catching-up':
      return {
        container: 'bg-yellow-900/40 border-yellow-700 text-yellow-300',
        dot: 'bg-yellow-400',
      };
    case 'errors':
      return {
        container: 'bg-red-900/40 border-red-700 text-red-300',
        dot: 'bg-red-400',
      };
    case 'pre-import':
    case 'source-missing':
    default:
      return {
        container: 'bg-gray-800/60 border-gray-600 text-gray-300',
        dot: 'bg-gray-400',
      };
  }
}
