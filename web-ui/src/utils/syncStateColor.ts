/** Visual category for sync lifecycle states in status dots and nav badges. */
export type SyncStateVisual = 'healthy' | 'active' | 'attention' | 'error' | 'disabled';

export function syncStateVisual(state?: string, enabled = true): SyncStateVisual {
  if (!enabled) {
    return 'disabled';
  }
  switch (state) {
    case 'idle':
      return 'healthy';
    case 'detecting':
    case 'applying':
    case 'syncing':
    case 'initializing':
      return 'active';
    case 'error':
    case 'failed':
      return 'error';
    case 'error_paused':
    case 'reconciliation_required':
    case 'conflict_found':
      return 'attention';
    default:
      return 'active';
  }
}
