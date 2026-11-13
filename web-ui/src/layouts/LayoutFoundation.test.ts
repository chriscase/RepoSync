import { afterEach, describe, expect, it, vi } from 'vitest';
import { createElement, isValidElement, type ReactElement, type ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter, Outlet, useRoutes } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import Layout, { LayoutFrame, PRIMARY_NAVIGATION, type LayoutFrameProps } from '../components/Layout';
import { LAYOUT_IDS, type LayoutId } from './layoutCatalog';
import { LAYOUT_PREFERENCE_PREFIX, preferenceKey } from './layoutPreference';

// #262 shell regressions without a DOM. Element-tree checks prove what React will
// reconcile; server rendering checks what each layout actually emits. Runtime
// switching (focus, drafts, scroll, requests) is in tests/layout-foundation.e2e.mjs.
const ORIGIN = 'https://ui.example';
const admin = { id: 'account-admin', username: 'admin-user', display_name: 'Synthetic administrator', role: 'admin' };
const member = { id: 'account-member', username: 'member-user', role: 'viewer' };

// The pre-#262 navigation, written out rather than derived from PRIMARY_NAVIGATION.
const ORIGINAL_NAV: ReadonlyArray<readonly [string, string]> = [
  ['/', 'Dashboard'], ['/repos', 'Repositories'], ['/conflicts', 'Conflicts'], ['/audit', 'Audit Log'], ['/settings', 'Settings'],
  ['/users', 'Users'], ['/ldap', 'LDAP'], ['/config', 'Config'], ['/setup', 'Setup'],
];
const MEMBER_NAV = ORIGINAL_NAV.slice(0, 5);

class MemoryStorage {
  values = new Map<string, string>();
  reads: string[] = [];
  writes: string[] = [];
  failPreferenceReads = false;
  getItem(key: string) {
    this.reads.push(key);
    if (this.failPreferenceReads && key.startsWith(LAYOUT_PREFERENCE_PREFIX)) throw new Error('Synthetic storage read refused');
    return this.values.get(key) ?? null;
  }
  setItem(key: string, value: string) { this.writes.push(key); this.values.set(key, value); }
  removeItem(key: string) { this.values.delete(key); }
  get length() { return this.values.size; }
  key(index: number) { return [...this.values.keys()][index] ?? null; }
}

function storedPreference(identity: string, layout: unknown): [string, string] {
  const key = preferenceKey(ORIGIN, identity);
  if (key === null) throw new Error(`expected an encodable preference key for ${JSON.stringify(identity)}`);
  return [key, JSON.stringify({ version: 1, layout, identity, origin: ORIGIN })];
}

function signIn(user: object | null, entries: ReadonlyArray<[string, string]> = []): MemoryStorage {
  const storage = new MemoryStorage();
  storage.values.set('session_token', 'synthetic-session-only');
  if (user) storage.values.set('user', JSON.stringify(user));
  for (const [key, value] of entries) storage.values.set(key, value);
  vi.stubGlobal('localStorage', storage);
  vi.stubGlobal('location', { origin: ORIGIN });
  return storage;
}

afterEach(() => { vi.unstubAllGlobals(); });

function ProbePage() {
  return createElement('section', { 'data-probe': 'routed-page' },
    createElement('h1', null, 'Synthetic routed page'),
    createElement('label', { htmlFor: 'probe-draft' }, 'Draft'),
    createElement('input', { id: 'probe-draft', defaultValue: 'unsaved synthetic draft' }));
}

function Routed() {
  return useRoutes([{ path: '/', element: createElement(Layout), children: [
    { index: true, element: createElement(ProbePage) },
    { path: 'repos', element: createElement(ProbePage) },
    { path: 'settings', element: createElement(ProbePage) },
  ] }]);
}

function renderShell(path = '/repos') {
  const client = new QueryClient();
  const html = renderToStaticMarkup(createElement(QueryClientProvider, { client },
    createElement(MemoryRouter, { initialEntries: [path] }, createElement(Routed))));
  return { html, client };
}

function count(html: string, needle: string) { return html.split(needle).length - 1; }

