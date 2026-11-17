import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement, type ComponentType } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { api, type CommitMapEntry, type SyncRecord } from '../api';
import { ConfirmModal } from '../components/ConfirmModal';
import Conflicts from './Conflicts';
import Dashboard from './Dashboard';
import { ResumeSyncDialog, resumeSyncConsequences, saveTextDownload, sectionNavDisplayed } from './RepoDetail';
import { DEFAULT_DATA, GitStep, SvnStep, SyncStep, testConnectionClass, type WizardData } from './SetupWizard';

// #264 fixes from the independent review (R2, R3, R4, R10, R11, R12). Static rendering and
// source checks only (no DOM). Synthetic fixtures only.

// This project installs no Node type definitions (see LayoutFoundation.test.ts), so node:fs is
// loaded through a specifier the type checker does not resolve. Vitest runs these tests in Node.
const NODE_FS = 'node:fs';
interface NodeFs {
  readFileSync(path: URL, encoding: 'utf8'): string;
  readdirSync(path: URL): string[];
  statSync(path: URL): { isDirectory(): boolean };
}
const nodeFs = async () => (await import(/* @vite-ignore */ NODE_FS)) as NodeFs;
const readSource = async (path: string) => (await nodeFs()).readFileSync(new URL(path, import.meta.url), 'utf8');

const noop = () => {};
const stamp = '2026-11-17T09:00:00Z';
const fetchMock = vi.fn();
let client: QueryClient;

beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false, retryOnMount: false, staleTime: Infinity } } });
  const storage = new Map([['session_token', 'fixture-token'], ['user', '{"id":"actor","role":"admin"}']]);
  vi.stubGlobal('localStorage', { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, v: string) => storage.set(key, v), removeItem: (key: string) => storage.delete(key), get length() { return storage.size; }, key: (index: number) => Array.from(storage.keys())[index] ?? null });
  vi.stubGlobal('window', { location: { pathname: '/', href: '/' } });
  vi.stubGlobal('fetch', fetchMock);
  fetchMock.mockReset();
});
afterEach(() => { client.clear(); vi.unstubAllGlobals(); vi.useRealTimers(); vi.restoreAllMocks(); });

function render(component: ComponentType, path = '/') {
  return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(MemoryRouter, { initialEntries: [path] }, createElement(component))));
}
/** Every button's opening-tag attributes and its text. */
function buttons(html: string) {
  return [...html.matchAll(/<button([^>]*)>([\s\S]*?)<\/button>/g)].map(([, attrs, inner]) => ({
    attrs,
    text: inner.replace(/<[^>]+>/g, ''),
    classes: (/ class="([^"]*)"/.exec(attrs)?.[1] ?? '').split(' '),
    pressed: / aria-pressed="(true|false)"/.exec(attrs)?.[1] ?? null,
  }));
}
const button = (html: string, text: string | RegExp) => buttons(html).find(b => (typeof text === 'string' ? b.text === text : text.test(b.text)));

