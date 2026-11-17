import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement, Fragment } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { api, type AuditEntry, type Repository } from '../api';
import { auditFilters, auditPageKey } from '../listPagination';
import AuditLog, { AuditStaleNote, AuditTableView } from './AuditLog';
import { renderAuditEvent } from '../components/ActivityEventRenderers';
import {
  AUDIT_ACTION_GROUPS,
  AUDIT_REPOSITORY_NOT_RECORDED,
  auditEntryToActivityEvent,
  auditListState,
  auditOutcomeWord,
  auditRepositoryName,
  isListedAuditAction,
} from '../utils/activityAdapter';

const stamp = '2026-11-04T01:00:00Z';
/** Actions written by the shipped Rust code to the audit log /api/audit reads (epic/reposync-partial-52). */
const PRODUCTION_ACTIONS = [
  'sync_cycle', 'sync_trigger', 'sync_refused', 'svn_to_git_no_target', 'svn_to_git_apply_failed', 'skip_commit',
  'path_violation_filtered', 'path_violation_skipped',
  'import_full', 'import_snapshot', 'file_policy_skip', 'reserved_metadata_exclude', 'unsupported_hardlink_reject',
  'auto_reconcile_finalized', 'auto_reconcile_resume_authorized', 'auto_reconcile_still_held', 'auto_reconcile_skipped', 'auto_reconcile_failed',
  'managed_remove_accepted', 'reviewed_managed_remove_accepted', 'managed_remove_completed', 'managed_remove_failed', 'managed_remove_restored',
  'errors_acknowledged',
];
/** Written only by the demonstration seed (or by library code no shipped path calls). */
const SEED_ONLY_ACTIONS = ['sync_error', 'webhook_received', 'daemon_started', 'auth_login', 'config_updated', 'conflict_detected', 'conflict_resolved'];

function entry(over: Partial<AuditEntry> = {}): AuditEntry {
  return { id: 1, repo_id: 'A', action: 'sync_cycle', direction: 'svn_to_git', svn_rev: 4127, git_sha: '9f3c2ab4deadbeef', author: 'mei', details: 'retained evidence', created_at: stamp, success: true, ...over };
}
function auditPage(entries: AuditEntry[], total = entries.length, page = 1) {
  return { entries, total, page, per_page: 50, total_pages: Math.ceil(total / 50), offset: (page - 1) * 50 };
}
function repo(id: string, name = `Repository name ${id}`): Repository {
  return { id, name, git_branch: 'main', svn_branch: 'trunk', enabled: true, updated_at: stamp, created_at: stamp } as Repository;
}
const ackAll = JSON.stringify({ schema: 'reposync.error-ack.v1', scope: 'all', through_id: 9, observed_at: stamp, acknowledged: 4 });

const fetchMock = vi.fn();
let client: QueryClient;
beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false, retryOnMount: false, staleTime: Infinity } } });
  const stored = new Map([['session_token', 'fixture-token'], ['user', '{"id":"actor","role":"admin"}']]);
  vi.stubGlobal('localStorage', { getItem: (k: string) => stored.get(k) ?? null, setItem: (k: string, v: string) => stored.set(k, v), removeItem: (k: string) => stored.delete(k), get length() { return stored.size; }, key: (i: number) => Array.from(stored.keys())[i] ?? null });
  vi.stubGlobal('window', { location: { pathname: '/audit', href: '/audit' } });
  vi.stubGlobal('fetch', fetchMock); fetchMock.mockReset();
  client.setQueryData(['repos'], [repo('A', 'kiln-controller'), repo('B', 'greenhouse-telemetry')]);
});
afterEach(() => { client.clear(); vi.unstubAllGlobals(); });
function renderPage(path: string) {
  return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(MemoryRouter, { initialEntries: [path] }, createElement(AuditLog))));
}
function seed(path: string, value: ReturnType<typeof auditPage>) {
  const filters = auditFilters(new URLSearchParams(path.split('?')[1] ?? ''));
  client.setQueryData(auditPageKey(filters), value);
  return filters;
}
async function failRead(filters: ReturnType<typeof auditFilters>) {
  fetchMock.mockResolvedValueOnce(new Response('{"error":"read failed"}', { status: 502 }));
  await expect(client.fetchQuery({ queryKey: auditPageKey(filters), queryFn: () => api.getAuditLog(50, filters.page, filters.success, filters.repoId, filters.action), staleTime: 0, retry: false })).rejects.toThrow();
}
const selectFor = (html: string, label: string) => html.match(new RegExp(`<select[^>]*aria-label="${label}"[^>]*>([\\s\\S]*?)</select>`))?.[1] ?? '';