function mainContent(html: string): string {
  const open = html.indexOf('<main');
  const start = html.indexOf('>', open) + 1;
  const end = html.indexOf('</main>');
  expect(open).toBeGreaterThanOrEqual(0);
  expect(end).toBeGreaterThan(start);
  return html.slice(start, end);
}

function navLinks(html: string) {
  const nav = html.slice(html.indexOf('<nav'), html.indexOf('</nav>'));
  return [...nav.matchAll(/<a\b([^>]*)>([^<]*)/g)].map(([, attributes, label]) => ({
    href: /\bhref="([^"]*)"/.exec(attributes)?.[1],
    label,
    current: /\baria-current="page"/.test(attributes),
    active: /\brs-shell-link--active\b/.test(attributes),
  }));
}

function layoutOf(html: string) { return /data-rs-layout="([^"]+)"/.exec(html)?.[1]; }

// ---- element-tree helpers -------------------------------------------------

interface Step { readonly type: string; readonly key: string | null; readonly index: number }

function typeName(element: ReactElement): string {
  const type: unknown = element.type;
  if (typeof type === 'string') return type;
  if (typeof type === 'function') return `component:${type.name}`;
  return `other:${String(type)}`;
}

function childList(element: ReactElement): ReactNode[] {
  const children = (element.props as { children?: ReactNode }).children;
  return Array.isArray(children) ? children : [children];
}

/** Path of (type, key, child index) steps from the root element to the first match. */
function pathTo(node: ReactNode, match: (element: ReactElement) => boolean, index = 0): Step[] | null {
  if (Array.isArray(node)) {
    for (let i = 0; i < node.length; i++) {
      const found = pathTo(node[i], match, i);
      if (found) return found;
    }
    return null;
  }
  if (!isValidElement(node)) return null;
  const step: Step = { type: typeName(node), key: node.key, index };
  if (match(node)) return [step];
  const children = childList(node);
  for (let i = 0; i < children.length; i++) {
    const found = pathTo(children[i], match, i);
    if (found) return [step, ...found];
  }
  return null;
}

function elementAt(node: ReactNode, match: (element: ReactElement) => boolean): ReactElement | null {
  if (Array.isArray(node)) {
    for (const child of node) { const found = elementAt(child, match); if (found) return found; }
    return null;
  }
  if (!isValidElement(node)) return null;
  if (match(node)) return node;
  for (const child of childList(node)) { const found = elementAt(child, match); if (found) return found; }
  return null;
}

const STATUS = createElement('div', { role: 'status' }, 'Synthetic status');
const CHOOSER = createElement('div', { 'data-probe': 'chooser' }, 'Synthetic chooser');
const noop = () => {};
function frame(layout: LayoutId, overrides: Partial<LayoutFrameProps> = {}) {
  return LayoutFrame({ layout, isAdmin: true, activeConflicts: 3, accountName: 'Synthetic administrator', syncStatus: STATUS, chooser: CHOOSER, onLogout: noop, ...overrides });
}
const isMain = (element: ReactElement) => element.type === 'main';

describe('#262 one stable shell subtree', () => {
  it('keeps <main> and its <Outlet> at the same element path, with no keys, in every layout', () => {
    const classic = pathTo(frame('classic'), isMain);
    expect(classic).toEqual([{ type: 'div', key: null, index: 0 }, { type: 'main', key: null, index: 1 }]);
    for (const layout of LAYOUT_IDS) {
      const tree = frame(layout);
      expect(pathTo(tree, isMain), layout).toEqual(classic);
      expect(childList(tree).map(child => isValidElement(child) ? typeName(child) : child)).toEqual(['nav', 'main']);
      const main = elementAt(tree, isMain);
      expect(main?.props).toMatchObject({ tabIndex: -1 });
      const outlet = main ? childList(main) : [];
      expect(outlet).toHaveLength(1);
      expect(isValidElement(outlet[0]) && outlet[0].type).toBe(Outlet);
      expect(isValidElement(outlet[0]) && outlet[0].key).toBeNull();
    }
  });

  it('places the same status and chooser elements in the same slot in every layout', () => {
    const chooserPath = pathTo(frame('classic'), element => element === CHOOSER);
    const statusPath = pathTo(frame('classic'), element => element === STATUS);
    expect(chooserPath).not.toBeNull();
    expect(statusPath).not.toBeNull();
    for (const layout of LAYOUT_IDS) {
      expect(pathTo(frame(layout), element => element === CHOOSER), layout).toEqual(chooserPath);
      expect(pathTo(frame(layout), element => element === STATUS), layout).toEqual(statusPath);
    }
  });

  it('changes only classes and data attributes on the shell spine', () => {
    const spine = (layout: LayoutId) => {
      const tree = frame(layout);
      const nav = elementAt(tree, element => element.type === 'nav');
      const main = elementAt(tree, isMain);
      const strip = (props: object | undefined) => Object.fromEntries(Object.entries(props ?? {})
        .filter(([key]) => key !== 'className' && key !== 'children' && !key.startsWith('data-rs-')));
      return { root: strip(tree.props), nav: strip(nav?.props), main: strip(main?.props) };
    };
    for (const layout of LAYOUT_IDS) expect(spine(layout), layout).toEqual(spine('classic'));
    expect(spine('classic').nav).toEqual({ 'aria-label': 'Primary' });
  });
});

