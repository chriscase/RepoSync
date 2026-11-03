import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { api, type SyncStatus } from './api';
import { errorStatusQuery, errorStatusView, refreshErrorStatus } from './errorStatus';
import { errorScope } from './errorAcknowledgement';

const fetchMock = vi.fn();
let client: QueryClient;
let storage: Map<string, string>;
const initial: SyncStatus = {
  state: 'idle', last_sync_at: null, last_svn_revision: null, last_git_hash: null,
  total_syncs: 0, total_conflicts: 0, active_conflicts: 0, total_errors: 2,
  last_error_at: '2026-11-03T10:00:00Z', uptime_secs: 0,
  error_observation: { through_id: '2', observed_at: '2026-11-03T10:00:01Z' },
};
beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  storage = new Map([['session_token', 'token'], ['user', '{"role":"admin"}']]);
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => storage.get(key) ?? null,
    removeItem: (key: string) => storage.delete(key),
  });
  vi.stubGlobal('window', { location: { pathname: '/repos/a', href: '/repos/a' } });
  vi.stubGlobal('fetch', fetchMock);
  fetchMock.mockReset();
});
afterEach(() => { client.clear(); vi.unstubAllGlobals(); });

describe('#114 directly used status query/view/action wiring', () => {
  it('treats HTTP 500 and transport errors as unavailable, never zero/no recent errors', async () => {
    for (const fail of [
      () => fetchMock.mockResolvedValue(new Response('{"error":"internal server error"}', { status: 500 })),
      () => fetchMock.mockRejectedValue(new Error('network unavailable')),
    ]) {
      fail();
      await expect(client.fetchQuery(errorStatusQuery('a'))).rejects.toThrow();
      const state = client.getQueryState(errorStatusQuery('a').queryKey)!;
      expect(state.status).toBe('error');
      expect(state.data).toBeUndefined();
      const view = errorStatusView(undefined, true);
      expect(view.value).toBe('Unavailable');
      expect(view.description).toContain('Unable to load');
      expect(view.description).not.toBe('No recent errors');
      expect(view.observation).toBeUndefined();
      expect(view.count).toBe(0); // action remains disabled, not a displayed counter
      client.clear();
    }
  });
  it('disables acknowledgement on failed refetch even with a cached valid observation', async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify(initial)));
    await client.fetchQuery(errorStatusQuery('a'));
    fetchMock.mockResolvedValue(new Response('{"error":"failure"}', { status: 500 }));
    await expect(client.fetchQuery({ ...errorStatusQuery('a'), staleTime: 0 })).rejects.toThrow();
    const state = client.getQueryState<SyncStatus>(errorStatusQuery('a').queryKey)!;
    expect(state.data).toEqual(initial);
    const view = errorStatusView(state.data, state.status === 'error');
    expect(view.value).toBe('Unavailable');
    expect(view.observation).toBeUndefined();
  });
  it('uses the checked status client for 401 and 403 without caching error JSON as data', async () => {
    for (const status of [403, 401]) {
      fetchMock.mockResolvedValue(new Response('{"error":"refused"}', { status }));
      await expect(client.fetchQuery(errorStatusQuery('a'))).rejects.toThrow(status === 401 ? 'Session expired' : 'API error 403');
      expect(client.getQueryData(errorStatusQuery('a').queryKey)).toBeUndefined();
      expect(storage.has('session_token')).toBe(status !== 401);
      client.clear();
    }
  });
  it('refetches the actual selected query after acknowledgement and shows a surviving new error', async () => {
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(initial)));
    await client.fetchQuery(errorStatusQuery('a'));
    client.setQueryData(errorStatusQuery('b').queryKey, { ...initial, total_errors: 7 });
    const observer = new QueryObserver(client, errorStatusQuery('a'));
    const unsubscribe = observer.subscribe(() => {});
    try {
      fetchMock.mockResolvedValueOnce(new Response('{"ok":true,"acknowledged":2}'));
      await api.acknowledgeErrors(errorScope('a'), initial.error_observation);
      const refreshed = { ...initial, total_errors: 1, error_observation: { through_id: '4', observed_at: '2026-11-03T10:01:00Z' } };
      fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(refreshed)));
      await refreshErrorStatus(client);
      expect(fetchMock.mock.calls.map(call => call[0])).toEqual([
        '/api/status?repo_id=a', '/api/status/acknowledge-errors', '/api/status?repo_id=a',
      ]);
      expect(JSON.parse(fetchMock.mock.calls[1][1].body).scope).toEqual({ kind: 'repository', repo_id: 'a' });
      const view = errorStatusView(observer.getCurrentResult().data, observer.getCurrentResult().isError);
      expect(view.value).toBe('1');
      expect(view.observation).toEqual(refreshed.error_observation);
      expect(client.getQueryData<SyncStatus>(errorStatusQuery('b').queryKey)?.total_errors).toBe(7);
    } finally { unsubscribe(); }
  });
  it('shows loading instead of claiming zero before a first successful observation', () => {
    const view = errorStatusView(undefined, false);
    expect(view.value).toBe('Loading…');
    expect(view.description).not.toBe('No recent errors');
    expect(view.observation).toBeUndefined();
  });
});
