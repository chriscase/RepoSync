import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement, type ComponentType } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { api, type CommitMapEntry, type Repository, type SyncStatus, type SystemMetrics } from '../api';
import Dashboard from './Dashboard';
import ErrorAcknowledgement, { ErrorAcknowledgementDialog } from '../components/ErrorAcknowledgement';
import SyncStatusView, { HealthMark } from '../components/SyncStatus';
import ServerMonitor from '../components/ServerMonitor';
import { errorStatusQuery } from '../errorStatus';
import {
  acknowledgedFeedback, acknowledgementConfirmLabel, acknowledgementFailure, acknowledgementPrompt, acknowledgementTitle, errorScope,
} from '../errorAcknowledgement';
import { statusObservationView, type EffectiveHealth, type StatusOperation } from '../statusObservation';

// This project installs no Node type definitions (see LayoutFoundation.test.ts), so node:fs is
// loaded through a specifier the type checker does not resolve. Vitest runs these tests in Node.
const NODE_FS = 'node:fs';
const readSource = async (path: URL) => ((await import(/* @vite-ignore */ NODE_FS)) as { readFileSync(path: URL, encoding: 'utf8'): string }).readFileSync(path, 'utf8');

const stamp = '2026-11-17T09:00:00Z';
const fetchMock = vi.fn();
let client: QueryClient;
let storage: Map<string, string>;

function repo(id: string, parent: string | null = null): Repository {
  return { id, name: parent ? `${parent} / ${id}` : id, git_branch: 'main', svn_branch: 'trunk', enabled: true, parent_id: parent, updated_at: stamp, created_at: stamp } as unknown as Repository;
}
function heldImport(id: string): StatusOperation {
  return { repository_id: id, kind: 'import', state: 'reconciliation_required', operation_id: null, updated_at: null, reason: null, target_ref: null, intended_git_sha: null, confirmed_svn_revision: null, held: true, details_visible: false };
}
function status(id: string | undefined, health: EffectiveHealth, operations: StatusOperation[] = []): SyncStatus {
  return {
    state: 'idle', last_sync_at: null, last_svn_revision: null, last_git_hash: null, total_syncs: 4, total_conflicts: 0, active_conflicts: 0,
    total_errors: 0, last_error_at: null, error_observation: { through_id: '1', observed_at: stamp }, uptime_secs: 0,
    observation: {
      version: 1, scope: id ? 'repository' : 'aggregate', repository_id: id ?? null, configured: 'enabled', health, observed_at: stamp,
      data_freshness: 'current', last_successful_sync_at: null, remote_freshness: 'not_verified_by_status', registry_busy: false,
      evidence_complete: true, operations, safe_next_action: `Next step for ${id ?? 'all'}`, uptime_available: false,
    },
  } as SyncStatus;
}
async function failRead(queryKey: readonly unknown[], queryFn: () => Promise<unknown>, code = 502) {
  fetchMock.mockResolvedValueOnce(new Response('{"error":"bad gateway"}', { status: code }));
  await expect(client.fetchQuery({ queryKey, queryFn, retry: false })).rejects.toThrow();
}
function render(component: ComponentType, path = '/') {
  return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(MemoryRouter, { initialEntries: [path] }, createElement(component))));
}
function renderNode(node: Parameters<typeof renderToStaticMarkup>[0]) {
  return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(MemoryRouter, null, node)));
}
/** The markup of one region, from its root to the next region at the same level. */
function region(html: string, name: string) {
  const start = html.indexOf(`data-rs-region="${name}"`);
  expect(start).toBeGreaterThan(-1);
  const next = html.slice(start + 1).search(/data-rs-region="/);
  return next === -1 ? html.slice(start) : html.slice(start, start + 1 + next);
}
const text = (html: string) => html.replace(/<[^>]+>/g, '').replace(/&quot;/g, '"').replace(/&#x27;/g, "'").replace(/&amp;/g, '&');

beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false, retryOnMount: false, staleTime: Infinity } } });
  storage = new Map([['session_token', 'fixture-token'], ['user', '{"id":"actor","role":"admin"}']]);
  vi.stubGlobal('localStorage', { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, v: string) => storage.set(key, v), removeItem: (key: string) => storage.delete(key), get length() { return storage.size; }, key: (index: number) => Array.from(storage.keys())[index] ?? null });
  vi.stubGlobal('window', { location: { pathname: '/', href: '/' } });
  vi.stubGlobal('fetch', fetchMock);
  fetchMock.mockReset();
});
afterEach(() => { client.clear(); vi.unstubAllGlobals(); });

