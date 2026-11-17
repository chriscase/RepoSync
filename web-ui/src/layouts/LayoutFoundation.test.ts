import { afterEach, describe, expect, it, vi } from 'vitest';
import { createElement, isValidElement, type ReactElement, type ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { Outlet, useRoutes } from 'react-router-dom';
import { StaticRouter } from 'react-router-dom/server';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import Layout, { LayoutFrame, type LayoutFrameProps } from '../components/Layout';
import { LAYOUT_CATALOG, LAYOUT_IDS, THEME_IDS, usesPageTokens, type LayoutId, type ThemeId } from './layoutCatalog';
import { LAYOUT_PREFERENCE_PREFIX, preferenceKey } from './layoutPreference';
import { VocabularyProvider } from './VocabularyProvider';
import { useVocabulary } from './vocabulary';

// #262 shell regressions without a DOM, extended by #264. Element-tree checks prove what React
// will reconcile; server rendering checks what each layout actually emits. Runtime switching
// (focus, drafts, scroll, requests) is in tests/layout-foundation.e2e.mjs. Rendering uses
// StaticRouter, which is made for the server, so react-router skips the layout effects that
// would only warn there.
const ORIGIN = 'https://ui.example';
const admin = { id: 'account-admin', username: 'admin-user', display_name: 'Synthetic administrator', role: 'admin' };
const member = { id: 'account-member', username: 'member-user', role: 'viewer' };

/** The layouts that existed before #264, written out rather than taken from the catalog. */
const EXISTING: readonly LayoutId[] = ['classic', 'individual', 'team'];
/** The three accepted #263 designs that #264 adds. */
const NEW_LAYOUTS: readonly LayoutId[] = ['ledger', 'console', 'guide'];
const isExisting = (layout: LayoutId) => EXISTING.includes(layout);
/** Every layout except Classic and Individual groups its navigation. */
const isGrouped = (layout: LayoutId) => layout !== 'classic' && layout !== 'individual';

// The pre-#262 navigation, written out rather than derived from PRIMARY_NAVIGATION.
const ORIGINAL_NAV: ReadonlyArray<readonly [string, string]> = [
  ['/', 'Dashboard'], ['/repos', 'Repositories'], ['/conflicts', 'Conflicts'], ['/audit', 'Audit Log'], ['/settings', 'Settings'],
  ['/users', 'Users'], ['/ldap', 'LDAP'], ['/config', 'Config'], ['/setup', 'Setup'],
];
const MEMBER_NAV = ORIGINAL_NAV.slice(0, 5);
// The #264 plain vocabulary of Ledger, Console and Guide, with E4's Help, written out the same way.
const PLAIN_NAV: ReadonlyArray<readonly [string, string]> = [
  ['/', 'Dashboard'], ['/repos', 'Repositories'], ['/conflicts', 'Conflicts'], ['/audit', 'Audit log'], ['/settings', 'My account'],
  ['/docs', 'Help'], ['/users', 'Users'], ['/ldap', 'Directory (LDAP)'], ['/config', 'Identity & notifications'], ['/setup', 'Server setup'],
];
const PLAIN_MEMBER_NAV = PLAIN_NAV.slice(0, 6);
const ADMIN_HREFS = ['/users', '/ldap', '/config', '/setup'];

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

function keyFor(identity: string): string {
  const key = preferenceKey(ORIGIN, identity);
  if (key === null) throw new Error(`expected an encodable preference key for ${JSON.stringify(identity)}`);
  return key;
}

/** A preference as #264 saves it: version 2 with a theme. */
function storedPreference(identity: string, layout: unknown, theme: unknown = 'dark'): [string, string] {
  return [keyFor(identity), JSON.stringify({ version: 2, layout, theme, identity, origin: ORIGIN })];
}

/** A preference as #262 saved it: version 1, layout only. */
function legacyPreference(identity: string, layout: unknown): [string, string] {
  return [keyFor(identity), JSON.stringify({ version: 1, layout, identity, origin: ORIGIN })];
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

/** A routed page that shows which vocabulary (E3) the shell gives it. */
function VocabularyProbe() {
  return createElement('p', { 'data-probe': 'vocabulary' }, useVocabulary());
}

function Routed() {
  return useRoutes([{ path: '/', element: createElement(Layout), children: [
    { index: true, element: createElement(ProbePage) },
    { path: 'repos', element: createElement(ProbePage) },
    { path: 'settings', element: createElement(ProbePage) },
    { path: 'docs', element: createElement(ProbePage) },
    { path: 'vocabulary', element: createElement(VocabularyProbe) },
  ] }]);
}

function renderShell(path = '/repos') {
  const client = new QueryClient();
  const html = renderToStaticMarkup(createElement(QueryClientProvider, { client },
    createElement(StaticRouter, { location: path }, createElement(Routed))));
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

function decode(text: string) {
  return text.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#x27;/g, "'").replace(/&amp;/g, '&');
}

function navHtml(html: string) { return html.slice(html.indexOf('<nav'), html.indexOf('</nav>')); }

/** Each primary navigation link: target, visible label, state, classes and the E6 icon that leads it. */
function navLinks(html: string) {
  return [...navHtml(html).matchAll(/<a\b([^>]*)>([\s\S]*?)<\/a>/g)].map(([, attributes, inner]) => {
    const icon = /^<svg\b([^>]*)>[\s\S]*?<\/svg>/.exec(inner);
    const rest = icon ? inner.slice(icon[0].length) : inner;
    return {
      href: /\bhref="([^"]*)"/.exec(attributes)?.[1],
      label: decode(/^[^<]*/.exec(rest)?.[0] ?? ''),
      className: /\bclass="([^"]*)"/.exec(attributes)?.[1],
      current: /\baria-current="page"/.test(attributes),
      active: /\brs-shell-link--active\b/.test(attributes),
      icon: icon ? {
        classes: (/\bclass="([^"]*)"/.exec(icon[1])?.[1] ?? '').split(' '),
        hidden: /\baria-hidden="true"/.test(icon[1]),
      } : null,
    };
  });
}

