/**
 * #262 presentation catalog and shared navigation metadata, extended by #264.
 *
 * A layout only arranges the one shared application shell. It never grants a
 * capability, changes server mode, adds or removes a route, or alters which
 * navigation entries an account may see. PRIMARY_NAVIGATION below is shared
 * metadata only: Layout.tsx still derives visibility from the signed-in
 * account exactly as before.
 *
 * #264 adds the three accepted #263 designs (Ledger, Console, Guide) after the
 * existing three, a light theme in every layout, and the shell extensions
 * E1 (More), E2 (theme), E3 (vocabulary), E4 (Help) and E6 (navigation icons).
 */
export const LAYOUT_IDS = ['classic', 'individual', 'team', 'ledger', 'console', 'guide'] as const;
export type LayoutId = (typeof LAYOUT_IDS)[number];
/** Classic is the default and the fallback for absent, unknown or unusable preferences. */
export const DEFAULT_LAYOUT: LayoutId = 'classic';
/** The layouts that existed before #264. They keep today's arrangement and wording. */
export const EXISTING_LAYOUT_IDS: readonly LayoutId[] = Object.freeze(['classic', 'individual', 'team'] as LayoutId[]);

/**
 * Theme seam. Dark is the default in every layout and stays until a person
 * chooses light. Every layout lists both. Existing layouts in dark render with
 * the page's own classes exactly as before; any other combination takes page
 * colour from tokens (see pageTokens.css and colourBridge.css).
 */
export const THEME_IDS = ['dark', 'light'] as const;
export type ThemeId = (typeof THEME_IDS)[number];
export const DEFAULT_THEME: ThemeId = 'dark';
export const THEME_LABELS: Readonly<Record<ThemeId, string>> = Object.freeze({ dark: 'Dark', light: 'Light' });

/**
 * E3: one text catalogue with two vocabularies. Classic, Individual and Team use the
 * existing vocabulary, which is today's strings byte for byte. Ledger, Console and
 * Guide use the plain vocabulary of the #263 crosswalk. Safety and truthful-status
 * sentences are identical in both (decision D17).
 */
export const VOCABULARY_IDS = ['existing', 'plain'] as const;
export type VocabularyId = (typeof VOCABULARY_IDS)[number];

export type NavPresentation = 'inline' | 'grouped';
export const NAV_GROUP_IDS = ['workspace', 'account', 'administration'] as const;
export type NavGroupId = (typeof NAV_GROUP_IDS)[number];
/** Group headings are presentation only; the entries inside them are unchanged. */
export const NAV_GROUP_LABELS: Readonly<Record<NavGroupId, string>> = Object.freeze({
  workspace: 'Workspace',
  account: 'Account',
  administration: 'Administration',
});

/** E6: an optional icon per navigation entry, rendered in every layout and hidden in the existing three. */
export type NavIconId = 'dashboard' | 'repositories' | 'conflicts' | 'audit' | 'account' | 'help' | 'users' | 'directory' | 'identity' | 'setup';

export interface PrimaryNavItem {
  readonly to: string;
  /** The existing vocabulary: today's label, byte for byte. */
  readonly label: string;
  /** The plain vocabulary used by Ledger, Console and Guide. */
  readonly plainLabel: string;
  readonly icon: NavIconId;
  readonly group: NavGroupId;
  readonly end?: boolean;
  readonly adminOnly?: boolean;
  readonly conflictCount?: boolean;
  /** E4: an entry shown only in the listed layouts. Absent means every layout. */
  readonly onlyIn?: readonly LayoutId[];
}

/** Targets, labels, order, active matching and admin-only visibility, shared by every layout. */
export const PRIMARY_NAVIGATION: readonly PrimaryNavItem[] = [
  { to: '/', label: 'Dashboard', plainLabel: 'Dashboard', icon: 'dashboard', group: 'workspace', end: true },
  { to: '/repos', label: 'Repositories', plainLabel: 'Repositories', icon: 'repositories', group: 'workspace' },
  { to: '/conflicts', label: 'Conflicts', plainLabel: 'Conflicts', icon: 'conflicts', group: 'workspace', conflictCount: true },
  { to: '/audit', label: 'Audit Log', plainLabel: 'Audit log', icon: 'audit', group: 'workspace' },
  { to: '/settings', label: 'Settings', plainLabel: 'My account', icon: 'account', group: 'account' },
  { to: '/docs', label: 'Help', plainLabel: 'Help', icon: 'help', group: 'account', onlyIn: ['ledger', 'console', 'guide'] },
  { to: '/users', label: 'Users', plainLabel: 'Users', icon: 'users', group: 'administration', adminOnly: true },
  { to: '/ldap', label: 'LDAP', plainLabel: 'Directory (LDAP)', icon: 'directory', group: 'administration', adminOnly: true },
  { to: '/config', label: 'Config', plainLabel: 'Identity & notifications', icon: 'identity', group: 'administration', adminOnly: true },
  { to: '/setup', label: 'Setup', plainLabel: 'Server setup', icon: 'setup', group: 'administration', adminOnly: true },
];