describe('#262 rendered shell', () => {
  it('renders the routed page once, inside the only main landmark, identically in every layout', () => {
    const contents = new Set<string>();
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const { html } = renderShell();
      expect(layoutOf(html)).toBe(layout);
      expect(html).toContain('data-rs-theme="dark"');
      expect(count(html, '<main')).toBe(1);
      expect(count(html, '<nav')).toBe(1);
      expect(html).toContain('<nav aria-label="Primary"');
      expect(count(html, 'data-probe="routed-page"')).toBe(1);
      const content = mainContent(html);
      expect(content).toContain('data-probe="routed-page"');
      expect(content).toContain('value="unsaved synthetic draft"');
      contents.add(content);
      expect(html).toMatch(/<main tabindex="-1"/);
    }
    expect(contents.size).toBe(1);
  });

  it('keeps navigation targets, labels, order, active matching and admin visibility in every layout', () => {
    expect(PRIMARY_NAVIGATION.map(item => [item.to, item.label])).toEqual(ORIGINAL_NAV);
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const adminLinks = navLinks(renderShell('/repos').html);
      expect(adminLinks.map(link => [link.href, link.label]), `${layout} admin`).toEqual(ORIGINAL_NAV);
      expect(adminLinks.filter(link => link.current).map(link => link.href)).toEqual(['/repos']);
      expect(adminLinks.filter(link => link.active).map(link => link.href)).toEqual(['/repos']);
      // Dashboard keeps `end`: it is active only on "/".
      expect(navLinks(renderShell('/').html).filter(link => link.current).map(link => link.href)).toEqual(['/']);

      signIn(member, [storedPreference(member.id, layout)]);
      const html = renderShell('/settings').html;
      expect(navLinks(html).map(link => [link.href, link.label]), `${layout} member`).toEqual(MEMBER_NAV);
      for (const href of ['/users', '/ldap', '/config', '/setup']) expect(html).not.toContain(`href="${href}"`);
      expect(html).not.toContain('Administration');
    }
  });

  it('shows the same status, account name and Logout action in every layout', () => {
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const { html } = renderShell();
      expect(html).toContain('role="status" aria-live="polite"');
      expect(count(html, 'Synthetic administrator')).toBe(1);
      expect(count(html, '>Logout</button>')).toBe(1);
    }
  });

  it('groups Team navigation under labelled groups and leaves Classic and Individual ungrouped', () => {
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const { html } = renderShell();
      const groups = [...html.matchAll(/<div role="group" aria-labelledby="([^"]+)"/g)].map(match => match[1]);
      if (layout === 'team') {
        expect(groups).toEqual(['rs-shell-nav-group-workspace', 'rs-shell-nav-group-account', 'rs-shell-nav-group-administration']);
        for (const [id, label] of [['workspace', 'Workspace'], ['account', 'Account'], ['administration', 'Administration']]) {
          expect(html).toMatch(new RegExp(`<p id="rs-shell-nav-group-${id}"[^>]*>${label}</p>`));
        }
      } else {
        expect(groups, layout).toEqual([]);
      }
    }
  });

  it('keeps Classic on its original shell structure', () => {
    signIn(admin);
    const { html } = renderShell();
    expect(layoutOf(html)).toBe('classic');
    expect(html).toContain('<div class="rs-layout-shell min-h-screen" data-rs-layout="classic" data-rs-theme="dark"><nav aria-label="Primary" class="rs-shell-chrome border-b"><div class="px-6 sm:px-8 lg:px-12"><div class="flex items-center justify-between h-16"><div class="flex items-center">');
    expect(html).toContain('<div class="ml-10 flex items-baseline space-x-4"><a');
    expect(html).toContain('<div class="flex items-center space-x-4">');
    expect(html).toContain('<main tabindex="-1" class="py-6 px-6 sm:px-8 lg:px-12">');
  });

  it('renders the conflict count badge in every layout from the same status value', () => {
    for (const layout of LAYOUT_IDS) {
      const html = renderToStaticMarkup(createElement(MemoryRouter, null, frame(layout)));
      expect(html, layout).toMatch(/>Conflicts<span class="[^"]*rs-shell-alert[^"]*">3<\/span><\/a>/);
      expect(renderToStaticMarkup(createElement(MemoryRouter, null, frame(layout, { activeConflicts: 0 })))).not.toContain('rs-shell-alert');
    }
  });
});