/** The labelled navigation groups, each with the targets inside it and whether it holds the More button. */
function navGroups(html: string) {
  const nav = navHtml(html);
  const starts = [...nav.matchAll(/<div id="([^"]*)" role="group" aria-labelledby="([^"]*)" data-group="([^"]*)" class="([^"]*)">/g)];
  return starts.map((start, index) => {
    const from = start.index ?? 0;
    const to = index + 1 < starts.length ? starts[index + 1].index ?? nav.length : nav.indexOf('<div class="rs-slot-account');
    const body = nav.slice(from, to);
    return {
      id: start[1],
      labelledBy: start[2],
      group: start[3],
      className: start[4],
      label: /<p id="([^"]*)" class="([^"]*)">([^<]*)<\/p>/.exec(body)?.slice(1),
      hrefs: [...body.matchAll(/<a\b[^>]*href="([^"]*)"/g)].map(match => match[1]),
      more: body.includes('class="rs-shell-more"'),
    };
  });
}

function rootAttributes(html: string): Record<string, string> {
  const tag = /^<div\b([^>]*)>/.exec(html)?.[1] ?? '';
  return Object.fromEntries([...tag.matchAll(/\s([\w-]+)="([^"]*)"/g)].map(match => [match[1], match[2]]));
}

function layoutOf(html: string) { return /data-rs-layout="([^"]+)"/.exec(html)?.[1]; }
function themeOf(html: string) { return /data-rs-theme="([^"]+)"/.exec(html)?.[1]; }

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

function found(element: ReactElement | null, what: string): ReactElement {
  if (!element) throw new Error(`expected ${what} in the shell tree`);
  return element;
}

const STATUS = createElement('div', { role: 'status' }, 'Synthetic status');
const CHOOSER = createElement('div', { 'data-probe': 'chooser' }, 'Synthetic chooser');
const noop = () => {};
const isMain = (element: ReactElement) => element.type === 'main';
const isNav = (element: ReactElement) => element.type === 'nav';
const isOutlet = (element: ReactElement) => element.type === Outlet;
const isMore = (element: ReactElement) => element.type === 'button' && (element.props as { className?: unknown }).className === 'rs-shell-more';

function frameProps(layout: LayoutId, overrides: Partial<LayoutFrameProps>): LayoutFrameProps {
  return { layout, isAdmin: true, activeConflicts: 3, accountName: 'Synthetic administrator', syncStatus: STATUS, chooser: CHOOSER, onLogout: noop, ...overrides };
}

type FrameAction = (tree: ReactElement) => void;

/**
 * LayoutFrame's element tree. Since #264 LayoutFrame uses router and state hooks, so it is
 * called inside a real (server) render instead of as a bare function. Each action runs on the
 * tree of one render; an action that updates state (a click on More, Escape) makes the
 * renderer render again before it finishes, and `html` is the markup of that last render.
 */
function captureFrame(layout: LayoutId, overrides: Partial<LayoutFrameProps> = {}, actions: readonly FrameAction[] = []) {
  const props = frameProps(layout, overrides);
  const trees: ReactElement[] = [];
  let step = 0;
  function Capture() {
    const tree = LayoutFrame(props);
    trees.push(tree);
    if (step < actions.length) actions[step++](tree);
    return tree;
  }
  const html = renderToStaticMarkup(createElement(StaticRouter, { location: '/' }, createElement(Capture)));
  return { trees, tree: trees[trees.length - 1], html, steps: step };
}

function frame(layout: LayoutId, overrides: Partial<LayoutFrameProps> = {}): ReactElement {
  return captureFrame(layout, overrides).tree;
}

function renderFrame(layout: LayoutId, overrides: Partial<LayoutFrameProps> = {}): string {
  return renderToStaticMarkup(createElement(StaticRouter, { location: '/' }, createElement(LayoutFrame, frameProps(layout, overrides))));
}

const clickMore: FrameAction = tree => { (found(elementAt(tree, isMore), 'the More button').props as { onClick: () => void }).onClick(); };
const pressEscape = (stopPropagation: () => void): FrameAction => tree => {
  (found(elementAt(tree, isNav), 'the nav').props as { onKeyDown: (event: unknown) => void }).onKeyDown({ key: 'Escape', stopPropagation });
};

// Every layout in every theme.
const COMBINATIONS: ReadonlyArray<readonly [LayoutId, ThemeId]> = LAYOUT_IDS.flatMap(layout => THEME_IDS.map(theme => [layout, theme] as const));

