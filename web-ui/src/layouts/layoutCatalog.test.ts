import { describe, expect, it } from 'vitest';
import {
  DEFAULT_LAYOUT, DEFAULT_THEME, EXISTING_LAYOUT_IDS, LAYOUT_CATALOG, LAYOUT_IDS, NAV_GROUP_IDS, NAV_GROUP_LABELS, PRIMARY_NAVIGATION,
  SHELL_TEXT, THEME_IDS, THEME_LABELS, VOCABULARY_IDS,
  isExistingLayout, isLayoutId, isThemeId, navLabel, resolveLayout, resolveTheme, usesPageTokens, visibleNavigation, type LayoutId,
} from './layoutCatalog';

// Values a stored or injected preference might carry. None of them is a layout id.
const NOT_LAYOUTS: readonly unknown[] = [
  'Classic', ' classic', 'classic ', 'TEAM', 'Individual', '', 'compact', 'light', 'dark',
  'Ledger', ' console', 'console ', 'GUIDE', 'existing', 'plain',
  'toString', 'constructor', '__proto__', 'hasOwnProperty', 'valueOf',
  null, undefined, 0, 1, true, NaN, {}, [], ['team'], { id: 'team' }, () => 'team', Symbol('team'),
];

/** The layouts that existed before #264, written out rather than taken from the catalog. */
const EXISTING: readonly LayoutId[] = ['classic', 'individual', 'team'];
/** The three accepted #263 designs that #264 adds. */
const NEW_LAYOUTS: readonly LayoutId[] = ['ledger', 'console', 'guide'];

// The pre-#262 navigation in the existing vocabulary, written out rather than derived from PRIMARY_NAVIGATION.
const ORIGINAL_NAV: ReadonlyArray<readonly [string, string]> = [
  ['/', 'Dashboard'], ['/repos', 'Repositories'], ['/conflicts', 'Conflicts'], ['/audit', 'Audit Log'], ['/settings', 'Settings'],
  ['/users', 'Users'], ['/ldap', 'LDAP'], ['/config', 'Config'], ['/setup', 'Setup'],
];
// The same entries in the plain vocabulary of Ledger, Console and Guide, with E4's Help after My account.
const PLAIN_NAV: ReadonlyArray<readonly [string, string]> = [
  ['/', 'Dashboard'], ['/repos', 'Repositories'], ['/conflicts', 'Conflicts'], ['/audit', 'Audit log'], ['/settings', 'My account'],
  ['/docs', 'Help'], ['/users', 'Users'], ['/ldap', 'Directory (LDAP)'], ['/config', 'Identity & notifications'], ['/setup', 'Server setup'],
];