describe('#264 G07.1 action filter is drawn from the actions production writes', () => {
  it('lists every production action once and none of the seed-only actions', () => {
    const values = AUDIT_ACTION_GROUPS.flatMap(g => g.options.map(o => o.value));
    expect(new Set(values).size).toBe(values.length);
    expect([...values].sort()).toEqual([...PRODUCTION_ACTIONS].sort());
    for (const action of SEED_ONLY_ACTIONS) expect(isListedAuditAction(action)).toBe(false);
    for (const group of AUDIT_ACTION_GROUPS) { expect(group.label).toBeTruthy(); expect(group.options.length).toBeGreaterThan(0); }
    expect(AUDIT_ACTION_GROUPS[0].options[0]).toEqual({ value: 'sync_cycle', label: 'Sync Cycles' });
    expect(isListedAuditAction('')).toBe(false);
  });
  it('renders All Actions and grouped production options; no unlisted option when the address names a listed action', () => {
    seed('/audit?action=sync_refused', auditPage([entry({ action: 'sync_refused', success: false })]));
    const options = selectFor(renderPage('/audit?action=sync_refused'), 'Action filter');
    expect(options).toContain('<option value="">All Actions</option>');
    expect(options).toContain('<optgroup label="Sync">');
    expect(options).toContain('<option value="sync_refused" selected="">Sync Refused</option>');
    expect(options).toContain('<option value="errors_acknowledged">Errors Acknowledged</option>');
    expect(options.match(/value="sync_refused"/g)).toHaveLength(1);
    for (const action of SEED_ONLY_ACTIONS) expect(options).not.toContain(`value="${action}"`);
  });
  it('keeps an action from the address that the list does not name, first, and still filtering', () => {
    seed('/audit?action=config_updated', auditPage([entry({ action: 'config_updated' })]));
    const options = selectFor(renderPage('/audit?action=config_updated'), 'Action filter');
    expect(options.indexOf('<option value="config_updated" selected="">config_updated</option>')).toBe(0);
    expect(options.indexOf('config_updated')).toBeLessThan(options.indexOf('All Actions'));
  });
  it('shows the view mode and the result filter as labelled pressed groups; only the pressed button is primary', () => {
    seed('/audit?success=false', auditPage([entry({ success: false })]));
    const html = renderPage('/audit?success=false');
    expect(html).toContain('role="group" aria-label="Result"');
    expect(html).toContain('role="group" aria-label="View"');
    const buttons = [...html.matchAll(/<button type="button" aria-pressed="(true|false)"[^>]*class="([^"]*)"[^>]*>([^<]*)<\/button>/g)].map(m => ({ pressed: m[1], cls: m[2], label: m[3] }));
    expect(buttons.map(b => `${b.label}:${b.pressed}`)).toEqual(['All:false', 'Success:false', 'Failures:true', 'Stream:true', 'Table:false']);
    for (const b of buttons) expect(b.cls.includes('rs-btn--primary')).toBe(b.pressed === 'true');
    expect(html).toContain('1 failures on this page');
    expect(html).toContain('Showing 1 of 1 matching entries');
  });
  it('a failed repository list is said to have failed, not shown as an empty choice', async () => {
    client.removeQueries({ queryKey: ['repos'] });
    fetchMock.mockResolvedValueOnce(new Response('{"error":"down"}', { status: 503 }));
    await expect(client.fetchQuery({ queryKey: ['repos'], queryFn: api.getRepos, retry: false })).rejects.toThrow();
    seed('/audit', auditPage([entry()]));
    expect(selectFor(renderPage('/audit'), 'Repository filter')).toContain('<option value="" disabled="">Repository list could not be loaded</option>');
  });
  it('an invalid filter is refused with Clear filters, and no counts are claimed', () => {
    const html = renderPage('/audit?success=maybe');
    expect(html).toContain('role="alert">Invalid success filter. Choose all, success or failures.');
    expect(html).toContain('Clear filters');
    expect(html).toContain('Success count unknown');
    expect(html).toContain('Matching total unknown');
    expect(html).not.toContain('No matching audit entries');
  });
});