describe('#262 one stable shell subtree, as extended by #264', () => {
  it('splits the catalog into the existing three layouts and the three #264 layouts', () => {
    // The split tests below cover these two lists; a new layout must be placed in one of them.
    expect([...EXISTING, ...NEW_LAYOUTS]).toEqual(LAYOUT_IDS);
  });

  // Covers all six layouts in both themes: the routed page must never remount when either changes.
  it('keeps <main>, the vocabulary provider and the <Outlet> at the same element path, with no keys, in every layout and theme', () => {
    const classic = pathTo(frame('classic'), isOutlet);
    expect(classic).toEqual([
      { type: 'div', key: null, index: 0 },
      { type: 'main', key: null, index: 1 },
      { type: 'component:VocabularyProvider', key: null, index: 0 },
      { type: 'component:Outlet', key: null, index: 0 },
    ]);
    for (const [layout, theme] of COMBINATIONS) {
      const tree = frame(layout, { theme });
      const label = `${layout} ${theme}`;
      expect(pathTo(tree, isOutlet), label).toEqual(classic);
      expect(pathTo(tree, isMain), label).toEqual(classic?.slice(0, 2));
      expect(childList(tree).map(child => isValidElement(child) ? typeName(child) : child)).toEqual(['nav', 'main']);
      const main = found(elementAt(tree, isMain), 'main');
      expect(main.props).toMatchObject({ tabIndex: -1 });
      // The provider is context only: one child, no key, the layout's vocabulary.
      const provided = childList(main);
      expect(provided).toHaveLength(1);
      const provider = provided[0];
      expect(isValidElement(provider) && provider.type).toBe(VocabularyProvider);
      expect(isValidElement(provider) && provider.key).toBeNull();
      expect(isValidElement(provider) && provider.props).toMatchObject({ vocabulary: LAYOUT_CATALOG[layout].vocabulary });
      const outlet = isValidElement(provider) ? childList(provider) : [];
      expect(outlet).toHaveLength(1);
      expect(isValidElement(outlet[0]) && outlet[0].type).toBe(Outlet);
      expect(isValidElement(outlet[0]) && outlet[0].key).toBeNull();
    }
  });

  // Covers all six layouts in both themes.
  it('places the same status and chooser elements in the same slot in every layout and theme', () => {
    const chooserPath = pathTo(frame('classic'), element => element === CHOOSER);
    const statusPath = pathTo(frame('classic'), element => element === STATUS);
    expect(chooserPath).not.toBeNull();
    expect(statusPath).not.toBeNull();
    for (const [layout, theme] of COMBINATIONS) {
      expect(pathTo(frame(layout, { theme }), element => element === CHOOSER), `${layout} ${theme}`).toEqual(chooserPath);
      expect(pathTo(frame(layout, { theme }), element => element === STATUS), `${layout} ${theme}`).toEqual(statusPath);
    }
  });

  // Covers all six layouts in both themes.
  it('changes only classes and data attributes on the shell spine', () => {
    const spine = (layout: LayoutId, theme: ThemeId) => {
      const tree = frame(layout, { theme });
      const nav = elementAt(tree, isNav);
      const main = elementAt(tree, isMain);
      // Handlers are created afresh on every render, so they are compared by kind.
      const strip = (props: object | undefined) => Object.fromEntries(Object.entries(props ?? {})
        .filter(([key]) => key !== 'className' && key !== 'children' && !key.startsWith('data-rs-'))
        .map(([key, value]) => [key, typeof value === 'function' ? 'function' : value]));
      return { root: strip(tree.props), nav: strip(nav?.props), main: strip(main?.props) };
    };
    for (const [layout, theme] of COMBINATIONS) expect(spine(layout, theme), `${layout} ${theme}`).toEqual(spine('classic', 'dark'));
    expect(spine('classic', 'dark')).toEqual({ root: { 'data-nav-open': undefined }, nav: { 'aria-label': 'Primary', onKeyDown: 'function' }, main: { tabIndex: -1 } });
  });

  // Covers all six layouts in both themes, plus an absent and an unknown theme.
  it('marks the root with the layout, the resolved theme, and the colour bridge only where page tokens apply', () => {
    const data = (tree: ReactElement) => Object.fromEntries(Object.entries(tree.props as object).filter(([key]) => key.startsWith('data-')));
    for (const [layout, theme] of COMBINATIONS) {
      // Written out: page tokens apply to every combination except an existing layout in dark.
      const bridged = !(isExisting(layout) && theme === 'dark');
      expect(usesPageTokens(layout, theme), `${layout} ${theme}`).toBe(bridged);
      expect(data(frame(layout, { theme })), `${layout} ${theme}`).toEqual({
        'data-rs-layout': layout, 'data-rs-theme': theme, 'data-rs-bridge': bridged ? '' : undefined, 'data-nav-open': undefined,
      });
    }
    for (const layout of LAYOUT_IDS) {
      // LayoutFrame's theme is optional and defaults to dark; a value no layout lists resolves to dark.
      for (const tree of [frame(layout), frame(layout, { theme: 'sepia' as ThemeId })]) {
        expect(data(tree), layout).toEqual({
          'data-rs-layout': layout, 'data-rs-theme': 'dark', 'data-rs-bridge': isExisting(layout) ? undefined : '', 'data-nav-open': undefined,
        });
      }
    }
  });
});