describe('#262 layout catalog, as extended by #264', () => {
  it('offers Classic, Individual and Team first, then Ledger, Console and Guide, with Classic as the default', () => {
    expect(LAYOUT_IDS).toEqual([...EXISTING, ...NEW_LAYOUTS]);
    expect(EXISTING_LAYOUT_IDS).toEqual(EXISTING);
    expect(DEFAULT_LAYOUT).toBe('classic');
    expect(Object.keys(LAYOUT_CATALOG)).toEqual([...LAYOUT_IDS]);
    for (const id of LAYOUT_IDS) expect(LAYOUT_CATALOG[id].id).toBe(id);
    expect(LAYOUT_IDS.map(id => LAYOUT_CATALOG[id].label)).toEqual(['Classic', 'Individual', 'Team', 'Ledger', 'Console', 'Guide']);
    // The existing three keep their #262 descriptions.
    expect(EXISTING.map(id => LAYOUT_CATALOG[id].description)).toEqual([
      'The original top navigation bar.', 'Compact tabs with centred content.', 'Grouped sidebar navigation.',
    ]);
    for (const id of LAYOUT_IDS) expect(isExistingLayout(id), id).toBe(EXISTING.includes(id));
  });

  // Covers all six layouts: every id must be accepted exactly and nothing else.
  it('accepts only exact catalog ids: no case folding, trimming or prototype keys', () => {
    for (const id of LAYOUT_IDS) expect(isLayoutId(id)).toBe(true);
    for (const value of NOT_LAYOUTS) expect(isLayoutId(value), String(typeof value === 'symbol' ? 'symbol' : value)).toBe(false);
  });

  // Covers all six layouts: each resolves to itself, anything else to Classic.
  it('resolves every unknown or malformed value to Classic and known ids to themselves', () => {
    for (const value of NOT_LAYOUTS) expect(resolveLayout(value)).toBe(LAYOUT_CATALOG.classic);
    for (const id of LAYOUT_IDS) expect(resolveLayout(id)).toBe(LAYOUT_CATALOG[id]);
  });

  // Covers all six layouts: none may carry a capability, role, route or server-mode field.
  it('describes presentation only: no capability, role, route or server-mode field', () => {
    const fields = ['classes', 'description', 'id', 'label', 'navPresentation', 'themes', 'vocabulary'];
    for (const id of LAYOUT_IDS) {
      const entry = LAYOUT_CATALOG[id];
      // Only Guide adds a field: the long text on its E1 "More" control.
      expect(Object.keys(entry).sort(), id).toEqual(id === 'guide' ? [...fields, 'moreLongLabel'].sort() : fields);
      expect(Object.keys(entry.classes).sort()).toEqual([
        'account', 'brand', 'brandName', 'group', 'groupLabel', 'groupLinks', 'link', 'links', 'main', 'nav', 'navInner', 'navRow', 'root',
      ]);
      // Guide's More text ("Account and admin") is display text, checked exactly below; every other field is scanned.
      const scanned = Object.fromEntries(Object.entries(entry).filter(([key]) => key !== 'moreLongLabel'));
      const text = JSON.stringify(scanned).toLowerCase();
      for (const word of ['admin', 'role', 'permission', 'capabilit', 'server', 'route', 'href', 'token', 'password']) {
        expect(text, `${id} mentions ${word}`).not.toContain(word);
      }
    }
    expect(LAYOUT_CATALOG.guide.moreLongLabel).toBe('Account and admin');
  });

  it('cannot be changed at runtime', () => {
    expect(Object.isFrozen(LAYOUT_CATALOG)).toBe(true);
    for (const id of LAYOUT_IDS) {
      const entry = LAYOUT_CATALOG[id];
      expect(Object.isFrozen(entry)).toBe(true);
      expect(Object.isFrozen(entry.classes)).toBe(true);
      expect(Object.isFrozen(entry.themes)).toBe(true);
    }
    expect(Reflect.set(LAYOUT_CATALOG, 'compact', LAYOUT_CATALOG.team)).toBe(false);
    expect(Reflect.set(LAYOUT_CATALOG.classic, 'navPresentation', 'grouped')).toBe(false);
    expect(Reflect.set(LAYOUT_CATALOG.classic, 'vocabulary', 'plain')).toBe(false);
    expect(Reflect.set(LAYOUT_CATALOG.ledger, 'vocabulary', 'existing')).toBe(false);
    expect(Reflect.set(LAYOUT_CATALOG.team.classes, 'main', '')).toBe(false);
    expect(isLayoutId('compact')).toBe(false);
    expect(LAYOUT_CATALOG.classic.navPresentation).toBe('inline');
    expect(LAYOUT_CATALOG.classic.vocabulary).toBe('existing');
    expect(Object.isFrozen(NAV_GROUP_LABELS)).toBe(true);
    expect(Object.isFrozen(EXISTING_LAYOUT_IDS)).toBe(true);
    expect(Object.isFrozen(THEME_LABELS)).toBe(true);
    expect(Object.isFrozen(SHELL_TEXT)).toBe(true);
    for (const vocabulary of VOCABULARY_IDS) expect(Object.isFrozen(SHELL_TEXT[vocabulary])).toBe(true);
  });

  it('keeps Classic on the original shell slots and spacing, wrapping only when it no longer fits', () => {
    // Spacing utilities from the pre-#262 Layout.tsx; colors moved to scoped tokens. The row is
    // at least 64px (min-h-16) and stays one row while the account group fits with its text
    // wrapped; otherwise that group wraps to a second, right-aligned line.
    expect(LAYOUT_CATALOG.classic.navPresentation).toBe('inline');
    expect(LAYOUT_CATALOG.classic.classes).toEqual({
      root: 'min-h-screen',
      nav: 'rs-shell-chrome border-b',
      navInner: 'px-6 sm:px-8 lg:px-12',
      navRow: 'flex flex-wrap items-center justify-between gap-y-2 min-h-16 py-2',
      brand: 'flex items-center',
      brandName: 'rs-shell-brand font-bold text-lg font-display tracking-wider',
      links: 'ml-10 flex items-baseline space-x-4',
      group: '',
      groupLabel: '',
      groupLinks: '',
      link: 'px-3 py-2 rounded-md text-sm font-medium transition-colors',
      account: 'flex flex-1 items-center justify-end space-x-4',
      main: 'py-6 px-6 sm:px-8 lg:px-12',
    });
  });

  it('keeps the #262 Individual and Team slot utilities unchanged', () => {
    expect(LAYOUT_CATALOG.individual.classes).toEqual({
      root: 'min-h-screen',
      nav: 'rs-shell-chrome border-b',
      navInner: 'mx-auto max-w-7xl px-4 sm:px-6',
      navRow: 'flex flex-wrap items-center justify-between gap-x-6 gap-y-2 py-2 md:h-16 md:flex-nowrap md:py-0',
      brand: 'flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 md:flex-nowrap',
      brandName: 'rs-shell-brand font-bold text-lg font-display tracking-wider',
      links: 'flex w-full gap-1 overflow-x-auto md:ml-8 md:w-auto',
      group: '',
      groupLabel: '',
      groupLinks: '',
      link: 'rs-shell-link--tab whitespace-nowrap border-b-2 px-3 py-2 text-sm font-medium transition-colors',
      account: 'flex flex-wrap items-center gap-3',
      main: 'mx-auto w-full max-w-7xl px-4 py-6 sm:px-6',
    });
    expect(LAYOUT_CATALOG.team.classes).toEqual({
      root: 'min-h-screen md:flex',
      nav: 'rs-shell-chrome border-b md:sticky md:top-0 md:h-screen md:w-64 md:flex-shrink-0 md:overflow-y-auto md:border-b-0 md:border-r',
      navInner: 'px-4 py-3 md:h-full md:py-6',
      navRow: 'flex flex-col gap-3 md:h-full md:gap-6',
      brand: 'flex min-w-0 flex-wrap items-center gap-x-2 gap-y-3 md:gap-y-6',
      brandName: 'rs-shell-brand font-bold text-lg font-display tracking-wider',
      links: 'flex w-full gap-4 overflow-x-auto md:flex-col md:gap-5 md:overflow-visible',
      group: 'flex-shrink-0',
      groupLabel: 'rs-shell-muted sr-only text-xs font-semibold uppercase tracking-wider md:not-sr-only md:px-3 md:pb-2',
      groupLinks: 'flex gap-1 md:flex-col',
      link: 'block whitespace-nowrap rounded-md px-3 py-2 text-sm font-medium transition-colors',
      account: 'flex flex-wrap items-center gap-3 md:mt-auto md:flex-col md:items-start',
      main: 'min-w-0 flex-1 px-4 py-6 sm:px-6 lg:px-10',
    });
  });

  it('gives Ledger, Console and Guide no slot utilities, so only their stylesheet arranges them', () => {
    for (const id of NEW_LAYOUTS) {
      for (const [slot, value] of Object.entries(LAYOUT_CATALOG[id].classes)) expect(value, `${id} ${slot}`).toBe('');
    }
  });

  it('gives Individual and Team navigation that scrolls instead of overflowing on narrow screens', () => {
    for (const id of ['individual', 'team'] as const) {
      const classes = LAYOUT_CATALOG[id].classes;
      expect(classes.links.split(' ')).toEqual(expect.arrayContaining(['flex', 'w-full', 'overflow-x-auto']));
      expect(classes.account.split(' ')).toContain('flex-wrap');
    }
    expect(LAYOUT_CATALOG.individual.classes.navRow.split(' ')).toEqual(expect.arrayContaining(['flex-wrap', 'md:flex-nowrap']));
    // Team becomes a sticky sidebar only from the md breakpoint; content keeps the remaining width.
    expect(LAYOUT_CATALOG.team.classes.root.split(' ')).toEqual(['min-h-screen', 'md:flex']);
    expect(LAYOUT_CATALOG.team.classes.nav.split(' ')).toEqual(expect.arrayContaining(['md:sticky', 'md:top-0', 'md:h-screen', 'md:w-64', 'md:overflow-y-auto']));
    expect(LAYOUT_CATALOG.team.classes.main.split(' ')).toEqual(expect.arrayContaining(['min-w-0', 'flex-1']));
    expect(LAYOUT_CATALOG.team.classes.groupLabel.split(' ')).toEqual(expect.arrayContaining(['sr-only', 'md:not-sr-only']));
  });

  // Covers all six layouts: Classic and Individual stay inline; Team and the three new designs group.
  it('groups navigation in Team, Ledger, Console and Guide, never in Classic or Individual, under presentation-only headings', () => {
    expect(LAYOUT_IDS.map(id => LAYOUT_CATALOG[id].navPresentation)).toEqual(['inline', 'inline', 'grouped', 'grouped', 'grouped', 'grouped']);
    expect(NAV_GROUP_IDS).toEqual(['workspace', 'account', 'administration']);
    expect(NAV_GROUP_LABELS).toEqual({ workspace: 'Workspace', account: 'Account', administration: 'Administration' });
  });
});

