import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import ConflictDetail, { ConflictRepository, ConflictStatusMark, DecisionConfirmation, ResolutionActions, type ConflictChoice } from './ConflictDetail';
import DiffViewer, { CommonBasePanel } from '../components/DiffViewer';
import { ApiError, type Conflict, type Repository } from '../api';

const stamp = '2026-11-04T00:00:00Z';
function conflict(overrides: Partial<Conflict> = {}): Conflict {
  return {
    id: 'c1', repo_id: 'A', file_path: 'src/example.txt', conflict_type: 'content',
    svn_content: 'svn line\n', git_content: 'git line\n', base_content: 'base line\n',
    svn_revision: 12, git_hash: 'a'.repeat(40), status: 'detected', resolution: null,
    resolved_by: null, created_at: stamp, resolved_at: null, ...overrides,
  };
}

let client: QueryClient;
beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false, retryOnMount: false, staleTime: Infinity } } });
  const entries = new Map([['session_token', 'fixture-token'], ['user', '{"id":"actor","role":"user"}']]);
  vi.stubGlobal('localStorage', { getItem: (key: string) => entries.get(key) ?? null, setItem: (key: string, v: string) => entries.set(key, v), removeItem: (key: string) => entries.delete(key), get length() { return entries.size; }, key: (index: number) => Array.from(entries.keys())[index] ?? null });
  vi.stubGlobal('window', { location: { pathname: '/conflicts/c1', href: '/conflicts/c1', search: '' } });
});
afterEach(() => { client.clear(); vi.unstubAllGlobals(); });

function renderPage(id = 'c1') {
  return renderToStaticMarkup(createElement(QueryClientProvider, { client },
    createElement(MemoryRouter, { initialEntries: [`/conflicts/${id}`] },
      createElement(Routes, null, createElement(Route, { path: '/conflicts/:id', element: createElement(ConflictDetail) })))));
}
function renderStatic(element: ReturnType<typeof createElement>) {
  return renderToStaticMarkup(createElement(MemoryRouter, null, element));
}
/** The opening tag of the button whose text ends with `label`. */
function buttonTag(html: string, label: string) {
  const escaped = label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return html.match(new RegExp(`<button[^>]*>(?:<span aria-hidden="true">✓ </span>)?${escaped}</button>`))?.[0] ?? '';
}
const failure = (status: number, text: string) => () => Promise.reject(new ApiError(status, `API error ${status}: ${text}`));