describe('#264 G07.3 table: outcome as a mark and a word, and a repository column', () => {
  const repos = [repo('A', 'kiln-controller'), repo('B', 'greenhouse-telemetry')];
  const rows = [
    entry({ id: 1 }),
    entry({ id: 2, repo_id: 'B', action: 'sync_refused', success: false, direction: null, svn_rev: null, git_sha: null, author: 'scheduler', details: 'writer busy; refused mutable prep' }),
    entry({ id: 3, repo_id: null, action: 'errors_acknowledged', direction: null, svn_rev: null, git_sha: null, author: 'ines', details: ackAll }),
    entry({ id: 4, repo_id: null, action: 'import_full', details: 'Imported 12 revisions' }),
    entry({ id: 5, repo_id: 'gone', action: 'managed_remove_completed', details: 'completed' }),
  ];
  const table = (expandedId: number | null = null) => renderToStaticMarkup(createElement(AuditTableView, { entries: rows, repos, expandedId, onToggleExpand: () => {} }));

  it('heads the columns Outcome, Time, Repository, then today’s columns unchanged', () => {
    const heads = [...table().matchAll(/<th scope="col"[^>]*>\s*([^<]*?)\s*<\/th>/g)].map(m => m[1]);
    expect(heads).toEqual(['Outcome', 'Time', 'Repository', 'Action', 'Direction', 'Details', 'Author', 'SVN Rev', 'Git SHA']);
    expect(table()).toContain('<caption class="sr-only">Audit entries</caption>');
    expect(table()).toMatch(/role="region" aria-label="Audit entries" tabindex="0"/);
  });
  it('shows each outcome as a hidden-from-readers mark followed by the word', () => {
    const cells = [...table().matchAll(/<td[^>]*><span class="inline-flex items-center gap-1 text-sm font-medium (text-green-400|text-red-400)"><svg[^>]*aria-hidden="true"[^>]*>[\s\S]*?<\/svg>(Success|Failed)<\/span><\/td>/g)].map(m => `${m[1]}:${m[2]}`);
    expect(cells).toEqual(['text-green-400:Success', 'text-red-400:Failed', 'text-green-400:Success', 'text-green-400:Success', 'text-green-400:Success']);
    expect(table()).not.toContain('title="Success"');
    expect(auditOutcomeWord(true)).toBe('Success');
    expect(auditOutcomeWord(false)).toBe('Failed');
  });
  it('names the repository from data in the entry; an entry without one says so', () => {
    const html = table();
    expect(html).toContain('>kiln-controller<');
    expect(html).toContain('>greenhouse-telemetry<');
    expect(html).toContain('>All repositories<');
    expect(html).toContain('>Repository gone<');
    expect(html.match(new RegExp(`>${AUDIT_REPOSITORY_NOT_RECORDED}<`, 'g'))).toHaveLength(1);
  });
  it('keeps the details disclosure wording and spans the new column count when expanded', () => {
    expect(table()).toContain('aria-expanded="false" class="text-left text-blue-300 underline">Show audit details: writer busy; refused mutable prep</button>');
    const html = table(2);
    expect(html).toContain('aria-expanded="true" class="text-left text-blue-300 underline">Hide audit details: ');
    expect(html).toContain('colSpan="9"');
    expect(html).toContain('Audit ID');
    expect(html).toContain('#2');
  });
});

describe('#264 audit repository names', () => {
  const repos = [repo('A', 'kiln-controller')];
  it.each([
    ['a known repository', { repo_id: 'A', action: 'sync_cycle', details: null }, repos, 'kiln-controller'],
    ['an id the list does not name', { repo_id: 'Z', action: 'sync_cycle', details: null }, repos, 'Repository Z'],
    ['an id while the list is unavailable', { repo_id: 'A', action: 'sync_cycle', details: null }, undefined, 'Repository A'],
    ['no repository', { repo_id: null, action: 'sync_cycle', details: null }, repos, undefined],
    ['an absent repository field', { action: 'sync_trigger', details: 'x' }, repos, undefined],
    ['an all-repository acknowledgement', { repo_id: null, action: 'errors_acknowledged', details: ackAll }, repos, 'All repositories'],
    ['an acknowledgement with unreadable details', { repo_id: null, action: 'errors_acknowledged', details: '{not json' }, repos, undefined],
    ['an acknowledgement that does not say all', { repo_id: null, action: 'errors_acknowledged', details: JSON.stringify({ schema: 'reposync.error-ack.v1', scope: 'repository' }) }, repos, undefined],
    ['another action with all-scope details', { repo_id: null, action: 'sync_cycle', details: ackAll }, repos, undefined],
  ] as const)('%s', (_name, value, list, expected) => {
    expect(auditRepositoryName(value as Pick<AuditEntry, 'repo_id' | 'action' | 'details'>, list)).toBe(expected);
  });
});