describe('#264 themes (E2)', () => {
  // Covers all six layouts: every layout lists both themes and defaults to dark.
  it('offers dark and light in every layout, with dark the default and the fallback', () => {
    expect(THEME_IDS).toEqual(['dark', 'light']);
    expect(DEFAULT_THEME).toBe('dark');
    expect(THEME_LABELS).toEqual({ dark: 'Dark', light: 'Light' });
    for (const id of THEME_IDS) expect(isThemeId(id)).toBe(true);
    for (const value of ['Dark', 'Light', ' dark', 'light ', '', 'sepia', 'classic', 'constructor', '__proto__', 'toString', null, undefined, 1, {}, ['dark']]) {
      expect(isThemeId(value), String(value)).toBe(false);
    }
    for (const id of LAYOUT_IDS) {
      const entry = LAYOUT_CATALOG[id];
      expect(entry.themes, id).toEqual(['dark', 'light']);
      expect(resolveTheme(entry)).toBe('dark');
      expect(resolveTheme(entry, 'dark')).toBe('dark');
      expect(resolveTheme(entry, 'light')).toBe('light');
      for (const value of ['Light', 'sepia', 'constructor', undefined, null, 7]) expect(resolveTheme(entry, value)).toBe('dark');
    }
  });

  // Covers all six layouts in both themes: the one combination left untouched is an existing layout in dark.
  it('takes page colour from tokens everywhere except an existing layout in dark', () => {
    const table = LAYOUT_IDS.flatMap(layout => THEME_IDS.map(theme => [layout, theme, usesPageTokens(layout, theme)]));
    expect(table).toEqual([
      ['classic', 'dark', false], ['classic', 'light', true],
      ['individual', 'dark', false], ['individual', 'light', true],
      ['team', 'dark', false], ['team', 'light', true],
      ['ledger', 'dark', true], ['ledger', 'light', true],
      ['console', 'dark', true], ['console', 'light', true],
      ['guide', 'dark', true], ['guide', 'light', true],
    ]);
  });
});

