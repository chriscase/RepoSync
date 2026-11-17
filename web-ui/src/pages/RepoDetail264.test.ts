import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import RepoDetail, {
  CredentialsSummary, RepositorySectionNav, RepositoryStatusLine, ResumeSyncDialog, SkipPrefixChoices, SyncRecordsBody,
  credentialState, fetchPreCommitHook, headerSyncSwitch, latePairStateWord, normaliseRepoSection, removePairWording,
  repositoryStatusLine, resumeSyncConsequences, saveTextDownload, sectionSearch, skipChoiceHelp, REPO_SECTIONS,
} from './RepoDetail';
import { StopImportDialog } from '../components/ImportProgressCard';
import { importPhaseLabel, importProgressState, importProgressView, importStateLabel } from '../importProgress';
import { statusObservationView } from '../statusObservation';
import type { ImportStatus, Repository, SyncRecord, SyncStatus } from '../api';

const stamp = '2026-11-17T12:00:00Z';
const now = Date.parse(stamp);
const EVERY = 'overview activity branches operations settings';

function storage() {
  const entries = new Map<string, string>();
  return {
    getItem: (key: string) => entries.get(key) ?? null,
    setItem: (key: string, value: string) => { entries.set(key, value); },
    removeItem: (key: string) => { entries.delete(key); },
    clear: () => entries.clear(),
    get length() { return entries.size; },
    key: (index: number) => Array.from(entries.keys())[index] ?? null,
  };
}

function status(overrides: Partial<SyncStatus> = {}, health: 'idle' | 'paused' | 'held' = 'idle'): SyncStatus {
  return {
    state: health === 'paused' ? 'error_paused' : 'idle', last_sync_at: stamp, last_svn_revision: 4127, last_git_hash: '9f3c2ab41c07'.padEnd(40, '0'),
    total_syncs: 12, total_conflicts: 0, active_conflicts: 0, total_errors: 0, last_error_at: null,
    error_observation: { through_id: '1', observed_at: stamp }, uptime_secs: 0,
    observation: {
      version: 1, scope: 'repository', repository_id: 'r1', configured: 'enabled', health, observed_at: stamp,
      data_freshness: 'recorded', last_successful_sync_at: stamp, remote_freshness: 'not_verified_by_status', registry_busy: false,
      evidence_complete: true,
      operations: health === 'held' ? [{ repository_id: 'r1', kind: 'import', state: 'reconciliation_required', operation_id: 'op', updated_at: stamp, reason: null, target_ref: null, intended_git_sha: null, confirmed_svn_revision: null, held: true, details_visible: true }] : [],
      safe_next_action: 'Inspect scoped evidence', uptime_available: false,
    },
    ...overrides,
  } as SyncStatus;
}

function repo(overrides: Partial<Repository> = {}): Repository {
  return {
    id: 'r1', name: 'kiln-controller', parent_id: null, svn_url: 'svn://fixture.invalid/kiln', svn_branch: 'trunk', svn_username: 'sync',
    git_provider: 'github', git_api_url: 'https://api.github.invalid', git_repo: 'maker/kiln', git_branch: 'main', sync_mode: 'direct',
    poll_interval_secs: 300, lfs_threshold_mb: 0, auto_merge: false, enabled: true, created_by: 'ines', created_at: stamp, updated_at: stamp,
    allowed_paths: '["src/"]', blocked_patterns: null,
    ...overrides,
  } as Repository;
}

const record: SyncRecord = { id: 'rec-1', svn_rev: 4127, git_sha: '9f3c2ab', direction: 'svn_to_git', author: 'mei', message: 'Tune ramp-rate limits', timestamp: stamp, synced_at: stamp, status: 'applied' };

const importIdle = { phase: 'idle', current_rev: 0, total_revs: 0, commits_created: 0, current_file_count: 0, lfs_unique_count: 0, files_skipped: 0, batches_pushed: 0, push_started_at: null, verification: null, errors: [], log_lines: [], started_at: null, completed_at: null, can_start: false } as ImportStatus;