describe('#262 presentation is not capability', () => {
  it('creates the same queries and no mutations whatever layout is chosen', () => {
    const hashes = new Set<string>();
    for (const layout of LAYOUT_IDS) {
      signIn(member, [storedPreference(member.id, layout)]);
      const { client } = renderShell();
      hashes.add(JSON.stringify(client.getQueryCache().getAll().map(query => query.queryHash).sort()));
      expect(client.getMutationCache().getAll()).toEqual([]);
    }
    expect(hashes.size).toBe(1);
  });

  it('does not let a stored Team preference reveal administration to a member', () => {
    signIn(member, [storedPreference(member.id, 'team')]);
    const { html } = renderShell();
    expect(layoutOf(html)).toBe('team');
    expect(navLinks(html).map(link => link.href)).toEqual(MEMBER_NAV.map(([href]) => href));
    expect(html).not.toContain('rs-shell-nav-group-administration');
  });
});

describe('#262 preference in the shell', () => {
  it('uses each account\'s own preference and Classic for an account without one', () => {
    const entries = [storedPreference(admin.id, 'team'), storedPreference(member.id, 'individual')];
    signIn(admin, entries);
    expect(layoutOf(renderShell().html)).toBe('team');
    signIn(member, entries);
    expect(layoutOf(renderShell().html)).toBe('individual');
    signIn({ id: 'account-third', username: 'third', role: 'viewer' }, entries);
    const html = renderShell().html;
    expect(layoutOf(html)).toBe('classic');
    expect(html).not.toContain('Using Classic');
  });

  it('falls back to Classic, visibly, when a stored preference is corrupt, unknown, foreign or unreadable', () => {
    const [key] = storedPreference(admin.id, 'team');
    const cases: ReadonlyArray<readonly [string, string]> = [
      ['corrupt', '{not json'],
      ['unknown', storedPreference(admin.id, 'compact')[1]],
      ['foreign', JSON.stringify({ version: 1, layout: 'team', identity: member.id, origin: ORIGIN })],
      ['future', JSON.stringify({ version: 2, layout: 'team', identity: admin.id, origin: ORIGIN })],
    ];
    for (const [name, raw] of cases) {
      signIn(admin, [[key, raw]]);
      const html = renderShell().html;
      expect(layoutOf(html), name).toBe('classic');
      expect(html, name).toContain('Using Classic');
    }
    const storage = signIn(admin, [storedPreference(admin.id, 'team')]);
    storage.failPreferenceReads = true;
    let html = '';
    expect(() => { html = renderShell().html; }).not.toThrow();
    expect(layoutOf(html)).toBe('classic');
    expect(html).toContain('Using Classic');
    expect(navLinks(html).map(link => link.href)).toEqual(ORIGINAL_NAV.map(([href]) => href));
  });

  it('does not read any preference for a sign-in without a stable account identity', () => {
    const storage = signIn({ username: 'legacy-login', role: 'admin' }, [storedPreference(admin.id, 'team')]);
    const html = renderShell().html;
    expect(layoutOf(html)).toBe('classic');
    expect(storage.reads.filter(key => key.startsWith(LAYOUT_PREFERENCE_PREFIX))).toEqual([]);
  });

  it('offers a labelled native layout select with the current layout selected', () => {
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const { html } = renderShell();
      const select = /<select id="([^"]+)" aria-describedby="([^" ]+) ([^"]+)"[^>]*data-rs-layout-chooser=""[^>]*>([\s\S]*?)<\/select>/.exec(html);
      expect(select, layout).not.toBeNull();
      const [, id, hintId, noticeId, options] = select ?? [];
      expect(html).toContain(`<label for="${id}"`);
      expect(html).toMatch(new RegExp(`<label for="${id}"[^>]*>[\\s\\S]*?<span class="sr-only">Layout</span></label>`));
      expect(html).toContain(`<span id="${hintId}" class="sr-only">Changes only how navigation is arranged. Pages, actions, permissions and server mode stay the same.</span>`);
      expect(html).toMatch(new RegExp(`<p id="${noticeId}" role="status"`));
      expect([...(options ?? '').matchAll(/<option value="([^"]+)"( selected="")?>([^<]+)<\/option>/g)].map(match => [match[1], match[3], Boolean(match[2])]))
        .toEqual(LAYOUT_IDS.map(id => [id, { classic: 'Classic', individual: 'Individual', team: 'Team' }[id], id === layout]));
    }
  });
});