describe('#264 vocabulary and navigation metadata (E3, E4, E6)', () => {
  it('keeps the existing vocabulary in Classic, Individual and Team and the plain one in the new layouts', () => {
    expect(VOCABULARY_IDS).toEqual(['existing', 'plain']);
    expect(LAYOUT_IDS.map(id => LAYOUT_CATALOG[id].vocabulary)).toEqual(['existing', 'existing', 'existing', 'plain', 'plain', 'plain']);
  });

  it('keeps every #262 entry byte for byte and adds only Help, limited to Ledger, Console and Guide', () => {
    expect(PRIMARY_NAVIGATION.filter(item => !item.onlyIn).map(item => [item.to, item.label])).toEqual(ORIGINAL_NAV);
    expect(PRIMARY_NAVIGATION.map(({ to, group, end, adminOnly, conflictCount, onlyIn }) => ({ to, group, end, adminOnly, conflictCount, onlyIn }))).toEqual([
      { to: '/', group: 'workspace', end: true },
      { to: '/repos', group: 'workspace' },
      { to: '/conflicts', group: 'workspace', conflictCount: true },
      { to: '/audit', group: 'workspace' },
      { to: '/settings', group: 'account' },
      { to: '/docs', group: 'account', onlyIn: ['ledger', 'console', 'guide'] },
      { to: '/users', group: 'administration', adminOnly: true },
      { to: '/ldap', group: 'administration', adminOnly: true },
      { to: '/config', group: 'administration', adminOnly: true },
      { to: '/setup', group: 'administration', adminOnly: true },
    ]);
  });

  it('gives every entry a plain label and its own icon', () => {
    expect(PRIMARY_NAVIGATION.map(item => [item.to, item.plainLabel])).toEqual(PLAIN_NAV);
    const icons = PRIMARY_NAVIGATION.map(item => item.icon);
    expect(icons).toEqual(['dashboard', 'repositories', 'conflicts', 'audit', 'account', 'help', 'users', 'directory', 'identity', 'setup']);
    expect(new Set(icons).size).toBe(icons.length);
    for (const item of PRIMARY_NAVIGATION) {
      expect(navLabel(item, 'existing')).toBe(item.label);
      expect(navLabel(item, 'plain')).toBe(item.plainLabel);
    }
  });

  // Existing three: exactly the #262 entries, labels and admin visibility. New three: plain labels plus Help for everyone.
  it('shows each layout its entries with admin visibility as before and Help only in the new layouts', () => {
    const shown = (layout: LayoutId, isAdmin: boolean) =>
      visibleNavigation(layout, isAdmin).map(item => [item.to, navLabel(item, LAYOUT_CATALOG[layout].vocabulary)]);
    for (const layout of EXISTING) {
      expect(shown(layout, true), layout).toEqual(ORIGINAL_NAV);
      expect(shown(layout, false), layout).toEqual(ORIGINAL_NAV.slice(0, 5));
    }
    for (const layout of NEW_LAYOUTS) {
      expect(shown(layout, true), layout).toEqual(PLAIN_NAV);
      expect(shown(layout, false), layout).toEqual(PLAIN_NAV.slice(0, 6));
    }
  });

  it('keeps Logout in the existing vocabulary and says Sign out in the plain one', () => {
    expect(SHELL_TEXT).toEqual({
      existing: { logout: 'Logout', more: 'More', moreLong: 'More' },
      plain: { logout: 'Sign out', more: 'More', moreLong: 'More' },
    });
  });
});