describe('#264 G07.4 list states are shown truthfully', () => {
  it.each([
    [{ filterError: 'Invalid page. Return to the first page.', isError: true, isPending: false }, 'invalid'],
    [{ isError: true, isPending: false }, 'failed'],
    [{ isError: true, isPending: false, page: { total: 0, entries: [] } }, 'failed'],
    [{ isError: true, isPending: false, page: { total: 3, entries: [1, 2, 3] } }, 'stale'],
    [{ isError: false, isPending: true }, 'loading'],
    [{ isError: false, isPending: false, page: { total: 0, entries: [] } }, 'no-matches'],
    [{ isError: false, isPending: false, page: { total: 65, entries: [] } }, 'nothing-on-page'],
    [{ isError: false, isPending: false, page: { total: 1, entries: [1] } }, 'ready'],
  ] as const)('%j is %s', (read, expected) => {
    expect(auditListState(read)).toBe(expected);
  });
  it('a failed refresh keeps the earlier entries, marked as possibly out of date, beside the error and Retry', async () => {
    const filters = seed('/audit?repo_id=A', auditPage([entry({ details: 'earlier evidence' })]));
    await failRead(filters);
    expect(client.getQueryData(auditPageKey(filters))).toBeDefined();
    const html = renderPage('/audit?repo_id=A');
    expect(html).toContain('role="alert" class="text-red-300">Error loading audit log: API error 502');
    expect(html).toContain('Results are unknown.');
    expect(html).toContain('>Retry</button>');
    expect(html).toMatch(/role="status"[^>]*>The entries below were last loaded at [^<]+ and may be out of date\.</);
    expect(html).toContain('earlier evidence');
    expect(html).toContain('Success count unknown');
    expect(html).toContain('Matching total unknown');
    expect(html).not.toContain('Page 1 of');
    expect(html).not.toContain('No matching audit entries');
  });
  it('a failed first read shows the failure only, with no stale note and no entries', async () => {
    await failRead(auditFilters(new URLSearchParams('repo_id=B')));
    const html = renderPage('/audit?repo_id=B');
    expect(html).toContain('Error loading audit log');
    expect(html).toContain('>Retry</button>');
    expect(html).not.toContain('may be out of date');
    expect(html).not.toContain('No matching audit entries');
  });
  it('a stale note without a load time still says the entries are from an earlier read', () => {
    expect(renderToStaticMarkup(createElement(AuditStaleNote, { loadedAt: 0 }))).toContain('The entries below are from an earlier read and may be out of date.');
  });
  it('no matches and nothing on this page stay distinct, with Return to first page for the latter', () => {
    seed('/audit?action=sync_refused', auditPage([], 0));
    expect(renderPage('/audit?action=sync_refused')).toContain('role="status">No matching audit entries</div>');
    seed('/audit?page=9', auditPage([], 65, 9));
    const late = renderPage('/audit?page=9');
    expect(late).toContain('No audit entries on this page; matching records exist on other pages.');
    expect(late).toContain('>Return to first page</button>');
  });
});

describe('#264 stream entries carry the repository and a visible failure word', () => {
  const repos = [repo('A', 'kiln-controller')];
  const render = (value: AuditEntry) => renderToStaticMarkup(createElement(Fragment, null, renderAuditEvent(auditEntryToActivityEvent(value, auditRepositoryName(value, repos)))));
  it('names the repository recorded in the entry, and nothing for an entry without one', () => {
    expect(render(entry())).toContain('>kiln-controller</span>');
    expect(render(entry({ repo_id: null }))).not.toContain('kiln-controller');
  });
  it('says Failed in words for a failed entry and keeps the labelled success mark for a successful one', () => {
    const failed = render(entry({ success: false }));
    expect(failed).toMatch(/<span aria-hidden="true" class="inline-block w-2 h-2 rounded-full bg-red-400"><\/span>Failed<\/span>/);
    expect(failed).not.toContain('aria-label="Failed"');
    expect(render(entry())).toContain('role="img" aria-label="Success"');
  });
  it('shows an unreadable timestamp as it was received instead of a fabricated age', () => {
    expect(render(entry({ created_at: 'not-a-time' }))).toContain('>not-a-time</span>');
  });
});
