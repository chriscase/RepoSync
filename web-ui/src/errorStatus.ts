import type { QueryClient } from '@tanstack/react-query';
import { api, type SyncStatus } from './api';

/** The same query definition is used by both acknowledgement callers. */
export function errorStatusQuery(repoId?: string) {
  return {
    queryKey: ['status', repoId] as const,
    queryFn: () => api.getStatus(repoId),
  };
}

/** A scoped acknowledgement changes aggregate status too; a global one changes all scopes. */
export function refreshErrorStatus(client: QueryClient) {
  return client.invalidateQueries({ queryKey: ['status'] });
}

/** Never present missing/failed observations, including stale cached data, as zero errors. */
export function errorStatusView(status: SyncStatus | undefined, failed: boolean) {
  if (failed || !status) {
    return {
      value: failed ? 'Unavailable' : 'Loading…',
      description: failed ? 'Unable to load error status. Retrying…' : 'Loading error status…',
      count: 0,
      observation: undefined,
      lastErrorAt: null,
    };
  }
  return {
    value: String(status.total_errors),
    description: 'No recent errors',
    count: status.total_errors,
    observation: status.error_observation,
    lastErrorAt: status.last_error_at,
  };
}