let client: QueryClient;
const fetchMock = vi.fn();
beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  const local = storage();
  local.setItem('session_token', 'fixture-token');
  local.setItem('user', JSON.stringify({ id: 'actor', username: 'ines', role: 'admin' }));
  vi.stubGlobal('localStorage', local);
  vi.stubGlobal('sessionStorage', storage());
  vi.stubGlobal('window', { location: { pathname: '/repos/r1', search: '', href: '/repos/r1' }, addEventListener: () => {}, removeEventListener: () => {} });
  vi.stubGlobal('fetch', fetchMock);
  fetchMock.mockReset();
});
afterEach(() => { client.clear(); vi.unstubAllGlobals(); });

function seed(options: { repo?: Repository; status?: SyncStatus; records?: SyncRecord[]; credentials?: object } = {}) {
  client.setQueryData(['repo', 'r1'], options.repo ?? repo());
  client.setQueryData(['status', 'r1'], options.status ?? status());
  if (options.records) client.setQueryData(['sync-records', 'r1'], { entries: options.records, total: options.records.length });
  client.setQueryData(['commit-map', 'r1'], { entries: [], total: 0 });
  client.setQueryData(['audit', 'r1'], { entries: [], total: 0 });
  client.setQueryData(['repo-credentials', 'r1'], options.credentials ?? { svn_password_set: true, git_token_set: false });
  client.setQueryData(['branch-pairs', 'r1'], []);
  client.setQueryData(['import-status', 'r1'], importIdle);
}

function renderPage(path: string) {
  return renderToStaticMarkup(createElement(QueryClientProvider, { client },
    createElement(MemoryRouter, { initialEntries: [path] },
      createElement(Routes, null, createElement(Route, { path: '/repos/:id', element: createElement(RepoDetail) })))));
}

function inRouter(element: ReturnType<typeof createElement>, path = '/repos/r1') {
  return renderToStaticMarkup(createElement(MemoryRouter, { initialEntries: [path] }, element));
}

/** Every element carrying data-rs-region, in document order, with its sections and derived flag. */
function regions(html: string) {
  return Array.from(html.matchAll(/<[a-z0-9]+\b[^>]*\bdata-rs-region="([^"]+)"[^>]*>/g)).map(match => ({
    name: match[1],
    sections: /data-rs-sections="([^"]*)"/.exec(match[0])?.[1],
    derived: /data-rs-derived=""/.test(match[0]),
  }));
}

describe('G04.31 sections in the address', () => {
  it('normalises absent and unknown values to Overview and keeps the five ids in order', () => {
    expect(REPO_SECTIONS.map(section => section.id)).toEqual(['overview', 'activity', 'branches', 'operations', 'settings']);
    expect(normaliseRepoSection(null)).toBe('overview');
    expect(normaliseRepoSection('bogus')).toBe('overview');
    expect(normaliseRepoSection('Settings')).toBe('overview');
    for (const id of ['overview', 'activity', 'branches', 'operations', 'settings']) expect(normaliseRepoSection(id)).toBe(id);
  });
  it('sets section and keeps other parameters', () => {
    expect(sectionSearch('', 'settings')).toBe('?section=settings');
    expect(sectionSearch('?section=activity&x=1', 'branches')).toBe('?section=branches&x=1');
  });
  it('renders the section navigation as derived links with the current one marked', () => {
    const html = inRouter(createElement(RepositorySectionNav, { current: 'branches' }), '/repos/r1?section=branches');
    expect(html).toContain('aria-label="Repository sections"');
    expect(html).toContain('class="rs-page-sections"');
    expect(html).toContain('data-rs-derived=""');
    const links = Array.from(html.matchAll(/<a ([^>]*)>([^<]*)<\/a>/g)).map(match => ({ attrs: match[1], label: match[2] }));
    expect(links.map(link => link.label)).toEqual(['Overview', 'Activity', 'Branch pairs', 'Operations', 'Settings']);
    expect(links.map(link => /href="([^"]+)"/.exec(link.attrs)?.[1])).toEqual(
      ['overview', 'activity', 'branches', 'operations', 'settings'].map(id => `/repos/r1?section=${id}`));
    expect(links.filter(link => link.attrs.includes('aria-current="page"')).map(link => link.label)).toEqual(['Branch pairs']);
  });
  it('puts the normalised section on the page root, including for an unknown value', () => {
    seed();
    expect(renderPage('/repos/r1?section=settings')).toMatch(/class="rs-page space-y-6"[^>]*data-rs-page="repo" data-rs-section="settings"/);
    expect(renderPage('/repos/r1?section=nope')).toContain('data-rs-section="overview"');
    expect(renderPage('/repos/r1')).toContain('data-rs-section="overview"');
  });
});