describe('#264 G06.1 header and facts', () => {
  it('shows the repository from repo_id as a link to its page, named from the list cache when present', () => {
    client.setQueryData(['conflict', 'c1'], conflict());
    client.setQueryData<Repository[]>(['repos'], [{ id: 'A', name: 'Alpha repository' } as Repository]);
    const html = renderPage();
    expect(html).toContain('>Repository</span>');
    expect(html).toMatch(/<a [^>]*href="\/repos\/A"[^>]*>Alpha repository<\/a>/);
  });

  it('falls back to the id without making a request, and says when no repository is recorded', () => {
    expect(renderStatic(createElement(ConflictRepository, { repoId: 'A&B' }))).toMatch(/href="\/repos\/A%26B"[^>]*>A&amp;B<\/a>/);
    const none = renderStatic(createElement(ConflictRepository, { repoId: null }));
    expect(none).toContain('Not recorded');
    expect(none).not.toContain('href=');
    client.setQueryData(['conflict', 'c1'], conflict({ repo_id: 'B' }));
    expect(renderPage()).toMatch(/href="\/repos\/B"[^>]*>B<\/a>/);
    expect(client.getQueryCache().find({ queryKey: ['repos'] })).toBeUndefined();
  });

  it.each([
    ['detected', 'bg-red-900/50 text-red-300'],
    ['queued', 'bg-yellow-900/50 text-yellow-300'],
    ['deferred', 'bg-gray-700 text-gray-300'],
    ['resolved', 'bg-green-900/50 text-green-300'],
    ['pending', 'bg-gray-700 text-gray-300'],
  ])('the status mark shows the real value %s with its own colour', (status, colour) => {
    const html = renderStatic(createElement(ConflictStatusMark, { status }));
    expect(html).toContain(`rs-mark`);
    expect(html).toContain(colour);
    expect(html).toMatch(new RegExp(`>${status}</span>$`));
  });

  it('a deferred conflict is no longer drawn as detected', () => {
    client.setQueryData(['conflict', 'c1'], conflict({ status: 'deferred', resolution: 'deferred', resolved_by: 'api', resolved_at: stamp }));
    const html = renderPage();
    expect(html).toMatch(/rs-mark[^"]*bg-gray-700 text-gray-300">deferred</);
    expect(html).not.toContain('bg-red-900/50');
    expect(html).toContain('It still needs a decision.');
  });
});

describe('#264 G06.2 diff and common base', () => {
  it('keeps the SVN and Git comparison and adds the common base from the same read', () => {
    client.setQueryData(['conflict', 'c1'], conflict());
    const html = renderPage();
    expect(html).toContain('SVN Version');
    expect(html).toContain('Git Version');
    expect(html).toMatch(/role="region" aria-labelledby="([^"]+)"[^>]*>.*?<div id="\1"[^>]*>Common base<\/div>/);
    expect(html).toContain('base line');
  });

  it('distinguishes a base the server did not return from an empty one', () => {
    expect(renderToStaticMarkup(createElement(CommonBasePanel, { value: null, title: 'Common base' }))).toContain('The server returned no common base for this conflict.');
    expect(renderToStaticMarkup(createElement(CommonBasePanel, { value: '', title: 'Common base' }))).toContain('The common base is empty.');
    const text = renderToStaticMarkup(createElement(CommonBasePanel, { value: 'line', title: 'Common base' }));
    expect(text).toMatch(/<pre tabindex="0"[^>]*>line<\/pre>/);
  });

  it('draws the base with the same theme tokens as the diff viewer', () => {
    const html = renderToStaticMarkup(createElement(CommonBasePanel, { value: 'line', title: 'Common base' }));
    expect(html).toContain('var(--rs-diff-bg, #fff)');
    expect(html).toContain('var(--rs-diff-text, #212529)');
    expect(html).toContain('var(--rs-diff-title-bg, #fafbfc)');
  });

  it('draws no base panel where a caller passes none', () => {
    const html = renderToStaticMarkup(createElement(DiffViewer, { oldValue: 'a', newValue: 'b', leftTitle: 'Left', rightTitle: 'Right' }));
    expect(html).toContain('Left');
    expect(html).not.toContain('Common base');
  });
});

