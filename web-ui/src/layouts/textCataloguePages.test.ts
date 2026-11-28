import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement, type ReactElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { Conflict, ImportStatus, Repository, SyncStatus, User } from '../api';
import Dashboard from '../pages/Dashboard';
import Repositories from '../pages/Repositories';
import RepoDetail from '../pages/RepoDetail';
import UserSettings from '../pages/UserSettings';
import Config from '../pages/Config';
import ConflictDetail, { ResolutionActions } from '../pages/ConflictDetail';
import { VocabularyProvider } from './VocabularyProvider';
import { TEXT_CATALOGUE, type TextKey } from './textCatalogue';
import type { VocabularyId } from './layoutCatalog';

/*
 * #264 E3: pages rendered on the server in each vocabulary. The existing vocabulary is the default
 * (outside the shell and in Classic, Individual and Team) and must render exactly as without a
 * provider; the plain vocabulary changes only the text of the catalogued labels, so swapping those
 * labels in the existing markup gives the plain markup, element for element and attribute for attribute.
 */

const stamp = '2026-11-17T12:00:00Z';
let client: QueryClient;
const fetchMock = vi.fn();

function storage(entries: Map<string, string>) {
  return {
    getItem: (key: string) => entries.get(key) ?? null,
    setItem: (key: string, value: string) => { entries.set(key, value); },
    removeItem: (key: string) => { entries.delete(key); },
    clear: () => entries.clear(),
    get length() { return entries.size; },
    key: (index: number) => Array.from(entries.keys())[index] ?? null,
  };
}

beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false, retryOnMount: false, staleTime: Infinity } } });
  vi.stubGlobal('localStorage', storage(new Map([['session_token', 'fixture-token'], ['user', JSON.stringify({ id: 'actor', username: 'ines', role: 'admin' })]])));
  vi.stubGlobal('sessionStorage', storage(new Map()));
  vi.stubGlobal('window', { location: { pathname: '/', search: '', href: '/' }, addEventListener: () => {}, removeEventListener: () => {} });
  // Nothing in these renders may reach the network; a request that starts never settles.
  vi.stubGlobal('fetch', fetchMock);
  fetchMock.mockReset();
  fetchMock.mockImplementation(() => new Promise(() => {}));
});
afterEach(() => { client.clear(); vi.unstubAllGlobals(); });

function repo(overrides: Partial<Repository> = {}): Repository {
  return {
    id: 'r1', name: 'kiln-controller', parent_id: null, svn_url: 'svn://fixture.invalid/kiln', svn_branch: 'trunk', svn_username: 'sync',
    git_provider: 'github', git_api_url: 'https://api.github.invalid', git_repo: 'maker/kiln', git_branch: 'main', sync_mode: 'direct',
    poll_interval_secs: 300, lfs_threshold_mb: 0, auto_merge: false, enabled: true, created_by: 'ines', created_at: stamp, updated_at: stamp,
    allowed_paths: '["src/"]', blocked_patterns: null,
    ...overrides,
  } as Repository;
}

function status(health: 'idle' | 'paused' = 'idle', repositoryId: string | null = 'r1'): SyncStatus {
  return {
    state: health === 'paused' ? 'error_paused' : 'idle', last_sync_at: stamp, last_svn_revision: 4127, last_git_hash: '9f3c2ab41c07'.padEnd(40, '0'),
    total_syncs: 12, total_conflicts: 0, active_conflicts: 0, total_errors: 0, last_error_at: null,
    error_observation: { through_id: '1', observed_at: stamp }, uptime_secs: 0,
    observation: {
      version: 1, scope: repositoryId ? 'repository' : 'aggregate', repository_id: repositoryId, configured: 'enabled', health, observed_at: stamp,
      data_freshness: 'recorded', last_successful_sync_at: stamp, remote_freshness: 'not_verified_by_status', registry_busy: false,
      evidence_complete: true, operations: [], safe_next_action: 'Inspect scoped evidence', uptime_available: false,
    },
  } as SyncStatus;
}

const importIdle = { phase: 'idle', current_rev: 0, total_revs: 0, commits_created: 0, current_file_count: 0, lfs_unique_count: 0, files_skipped: 0, batches_pushed: 0, push_started_at: null, verification: null, errors: [], log_lines: [], started_at: null, completed_at: null, can_start: true } as ImportStatus;
const importRunning = { ...importIdle, phase: 'importing', lifecycle: 'running', operation_id: 'op-9', started_at: stamp, current_rev: 3, total_revs: 10 } as ImportStatus;

