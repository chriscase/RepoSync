/**
 * #262 presentation catalog and shared navigation metadata.
 *
 * A layout only arranges the one shared application shell. It never grants a
 * capability, changes server mode, adds or removes a route, or alters which
 * navigation entries an account may see. PRIMARY_NAVIGATION below is shared
 * metadata only: Layout.tsx still derives visibility from the signed-in
 * account exactly as before.
 */
export const LAYOUT_IDS = ['classic', 'individual', 'team'] as const;
export type LayoutId = (typeof LAYOUT_IDS)[number];
/** Classic is the default and the fallback for absent, unknown or unusable preferences. */
export const DEFAULT_LAYOUT: LayoutId = 'classic';

/**
 * Theme seam. Only the current dark shell palette exists in this build. A later
 * design adds an id here, a token block in layoutTokens.css and lists the id in
 * each layout that supports it. Page content is not themed by this seam.
 */
export const THEME_IDS = ['dark'] as const;
export type ThemeId = (typeof THEME_IDS)[number];
export const DEFAULT_THEME: ThemeId = 'dark';

export type NavPresentation = 'inline' | 'grouped';
export const NAV_GROUP_IDS = ['workspace', 'account', 'administration'] as const;
export type NavGroupId = (typeof NAV_GROUP_IDS)[number];
/** Group headings are presentation only; the entries inside them are unchanged. */
export const NAV_GROUP_LABELS: Readonly<Record<NavGroupId, string>> = Object.freeze({
  workspace: 'Workspace',
  account: 'Account',
  administration: 'Administration',
});

export interface PrimaryNavItem {
  readonly to: string;
  readonly label: string;
  readonly group: NavGroupId;
  readonly end?: boolean;
  readonly adminOnly?: boolean;
  readonly conflictCount?: boolean;
}

/** Targets, labels, order, active matching and admin-only visibility, shared by every layout. */
export const PRIMARY_NAVIGATION: readonly PrimaryNavItem[] = [
  { to: '/', label: 'Dashboard', group: 'workspace', end: true },
  { to: '/repos', label: 'Repositories', group: 'workspace' },
  { to: '/conflicts', label: 'Conflicts', group: 'workspace', conflictCount: true },
  { to: '/audit', label: 'Audit Log', group: 'workspace' },
  { to: '/settings', label: 'Settings', group: 'account' },
  { to: '/users', label: 'Users', group: 'administration', adminOnly: true },
  { to: '/ldap', label: 'LDAP', group: 'administration', adminOnly: true },
  { to: '/config', label: 'Config', group: 'administration', adminOnly: true },
  { to: '/setup', label: 'Setup', group: 'administration', adminOnly: true },
];
/** Layout and spacing utilities for each fixed shell slot. Colors come from layoutTokens.css. */
export interface ShellClasses {
  readonly root: string;
  readonly nav: string;
  readonly navInner: string;
  readonly navRow: string;
  readonly brand: string;
  readonly brandName: string;
  readonly links: string;
  readonly group: string;
  readonly groupLabel: string;
  readonly groupLinks: string;
  readonly link: string;
  readonly account: string;
  readonly main: string;
}

export interface LayoutPresentation {
  readonly id: LayoutId;
  readonly label: string;
  readonly description: string;
  readonly navPresentation: NavPresentation;
  readonly themes: readonly ThemeId[];
  readonly classes: ShellClasses;
}

function presentation(value: LayoutPresentation): LayoutPresentation {
  return Object.freeze({ ...value, themes: Object.freeze([...value.themes]), classes: Object.freeze({ ...value.classes }) });
}

export const LAYOUT_CATALOG: Readonly<Record<LayoutId, LayoutPresentation>> = Object.freeze({
  // The single shell as it was before #262: same slots and spacing.
  classic: presentation({
    id: 'classic',
    label: 'Classic',
    description: 'The original top navigation bar.',
    navPresentation: 'inline',
    themes: ['dark'],
    classes: {
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
    },
  }),
  // One person's working view: centred content and tab-style links that wrap below
  // the brand on narrow screens instead of overflowing.
  individual: presentation({
    id: 'individual',
    label: 'Individual',
    description: 'Compact tabs with centred content.',
    navPresentation: 'inline',
    themes: ['dark'],
    classes: {
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
    },
  }),
  // Shared operations view: a grouped sidebar on wide screens that becomes a
  // scrollable top bar on narrow screens.
  team: presentation({
    id: 'team',
    label: 'Team',
    description: 'Grouped sidebar navigation.',
    navPresentation: 'grouped',
    themes: ['dark'],
    classes: {
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
    },
  }),
});

/** Strict enum membership: no prototype keys, case folding or trimming. */
export function isLayoutId(value: unknown): value is LayoutId {
  return typeof value === 'string' && (LAYOUT_IDS as readonly string[]).includes(value);
}

export function resolveLayout(value: unknown): LayoutPresentation {
  return LAYOUT_CATALOG[isLayoutId(value) ? value : DEFAULT_LAYOUT];
}

export function isThemeId(value: unknown): value is ThemeId {
  return typeof value === 'string' && (THEME_IDS as readonly string[]).includes(value);
}

/** A theme a layout does not list falls back to the default theme. */
export function resolveTheme(layout: LayoutPresentation, requested: unknown = DEFAULT_THEME): ThemeId {
  return isThemeId(requested) && layout.themes.includes(requested) ? requested : DEFAULT_THEME;
}
