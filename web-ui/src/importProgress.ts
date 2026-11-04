import type { ImportStatus } from './api';

export type ProgressInput = Pick<ImportStatus, 'phase' | 'current_rev' | 'total_revs' | 'lifecycle'>;

export function importProgressView(status: ProgressInput) {
  const measured = Number.isSafeInteger(status.current_rev) && Number.isSafeInteger(status.total_revs)
    && status.total_revs > 0 && status.current_rev >= 0 && status.current_rev <= status.total_revs;
  const percent = measured ? Number(BigInt(status.current_rev) * 100n / BigInt(status.total_revs)) : undefined;
  const state = (status.lifecycle && status.lifecycle !== status.phase ? `${status.lifecycle} — ${status.phase}` : status.phase).replace(/_/g, ' ');
  // Announce phase/lifecycle and coarse progress only; poll/log-line churn is not live output.
  const announcement = `${state}. ${percent === undefined ? 'Progress not yet measurable.' : `${Math.floor(percent / 10) * 10}% of reported revisions.`}`;
  return { percent, announcement, description: percent === undefined ? `${state}: progress not yet measurable` : `${state}: ${percent}%` };
}