describe('#264 G06.4 resolution and defer', () => {
  it('preselects nothing, keeps the labels, and leaves "Record decision…" unavailable with its reason', () => {
    client.setQueryData(['conflict', 'c1'], conflict());
    const html = renderPage();
    expect(buttonTag(html, 'Accept SVN')).toContain('aria-pressed="false"');
    expect(buttonTag(html, 'Accept Git')).toContain('aria-pressed="false"');
    expect(html).not.toContain('Apply Manual Edit');
    const record = buttonTag(html, 'Record decision…');
    expect(record).toContain('disabled=""');
    const reasonId = record.match(/aria-describedby="([^"]+)"/)?.[1];
    expect(html).toContain(`id="${reasonId}" class="text-sm text-gray-400">Choose a decision first.</span>`);
    expect(buttonTag(html, 'Defer')).not.toContain('disabled=""');
    expect(html).not.toContain('<dialog');
  });

  it('states that the decision is recorded and no content is written (#73)', () => {
    client.setQueryData(['conflict', 'c1'], conflict());
    const html = renderPage();
    expect(html).toContain('At this version RepoSync records your decision. It does not write the chosen content to SVN or Git.');
    expect(html).toContain('issue #73');
  });

  const base = { headingId: 'h', status: 'detected', deferredAt: null, manualOffered: false, busy: false, stale: false, pendingText: '', onChoose: () => {}, onRecord: () => {}, onDefer: () => {} };
  it.each<ConflictChoice>(['accept_svn', 'accept_git', 'custom'])('a chosen %s is pressed alone and makes "Record decision…" available', choice => {
    const html = renderToStaticMarkup(createElement(ResolutionActions, { ...base, manualOffered: true, choice }));
    const labels = { accept_svn: 'Accept SVN', accept_git: 'Accept Git', custom: 'Apply Manual Edit' } as const;
    for (const [value, label] of Object.entries(labels)) expect(buttonTag(html, label)).toContain(`aria-pressed="${value === choice}"`);
    expect(html.match(/aria-pressed="true"/g)).toHaveLength(1);
    expect(buttonTag(html, labels[choice])).toContain('<span aria-hidden="true">✓ </span>');
    expect(buttonTag(html, 'Record decision…')).not.toContain('disabled=""');
    expect(html).not.toContain('Choose a decision first.');
  });

  it('offers "Apply Manual Edit" in the manual edit view only, as today', () => {
    expect(renderToStaticMarkup(createElement(ResolutionActions, { ...base, choice: null }))).not.toContain('Apply Manual Edit');
    expect(renderToStaticMarkup(createElement(ResolutionActions, { ...base, choice: null, manualOffered: true }))).toContain('Apply Manual Edit');
  });

  it('locks every action while a request is pending and says so in a status region', () => {
    const html = renderToStaticMarkup(createElement(ResolutionActions, { ...base, choice: 'accept_git', busy: true, pendingText: 'Recording decision…' }));
    for (const label of ['Accept SVN', 'Accept Git', 'Defer', 'Record decision…']) expect(buttonTag(html, label)).toContain('disabled=""');
    expect(html).toContain('<p role="status" class="text-sm text-gray-300">Recording decision…</p>');
  });

  it('shows when a deferred conflict was deferred and that it still needs a decision', () => {
    const html = renderToStaticMarkup(createElement(ResolutionActions, { ...base, choice: null, status: 'deferred', deferredAt: stamp }));
    expect(html).toContain('Deferred on ');
    expect(html).toContain('It still needs a decision.');
    expect(renderToStaticMarkup(createElement(ResolutionActions, { ...base, choice: null, status: 'deferred' }))).toContain('Deferred. It still needs a decision.');
  });

  it('asks once, naming the file, repository, decision and what is not chosen; going back is the safe default', () => {
    const html = renderStatic(createElement(DecisionConfirmation, { isOpen: true, conflict: conflict(), choice: 'accept_git', repoName: 'Alpha repository', busy: false, stale: false, onConfirm: () => {}, onCancel: () => {} }));
    expect(html.match(/<dialog/g)).toHaveLength(1);
    expect(html).toContain('Record this decision?');
    expect(html).toContain('This marks the conflict resolved in RepoSync. It does not change either repository.');
    expect(html).toContain('src/example.txt');
    expect(html).toContain('Alpha repository');
    expect(html).toContain('>Accept Git</dd>');
    expect(html).toContain('>SVN version (r12)</dd>');
    expect(html).toMatch(/<button type="button" data-dialog-cancel="true"[^>]*>Go back<\/button><button[^>]*>Record decision<\/button>/);
    expect(buttonTag(html, 'Record decision')).not.toContain('disabled=""');
  });

  it('names both sides as not chosen for a manual edit and says the server keeps no content', () => {
    const html = renderStatic(createElement(DecisionConfirmation, { isOpen: true, conflict: conflict(), choice: 'custom', busy: false, stale: false, onConfirm: () => {}, onCancel: () => {} }));
    expect(html).toContain('>Apply Manual Edit</dd>');
    expect(html).toContain(`>SVN version (r12) and Git version (${'a'.repeat(12)})</dd>`);
    expect(html).toContain('At this version the server does not store or apply it.');
    expect(html).toContain('>A</dd>');
  });

  it('cannot confirm while pending or while the conflict read is stale', () => {
    const props = { isOpen: true, conflict: conflict(), choice: 'accept_svn' as const, onConfirm: () => {}, onCancel: () => {} };
    const busy = renderStatic(createElement(DecisionConfirmation, { ...props, busy: true, stale: false }));
    expect(buttonTag(busy, 'Go back')).toContain('disabled=""');
    expect(buttonTag(busy, 'Record decision')).toContain('disabled=""');
    expect(busy).toContain('aria-busy="true"');
    const stale = renderStatic(createElement(DecisionConfirmation, { ...props, busy: false, stale: true }));
    expect(buttonTag(stale, 'Record decision')).toContain('disabled=""');
    expect(stale).toContain('Unavailable until the conflict is read again.');
  });

  it('draws no confirmation without a choice', () => {
    expect(renderStatic(createElement(DecisionConfirmation, { isOpen: true, conflict: conflict(), choice: null, busy: false, stale: false, onConfirm: () => {}, onCancel: () => {} }))).toBe('');
  });

  it('a resolved conflict shows its record and no decision controls', () => {
    client.setQueryData(['conflict', 'c1'], conflict({ status: 'resolved', resolution: 'accept_svn', resolved_by: 'api', resolved_at: stamp }));
    const html = renderPage();
    expect(html).toContain('Resolved as <strong>accept_svn</strong>');
    expect(html).not.toContain('Record decision');
    expect(html).toContain('bg-green-900/50 text-green-300');
  });
});