describe('page regions on /repos/:id', () => {
  it('renders every region once in today\'s order with its sections; the new ones are derived', () => {
    seed({ repo: repo(), status: status({}, 'paused'), records: [record] });
    const found = regions(renderPage('/repos/r1?section=activity'));
    expect(found.map(item => item.name)).toEqual([
      'head', 'head', 'status-line', 'svn', 'git', 'credentials', 'sync', 'about', 'paused', 'status', 'status', 'import',
      'records', 'commit-map', 'audit', 'pairs', 'server', 'stop-remove',
    ]);
    const sections = Object.fromEntries(found.map(item => [item.name, item.sections]));
    expect(sections).toEqual({
      head: EVERY, 'status-line': EVERY, svn: 'settings', git: 'settings', credentials: 'settings', sync: 'settings',
      about: 'settings', paused: 'overview', status: 'overview', import: 'overview operations', records: 'overview activity',
      'commit-map': 'activity', audit: 'activity', pairs: 'overview branches', server: 'overview', 'stop-remove': 'settings',
    });
    expect(found.filter(item => item.derived).map(item => item.name)).toEqual(['status-line', 'credentials']);
  });
  it('a branch pair page adds the refresh and re-anchor regions in Branch pairs', () => {
    seed({ repo: repo({ parent_id: 'root' }) });
    const found = regions(renderPage('/repos/r1'));
    expect(found.filter(item => ['refresh', 'reanchor'].includes(item.name))).toEqual([
      { name: 'refresh', sections: 'branches', derived: false }, { name: 'reanchor', sections: 'branches', derived: false },
    ]);
  });
  it('the section navigation follows the head and the status line, and the short lists are not copied', () => {
    seed({ records: [record] });
    const html = renderPage('/repos/r1');
    expect(html.indexOf('data-rs-region="status-line"')).toBeLessThan(html.indexOf('aria-label="Repository sections"'));
    expect(html.indexOf('aria-label="Repository sections"')).toBeLessThan(html.indexOf('data-rs-region="svn"'));
    expect(html.match(/Tune ramp-rate limits/g)).toHaveLength(1);
    expect(html).toContain('data-rs-list=""');
  });
  it('keeps the existing labels in the head (D11 condition for "Request sync" not met)', () => {
    seed();
    const html = renderPage('/repos/r1');
    expect(html).toContain('Trigger Sync');
    expect(html).not.toContain('Request sync');
    expect(html).toContain('Turn sync off');
    expect(html).toContain('>Edit<');
  });
});

describe('G04.32 status line', () => {
  const view = (data: SyncStatus | undefined, extra: { isError?: boolean; error?: unknown; dataUpdatedAt?: number } = {}) =>
    statusObservationView({ data, dataUpdatedAt: now, ...extra }, now);
  it('states the observed state, time, last sync and position', () => {
    const line = repositoryStatusLine(view(status()));
    expect(line.word).toBe('Idle (stored state)');
    expect(line.tone).toBe('ok');
    expect(line.observed).toMatch(/^Observed /);
    expect(line.lastSync).not.toBe('Unknown');
    expect(line.position).toBe('r4127 · 9f3c2ab41c07');
  });
  it('never shows a stale or failed read as current', () => {
    const stale = repositoryStatusLine(view(status(), { dataUpdatedAt: now - 60_000 }));
    expect(stale.word).toBe('Unknown');
    expect(stale.observed).toMatch(/^Status observation stale · last observed /);
    expect(stale.position).toContain('(last known)');
    const failed = repositoryStatusLine(view(status(), { isError: true, error: new Error('down') }));
    expect(failed.observed).toMatch(/^Status unavailable \/ malformed · last observed /);
    const none = repositoryStatusLine(view(undefined, { isError: true, error: new Error('down') }));
    expect(none).toMatchObject({ word: 'Unknown', observed: 'Status unavailable / malformed', lastSync: 'Unknown', position: 'Unknown', tone: 'muted' });
  });
  it('reports held and paused with their words and nothing recorded as not recorded', () => {
    expect(repositoryStatusLine(view(status({}, 'held')))).toMatchObject({ word: 'Held for reconciliation', tone: 'warn' });
    const paused = status({ last_svn_revision: null, last_git_hash: null }, 'paused');
    paused.observation!.last_successful_sync_at = null; paused.observation!.data_freshness = 'never_recorded';
    expect(repositoryStatusLine(view(paused))).toMatchObject({ word: 'Paused after error', lastSync: 'Not recorded', position: 'Not recorded' });
  });
  it('renders as a derived region with a link to the evidence on Overview and no control that sends', () => {
    const html = inRouter(createElement(RepositoryStatusLine, { view: view(status()) }), '/repos/r1?section=settings');
    expect(html).toContain('data-rs-region="status-line"');
    expect(html).toContain(`data-rs-sections="${EVERY}"`);
    expect(html).toContain('data-rs-derived=""');
    expect(html).toContain('href="/repos/r1?section=overview"');
    expect(html).toContain('What this status proves');
    expect(html).toContain('role="status"');
    expect(html).not.toContain('<button');
  });
});

