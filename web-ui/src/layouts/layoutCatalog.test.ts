import { describe, expect, it } from 'vitest';
import {
  DEFAULT_LAYOUT, DEFAULT_THEME, LAYOUT_CATALOG, LAYOUT_IDS, NAV_GROUP_IDS, NAV_GROUP_LABELS, THEME_IDS,
  isLayoutId, isThemeId, resolveLayout, resolveTheme,
} from './layoutCatalog';

// Values a stored or injected preference might carry. None of them is a layout id.
const NOT_LAYOUTS: readonly unknown[] = [
  'Classic', ' classic', 'classic ', 'TEAM', 'Individual', '', 'compact', 'light',
  'toString', 'constructor', '__proto__', 'hasOwnProperty', 'valueOf',
  null, undefined, 0, 1, true, NaN, {}, [], ['team'], { id: 'team' }, () => 'team', Symbol('team'),
];

describe('#262 layout catalog', () => {
  it('offers exactly Classic, Individual and Team, with Classic as the default', () => {
    expect(LAYOUT_IDS).toEqual(['classic', 'individual', 'team']);
    expect(DEFAULT_LAYOUT).toBe('classic');
    expect(Object.keys(LAYOUT_CATALOG)).toEqual([...LAYOUT_IDS]);
    for (const id of LAYOUT_IDS) expect(LAYOUT_CATALOG[id].id).toBe(id);
    expect(LAYOUT_IDS.map(id => LAYOUT_CATALOG[id].label)).toEqual(['Classic', 'Individual', 'Team']);
  });

  it('accepts only exact catalog ids: no case folding, trimming or prototype keys', () => {
    for (const id of LAYOUT_IDS) expect(isLayoutId(id)).toBe(true);
    for (const value of NOT_LAYOUTS) expect(isLayoutId(value), String(typeof value === 'symbol' ? 'symbol' : value)).toBe(false);
  });

  it('resolves every unknown or malformed value to Classic and known ids to themselves', () => {
    for (const value of NOT_LAYOUTS) expect(resolveLayout(value)).toBe(LAYOUT_CATALOG.classic);
    for (const id of LAYOUT_IDS) expect(resolveLayout(id)).toBe(LAYOUT_CATALOG[id]);
  });

  it('describes presentation only: no capability, role, route or server-mode field', () => {
    for (const id of LAYOUT_IDS) {
      const entry = LAYOUT_CATALOG[id];
      expect(Object.keys(entry).sort()).toEqual(['classes', 'description', 'id', 'label', 'navPresentation', 'themes']);
      expect(Object.keys(entry.classes).sort()).toEqual([
        'account', 'brand', 'brandName', 'group', 'groupLabel', 'groupLinks', 'link', 'links', 'main', 'nav', 'navInner', 'navRow', 'root',
      ]);
      const text = JSON.stringify(entry).toLowerCase();
      for (const word of ['admin', 'role', 'permission', 'capabilit', 'server', 'route', 'href', 'token', 'password']) {
        expect(text, `${id} mentions ${word}`).not.toContain(word);
      }
    }
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
    expect(Reflect.set(LAYOUT_CATALOG.team.classes, 'main', '')).toBe(false);
    expect(isLayoutId('compact')).toBe(false);
    expect(LAYOUT_CATALOG.classic.navPresentation).toBe('inline');
    expect(Object.isFrozen(NAV_GROUP_LABELS)).toBe(true);
  });

  it('keeps Classic on the original shell slots and spacing', () => {
    // Spacing utilities from the pre-#262 Layout.tsx; colors moved to scoped tokens.
    expect(LAYOUT_CATALOG.classic.navPresentation).toBe('inline');
    expect(LAYOUT_CATALOG.classic.classes).toEqual({
      root: 'min-h-screen',
      nav: 'rs-shell-chrome border-b',
      navInner: 'px-6 sm:px-8 lg:px-12',
      navRow: 'flex items-center justify-between h-16',
      brand: 'flex items-center',
      brandName: 'rs-shell-brand font-bold text-lg font-display tracking-wider',
      links: 'ml-10 flex items-baseline space-x-4',
      group: '',
      groupLabel: '',
      groupLinks: '',
      link: 'px-3 py-2 rounded-md text-sm font-medium transition-colors',
      account: 'flex items-center space-x-4',
      main: 'py-6 px-6 sm:px-8 lg:px-12',
    });
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

  it('groups navigation only in Team, under presentation-only headings', () => {
    expect(LAYOUT_IDS.map(id => LAYOUT_CATALOG[id].navPresentation)).toEqual(['inline', 'inline', 'grouped']);
    expect(NAV_GROUP_IDS).toEqual(['workspace', 'account', 'administration']);
    expect(NAV_GROUP_LABELS).toEqual({ workspace: 'Workspace', account: 'Account', administration: 'Administration' });
  });

  it('keeps every layout on the current dark theme until a later design adds another', () => {
    expect(THEME_IDS).toEqual(['dark']);
    expect(DEFAULT_THEME).toBe('dark');
    expect(isThemeId('dark')).toBe(true);
    for (const value of ['light', 'Dark', '', 'constructor', null, 1]) expect(isThemeId(value)).toBe(false);
    for (const id of LAYOUT_IDS) {
      const entry = LAYOUT_CATALOG[id];
      expect(entry.themes).toEqual(['dark']);
      expect(resolveTheme(entry)).toBe('dark');
      expect(resolveTheme(entry, 'dark')).toBe('dark');
      for (const value of ['light', 'constructor', undefined, null, 7]) expect(resolveTheme(entry, value)).toBe('dark');
    }
  });
});
