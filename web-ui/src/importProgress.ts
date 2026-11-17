import type { ImportStatus } from './api';

export type ProgressInput = Pick<ImportStatus, 'phase' | 'current_rev' | 'total_revs' | 'lifecycle'>;

/**
 * S20: words for the import lifecycle values (the text #121 announces). An unrecognised value
 * is said to be unrecognised rather than read out raw; the raw values stay in the card's details.
 */
const LIFECYCLE_WORDS: Record<string, string> = {
  queued: 'queued',
  running: 'running',
  cancel_requested: 'cancel requested',
  cancelling: 'cancelling',
  completed: 'completed',
  cancelled: 'cancelled',
  failed: 'failed',
  reconciliation_required: 'reconciliation required',
};

/** The card's existing phase labels. */
const PHASE_LABELS: Record<string, string> = {
  idle: 'Idle',
  connecting: 'Connecting',
  importing: 'Importing',
  verifying: 'Verifying',
  final_push: 'Final Push',
  completed: 'Completed',
  failed: 'Failed',
  cancelled: 'Cancelled',
};

/** An unrecognised phase is said to be unrecognised, never shown raw as if it were a word. */
export function importPhaseLabel(phase: string): string {
  return PHASE_LABELS[phase] ?? 'Unrecognised state';
}

/**
 * The card's state mark. Existing words; the lifecycle decides where the phase alone would
 * misstate it (a queued, failed, stopped or completed operation whose phase has not moved).
 */
export function importStateLabel(status: Pick<ImportStatus, 'phase' | 'lifecycle'>): string {
  switch (status.lifecycle as string | undefined) {
    case 'cancel_requested':
    case 'cancelling':
      return 'Cancellation requested — stopping';
    case 'reconciliation_required':
      return 'Reconciliation required';
    case 'queued':
      return 'Queued';
    case 'failed':
      return 'Failed';
    case 'cancelled':
      return 'Cancelled';
    case 'completed':
      return 'Completed';
    case undefined:
    case 'running':
      return importPhaseLabel(status.phase);
    default:
      return 'Unrecognised state';
  }
}

/** The state text the meter announces: words for each known value, never an unrecognised raw value. */
export function importProgressState(status: Pick<ImportStatus, 'phase' | 'lifecycle'>): string {
  const phase = PHASE_LABELS[status.phase]?.toLowerCase() ?? 'unrecognised phase';
  if (!status.lifecycle || status.lifecycle === status.phase) return phase;
  return `${LIFECYCLE_WORDS[status.lifecycle] ?? 'unrecognised state'} — ${phase}`;
}

export function importProgressView(status: ProgressInput) {
  const measured = Number.isSafeInteger(status.current_rev) && Number.isSafeInteger(status.total_revs)
    && status.total_revs > 0 && status.current_rev >= 0 && status.current_rev <= status.total_revs;
  const percent = measured ? Number(BigInt(status.current_rev) * 100n / BigInt(status.total_revs)) : undefined;
  const state = importProgressState(status);
  // Announce phase/lifecycle and coarse progress only; poll/log-line churn is not live output.
  const announcement = `${state}. ${percent === undefined ? 'Progress not yet measurable.' : `${Math.floor(percent / 10) * 10}% of reported revisions.`}`;
  return { percent, announcement, description: percent === undefined ? `${state}: progress not yet measurable` : `${state}: ${percent}%` };
}