describe('#262 rendered shell, as extended by #264', () => {
  // Covers all six layouts in both themes: the page is the same whatever presentation is chosen.
  it('renders the routed page once, inside the only main landmark, identically in every layout and theme', () => {
    const contents = new Set<string>();
    for (const [layout, theme] of COMBINATIONS) {
      signIn(admin, [storedPreference(admin.id, layout, theme)]);
      const { html } = renderShell();
      const label = `${layout} ${theme}`;
      const bridged = !(isExisting(layout) && theme === 'dark');
      expect(rootAttributes(html), label).toEqual({
        class: `rs-layout-shell ${LAYOUT_CATALOG[layout].classes.root}`,
        'data-rs-layout': layout,
        'data-rs-theme': theme,
        ...(bridged ? { 'data-rs-bridge': '' } : {}),
      });
      expect(html.includes('data-rs-bridge'), label).toBe(usesPageTokens(layout, theme));
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

  // Covers all six layouts: the existing three keep today's wording; the new three use the plain vocabulary.
  it('gives the routed page the vocabulary of the current layout', () => {
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      expect(mainContent(renderShell('/vocabulary').html), layout).toBe(`<p data-probe="vocabulary">${isExisting(layout) ? 'existing' : 'plain'}</p>`);
    }
  });

  // The existing three only: their navigation must be exactly the #262 navigation.
  it('keeps navigation targets, labels, order, active matching and admin visibility exactly as before in Classic, Individual and Team', () => {
    for (const layout of EXISTING) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const adminHtml = renderShell('/repos').html;
      const adminLinks = navLinks(adminHtml);
      expect(adminLinks.map(link => [link.href, link.label]), `${layout} admin`).toEqual(ORIGINAL_NAV);
      expect(adminLinks.filter(link => link.current).map(link => link.href)).toEqual(['/repos']);
      expect(adminLinks.filter(link => link.active).map(link => link.href)).toEqual(['/repos']);
      // Dashboard keeps `end`: it is active only on "/".
      expect(navLinks(renderShell('/').html).filter(link => link.current).map(link => link.href)).toEqual(['/']);
      // E4's Help is not part of the existing layouts.
      expect(adminHtml).not.toContain('href="/docs"');
      expect(adminHtml).not.toContain('>Help<');

      signIn(member, [storedPreference(member.id, layout)]);
      const html = renderShell('/settings').html;
      expect(navLinks(html).map(link => [link.href, link.label]), `${layout} member`).toEqual(MEMBER_NAV);
      for (const href of [...ADMIN_HREFS, '/docs']) expect(html).not.toContain(`href="${href}"`);
      expect(html).not.toContain('Administration');
    }
  });

  // The new three only: plain labels, Help for every account, and the same targets and admin visibility.
  it('gives Ledger, Console and Guide plain labels and Help, with the same targets, active matching and admin visibility', () => {
    for (const layout of NEW_LAYOUTS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const adminLinks = navLinks(renderShell('/repos').html);
      expect(adminLinks.map(link => [link.href, link.label]), `${layout} admin`).toEqual(PLAIN_NAV);
      expect(adminLinks.filter(link => link.current).map(link => link.href)).toEqual(['/repos']);
      expect(adminLinks.filter(link => link.active).map(link => link.href)).toEqual(['/repos']);
      expect(navLinks(renderShell('/').html).filter(link => link.current).map(link => link.href)).toEqual(['/']);
      expect(navLinks(renderShell('/docs').html).filter(link => link.current).map(link => link.href)).toEqual(['/docs']);

      signIn(member, [storedPreference(member.id, layout)]);
      const html = renderShell('/settings').html;
      expect(navLinks(html).map(link => [link.href, link.label]), `${layout} member`).toEqual(PLAIN_MEMBER_NAV);
      for (const href of ADMIN_HREFS) expect(html).not.toContain(`href="${href}"`);
      expect(html).not.toContain('Administration');
    }
  });

  // Covers all six layouts: the action is the same everywhere; only its wording follows the vocabulary.
  it('shows the same status and account name in every layout, Logout in the existing layouts and Sign out in the new ones', () => {
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const { html } = renderShell();
      expect(html).toContain('role="status" aria-live="polite"');
      expect(count(html, 'Synthetic administrator')).toBe(1);
      expect(count(html, '<button class="rs-shell-logout rs-shell-action text-sm">'), layout).toBe(1);
      if (isExisting(layout)) {
        expect(count(html, '>Logout</button>'), layout).toBe(1);
        expect(html, layout).not.toContain('Sign out');
      } else {
        expect(count(html, '>Sign out</button>'), layout).toBe(1);
        expect(html, layout).not.toContain('Logout');
      }
    }
  });

  // Covers all six layouts: Classic and Individual stay ungrouped; Team and the new three group the same way.
  it('groups navigation under labelled groups in Team, Ledger, Console and Guide and leaves Classic and Individual ungrouped', () => {
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const { html } = renderShell();
      const groups = navGroups(html);
      if (!isGrouped(layout)) {
        expect(groups, layout).toEqual([]);
        expect(html, layout).not.toContain('role="group"');
        continue;
      }
      const classes = LAYOUT_CATALOG[layout].classes;
      const group = (id: string, label: string, hrefs: string[], more: boolean) => ({
        id: `rs-shell-nav-${id}`, labelledBy: `rs-shell-nav-group-${id}`, group: id, className: `rs-slot-group ${classes.group}`,
        label: [`rs-shell-nav-group-${id}`, `rs-slot-group-label ${classes.groupLabel}`, label], hrefs, more,
      });
      expect(groups, layout).toEqual([
        group('workspace', 'Workspace', ['/', '/repos', '/conflicts', '/audit'], true),
        group('account', 'Account', isExisting(layout) ? ['/settings'] : ['/settings', '/docs'], false),
        group('administration', 'Administration', ADMIN_HREFS, false),
      ]);
      expect(count(navHtml(html), `<div class="rs-slot-group-links ${classes.groupLinks}">`), layout).toBe(3);
    }
  });

  it('keeps Classic on its original shell structure, adding only the fixed rs-slot classes', () => {
    signIn(admin);
    const { html } = renderShell();
    expect(layoutOf(html)).toBe('classic');
    // No data-rs-bridge: Classic in dark keeps the page's own colours.
    expect(html).toContain('<div class="rs-layout-shell min-h-screen" data-rs-layout="classic" data-rs-theme="dark"><nav aria-label="Primary" class="rs-slot-nav rs-shell-chrome border-b"><div class="rs-slot-nav-inner px-6 sm:px-8 lg:px-12"><div class="rs-slot-nav-row flex flex-wrap items-center justify-between gap-y-2 min-h-16 py-2"><div class="rs-slot-brand flex items-center"><svg class="rs-slot-logo" width="32" height="32"');
    expect(html).toContain('<span class="rs-slot-brand-name rs-shell-brand font-bold text-lg font-display tracking-wider">RepoSync</span><div class="rs-slot-links ml-10 flex items-baseline space-x-4"><a class="rs-shell-link px-3 py-2 rounded-md text-sm font-medium transition-colors" href="/">');
    expect(html).toContain('<div class="rs-slot-account flex flex-1 items-center justify-end space-x-4"><div role="status"');
    expect(html).toContain('<button class="rs-shell-logout rs-shell-action text-sm">Logout</button></div></div></div></nav><main tabindex="-1" class="rs-slot-main py-6 px-6 sm:px-8 lg:px-12">');
  });

  // Covers all six layouts: the slot classes are fixed; the catalog utilities (empty for the new three) follow them.
  it('gives every shell slot its fixed rs-slot class followed by the catalog utilities, in every layout', () => {
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const { html } = renderShell();
      const classes = LAYOUT_CATALOG[layout].classes;
      expect(html, layout).toContain(`<nav aria-label="Primary" class="rs-slot-nav ${classes.nav}"><div class="rs-slot-nav-inner ${classes.navInner}"><div class="rs-slot-nav-row ${classes.navRow}"><div class="rs-slot-brand ${classes.brand}"><svg class="rs-slot-logo"`);
      expect(html, layout).toContain(`<span class="rs-slot-brand-name ${classes.brandName}">RepoSync</span><div class="rs-slot-links ${classes.links}">`);
      expect(html, layout).toContain(`<div class="rs-slot-account ${classes.account}">`);
      expect(html, layout).toContain(`<main tabindex="-1" class="rs-slot-main ${classes.main}">`);
      for (const link of navLinks(html)) {
        expect(link.className, `${layout} ${link.href}`).toBe(`rs-shell-link ${classes.link}${link.active ? ' rs-shell-link--active' : ''}`);
      }
    }
  });

  // Covers all six layouts.
  it('renders the conflict count badge in every layout from the same status value', () => {
    for (const layout of LAYOUT_IDS) {
      const html = renderFrame(layout);
      expect(html, layout).toMatch(/>Conflicts<span class="[^"]*rs-shell-alert[^"]*">3<\/span><\/a>/);
      expect(renderFrame(layout, { activeConflicts: 0 })).not.toContain('rs-shell-alert');
    }
  });

  // Covers all six layouts: E6 icons are in every layout's tree (hidden by CSS in the existing three).
  it('puts an aria-hidden icon before the label of every navigation link, the same icon for an entry in every layout', () => {
    const iconFor = new Map<string, string | undefined>();
    for (const layout of LAYOUT_IDS) {
      signIn(admin, [storedPreference(admin.id, layout)]);
      const links = navLinks(renderShell().html);
      expect(links, layout).toHaveLength(isExisting(layout) ? 9 : 10);
      for (const link of links) {
        const where = `${layout} ${link.href}`;
        expect(link.icon, where).not.toBeNull();
        expect(link.icon?.hidden, where).toBe(true);
        expect(link.icon?.classes, where).toContain('rs-nav-icon');
        expect(link.label, where).not.toBe('');
        const name = link.icon?.classes.find(value => value.startsWith('lucide-'));
        expect(name, where).toBeDefined();
        if (!iconFor.has(link.href ?? '')) iconFor.set(link.href ?? '', name);
        else expect(name, where).toBe(iconFor.get(link.href ?? ''));
      }
    }
    expect(iconFor.size).toBe(10);
    expect(new Set(iconFor.values()).size).toBe(10);
  });

  // Covers all six layouts for an administrator and a member: E1's button is in every layout's tree.
  it('places one closed More button immediately after the last workspace link (Audit Log), in every layout', () => {
    for (const layout of LAYOUT_IDS) {
      for (const [user, isAdmin] of [[admin, true], [member, false]] as const) {
        signIn(user, [storedPreference(user.id, layout)]);
        const html = renderShell().html;
        const nav = navHtml(html);
        const where = `${layout} ${isAdmin ? 'admin' : 'member'}`;
        expect(count(nav, 'rs-shell-more'), where).toBe(1);
        const more = /<a\b[^>]*href="\/audit"[^>]*>(?:(?!<\/a>)[\s\S])*<\/a><button type="button" class="rs-shell-more" aria-expanded="false"( aria-controls="([^"]*)")?>([\s\S]*?)<\/button>/.exec(nav);
        expect(more, where).not.toBeNull();
        const [, , controls, content] = more ?? [];
        const longLabel = layout === 'guide' ? 'Account and admin' : 'More';
        expect(content, where).toMatch(new RegExp(`^<svg\\b[^>]*class="lucide lucide-menu rs-more-icon" aria-hidden="true">[\\s\\S]*?</svg><span class="rs-more-long">${longLabel}</span><span class="rs-more-short">More</span>$`));
        if (isGrouped(layout)) {
          // The button names the groups it reveals; each is a group in the rendered navigation.
          const ids = isAdmin ? ['rs-shell-nav-account', 'rs-shell-nav-administration'] : ['rs-shell-nav-account'];
          expect(controls, where).toBe(ids.join(' '));
          for (const id of ids) expect(nav, where).toContain(`<div id="${id}" role="group"`);
          expect(navGroups(html).map(group => group.more), where).toEqual(isAdmin ? [true, false, false] : [true, false]);
        } else {
          expect(controls, where).toBeUndefined();
          // In the inline layouts the next entry follows the button directly.
          expect(nav, where).toMatch(/<\/button><a\b[^>]*href="\/settings"/);
        }
      }
    }
  });

  // Covers all six layouts: the More state is in every layout's tree, even where CSS hides the button.
  it('opens More from the button and closes it from the button or Escape, reflecting it on the button and the root', () => {
    for (const layout of LAYOUT_IDS) {
      const closed = captureFrame(layout);
      expect(closed.trees).toHaveLength(1);
      expect(found(elementAt(closed.tree, isMore), 'More').props['aria-expanded'], layout).toBe(false);
      expect(rootAttributes(closed.html)['data-nav-open'], layout).toBeUndefined();

      const opened = captureFrame(layout, {}, [clickMore]);
      expect(opened.trees, layout).toHaveLength(2);
      expect(found(elementAt(opened.tree, isMore), 'More').props['aria-expanded'], layout).toBe(true);
      expect(opened.html, layout).toContain('class="rs-shell-more" aria-expanded="true"');
      // Only data-nav-open is added to the root.
      expect(rootAttributes(opened.html), layout).toEqual({ ...rootAttributes(closed.html), 'data-nav-open': '' });

      const toggled = captureFrame(layout, {}, [clickMore, clickMore]);
      expect(toggled.trees, layout).toHaveLength(3);
      expect(found(elementAt(toggled.tree, isMore), 'More').props['aria-expanded'], layout).toBe(false);
      expect(rootAttributes(toggled.html), layout).toEqual(rootAttributes(closed.html));

      const stop = vi.fn();
      const escaped = captureFrame(layout, {}, [clickMore, pressEscape(stop)]);
      expect(escaped.trees, layout).toHaveLength(3);
      expect(stop).toHaveBeenCalledTimes(1);
      expect(rootAttributes(escaped.html), layout).toEqual(rootAttributes(closed.html));

      // Escape with More closed is left alone for whatever else handles it (a dialog, for example).
      const ignored = vi.fn();
      const idle = captureFrame(layout, {}, [pressEscape(ignored)]);
      expect(idle.trees, layout).toHaveLength(1);
      expect(ignored).not.toHaveBeenCalled();
    }
  });
});