describe('#264 G06.7 loading, not found and read failure', () => {
  it('shows loading only while the first read is pending', () => {
    const html = renderPage();
    expect(html).toContain('<div role="status" class="text-center py-8 text-gray-400">Loading...</div>');
    expect(html).not.toContain('not found');
  });

  it('shows a missing conflict as not found, with the client error and a way back', async () => {
    await client.prefetchQuery({ queryKey: ['conflict', 'c1'], queryFn: failure(404, '{"error":"conflict \'c1\' not found"}') });
    const html = renderPage();
    expect(html).toContain('Conflict not found');
    expect(html).toContain('It may belong to a repository that was removed.');
    expect(html).toContain('API error 404:');
    expect(html).toContain('href="/conflicts"');
    expect(html).not.toContain('Loading...');
    expect(html).not.toContain('Record decision');
  });

  it('shows any other read failure as failed, with the client error and Retry', async () => {
    await client.prefetchQuery({ queryKey: ['conflict', 'c1'], queryFn: failure(500, '{"error":"internal server error"}') });
    const html = renderPage();
    expect(html).toMatch(/<div role="alert"[^>]*><p class="font-medium">The conflict could not be loaded\.<\/p><p>API error 500: \{&quot;error&quot;:&quot;internal server error&quot;\}<\/p>/);
    expect(buttonTag(html, 'Retry')).toBeTruthy();
    expect(html).not.toContain('Loading...');
    expect(html).not.toContain('Conflict not found');
  });

  it('keeps an earlier read on a failed refresh, marks it, and holds the actions that depend on it', async () => {
    client.setQueryData(['conflict', 'c1'], conflict());
    await client.fetchQuery({ queryKey: ['conflict', 'c1'], queryFn: failure(503, 'unavailable'), staleTime: 0 }).catch(() => undefined);
    const html = renderPage();
    expect(html).toContain('The latest read of this conflict failed: API error 503: unavailable. The details below are from an earlier read.');
    expect(html).toContain('src/example.txt');
    expect(buttonTag(html, 'Record decision…')).toContain('disabled=""');
    expect(buttonTag(html, 'Defer')).toContain('disabled=""');
    expect(html).toContain('Unavailable until the conflict is read again.');
  });
});