describe('G04.4 / S23 credentials summary', () => {
  it('states each secret as a word and a mark, never a placeholder', () => {
    const html = renderToStaticMarkup(createElement(CredentialsSummary, { read: { data: { svn_password_set: true, git_token_set: false }, isError: false }, onReplace: () => {} }));
    expect(html).toContain('data-rs-region="credentials"');
    expect(html).toContain('data-rs-sections="settings"');
    expect(html).toContain('data-rs-derived=""');
    expect(html).toMatch(/SVN password<\/span><span class="rs-mark[^"]*">Set<\/span>/);
    expect(html).toMatch(/Git token<\/span><span class="rs-mark[^"]*">Not set<\/span>/);
    expect(html).not.toContain('●');
    expect(html).toContain('Replace…');
    expect(html).toContain('Set…');
    expect(html).toContain('Origin not reported');
    expect(html.match(/<button type="button"/g)).toHaveLength(2);
  });
  it('shows a failed read as failed, a missing flag as not reported and loading as loading', () => {
    expect(credentialState(undefined, { isError: true, error: new Error('API error 500: boom') })).toMatchObject({ word: 'Could not be read', note: 'API error 500: boom' });
    expect(credentialState(undefined, { data: {}, isError: false })).toMatchObject({ word: 'Not reported', set: false });
    expect(credentialState(undefined, { isError: false })).toMatchObject({ word: 'Loading…' });
    expect(credentialState(true, { data: { svn_password_set: true }, isError: true, error: new Error('later failure') }))
      .toMatchObject({ word: 'Set', note: 'Last known. The latest read failed: later failure' });
  });
  it('“Replace…” sends nothing while rendering and is described by its row', () => {
    const calls: string[] = [];
    const html = renderToStaticMarkup(createElement(CredentialsSummary, { read: { data: { svn_password_set: true, git_token_set: true }, isError: false }, onReplace: field => { calls.push(field); } }));
    expect(html.match(/aria-describedby="[^"]+-(svn|git)"[^>]*>Replace…<\/button>/g)).toHaveLength(2);
    expect(calls).toEqual([]);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe('G04.7 hook download with the session', () => {
  it('requests the same hook path with the session header and returns the script', async () => {
    fetchMock.mockResolvedValueOnce(new Response('#!/bin/sh\nexit 0\n', { status: 200 }));
    await expect(fetchPreCommitHook('r1')).resolves.toBe('#!/bin/sh\nexit 0\n');
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe('/api/repos/r1/hooks/pre-commit');
    expect(new Headers(init.headers).get('Authorization')).toBe('Bearer fixture-token');
    expect(init.method).toBeUndefined();
  });
  it('reports a failed download with the status and server text', async () => {
    const fetcher = vi.fn(async () => new Response('{"error":"repository not found"}', { status: 404 }));
    await expect(fetchPreCommitHook('r1', fetcher)).rejects.toThrow('API error 404: {"error":"repository not found"}');
  });
  it('hands the text to the browser as a file named pre-commit', () => {
    const clicked: { download: string; href: string }[] = [];
    const link = { href: '', download: '', rel: '', style: { display: '' }, click() { clicked.push({ download: this.download, href: this.href }); }, remove: vi.fn() };
    const doc = { createElement: vi.fn(() => link), body: { appendChild: vi.fn() } } as unknown as Document;
    const create = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:hook');
    vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});
    saveTextDownload('pre-commit', '#!/bin/sh', doc);
    expect(create).toHaveBeenCalledTimes(1);
    expect(clicked).toEqual([{ download: 'pre-commit', href: 'blob:hook' }]);
    expect(link.remove).toHaveBeenCalled();
  });
  it('renders a button, not a link that cannot carry the session', () => {
    seed();
    const html = renderPage('/repos/r1');
    expect(html).toMatch(/<button type="button" data-testid="download-pre-commit-hook"[^>]*>.*Git Hook<\/button>/s);
    expect(html).not.toContain('hooks/pre-commit');
  });
});

describe('G04.9 paused after error: Resume sync…', () => {
  it('replaces “Retry” with “Resume sync…”, which opens a confirmation', () => {
    seed({ status: status({}, 'paused') });
    const html = renderPage('/repos/r1');
    expect(html).toContain('Resume sync…');
    expect(html).not.toMatch(/>\s*Retry\s*</);
    expect(html).toContain('data-testid="resume-sync-button"');
  });
  it('says what resuming does and does not do', () => {
    expect(resumeSyncConsequences(2)[1]).toBe('It does not skip or change the 2 pending commits. If the same errors keep recurring, sync pauses again.');
    expect(resumeSyncConsequences(1)[1]).toContain('the 1 pending commit.');
    expect(resumeSyncConsequences(null)[1]).toContain('change pending commits.');
  });
  it('shows ready, pending, failed and no-longer-paused states', () => {
    const base = { repoName: 'kiln-controller', pendingCount: 2, allowed: true, pending: false, error: null, onConfirm: () => {}, onClose: () => {} };
    const ready = renderToStaticMarkup(createElement(ResumeSyncDialog, base));
    expect(ready).toContain('Resume sync?');
    expect(ready).toMatch(/data-testid="confirm-resume-sync"[^>]*>Resume sync<\/button>/);
    expect(ready).toContain('data-dialog-cancel');
    const pending = renderToStaticMarkup(createElement(ResumeSyncDialog, { ...base, pending: true }));
    expect(pending).toMatch(/data-testid="confirm-resume-sync" disabled=""[^>]*>Resuming…<\/button>/);
    expect(pending).toContain('aria-busy="true"');
    const failed = renderToStaticMarkup(createElement(ResumeSyncDialog, { ...base, error: 'API error 409: held' }));
    expect(failed).toMatch(/role="alert">Sync could not be resumed: API error 409: held/);
    const gone = renderToStaticMarkup(createElement(ResumeSyncDialog, { ...base, allowed: false }));
    expect(gone).toContain('No resume request will be sent.');
    expect(gone).toMatch(/data-testid="confirm-resume-sync" disabled=""/);
  });
});

describe('G04.10 skip prefix as radios', () => {
  const commits = [{ sha: 'a91f3e2'.padEnd(40, '1'), subject: 'Recalibrate', excluded: false }, { sha: '4c2e19d'.padEnd(40, '2'), subject: 'Bump', excluded: false }];
  it('renders one radio group with the existing test ids and what each choice skips', () => {
    const html = renderToStaticMarkup(createElement(SkipPrefixChoices, { name: 'skip', commits, selectedCount: 1, onSelect: () => {} }));
    expect(html).toContain('role="radiogroup"');
    expect(html).toContain('aria-labelledby="skip-label"');
    expect(html).toContain('Skip through');
    expect(html.match(/type="radio" name="skip"/g)).toHaveLength(2);
    expect(html).not.toContain('type="checkbox"');
    expect(html).toMatch(/data-testid="skip-commit-select-0" checked=""/);
    expect(html).not.toMatch(/data-testid="skip-commit-select-1" checked=""/);
    expect(html).toContain('Skips 1 commit. 1 commit stays pending.');
    expect(html).toContain('Skips 2 commits. Nothing stays pending.');
  });
  it('words the remaining count', () => {
    expect(skipChoiceHelp(1, 3)).toBe('Skips 1 commit. 2 commits stay pending.');
  });
});

describe('G04.13 stop import asks first', () => {
  const base = { operationId: 'op-7', repoName: 'loom', allowed: true, pending: false, error: null, onConfirm: () => {}, onClose: () => {} };
  it('says what stopping does not undo and names the operation', () => {
    const html = renderToStaticMarkup(createElement(StopImportDialog, base));
    expect(html).toContain('Stop import?');
    expect(html).toContain('Stopping does not undo commits already published.');
    expect(html).toContain('This repository stays held until its local and remote history is reconciled.');
    expect(html).toContain('Operation op-7');
    expect(html).toMatch(/data-testid="confirm-stop-import"[^>]*>Stop import<\/button>/);
    expect(html).toContain('data-dialog-cancel');
  });
  it('shows pending, failure and no-longer-running states', () => {
    expect(renderToStaticMarkup(createElement(StopImportDialog, { ...base, pending: true }))).toMatch(/disabled=""[^>]*>Requesting stop…<\/button>/);
    expect(renderToStaticMarkup(createElement(StopImportDialog, { ...base, error: 'Cancellation failed (500)' }))).toContain('role="alert" class="text-red-400">Cancellation failed (500)');
    const gone = renderToStaticMarkup(createElement(StopImportDialog, { ...base, allowed: false }));
    expect(gone).toContain('No stop request will be sent.');
    expect(gone).toMatch(/data-testid="confirm-stop-import" disabled=""/);
  });
});

describe('G04.15 sync records', () => {
  it('shows a failed read as failed, not as empty', () => {
    const html = renderToStaticMarkup(createElement(SyncRecordsBody, { read: { isError: true, error: new Error('API error 500: down') } }));
    expect(html).toContain('role="alert"');
    expect(html).toContain('Sync records could not be loaded: API error 500: down');
    expect(html).not.toContain('No sync records yet');
  });
  it('distinguishes loading, empty, ready and a failed refresh of a loaded list', () => {
    expect(renderToStaticMarkup(createElement(SyncRecordsBody, { read: { isError: false } }))).toContain('Loading sync records…');
    expect(renderToStaticMarkup(createElement(SyncRecordsBody, { read: { data: { entries: [] }, isError: false } }))).toContain('No sync records yet');
    const stale = renderToStaticMarkup(createElement(SyncRecordsBody, { read: { data: { entries: [record] }, isError: true, error: new Error('timeout') } }));
    expect(stale).toContain('Sync records could not be refreshed: timeout');
    expect(stale).toContain('Tune ramp-rate limits');
    expect(stale).toContain('data-rs-list=""');
  });
  it('the page never shows a failed read as an empty list (its retry on mount reads as loading)', async () => {
    seed();
    await expect(client.fetchQuery({ queryKey: ['sync-records', 'r1'], queryFn: () => { throw new Error('API error 500: down'); } })).rejects.toThrow();
    const html = renderPage('/repos/r1');
    expect(html).toContain('Loading sync records…');
    expect(html).not.toContain('No sync records yet');
  });
});

describe('G04.23 one pause control for a repository', () => {
  it('a repository pauses through the explicit dialog; a pair and turning on stay one click', () => {
    expect(headerSyncSwitch({ parent_id: null, enabled: true })).toBe('explicit-pause');
    expect(headerSyncSwitch({ parent_id: 'root', enabled: true })).toBe('turn-off');
    expect(headerSyncSwitch({ parent_id: null, enabled: false })).toBe('turn-on');
    expect(headerSyncSwitch({ parent_id: 'root', enabled: false })).toBe('turn-on');
  });
  it('keeps the existing label and says the switch opens the explicit dialog', () => {
    seed();
    const html = renderPage('/repos/r1');
    expect(html).toMatch(/data-testid="header-sync-switch"[^>]*title="Opens Pause \/ disable sync, which uses the explicit disable API\."/);
    expect(html).toContain('Turn sync off');
    seed({ repo: repo({ parent_id: 'root' }) });
    expect(renderPage('/repos/r1')).toContain('title="Quick sync on/off via registration update.');
  });
});

describe('G04.25 remove pair wording', () => {
  it('archives for one selected side and preserves remove wording for zero or both sides', () => {
    expect(removePairWording({ delete_git: false, delete_svn: false })).toEqual({ title: 'Remove branch pair', confirm: 'Remove pair', pending: 'Removing…' });
    expect(removePairWording({ delete_git: true, delete_svn: false })).toMatchObject({ title: 'Archive pair and delete Git branch', confirm: 'Archive pair' });
    expect(removePairWording({ delete_git: false, delete_svn: true })).toMatchObject({ title: 'Archive pair and delete SVN branch', confirm: 'Archive pair' });
    expect(removePairWording({ delete_git: true, delete_svn: true })).toMatchObject({ title: 'Remove branch pair and delete its Git and SVN branches', confirm: 'Remove pair and delete Git and SVN branches' });
  });
});

describe('S20 plain lifecycle words, raw value kept in details', () => {
  it('the import card mark keeps its words and says what the lifecycle is when the phase would misstate it', () => {
    expect(importStateLabel({ phase: 'importing', lifecycle: 'running' })).toBe('Importing');
    expect(importStateLabel({ phase: 'importing', lifecycle: 'cancelling' })).toBe('Cancellation requested — stopping');
    expect(importStateLabel({ phase: 'final_push', lifecycle: 'reconciliation_required' })).toBe('Reconciliation required');
    expect(importStateLabel({ phase: 'importing', lifecycle: 'failed' })).toBe('Failed');
    expect(importStateLabel({ phase: 'connecting', lifecycle: 'queued' })).toBe('Queued');
    expect(importStateLabel({ phase: 'importing', lifecycle: 'cancelled' })).toBe('Cancelled');
    expect(importStateLabel({ phase: 'final_push' })).toBe('Final Push');
    expect(importStateLabel({ phase: 'mystery' as ImportStatus['phase'] })).toBe('Unrecognised state');
    expect(importStateLabel({ phase: 'importing', lifecycle: 'paused_somehow' as ImportStatus['lifecycle'] })).toBe('Unrecognised state');
    expect(importPhaseLabel('mystery')).toBe('Unrecognised state');
  });
  it('progress text keeps the #121 wording for known values and never reads out an unrecognised raw value', () => {
    expect(importProgressState({ phase: 'importing', lifecycle: 'running' })).toBe('running — importing');
    expect(importProgressState({ phase: 'final_push', lifecycle: 'reconciliation_required' })).toBe('reconciliation required — final push');
    expect(importProgressState({ phase: 'verifying' })).toBe('verifying');
    expect(importProgressState({ phase: 'importing', lifecycle: 'odd_value' as ImportStatus['lifecycle'] })).toBe('unrecognised state — importing');
    expect(importProgressState({ phase: 'odd_phase' as ImportStatus['phase'] })).toBe('unrecognised phase');
    const view = importProgressView({ phase: 'final_push', lifecycle: 'reconciliation_required', current_rev: 1, total_revs: 4 });
    expect(view.description).toBe('reconciliation required — final push: 25%');
    expect(view.announcement).not.toContain('_');
  });
  it('the import card keeps the raw lifecycle and phase in its details', () => {
    seed();
    client.setQueryData(['import-status', 'r1'], { ...importIdle, phase: 'importing', lifecycle: 'running', operation_id: 'op-9', started_at: stamp, current_rev: 3, total_revs: 10 });
    const html = renderPage('/repos/r1?section=operations');
    expect(html).toMatch(/rs-mark[^>]*>Importing<\/span>/);
    expect(html).toContain('Reported lifecycle: running · phase: importing');
    expect(html).toMatch(/data-testid="stop-import"[^>]*>Stop import<\/button>/);
    expect(html).toContain('href="/repos/r1?section=operations"');
    expect(html).not.toContain('stop-import-modal');
  });
  it('late-pairing states read as words and unknown values are said to be unrecognised', () => {
    expect(latePairStateWord('svn_copy_pending')).toBe('SVN copy pending');
    expect(latePairStateWord('reconciliation_held')).toBe('held for reconciliation');
    expect(latePairStateWord('brand_new')).toBe('state not recognised');
  });
});


describe('#199 archived detail is read-only', () => {
  it('retains history navigation and disables mutation controls', () => {
    seed({ repo: repo({ parent_id: 'root', enabled: false, sync_status: 'archived' }) });
    const html = renderPage('/repos/r1');
    expect(html).toContain('archived-pair-notice');
    expect(html).toContain('Archived · sync permanently stopped');
    expect(html).toMatch(/data-testid="header-sync-switch"[^>]*disabled/);
    expect(html).not.toContain('data-testid="delete-viewed-branch-pair"');
    expect(html).not.toContain('data-testid="preview-update-from-parent"');
    expect(html).not.toContain('data-testid="reanchor-not-implemented"');
    expect(html).toContain('data-rs-region="audit"');
    expect(html).not.toContain('Add Branch Pair</button>');
  });
});