function seedRepository(options: { repo?: Repository; status?: SyncStatus; imports?: ImportStatus } = {}) {
  client.setQueryData(['repo', 'r1'], options.repo ?? repo());
  client.setQueryData(['status', 'r1'], options.status ?? status());
  client.setQueryData(['sync-records', 'r1'], { entries: [], total: 0 });
  client.setQueryData(['commit-map', 'r1'], { entries: [], total: 0 });
  client.setQueryData(['audit', 'r1'], { entries: [], total: 0 });
  client.setQueryData(['repo-credentials', 'r1'], { svn_password_set: true, git_token_set: false });
  client.setQueryData(['branch-pairs', 'r1'], []);
  client.setQueryData(['import-status', 'r1'], options.imports ?? importIdle);
}

/** Renders `page` at `path`, inside the vocabulary when one is given (none: outside the shell). */
function render(page: ReactElement, path: string, vocabulary?: VocabularyId, route?: string) {
  const routed = route ? createElement(Routes, null, createElement(Route, { path: route, element: page })) : page;
  const tree = createElement(MemoryRouter, { initialEntries: [path] }, routed);
  return renderToStaticMarkup(createElement(QueryClientProvider, { client }, vocabulary ? createElement(VocabularyProvider, { vocabulary, children: tree }) : tree));
}

const escapeText = (text: string) => text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

/**
 * A catalogued label and where it ends: a key alone is swapped wherever it is a whole text node;
 * with `close` (e.g. '</h3>') only where that text node is followed by it, for a page that also
 * shows the same existing text elsewhere outside the catalogue.
 */
type Label = TextKey | { readonly key: TextKey; readonly close: string };
const labelKey = (label: Label) => (typeof label === 'string' ? label : label.key);
const labelEnd = (label: Label) => (typeof label === 'string' ? '<' : label.close);

/** Replaces each label's existing text with its plain text, as whole text nodes. */
function swapLabels(html: string, labels: readonly Label[]) {
  return labels.reduce((out, label) => {
    const entry = TEXT_CATALOGUE[labelKey(label)];
    return out.split(`>${escapeText(entry.existing)}${labelEnd(label)}`).join(`>${escapeText(entry.plain)}${labelEnd(label)}`);
  }, html);
}

/** Renders a page in both vocabularies and checks that only the given labels differ. */
function inBothVocabularies(page: () => ReactElement, path: string, labels: readonly Label[], route?: string) {
  const outside = render(page(), path, undefined, route);
  const existing = render(page(), path, 'existing', route);
  const plain = render(page(), path, 'plain', route);
  expect(existing).toBe(outside);
  expect(plain).not.toBe(existing);
  expect(swapLabels(existing, labels)).toBe(plain);
  for (const label of labels) {
    const entry = TEXT_CATALOGUE[labelKey(label)];
    expect(existing, labelKey(label)).toContain(`>${escapeText(entry.existing)}${labelEnd(label)}`);
    expect(plain, labelKey(label)).toContain(`>${escapeText(entry.plain)}${labelEnd(label)}`);
  }
  return { existing, plain };
}

describe('#264 E3 Dashboard in each vocabulary', () => {
  it('keeps today\'s text in the existing vocabulary and uses the plain labels in the new one', () => {
    client.setQueryData(['repos'], [repo({ id: 'A', name: 'Alpha' }), repo({ id: 'B', name: 'Beta' })]);
    const { existing, plain } = inBothVocabularies(() => createElement(Dashboard), '/',
      ['dashboard.scopeAll', 'dashboard.manageRepositories', 'dashboard.records', 'dashboard.commitMap', 'dashboard.activity', 'server.title']);

    expect(existing).toContain('<option value="all" selected="">All Repositories</option>');
    expect(existing).toContain('<span class="text-xs text-blue-400">All Repositories</span>');
    expect(existing).toContain('>Manage Repositories →</button>');
    expect(existing).toMatch(/<h2 class="rs-heading[^"]*">Recent Sync Records<\/h2>/);
    expect(existing).toMatch(/<h2 class="rs-heading[^"]*">Commit Map \(SVN ↔ Git\)<\/h2>/);
    expect(existing).toMatch(/<h2 class="rs-heading[^"]*">Recent Activity<\/h2>/);
    expect(existing).toContain('<span>Server Monitor</span>');
    for (const text of ['All repositories', 'Manage repositories', 'Recent sync records', 'Commit map', 'Recent activity', '<span>Server</span>']) expect(existing).not.toContain(text);

    expect(plain).toContain('<option value="all" selected="">All repositories</option>');
    expect(plain).toContain('<span class="text-xs text-blue-400">All repositories</span>');
    expect(plain).toContain('>Manage repositories →</button>');
    expect(plain).toMatch(/<h2 class="rs-heading[^"]*">Commit map \(SVN ⇄ Git\)<\/h2>/);
    expect(plain).toContain('<span>Server</span>');
    for (const text of ['All Repositories', 'Manage Repositories', 'Recent Sync Records', 'Commit Map', 'Recent Activity', 'Server Monitor']) expect(plain).not.toContain(text);
  });

  it('keeps the global acknowledgement prompt and other sentences the same in both', () => {
    client.setQueryData(['repos'], [repo({ id: 'A', name: 'Alpha' })]);
    const plain = render(createElement(Dashboard), '/', 'plain');
    expect(plain).toContain('Individual commits synced between SVN and Git (click to expand)');
    expect(plain).toContain('Bidirectional mapping between SVN revisions and Git commits');
  });
});