/** The entries a layout shows to an account: admin visibility as before, plus E4's layout limit. */
export function visibleNavigation(layout: LayoutId, isAdmin: boolean): readonly PrimaryNavItem[] {
  return PRIMARY_NAVIGATION.filter(item => (!item.adminOnly || isAdmin) && (!item.onlyIn || item.onlyIn.includes(layout)));
}

export function navLabel(item: PrimaryNavItem, vocabulary: VocabularyId): string {
  return vocabulary === 'plain' ? item.plainLabel : item.label;
}

/** Shell text in each vocabulary (E3). The existing column is today's text. */
export interface ShellText {
  readonly logout: string;
  readonly more: string;
  readonly moreLong: string;
}
export const SHELL_TEXT: Readonly<Record<VocabularyId, ShellText>> = Object.freeze({
  existing: Object.freeze({ logout: 'Logout', more: 'More', moreLong: 'More' }),
  plain: Object.freeze({ logout: 'Sign out', more: 'More', moreLong: 'More' }),
});

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
  readonly vocabulary: VocabularyId;
  /** Text on the E1 control when it shows a long label (Guide at wide sizes). */
  readonly moreLongLabel?: string;
  readonly classes: ShellClasses;
}

function presentation(value: LayoutPresentation): LayoutPresentation {
  return Object.freeze({ ...value, themes: Object.freeze([...value.themes]), classes: Object.freeze({ ...value.classes }) });
}

const EXTENDED_SHELL_CLASSES: ShellClasses = {
  root: '', nav: '', navInner: '', navRow: '', brand: '', brandName: '', links: '',
  group: '', groupLabel: '', groupLinks: '', link: '', account: '', main: '',
};

export const LAYOUT_CATALOG: Readonly<Record<LayoutId, LayoutPresentation>> = Object.freeze({
  // The single shell as it was before #262: same slots and spacing. The account group
  // fills the rest of the row (flex-1, right-aligned), so it wraps to a second line only
  // when it cannot fit even with its text wrapped, instead of pushing Logout off screen
  // (the chooser made the administrator bar wider than common laptop widths).
  classic: presentation({
    id: 'classic',
    label: 'Classic',
    description: 'The original top navigation bar.',
    navPresentation: 'inline',
    themes: ['dark', 'light'],
    vocabulary: 'existing',
    classes: {
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
    },
  }),
  // One person's working view: centred content and tab-style links that wrap below
  // the brand on narrow screens instead of overflowing.
  individual: presentation({
    id: 'individual',
    label: 'Individual',
    description: 'Compact tabs with centred content.',
    navPresentation: 'inline',
    themes: ['dark', 'light'],
    vocabulary: 'existing',
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
    themes: ['dark', 'light'],
    vocabulary: 'existing',
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
  // #264, the three accepted #263 designs. Their arrangement lives in layoutOptions.css,
  // keyed on data-rs-layout; the slot utilities here are empty so nothing competes with it.
  ledger: presentation({
    id: 'ledger',
    label: 'Ledger',
    description: 'A record to read: a two-row masthead, ruled sections and stamps.',
    navPresentation: 'grouped',
    themes: ['dark', 'light'],
    vocabulary: 'plain',
    classes: EXTENDED_SHELL_CLASSES,
  }),
  console: presentation({
    id: 'console',
    label: 'Console',
    description: 'An operations console: a grouped sidebar, dense tables and status beside the work.',
    navPresentation: 'grouped',
    themes: ['dark', 'light'],
    vocabulary: 'plain',
    classes: EXTENDED_SHELL_CLASSES,
  }),
  guide: presentation({
    id: 'guide',
    label: 'Guide',
    description: 'Task-led: four large destinations, roomy sections and big targets.',
    navPresentation: 'grouped',
    themes: ['dark', 'light'],
    vocabulary: 'plain',
    moreLongLabel: 'Account and admin',
    classes: EXTENDED_SHELL_CLASSES,
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

export function isExistingLayout(layout: LayoutId): boolean {
  return EXISTING_LAYOUT_IDS.includes(layout);
}

/**
 * Page colour comes from tokens everywhere except an existing layout in dark, which keeps
 * the page's own utility colours exactly as before #264.
 */
export function usesPageTokens(layout: LayoutId, theme: ThemeId): boolean {
  return !(isExistingLayout(layout) && theme === 'dark');
}