describe('#264 R2: Edit changes section only where the section navigation is displayed', () => {
  it('decides from the navigation\'s computed display, as the scroll effect does', () => {
    // Classic, Individual and Team hide the section navigation: Edit leaves the URL, history and scroll.
    expect(sectionNavDisplayed('none')).toBe(false);
    // Before the navigation renders there is nothing to decide from: no switch.
    expect(sectionNavDisplayed(null)).toBe(false);
    expect(sectionNavDisplayed(undefined)).toBe(false);
    // Ledger, Console and Guide display it: Edit and Replace show Settings.
    for (const display of ['flex', 'block', 'grid', 'inline-flex']) expect(sectionNavDisplayed(display), display).toBe(true);
  });

  it('routes every programmatic switch to Settings through that decision, and the scroll effect too', async () => {
    const source = await readSource('./RepoDetail.tsx');
    const switches = [...source.matchAll(/^.*showSection\('settings'\).*$/gm)].map(([line]) => line.trim());
    expect(switches).toEqual(["if (sectionNavDisplayed(readSectionNavDisplay())) showSection('settings');"]);
    // Replace on the credentials summary goes through Edit when the form is closed.
    expect(source).toMatch(/function replaceCredential\(field: CredentialField\) \{\n\s+if \(!editing\) startEdit\(\);/);
    expect(source).toContain('if (sectionNavDisplayed(getComputedStyle(nav).display)) return;');
    expect(source).not.toContain("getComputedStyle(nav).display !== 'none'");
  });
});

describe('#264 R3: a destructive confirmation keeps its danger styling', () => {
  const modal = (variant: 'danger' | 'default') => renderToStaticMarkup(createElement(ConfirmModal, {
    isOpen: true, title: 'Fixture?', message: 'Fixture message.', confirmLabel: 'Do it', onConfirm: noop, onCancel: noop, variant,
  }));

  it('marks the danger confirm button as danger, never primary', () => {
    const confirm = button(modal('danger'), 'Do it');
    expect(confirm?.classes.slice(0, 2)).toEqual(['rs-btn', 'rs-btn--danger']);
    expect(confirm?.classes).not.toContain('rs-btn--primary');
    expect(confirm?.classes).toEqual(expect.arrayContaining(['rs-text-on-danger', 'bg-red-600', 'hover:bg-red-700']));
  });

  it('marks any other confirm button as primary, never danger', () => {
    const confirm = button(modal('default'), 'Do it');
    expect(confirm?.classes.slice(0, 2)).toEqual(['rs-btn', 'rs-btn--primary']);
    expect(confirm?.classes).not.toContain('rs-btn--danger');
    expect(confirm?.classes).toEqual(expect.arrayContaining(['rs-text-on-accent', 'bg-blue-600', 'hover:bg-blue-700']));
  });

  it('keeps Cancel first and unstyled by a variant', () => {
    const [cancel] = buttons(modal('danger'));
    expect(cancel.text).toBe('Cancel');
    expect(cancel.attrs).toContain('data-dialog-cancel');
    expect(cancel.classes.some(c => c.startsWith('rs-btn'))).toBe(false);
  });
});

describe('#264 R4: a control that shows its state by colour carries the primary variant only when selected', () => {
  const wizard = (over: Partial<WizardData> = {}): WizardData => ({ ...DEFAULT_DATA, ...over });

  /** Selected: rs-btn rs-btn--primary and aria-pressed true. Not selected: rs-btn rs-btn--quiet and aria-pressed false. */
  function expectChoice(choice: ReturnType<typeof button>, selected: boolean) {
    expect(choice).toBeDefined();
    expect(choice?.classes).toContain('rs-btn');
    expect(choice?.classes.includes('rs-btn--primary')).toBe(selected);
    expect(choice?.classes.includes('rs-btn--quiet')).toBe(!selected);
    expect(choice?.classes.some(c => c === 'rs-btn--danger' || c === 'rs-btn--state')).toBe(false);
    expect(choice?.pressed).toBe(String(selected));
  }

  it('conflict status filter: only the current status is primary', () => {
    for (const [path, current] of [['/conflicts', 'All'], ['/conflicts?status=queued', 'Queued'], ['/conflicts?status=resolved', 'Resolved']] as const) {
      const html = render(Conflicts, path);
      for (const label of ['All', 'Detected', 'Queued', 'Deferred', 'Resolved']) expectChoice(button(html, label), label === current);
    }
  });

  it('Git provider: the chosen provider is primary and pressed, the other quiet and not pressed', () => {
    for (const provider of ['github', 'gitea'] as const) {
      const html = renderToStaticMarkup(createElement(GitStep, { data: wizard({ git_provider: provider }), update: noop, errors: {}, gitTokenSaved: false, gitTokenDirty: false, onGitTokenChange: noop }));
      expectChoice(button(html, 'GitHub / GitHub Enterprise'), provider === 'github');
      expectChoice(button(html, 'Gitea'), provider === 'gitea');
    }
  });

  it('sync mode: the chosen mode is primary and pressed, the other quiet', () => {
    for (const mode of ['direct', 'pr'] as const) {
      const html = renderToStaticMarkup(createElement(SyncStep, { data: wizard({ sync_mode: mode }), update: noop, errors: {} }));
      expectChoice(button(html, /^Direct Push/), mode === 'direct');
      expectChoice(button(html, /^Pull Request/), mode === 'pr');
    }
  });

  it('switches keep their own shape: no rs-btn class on a role="switch" in the wizard', () => {
    for (const on of [true, false]) {
      const html = renderToStaticMarkup(createElement(SyncStep, { data: wizard({ sync_auto_merge: on, sync_tags: on }), update: noop, errors: {} }));
      const switches = buttons(html).filter(b => / role="switch"/.test(b.attrs));
      expect(switches.length).toBeGreaterThanOrEqual(2);
      for (const toggle of switches) {
        expect(toggle.classes.some(c => c.startsWith('rs-btn')), toggle.attrs).toBe(false);
        expect(toggle.attrs).toContain(`aria-checked="${on}"`);
        expect(toggle.classes).toContain(on ? 'bg-blue-600' : 'bg-gray-600');
      }
    }
  });

  it('switches keep their own shape in every page and component source', async () => {
    const fs = await nodeFs();
    const offenders: string[] = [];
    let seen = 0;
    for (const dir of ['../pages/', '../components/']) {
      const base = new URL(dir, import.meta.url);
      for (const name of fs.readdirSync(base)) {
        if (!name.endsWith('.tsx')) continue;
        const source = fs.readFileSync(new URL(name, base), 'utf8');
        for (const match of source.matchAll(/role="switch"/g)) {
          seen++;
          const start = source.lastIndexOf('<button', match.index);
          const end = source.indexOf('<span', match.index);
          if (/rs-btn/.test(source.slice(start, end))) offenders.push(`${name}:${source.slice(0, match.index).split('\n').length}`);
        }
      }
    }
    expect(seen).toBeGreaterThanOrEqual(3);
    expect(offenders).toEqual([]);
  });

  it('connection test button: quiet while idle or testing, its own result colours after, never danger', () => {
    const svn = renderToStaticMarkup(createElement(SvnStep, { data: wizard(), update: noop, errors: {}, svnPasswordSaved: false, svnPasswordDirty: false, onSvnPasswordChange: noop }));
    const git = renderToStaticMarkup(createElement(GitStep, { data: wizard(), update: noop, errors: {}, gitTokenSaved: false, gitTokenDirty: false, onGitTokenChange: noop }));
    for (const html of [svn, git]) {
      const idle = button(html, 'Test Connection');
      expect(idle?.classes).toEqual(expect.arrayContaining(['rs-btn', 'rs-btn--quiet', 'bg-gray-700', 'text-gray-300']));
      expect(idle?.classes).not.toContain('rs-btn--danger');
    }
    for (const state of ['idle', 'loading'] as const) {
      const classes = testConnectionClass(state).split(' ');
      expect(classes).toEqual(expect.arrayContaining(['rs-btn', 'rs-btn--quiet', 'bg-gray-700', 'text-gray-300']));
      expect(classes).not.toContain('rs-btn--danger');
    }
    const success = testConnectionClass('success').split(' ');
    expect(success).toEqual(expect.arrayContaining(['rs-btn', 'bg-emerald-600/20', 'text-emerald-400', 'border-emerald-600']));
    const failure = testConnectionClass('error').split(' ');
    expect(failure).toEqual(expect.arrayContaining(['rs-btn', 'bg-red-600/20', 'text-red-400', 'border-red-600']));
    for (const classes of [success, failure]) {
      expect(classes.filter(c => c.startsWith('rs-btn--'))).toEqual([]);
    }
  });

  it('no page gives a fixed primary, danger or state variant to a button whose colour classes change with a condition', async () => {
    const fs = await nodeFs();
    const offenders: string[] = [];
    for (const dir of ['../pages/', '../components/']) {
      const base = new URL(dir, import.meta.url);
      for (const name of fs.readdirSync(base)) {
        if (!name.endsWith('.tsx')) continue;
        const source = fs.readFileSync(new URL(name, base), 'utf8');
        for (const match of source.matchAll(/className=\{`/g)) {
          // The template literal, with the text inside ${…} set apart from its fixed text.
          let depth = 0;
          let fixed = '';
          let dynamic = '';
          let i = (match.index ?? 0) + match[0].length;
          for (; i < source.length; i++) {
            const char = source[i];
            if (depth === 0 && char === '`') break;
            if (char === '$' && source[i + 1] === '{' && depth === 0) { depth = 1; i++; continue; }
            if (depth > 0) {
              if (char === '{') depth++;
              if (char === '}') depth--;
              if (depth > 0) dynamic += char;
              continue;
            }
            fixed += char;
          }
          const colourChoice = /\?[\s\S]*\b(bg|text|border)-[a-z]+-\d{2,3}[\s\S]*:[\s\S]*\b(bg|text|border)-[a-z]+-\d{2,3}/.test(dynamic);
          if (colourChoice && /\brs-btn--(primary|danger|state)\b/.test(fixed)) {
            offenders.push(`${name}:${source.slice(0, match.index).split('\n').length}`);
          }
        }
      }
    }
    expect(offenders).toEqual([]);
  });
});

describe('#264 R10: a failed dashboard refresh keeps the last loaded list', () => {
  const syncRecord: SyncRecord = { id: 'rec-1', repo_id: 'A', svn_rev: 4127, git_sha: '9f3c2ab', direction: 'svn_to_git', author: 'mei', message: 'Tune ramp-rate limits', timestamp: stamp, synced_at: stamp, status: 'applied' };
  const mapping: CommitMapEntry = { id: 1, repo_id: 'A', svn_rev: 41, git_sha: 'a'.repeat(40), direction: 'svn_to_git', synced_at: stamp, svn_author: 'mei', git_author: 'mei' };

  /** A refetch that fails after a successful read: the query keeps its data and reports the error. */
  async function failRefetch(queryKey: readonly unknown[], data: unknown, queryFn: () => Promise<unknown>) {
    client.setQueryData(queryKey, data, { updatedAt: Date.now() - 60_000 });
    fetchMock.mockResolvedValueOnce(new Response('{"error":"bad gateway"}', { status: 502 }));
    await expect(client.fetchQuery({ queryKey, queryFn, retry: false, staleTime: 0 })).rejects.toThrow();
    const state = client.getQueryState(queryKey);
    expect(state?.status).toBe('error');
    expect(state?.data).toEqual(data);
  }
  function region(html: string, name: string) {
    const start = html.indexOf(`data-rs-region="${name}"`);
    expect(start).toBeGreaterThan(-1);
    const next = html.slice(start + 1).search(/data-rs-region="/);
    return next === -1 ? html.slice(start) : html.slice(start, start + 1 + next);
  }
  const text = (html: string) => html.replace(/<[^>]+>/g, '').replace(/&quot;/g, '"').replace(/&amp;/g, '&');

  it('sync records: the last list stays, with a note that it may be out of date and the client error text', async () => {
    await failRefetch(['sync-records', undefined], { entries: [syncRecord], total: 1 }, () => api.getSyncRecords(20));
    const records = region(render(Dashboard, '/'), 'records');
    expect(records).toMatch(/<div role="status" class="rs-bn rs-bn--warn [^"]*"><p class="font-medium">Sync records could not be refreshed\.<\/p>/);
    expect(text(records)).toMatch(/The records below were last loaded at .+ and may be out of date\./);
    expect(text(records)).toContain('API error 502: {"error":"bad gateway"}');
    expect(records).not.toContain('Sync records could not be loaded.');
    expect(records).not.toContain('Results are unknown. This is not the same as none.');
    expect(records).not.toContain('No sync records yet');
  });

  it('sync records: a refresh that fails after an empty read still says the empty list may be out of date', async () => {
    await failRefetch(['sync-records', undefined], { entries: [], total: 0 }, () => api.getSyncRecords(20));
    const records = region(render(Dashboard, '/'), 'records');
    expect(records).toContain('Sync records could not be refreshed.');
    expect(records).toContain('No sync records yet');
  });

  it('commit map: the last table stays, with the note above it', async () => {
    await failRefetch(['commit-map', undefined], { entries: [mapping], total: 1 }, () => api.getCommitMap(15));
    const map = region(render(Dashboard, '/'), 'commit-map');
    expect(map).toContain('Commit map could not be refreshed.');
    expect(text(map)).toMatch(/The mappings below were last loaded at .+ and may be out of date\./);
    expect(text(map)).toContain('API error 502');
    expect(map).toContain('<table');
    expect(map).toContain('r41');
    expect(map.indexOf('could not be refreshed')).toBeLessThan(map.indexOf('<table'));
    expect(map).not.toContain('Commit map could not be loaded.');
  });

  it('shows the failed panel, not a stale note, when nothing was loaded', async () => {
    fetchMock.mockResolvedValueOnce(new Response('{"error":"bad gateway"}', { status: 502 }));
    await expect(client.fetchQuery({ queryKey: ['commit-map', undefined], queryFn: () => api.getCommitMap(15), retry: false })).rejects.toThrow();
    const map = region(render(Dashboard, '/'), 'commit-map');
    expect(map).toContain('Commit map could not be loaded.');
    expect(map).not.toContain('could not be refreshed');
    expect(map).not.toContain('<table');
  });
});

describe('#264 R11: the hook download keeps its object URL long enough', () => {
  it('revokes the object URL 60 seconds after the click, not at once', () => {
    vi.useFakeTimers();
    const link = { href: '', download: '', rel: '', style: { display: '' }, click: vi.fn(), remove: vi.fn() };
    const doc = { createElement: vi.fn(() => link), body: { appendChild: vi.fn() } } as unknown as Document;
    vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:hook');
    const revoke = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});
    saveTextDownload('pre-commit', '#!/bin/sh', doc);
    expect(link.click).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(0);
    expect(revoke).not.toHaveBeenCalled();
    vi.advanceTimersByTime(59_999);
    expect(revoke).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(revoke).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledWith('blob:hook');
  });
});

describe('#264 R12: accessibility', () => {
  it('keeps the empty layout notice in the accessibility tree: hidden visually, never display:none', async () => {
    const css = (await readSource('../layouts/layoutOptions.css')).replace(/\/\*[\s\S]*?\*\//g, '');
    const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].filter(([, selector]) => selector.includes('rs-layout-chooser__notice:empty'));
    expect(rules.length).toBeGreaterThan(0);
    for (const [, , body] of rules) {
      expect(body).not.toMatch(/display\s*:\s*none/);
      for (const declaration of [/position:\s*absolute/, /width:\s*1px/, /height:\s*1px/, /overflow:\s*hidden/, /clip:\s*rect\(0, 0, 0, 0\)/]) {
        expect(body).toMatch(declaration);
      }
    }
    // No rule anywhere in the file hides the notice with display:none.
    for (const [, selector, body] of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
      if (selector.includes('rs-layout-chooser__notice')) expect(body, selector).not.toMatch(/display\s*:\s*none/);
    }
  });

  it('describes the resume dialog with the repository and what resuming does and does not do', () => {
    const html = renderToStaticMarkup(createElement(ResumeSyncDialog, {
      repoName: 'kiln-controller', pendingCount: 2, allowed: true, pending: false, error: null, onConfirm: noop, onClose: noop,
    }));
    const describedBy = /<dialog[^>]* aria-describedby="([^"]+)"/.exec(html)?.[1];
    expect(describedBy).toBeTruthy();
    const description = new RegExp(`<div id="${describedBy}"[^>]*>([\\s\\S]*?)</div>`).exec(html)?.[1] ?? '';
    expect(description).toContain('kiln-controller');
    for (const line of resumeSyncConsequences(2)) expect(description).toContain(line);
    // The consequences are said once, in the description.
    expect(html.split(resumeSyncConsequences(2)[0]).length - 1).toBe(1);
  });
});