describe('#264 dashboard page regions', () => {
  it('marks the page and its regions in today\'s order, with Needs attention derived and right after the scope', () => {
    client.setQueryData(['repos'], [repo('A'), repo('B')]);
    const html = render(Dashboard, '/');
    expect(html).toContain('class="rs-page space-y-6" data-rs-page="dashboard"');
    const order = ['scope', 'attention', 'imports', 'repositories', 'health', 'records', 'commit-map', 'activity', 'server'];
    const positions = order.map(name => html.indexOf(`data-rs-region="${name}"`));
    expect(positions.every(position => position > -1)).toBe(true);
    expect([...positions].sort((a, b) => a - b)).toEqual(positions);
    expect(html).not.toContain('data-rs-region="position"');
    expect(html).toMatch(/<section data-rs-region="attention" data-rs-derived=""/);
    // Only the new region is derived; the existing ones keep their markup.
    expect(html.match(/data-rs-derived/g)).toHaveLength(1);
  });

  it('adds the position region between health and records when one repository is in scope', () => {
    client.setQueryData(['repos'], [repo('A'), repo('B')]);
    const html = render(Dashboard, '/?repo_id=A');
    const health = html.indexOf('data-rs-region="health"'), position = html.indexOf('data-rs-region="position"'), records = html.indexOf('data-rs-region="records"');
    expect(health).toBeLessThan(position);
    expect(position).toBeLessThan(records);
  });

  it('puts imports on each import card (no wrapper), and wraps the three health siblings with the page spacing', () => {
    client.setQueryData(['repos'], [repo('A'), repo('A1', 'A'), repo('B')]);
    const html = render(Dashboard, '/');
    // One card per root repository, each card's own root is the region.
    expect(html.match(/<div class="rs-sec [^"]*" data-rs-region="imports"/g)).toHaveLength(2);
    expect(html).toContain('<div data-rs-region="health" class="space-y-6"><section aria-label="Repository status and evidence"');
    expect(render(Dashboard, '/?repo_id=A').match(/data-rs-region="imports"/g)).toHaveLength(1);
  });
});

describe('#264 G02.7 recent sync records', () => {
  it('shows a failed read as failed, with the client error text, never as "No sync records yet"', async () => {
    await failRead(['sync-records', undefined], () => api.getSyncRecords(20));
    const records = region(render(Dashboard, '/'), 'records');
    expect(records).toContain('role="alert"');
    expect(records).toContain('Sync records could not be loaded.');
    expect(records).toContain('Results are unknown. This is not the same as none.');
    expect(text(records)).toContain('API error 502: {"error":"bad gateway"}');
    expect(records).not.toContain('No sync records yet');
  });
  it('shows a pending read as loading, not as empty', () => {
    const records = region(render(Dashboard, '/'), 'records');
    expect(records).toContain('Loading sync records…');
    expect(records).not.toContain('No sync records yet');
  });
  it('keeps "No sync records yet" for a confirmed empty read', () => {
    client.setQueryData(['sync-records', undefined], { entries: [], total: 0 });
    const records = region(render(Dashboard, '/'), 'records');
    expect(records).toContain('No sync records yet');
    expect(records).not.toContain('could not be loaded');
  });
});

describe('#264 G02.8 commit map', () => {
  const entry: CommitMapEntry = { id: 1, repo_id: 'A', svn_rev: 41, git_sha: 'a'.repeat(40), direction: 'svn_to_git', synced_at: stamp, svn_author: 'mei', git_author: 'mei' };
  it('shows a failed read as failed, with the client error text, never as "No commit mappings yet"', async () => {
    await failRead(['commit-map', undefined], () => api.getCommitMap(15), 500);
    const map = region(render(Dashboard, '/'), 'commit-map');
    expect(map).toContain('role="alert"');
    expect(map).toContain('Commit map could not be loaded.');
    expect(text(map)).toContain('API error 500');
    expect(map).not.toContain('No commit mappings yet');
    expect(map).not.toContain('<table');
  });
  it('shows a pending read as loading, and keeps the empty and ready states', () => {
    expect(region(render(Dashboard, '/'), 'commit-map')).toContain('Loading commit map…');
    client.setQueryData(['commit-map', undefined], { entries: [], total: 0 });
    expect(region(render(Dashboard, '/'), 'commit-map')).toContain('No commit mappings yet');
    client.setQueryData(['commit-map', undefined], { entries: [entry, { ...entry, id: 2, svn_rev: 42, git_sha: 'b'.repeat(40) }], total: 2 });
    const map = region(render(Dashboard, '/'), 'commit-map');
    expect(map).toContain('<table');
    expect(map).toContain('r41');
    // The Git column still renders for every row.
    expect(map).toContain('a'.repeat(12));
    expect(map.match(/<td class="px-6 py-3 text-sm font-mono text-purple-400 truncate max-w-\[200px\]"/g)).toHaveLength(2);
  });
});

describe('#264 G02.11 needs attention', () => {
  it('lists held, failed and stale pairs with their state word and a link to the right section', async () => {
    client.setQueryData(['repos'], [repo('A'), repo('A1', 'A'), repo('B'), repo('C'), repo('D')]);
    client.setQueryData(errorStatusQuery('A').queryKey, status('A', 'idle'));
    client.setQueryData(errorStatusQuery('A1').queryKey, status('A1', 'held', [heldImport('A1')]));
    await failRead(errorStatusQuery('B').queryKey, () => api.getStatus('B'));
    client.setQueryData(errorStatusQuery('C').queryKey, status('C', 'idle'), { updatedAt: Date.now() - 60_000 });
    client.setQueryData(errorStatusQuery('D').queryKey, status('D', 'paused'));
    const attention = region(render(Dashboard, '/'), 'attention');
    expect(attention).toContain('Needs attention');
    expect(attention).toContain('4 of 5 pairs need attention.');
    // A, A1 and D answered with a current status; B failed and C is stale.
    expect(attention).toContain('3 of 5 pairs answered with a current status.');
    // A held import goes to Operations; everything else to Overview.
    expect(attention).toContain('href="/repos/A1?section=operations"');
    expect(attention).toContain('href="/repos/B?section=overview"');
    expect(attention).toContain('href="/repos/C?section=overview"');
    expect(attention).toContain('href="/repos/D?section=overview"');
    expect(attention).not.toContain('href="/repos/A?');
    const t = text(attention);
    expect(t).toContain('Held for reconciliation');
    expect(t).toContain('Operation: import — reconciliation_required (hold retained).');
    expect(t).toContain('Paused after error');
    // A failed read is Unknown, with the client's error text.
    expect(t).toContain('Status unavailable / malformed.');
    expect(t).toContain('API error 502: {"error":"bad gateway"}');
    // A stale read is Unknown even though its last-known health was Idle.
    expect(t).toContain('Status observation stale.');
    expect(attention).toMatch(/data-rs-health="unknown"[^>]*>.*?Unknown 2</);
    // In the order of the Repositories region.
    expect(attention.indexOf('/repos/A1?')).toBeLessThan(attention.indexOf('/repos/B?'));
    expect(attention.indexOf('/repos/B?')).toBeLessThan(attention.indexOf('/repos/C?'));
  });

  it('says so in one line when nothing needs attention', () => {
    client.setQueryData(['repos'], [repo('A'), repo('A1', 'A')]);
    client.setQueryData(errorStatusQuery('A').queryKey, status('A', 'idle'));
    client.setQueryData(errorStatusQuery('A1').queryKey, status('A1', 'running'));
    const attention = region(render(Dashboard, '/'), 'attention');
    expect(attention).toContain('Nothing needs attention: no pair is held, paused, in conflict, in error or of unknown status.');
    expect(attention).not.toContain('Pairs that need attention');
    expect(attention).not.toContain('href=');
  });

  it('never says nothing needs attention while reads are outstanding or the repository list failed', async () => {
    client.setQueryData(['repos'], [repo('A'), repo('B')]);
    client.setQueryData(errorStatusQuery('A').queryKey, status('A', 'idle'));
    let attention = region(render(Dashboard, '/'), 'attention');
    expect(attention).toContain('Still reading status for 1 of 2 pairs. None of the others needs attention.');
    expect(attention).not.toContain('Nothing needs attention');
    client.removeQueries({ queryKey: ['status'] });
    expect(region(render(Dashboard, '/'), 'attention')).toContain('Reading status for 2 pairs…');
    client.removeQueries({ queryKey: ['repos'] });
    await failRead(['repos'], () => api.getRepos(), 500);
    attention = region(render(Dashboard, '/'), 'attention');
    expect(attention).toContain('Repositories could not be read.');
    expect(attention).toContain('What needs attention is unknown.');
    expect(text(attention)).toContain('API error 500');
    expect(attention).not.toContain('Nothing needs attention');
  });

  it('lists a current read without a supported observation, or with incomplete evidence, as Unknown and says why', () => {
    client.setQueryData(['repos'], [repo('A'), repo('B')]);
    const legacy = status('A', 'idle');
    delete (legacy as { observation?: unknown }).observation;
    client.setQueryData(errorStatusQuery('A').queryKey, legacy);
    const partial = status('B', 'idle');
    partial.observation!.evidence_complete = false;
    client.setQueryData(errorStatusQuery('B').queryKey, partial);
    const t = text(region(render(Dashboard, '/'), 'attention'));
    expect(t).toContain('2 of 2 pairs need attention.');
    expect(t).toContain('No supported observation is available. Health, freshness and operation evidence remain unknown.');
    expect(t).toContain('Supported durable-read coverage: incomplete / unknown.');
  });

  it('is read-only: links only, no buttons, forms or fields', () => {
    client.setQueryData(['repos'], [repo('A')]);
    client.setQueryData(errorStatusQuery('A').queryKey, status('A', 'conflict'));
    const attention = region(render(Dashboard, '/'), 'attention');
    expect(attention).toContain('href="/repos/A?section=overview"');
    expect(attention).not.toMatch(/<(button|form|input|select|textarea)\b/);
  });
});

describe('#264 G02.5 error acknowledgement in the shared dialog', () => {
  const scope = errorScope('A');
  const props = { isOpen: true, scope, repositoryName: 'Alpha', count: 3, available: true, pending: false, onConfirm: () => {}, onCancel: () => {} };
  it('shows the existing prompt word for word in a native dialog, Cancel first and marked as the default focus', () => {
    const html = renderNode(createElement(ErrorAcknowledgementDialog, props));
    expect(html).toMatch(/^<dialog /);
    expect(html).toContain(acknowledgementTitle);
    expect(text(html)).toContain(acknowledgementPrompt(scope, 'Alpha'));
    expect(acknowledgementPrompt(errorScope(undefined), '')).toBe('Acknowledge the displayed recent errors for All Repositories (global scope)? Original audit evidence will be preserved. New errors will remain visible.');
    expect(html).toMatch(/<button type="button" data-dialog-cancel="true"[^>]*>Cancel<\/button>/);
    expect(html.indexOf('Cancel')).toBeLessThan(html.indexOf('Acknowledge 3 errors'));
    expect(html).not.toMatch(/<button type="button" disabled=""[^>]*>Acknowledge 3 errors/);
  });
  it('shows pending, failure and no-longer-current states', () => {
    const pending = renderNode(createElement(ErrorAcknowledgementDialog, { ...props, pending: true }));
    expect(pending).toContain('aria-busy="true"');
    expect(pending.match(/<button type="button"[^>]*disabled=""/g)).toHaveLength(2);
    const failed = renderNode(createElement(ErrorAcknowledgementDialog, { ...props, error: 'API error 403: forbidden' }));
    expect(failed).toContain('<p role="alert" class="mb-3 text-sm text-red-300">API error 403: forbidden</p>');
    const unavailable = renderNode(createElement(ErrorAcknowledgementDialog, { ...props, available: false }));
    expect(unavailable).toContain('The error status is no longer current');
    expect(unavailable).toMatch(/<button type="button" disabled=""[^>]*>Acknowledge 3 errors/);
    expect(renderNode(createElement(ErrorAcknowledgementDialog, { ...props, isOpen: false }))).toBe('');
  });
  it('keeps the Acknowledge control and its wording, and no longer uses the browser prompt', async () => {
    const control = renderNode(createElement(ErrorAcknowledgement, { scope, repositoryName: 'Alpha', observation: { through_id: '9', observed_at: stamp }, count: 2, onSuccess: () => {} }));
    expect(control).toMatch(/<button type="button" class="rs-btn rs-btn--quiet[^"]*">Acknowledge<\/button>/);
    expect(control).not.toContain('<dialog');
    const unavailable = renderNode(createElement(ErrorAcknowledgement, { scope, repositoryName: 'Alpha', observation: undefined, count: 0, onSuccess: () => {} }));
    expect(unavailable).toMatch(/<button type="button" disabled=""[^>]*>Acknowledge<\/button>/);
    storage.set('user', '{"id":"viewer","role":"user"}');
    expect(renderNode(createElement(ErrorAcknowledgement, { scope, repositoryName: 'Alpha', observation: { through_id: '9', observed_at: stamp }, count: 2, onSuccess: () => {} }))).toBe('');
    const source = await readSource(new URL('../components/ErrorAcknowledgement.tsx', import.meta.url));
    expect(source).not.toMatch(/window\.(confirm|prompt)/);
    expect(source).toContain('api.acknowledgeErrors(scope, confirming.observation)');
  });
  it('keeps the feedback wording and labels the confirmation with what it does', () => {
    expect(acknowledgementConfirmLabel(1)).toBe('Acknowledge 1 error');
    expect(acknowledgementConfirmLabel(3)).toBe('Acknowledge 3 errors');
    expect(acknowledgedFeedback(2)).toEqual({ error: false, text: 'Acknowledged 2. Audit evidence preserved.' });
    expect(acknowledgementFailure(new Error('API error 403: forbidden'))).toEqual({ error: true, text: 'API error 403: forbidden' });
    expect(acknowledgementFailure('opaque')).toEqual({ error: true, text: 'Acknowledgement failed. Try again.' });
  });
});

describe('#264 S03 status in the shell', () => {
  const now = Date.now();
  const cases: [string, Parameters<typeof statusObservationView>[0], EffectiveHealth][] = [
    ['idle', { data: status(undefined, 'idle'), dataUpdatedAt: now }, 'idle'],
    ['held', { data: status(undefined, 'held', [heldImport('A')]), dataUpdatedAt: now }, 'held'],
    ['stale', { data: status(undefined, 'idle'), dataUpdatedAt: now - 60_000 }, 'unknown'],
    ['unavailable', { error: new Error('API error 502'), isError: true }, 'unknown'],
    ['loading', {}, 'unknown'],
  ];
  it.each(cases)('%s: a mark and the health word, with the same text as before', (_name, query, health) => {
    const html = renderToStaticMarkup(createElement(SyncStatusView, { query }));
    const view = statusObservationView(query, now);
    // Screen-reader output stays "<health>. <availability>."
    expect(text(html)).toBe(`${view.healthLabel}. ${view.availability}.`);
    expect(html).toMatch(/^<div role="status" aria-live="polite" aria-atomic="true" class="rs-shell-status /);
    expect(html).toContain(`<span data-rs-health="${health}" class="rs-mark `);
    expect(html).toMatch(/<svg[^>]*aria-hidden="true"/);
    // The colour dot alone is gone.
    expect(html).not.toContain('rounded-full');
  });
  it('gives every health its own shape', () => {
    const healths: EffectiveHealth[] = ['idle', 'running', 'initializing', 'busy', 'disabled', 'paused', 'conflict', 'held', 'error', 'unknown'];
    const shapes = healths.map(health => renderToStaticMarkup(createElement(HealthMark, { health, label: health })).match(/<svg[^>]*class="([^"]*)"/)?.[1]);
    expect(new Set(shapes).size).toBe(healths.length);
  });
});

describe('#264 server region', () => {
  const metrics = { disk_free_bytes: 1, disk_total_bytes: 2, disk_usage_percent: 50, mem_used_bytes: 1, mem_total_bytes: 2, mem_usage_percent: 50, cpu_load_1m: 0, cpu_load_5m: 0, cpu_load_15m: 0, git_push_active: false, git_push_pid: null, git_push_elapsed_secs: null, data_dir_size_bytes: 0, net_bytes_sent: 0, net_bytes_recv: 0, net_up_bytes_per_sec: 0, net_down_bytes_per_sec: 0, svn_active: false } as unknown as SystemMetrics;
  it('puts the region attributes the page gives it on its one root, loading or loaded, and none by default', () => {
    expect(renderNode(createElement(ServerMonitor, { 'data-rs-region': 'server', 'data-rs-sections': 'overview' }))).toMatch(/^<div class="bg-gray-900 border border-gray-700 rounded-lg" data-rs-region="server" data-rs-sections="overview">/);
    expect(renderNode(createElement(ServerMonitor))).toMatch(/^<div class="bg-gray-900 border border-gray-700 rounded-lg">/);
    client.setQueryData(['system-metrics'], metrics);
    const loaded = renderNode(createElement(ServerMonitor, { 'data-rs-region': 'server' }));
    expect(loaded).toMatch(/^<div class="bg-gray-900 border border-gray-700 rounded-lg" data-rs-region="server">/);
    expect(loaded).toContain('Server Monitor');
  });
});