// ---- stylesheet checks ------------------------------------------------------
// Unit tests cannot compute styles, so these check the rule text: the existing three layouts
// hide the E1 button and E6 icons, and only rules scoped to the new layouts style them.

// This project installs no Node type definitions, so node:fs is loaded through a specifier
// the type checker does not resolve. Vitest runs these tests in Node.
interface NodeFs {
  readFileSync(path: URL, encoding: 'utf8'): string;
  readdirSync(path: URL): string[];
}
const NODE_FS = 'node:fs';
const nodeFs = async () => (await import(/* @vite-ignore */ NODE_FS)) as NodeFs;

interface CssRule { readonly selectors: readonly string[]; readonly declarations: string; readonly conditions: readonly string[] }

/** Splits a selector list on commas outside parentheses, so :is(a, b) stays whole. */
function selectorList(text: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < text.length; i++) {
    if (text[i] === '(') depth++;
    else if (text[i] === ')') depth--;
    else if (text[i] === ',' && depth === 0) { parts.push(text.slice(start, i)); start = i + 1; }
  }
  parts.push(text.slice(start));
  return parts.map(part => part.trim().replace(/\s+/g, ' '));
}

/** The style rules of a plain (unnested) stylesheet, each with the at-rule blocks (such as @media) around it. */
function cssRules(source: string): CssRule[] {
  const text = source.replace(/\/\*[\s\S]*?\*\//g, '');
  const rules: CssRule[] = [];
  const conditions: string[] = [];
  let prelude = '';
  for (let i = 0; i < text.length; i++) {
    const char = text[i];
    if (char === '{') {
      const head = prelude.trim().replace(/\s+/g, ' ');
      prelude = '';
      if (head.startsWith('@')) { conditions.push(head); continue; }
      const close = text.indexOf('}', i);
      rules.push({ selectors: selectorList(head), declarations: text.slice(i + 1, close).trim().replace(/\s+/g, ' '), conditions: [...conditions] });
      i = close;
    } else if (char === '}') {
      conditions.pop();
      prelude = '';
    } else if (char === ';' && prelude.trim().startsWith('@')) {
      prelude = '';
    } else {
      prelude += char;
    }
  }
  return rules;
}

const TARGETS_MORE_OR_ICON = /\.rs-nav-icon\b|\.rs-shell-more\b/;

describe('#264 hidden shell extensions in the existing layouts (E1, E6)', () => {
  it('hides the navigation icons and the More button by one unconditional rule in layoutTokens.css', async () => {
    const fs = await nodeFs();
    const rules = cssRules(fs.readFileSync(new URL('./layoutTokens.css', import.meta.url), 'utf8'));
    expect(rules.length).toBeGreaterThan(10);
    expect(rules.filter(rule => rule.selectors.some(selector => TARGETS_MORE_OR_ICON.test(selector)))).toEqual([{
      selectors: ['.rs-layout-shell .rs-nav-icon', '.rs-layout-shell .rs-shell-more'],
      declarations: 'display: none;',
      conditions: [],
    }]);
  });

  it('styles the icons and the More button elsewhere only in rules scoped to Ledger, Console or Guide', async () => {
    const fs = await nodeFs();
    const directory = new URL('./', import.meta.url);
    const sheets = fs.readdirSync(directory).filter(name => name.endsWith('.css')).sort();
    expect(sheets).toEqual(expect.arrayContaining(['layoutOptions.css', 'layoutTokens.css']));
    const newLayout = /\[data-rs-layout=['"]?(ledger|console|guide)['"]?\]/;
    const existingLayout = /\[data-rs-layout=['"]?(classic|individual|team)['"]?\]/;
    let scoped = 0;
    for (const name of sheets) {
      if (name === 'layoutTokens.css') continue;
      for (const rule of cssRules(fs.readFileSync(new URL(name, directory), 'utf8'))) {
        for (const selector of rule.selectors.filter(value => TARGETS_MORE_OR_ICON.test(value))) {
          expect(selector, name).toMatch(newLayout);
          expect(selector, name).not.toMatch(existingLayout);
          scoped++;
        }
      }
    }
    expect(scoped).toBeGreaterThan(0);
  });
});

describe('#262 presentation is not capability', () => {
  // Covers all six layouts in both themes.
  it('creates the same queries and no mutations whatever layout or theme is chosen', () => {
    const hashes = new Set<string>();
    for (const [layout, theme] of COMBINATIONS) {
      signIn(member, [storedPreference(member.id, layout, theme)]);
      const { client } = renderShell();
      hashes.add(JSON.stringify(client.getQueryCache().getAll().map(query => query.queryHash).sort()));
      expect(client.getMutationCache().getAll()).toEqual([]);
    }
    expect(hashes.size).toBe(1);
  });

  // Every grouped layout: Team and the three new layouts.
  it('does not let a stored grouped-layout preference reveal administration to a member', () => {
    for (const layout of LAYOUT_IDS.filter(isGrouped)) {
      signIn(member, [storedPreference(member.id, layout)]);
      const { html } = renderShell();
      expect(layoutOf(html)).toBe(layout);
      expect(navLinks(html).map(link => link.href), layout).toEqual((isExisting(layout) ? MEMBER_NAV : PLAIN_MEMBER_NAV).map(([href]) => href));
      expect(html, layout).not.toContain('rs-shell-nav-group-administration');
      expect(html, layout).not.toContain('rs-shell-nav-administration');
      expect(html, layout).not.toContain('Administration');
    }
  });
});

describe('#262 preference in the shell, as extended by #264', () => {
  it('uses each account\'s own layout and theme, and Classic in dark for an account without one', () => {
    const entries = [storedPreference(admin.id, 'team', 'light'), storedPreference(member.id, 'ledger')];
    signIn(admin, entries);
    let html = renderShell().html;
    expect([layoutOf(html), themeOf(html)]).toEqual(['team', 'light']);
    signIn(member, entries);
    html = renderShell().html;
    expect([layoutOf(html), themeOf(html)]).toEqual(['ledger', 'dark']);
    signIn({ id: 'account-third', username: 'third', role: 'viewer' }, entries);
    html = renderShell().html;
    expect([layoutOf(html), themeOf(html)]).toEqual(['classic', 'dark']);
    expect(html).not.toContain('data-rs-bridge');
    expect(html).not.toContain('Using Classic');
  });

  it('applies a #262 version-1 preference as its layout in dark, quietly and without rewriting it', () => {
    // A #262 build could only have saved one of the existing three layouts.
    for (const layout of EXISTING) {
      const storage = signIn(admin, [legacyPreference(admin.id, layout)]);
      const html = renderShell().html;
      expect([layoutOf(html), themeOf(html)], layout).toEqual([layout, 'dark']);
      expect(html, layout).not.toContain('data-rs-bridge');
      expect(html, layout).not.toContain('Using Classic');
      expect(navLinks(html).map(link => [link.href, link.label]), layout).toEqual(ORIGINAL_NAV);
      expect(storage.writes, layout).toEqual([]);
    }
  });

  it('falls back to Classic in dark, visibly, when a stored preference is corrupt, unknown, foreign, future or unreadable', () => {
    const [key] = storedPreference(admin.id, 'team');
    const cases: ReadonlyArray<readonly [string, string]> = [
      ['corrupt', '{not json'],
      ['unknown layout', storedPreference(admin.id, 'compact', 'light')[1]],
      ['unknown layout in version 1', legacyPreference(admin.id, 'compact')[1]],
      ['unknown theme', storedPreference(admin.id, 'team', 'sepia')[1]],
      ['foreign', JSON.stringify({ version: 2, layout: 'team', theme: 'light', identity: member.id, origin: ORIGIN })],
      ['future', JSON.stringify({ version: 3, layout: 'team', theme: 'light', identity: admin.id, origin: ORIGIN })],
      ['version 2 without a theme', JSON.stringify({ version: 2, layout: 'team', identity: admin.id, origin: ORIGIN })],
      ['version 1 with a theme', JSON.stringify({ version: 1, layout: 'team', theme: 'light', identity: admin.id, origin: ORIGIN })],
    ];
    const isClassicFallback = (html: string, name: string) => {
      expect(layoutOf(html), name).toBe('classic');
      expect(themeOf(html), name).toBe('dark');
      expect(html, name).not.toContain('data-rs-bridge');
      expect(html, name).toContain('Using Classic');
      expect(navLinks(html).map(link => [link.href, link.label]), name).toEqual(ORIGINAL_NAV);
      expect(count(html, '>Logout</button>'), name).toBe(1);
    };
    for (const [name, raw] of cases) {
      signIn(admin, [[key, raw]]);
      isClassicFallback(renderShell().html, name);
    }
    const storage = signIn(admin, [storedPreference(admin.id, 'team', 'light')]);
    storage.failPreferenceReads = true;
    let html = '';
    expect(() => { html = renderShell().html; }).not.toThrow();
    isClassicFallback(html, 'unreadable');
  });

  it('does not read any preference for a sign-in without a stable account identity', () => {
    const storage = signIn({ username: 'legacy-login', role: 'admin' }, [storedPreference(admin.id, 'team', 'light')]);
    const html = renderShell().html;
    expect(layoutOf(html)).toBe('classic');
    expect(themeOf(html)).toBe('dark');
    expect(storage.reads.filter(key => key.startsWith(LAYOUT_PREFERENCE_PREFIX))).toEqual([]);
  });

  // Covers all six layouts in both themes: both selects list every choice with the current one selected.
  it('offers labelled native layout and theme selects with the current choices selected', () => {
    const labels: Readonly<Record<LayoutId, string>> = { classic: 'Classic', individual: 'Individual', team: 'Team', ledger: 'Ledger', console: 'Console', guide: 'Guide' };
    for (const [layout, theme] of COMBINATIONS) {
      signIn(admin, [storedPreference(admin.id, layout, theme)]);
      const { html } = renderShell();
      const where = `${layout} ${theme}`;
      const select = /<select id="([^"]+)" aria-describedby="([^" ]+) ([^"]+)"[^>]*data-rs-layout-chooser=""[^>]*>([\s\S]*?)<\/select>/.exec(html);
      expect(select, where).not.toBeNull();
      const [, id, hintId, noticeId, options] = select ?? [];
      expect(html).toContain(`<label for="${id}"`);
      expect(html).toMatch(new RegExp(`<label for="${id}"[^>]*>[\\s\\S]*?<span class="sr-only">Layout</span></label>`));
      const hint = isExisting(layout)
        ? 'Changes only how navigation is arranged. Pages, actions, permissions and server mode stay the same.'
        : 'Changes how navigation and pages are arranged and worded. Actions, permissions and server mode stay the same.';
      expect(html, where).toContain(`<span id="${hintId}" class="sr-only">${hint}</span>`);
      expect(html).toMatch(new RegExp(`<p id="${noticeId}" role="status"`));
      expect([...(options ?? '').matchAll(/<option value="([^"]+)"( selected="")?>([^<]+)<\/option>/g)].map(match => [match[1], match[3], Boolean(match[2])]), where)
        .toEqual(LAYOUT_IDS.map(option => [option, labels[option], option === layout]));

      const themeSelect = /<select id="([^"]+)" aria-describedby="([^" ]+) ([^"]+)"[^>]*data-rs-theme-chooser=""[^>]*>([\s\S]*?)<\/select>/.exec(html);
      expect(themeSelect, where).not.toBeNull();
      const [, themeSelectId, themeHintId, themeNoticeId, themeOptions] = themeSelect ?? [];
      expect(themeSelectId).not.toBe(id);
      expect(themeNoticeId).toBe(noticeId);
      expect(html).toMatch(new RegExp(`<label for="${themeSelectId}"[^>]*>[\\s\\S]*?<span class="sr-only">Theme</span></label>`));
      expect(html).toContain(`<span id="${themeHintId}" class="sr-only">Changes only colours. Pages, actions, permissions and server mode stay the same.</span>`);
      expect([...(themeOptions ?? '').matchAll(/<option value="([^"]+)"( selected="")?>([^<]+)<\/option>/g)].map(match => [match[1], match[3], Boolean(match[2])]), where)
        .toEqual([['dark', 'Dark', theme === 'dark'], ['light', 'Light', theme === 'light']]);
      // The theme select follows the layout select.
      expect(html.indexOf('data-rs-layout-chooser=""')).toBeLessThan(html.indexOf(`<label for="${themeSelectId}"`));
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
    const storage = signIn({ id, username: 'pair-account', role: 'viewer' }, [storedPreference(id, 'team', 'light')]);
    const html = renderShell().html;
    expect([layoutOf(html), themeOf(html)]).toEqual(['team', 'light']);
    expect(preferenceReads(storage)).toEqual([keyFor(id)]);
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