describe('#264 E3 Repositories in each vocabulary', () => {
  it('labels both Add Repository buttons from the catalogue and leaves the rest of the page alone', () => {
    client.setQueryData(['repos'], []);
    const { existing, plain } = inBothVocabularies(() => createElement(Repositories), '/repos', ['repos.add']);
    expect(existing.match(/<\/svg>Add Repository<\/button>/g)).toHaveLength(2);
    expect(plain.match(/<\/svg>Add repository<\/button>/g)).toHaveLength(2);
    expect(plain).toContain('No repositories configured');
    expect(plain).toContain('Add your first SVN ↔ Git sync');
  });
});

describe('#264 E3 repository page in each vocabulary', () => {
  const page = () => createElement(RepoDetail);
  const SECTIONS: TextKey[] = ['repo.section.svn', 'repo.section.git', 'repo.section.sync', 'repo.section.records', 'repo.section.commitMap', 'repo.section.audit', 'repo.section.pairs', 'repo.section.stopOrRemove'];
  const READ_FIELDS: TextKey[] = ['field.svnPassword', 'repo.field.gitToken', 'field.pollInterval', 'field.lfsThreshold', 'field.autoMerge', 'repo.about.lastUpdated', 'repo.about.createdBy'];
  const IDLE_IMPORT: TextKey[] = ['import.manageRepositories'];

  it('renames headings, read labels, the pause and hook controls for a repository', () => {
    seedRepository();
    const { existing, plain } = inBothVocabularies(page, '/repos/r1', [...SECTIONS, ...READ_FIELDS, ...IDLE_IMPORT, 'repo.header.pause', 'repo.header.hook', 'server.title'], '/repos/:id');
    // The stored sync mode is not applied by sync, so the page does not show it.
    expect(existing).not.toContain('Sync Mode');
    expect(plain).not.toContain('Sync mode');
    expect(existing).toMatch(/<button data-testid="header-sync-switch"[^>]*>.*?<\/svg>Turn sync off<\/button>/s);
    expect(plain).toMatch(/<button data-testid="header-sync-switch"[^>]*>.*?<\/svg>Pause sync…<\/button>/s);
    expect(existing).toMatch(/data-testid="download-pre-commit-hook"[^>]*>.*?<\/svg>Git Hook<\/button>/s);
    expect(plain).toMatch(/data-testid="download-pre-commit-hook"[^>]*title="Download Git pre-commit hook script">.*?<\/svg>Download Git pre-commit hook<\/button>/s);
    // Unchanged in both: the truthful-status mark, the safety paragraph and other controls.
    for (const html of [existing, plain]) {
      expect(html).toContain('Sync configured enabled');
      expect(html).toContain('The header “Turn sync off” opens');
      expect(html).toContain('>Add Branch Pair<');
    }
  });

  it('keeps “Turn sync off” for a branch pair and “Turn sync on” when sync is off, in both vocabularies', () => {
    seedRepository({ repo: repo({ parent_id: 'root', name: 'root / feature' }) });
    for (const vocabulary of ['existing', 'plain'] as const) {
      expect(render(page(), '/repos/r1', vocabulary, '/repos/:id')).toMatch(/data-testid="header-sync-switch"[^>]*>.*?<\/svg>Turn sync off<\/button>/s);
    }
    seedRepository({ repo: repo({ enabled: false }) });
    for (const vocabulary of ['existing', 'plain'] as const) {
      expect(render(page(), '/repos/r1', vocabulary, '/repos/:id')).toMatch(/data-testid="header-sync-switch"[^>]*>.*?<\/svg>Turn sync on<\/button>/s);
    }
  });

  it('labels the paused banner\'s skip control “Skip commits…” in the plain vocabulary only', () => {
    seedRepository({ status: status('paused') });
    const { existing, plain } = inBothVocabularies(page, '/repos/r1', [...SECTIONS, ...READ_FIELDS, ...IDLE_IMPORT, 'repo.header.pause', 'repo.header.hook', 'server.title', 'repo.paused.skip'], '/repos/:id');
    expect(existing).toMatch(/data-testid="skip-commit-button"[^>]*>Skip Commit<\/button>/);
    expect(plain).toMatch(/data-testid="skip-commit-button"[^>]*>Skip commits…<\/button>/);
    for (const html of [existing, plain]) expect(html).toMatch(/data-testid="resume-sync-button"[^>]*>Resume sync…<\/button>/);
  });

  it('labels the import card\'s stop control “Stop import…” in the plain vocabulary only', () => {
    seedRepository({ imports: importRunning });
    const { existing, plain } = inBothVocabularies(page, '/repos/r1?section=operations', [...SECTIONS, ...READ_FIELDS, 'repo.header.pause', 'repo.header.hook', 'server.title', 'import.stop'], '/repos/:id');
    expect(existing).toMatch(/data-testid="stop-import"[^>]*>Stop import<\/button>/);
    expect(plain).toMatch(/data-testid="stop-import"[^>]*>Stop import…<\/button>/);
    expect(plain).not.toContain('stop-import-modal');
  });
});