describe('#262 retained-review corrections in the shell', () => {
  const preferenceReads = (storage: MemoryStorage) => storage.reads.filter(key => key.startsWith(LAYOUT_PREFERENCE_PREFIX));

  it('renders Classic for a stored account id with a lone high or low surrogate, with no preference read or write (RS262-1)', () => {
    for (const id of ['\ud800', '\udc00']) {
      const storage = signIn({ id, username: 'surrogate-account', role: 'admin' }, [storedPreference(admin.id, 'team')]);
      // The unchanged session reader really hands Layout the lone surrogate.
      expect(JSON.parse(storage.values.get('user') ?? '{}').id).toBe(id);
      let html = '';
      expect(() => { html = renderShell().html; }).not.toThrow();
      expect(layoutOf(html)).toBe('classic');
      expect(html).not.toContain('Using Classic');
      expect(navLinks(html).map(link => link.href)).toEqual(ORIGINAL_NAV.map(([href]) => href));
      expect(preferenceReads(storage)).toEqual([]);
      expect(storage.writes).toEqual([]);
    }
  });

  it('applies a valid surrogate-pair account\'s own stored preference (RS262-1)', () => {
    const id = '😀';
    const storage = signIn({ id, username: 'pair-account', role: 'viewer' }, [storedPreference(id, 'team')]);
    expect(layoutOf(renderShell().html)).toBe('team');
    expect(preferenceReads(storage)).toEqual([storedPreference(id, 'team')[0]]);
    expect(storage.writes).toEqual([]);
  });

  it('shows Classic quietly and touches no preference for an id whose record would be too long (RS262-2)', () => {
    const storage = signIn({ id: '"'.repeat(256), username: 'quoted-account', role: 'viewer' });
    const html = renderShell().html;
    expect(layoutOf(html)).toBe('classic');
    expect(html).not.toContain('Using Classic');
    expect(preferenceReads(storage)).toEqual([]);
    expect(storage.writes).toEqual([]);
  });

  it('keeps existing auth-storage behavior when the browser refuses localStorage entirely (RS262-3)', () => {
    signIn(admin, [storedPreference(admin.id, 'team')]);
    const saved = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    Object.defineProperty(globalThis, 'localStorage', { configurable: true, get() { throw new Error('Synthetic SecurityError'); } });
    try {
      let html = '';
      expect(() => { html = renderShell().html; }).not.toThrow();
      // The unchanged session reader cannot read an account either, so there is no account preference to report.
      expect(layoutOf(html)).toBe('classic');
      expect(html).not.toContain('Using Classic');
      expect(html).not.toContain('Synthetic administrator');
      expect(navLinks(html).map(link => [link.href, link.label])).toEqual(MEMBER_NAV);
    } finally {
      if (saved) Object.defineProperty(globalThis, 'localStorage', saved); else Reflect.deleteProperty(globalThis, 'localStorage');
    }
  });
});