describe('#264 E3 account, identity and conflict pages in each vocabulary', () => {
  it('My account: section headings and credential cards; the Change Password button keeps its label', () => {
    client.setQueryData(['me'], { id: 'actor', username: 'ines', display_name: 'Ines', email: 'ines@fixture.invalid', role: 'admin' } as User);
    client.setQueryData(['credentials', 'actor'], []);
    const { existing, plain } = inBothVocabularies(() => createElement(UserSettings), '/settings',
      [{ key: 'account.title', close: '</h1>' }, 'account.profile', { key: 'account.changePassword', close: '</h3>' }, 'account.credentials', 'account.credential.svnPassword', 'account.credential.svnToken', 'account.credential.gitToken']);
    expect([...existing.matchAll(/<h3 class="rs-heading[^"]*">([^<]+)<\/h3>/g)].map(m => m[1])).toEqual(['Change Password', 'SVN Password', 'SVN Token', 'Git Token']);
    expect([...plain.matchAll(/<h3 class="rs-heading[^"]*">([^<]+)<\/h3>/g)].map(m => m[1])).toEqual(['Change password', 'SVN password', 'SVN token', 'Git token']);
    for (const html of [existing, plain]) expect(html).toMatch(/<button[^>]*>Change Password<\/button>/);
  });

  it('Identity & notifications: both section headings', () => {
    client.setQueryData(['identityMappings'], [{ svn_username: 'mei', name: 'Mei', email: 'mei@fixture.invalid' }]);
    const { plain } = inBothVocabularies(() => createElement(Config), '/config', [{ key: 'config.title', close: '</h1>' }, 'config.identity', 'config.notifications']);
    expect(plain).toContain('>Author identity mappings</h2>');
    expect(plain).toContain('>Notifications: Microsoft Teams</h2>');
    expect(plain).toContain('aria-label="Microsoft Teams notification settings"');
  });

  it('Conflict: the view toggle only; the resolution choices keep their labels', () => {
    const conflict: Conflict = {
      id: 'c1', repo_id: 'A', file_path: 'src/example.txt', conflict_type: 'content', svn_content: 'svn\n', git_content: 'git\n', base_content: 'base\n',
      svn_revision: 12, git_hash: 'a'.repeat(40), status: 'detected', resolution: null, resolved_by: null, created_at: stamp, resolved_at: null,
    };
    client.setQueryData(['conflict', 'c1'], conflict);
    const { existing, plain } = inBothVocabularies(() => createElement(ConflictDetail), '/conflicts/c1', ['conflict.view.diff', 'conflict.view.edit'], '/conflicts/:id');
    expect(existing).toMatch(/<button aria-pressed="true"[^>]*>Side-by-Side Diff<\/button><button aria-pressed="false"[^>]*>Manual Edit<\/button>/);
    expect(plain).toMatch(/<button aria-pressed="true"[^>]*>Side-by-side diff<\/button><button aria-pressed="false"[^>]*>Manual edit<\/button>/);
    for (const html of [existing, plain]) expect(html).toMatch(/<button type="button" aria-pressed="false"[^>]*>Accept SVN<\/button>/);
    const noop = () => {};
    const actions = createElement(ResolutionActions, {
      headingId: 'h', status: 'detected', deferredAt: null, choice: null, manualOffered: true, busy: false, stale: false, pendingText: '',
      onChoose: noop, onRecord: noop, onDefer: noop,
    });
    expect(renderToStaticMarkup(createElement(VocabularyProvider, { vocabulary: 'plain', children: actions }))).toMatch(/>Apply Manual Edit<\/button>/);
  });
});
