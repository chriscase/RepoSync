import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { Outlet, NavLink, useLocation } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import {
  ArrowLeftRight, Database, GitMerge, HelpCircle, KeyRound, LayoutDashboard, Menu, ScrollText, Server, User, Users,
  type LucideIcon,
} from 'lucide-react';
import { api } from '../api';
import { errorStatusQuery } from '../errorStatus';
import SyncStatus from './SyncStatus';
import { getStoredUser } from '../utils/auth';
import { LogoSvg } from './Logo';
import LayoutChooser from '../layouts/LayoutChooser';
import {
  DEFAULT_THEME, LAYOUT_CATALOG, NAV_GROUP_IDS, NAV_GROUP_LABELS, SHELL_TEXT, navLabel, resolveTheme, usesPageTokens, visibleNavigation,
  type LayoutId, type NavIconId, type PrimaryNavItem, type ThemeId,
} from '../layouts/layoutCatalog';
import { useLayoutPreference } from '../layouts/layoutPreference';
import { VocabularyProvider } from '../layouts/VocabularyProvider';
// Order matters for rules of equal specificity: tokens, then the generated colour bridge,
// then the option arrangements and page styling, which may refine a bridged colour.
import '../layouts/layoutTokens.css';
import '../layouts/pageTokens.css';
import '../layouts/colourBridge.css';
import '../layouts/layoutOptions.css';
import '../layouts/pageClasses.css';


export interface LayoutFrameProps {
  readonly layout: LayoutId;
  /** #264 E2. Absent means dark, the default. */
  readonly theme?: ThemeId;
  readonly isAdmin: boolean;
  readonly activeConflicts: number;
  readonly accountName: string | null;
  readonly syncStatus: ReactNode;
  readonly chooser: ReactNode;
  readonly onLogout: () => void;
}

/** E6: one icon per navigation entry. Rendered in every layout, hidden in Classic, Individual and Team. */
const NAV_ICONS: Readonly<Record<NavIconId, LucideIcon>> = {
  dashboard: LayoutDashboard,
  repositories: Database,
  conflicts: GitMerge,
  audit: ScrollText,
  account: User,
  help: HelpCircle,
  users: Users,
  directory: KeyRound,
  identity: ArrowLeftRight,
  setup: Server,
};

const groupId = (group: string) => `rs-shell-nav-${group}`;

/**
 * Brings a focused navigation link fully into view inside its horizontally scrolling
 * link strip (Individual, and Team below md), if it sits in one. It only ever changes that
 * strip's scrollLeft and never the page's scroll position, so focus restored by a dialog, a
 * returning window or focus({ preventScroll: true }) cannot move the page.
 */
function revealInLinkStrip(link: HTMLElement) {
  for (let strip = link.parentElement; strip && strip.tagName !== 'NAV'; strip = strip.parentElement) {
    const overflowX = getComputedStyle(strip).overflowX;
    if (overflowX !== 'auto' && overflowX !== 'scroll') continue;
    const left = strip.getBoundingClientRect().left + strip.clientLeft;
    const right = left + strip.clientWidth;
    const target = link.getBoundingClientRect();
    if (target.left < left) strip.scrollLeft -= Math.ceil(left - target.left);
    else if (target.right > right) strip.scrollLeft += Math.ceil(target.right - right);
    return;
  }
}

/**
 * #262: one fixed shell tree for every layout. A layout changes classes, data
 * attributes and how the navigation entries are grouped, never the element
 * types or keys on the path to <main>/<Outlet>, so switching keeps the routed
 * page, its queries, drafts, focus and scroll.
 *
 * #264 adds, in every layout: the E1 "More" button after the last workspace link,
 * an E6 icon in each link and fixed rs-slot-* classes that the option stylesheets
 * arrange. In Classic, Individual and Team the button and the icons are hidden by CSS,
 * so those layouts look and tab as before. Page colour switches to tokens through
 * data-rs-bridge, which is absent for an existing layout in dark.
 */
export function LayoutFrame({ layout, theme = DEFAULT_THEME, isAdmin, activeConflicts, accountName, syncStatus, chooser, onLogout }: LayoutFrameProps) {
  const presentation = LAYOUT_CATALOG[layout];
  const classes = presentation.classes;
  const vocabulary = presentation.vocabulary;
  const text = SHELL_TEXT[vocabulary];
  const resolvedTheme = resolveTheme(presentation, theme);
  const visible = visibleNavigation(layout, isAdmin);
  const lastWorkspace = visible.reduce((last, item, index) => (item.group === 'workspace' ? index : last), -1);
  const location = useLocation();
  const [navOpen, setNavOpen] = useState(false);
  const focusOnOpen = useRef(false);
  const moreRef = useRef<HTMLButtonElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  // E1: a route change closes "More"; opening it moves focus to the first link it reveals.
  useEffect(() => { setNavOpen(false); }, [location.pathname]);
  useEffect(() => {
    if (!navOpen || !focusOnOpen.current) return;
    focusOnOpen.current = false;
    const first = rootRef.current?.querySelector<HTMLElement>(`#${groupId('account')} a, #${groupId('administration')} a`);
    first?.focus();
  }, [navOpen]);

  const toggleMore = () => {
    focusOnOpen.current = !navOpen;
    setNavOpen(open => !open);
  };
  const onNavKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key !== 'Escape' || !navOpen) return;
    event.stopPropagation();
    setNavOpen(false);
    moreRef.current?.focus();
  };

  const navLinkClass = ({ isActive }: { isActive: boolean }) =>
    `rs-shell-link ${classes.link}${isActive ? ' rs-shell-link--active' : ''}`;

  const renderLink = (item: PrimaryNavItem) => {
    const Icon = NAV_ICONS[item.icon];
    return (
      // Focus leaves a partly hidden link in a scrolling strip partly hidden, so reveal it
      // within the strip only (a no-op outside a strip or when it is already fully visible).
      <NavLink key={item.to} to={item.to} end={item.end} className={navLinkClass}
        onFocus={event => revealInLinkStrip(event.currentTarget)}>
        <Icon aria-hidden="true" className="rs-nav-icon" />
        {navLabel(item, vocabulary)}
        {item.conflictCount && activeConflicts > 0 && (
          <span className="rs-shell-count ml-2 inline-flex items-center justify-center px-2 py-1 text-xs font-bold leading-none rs-shell-alert rounded-full">
            {activeConflicts}
          </span>
        )}
      </NavLink>
    );
  };

  const revealed = NAV_GROUP_IDS.filter(group => group !== 'workspace' && visible.some(item => item.group === group));
  const more = (
    <button
      key="rs-shell-more"
      ref={moreRef}
      type="button"
      className="rs-shell-more"
      aria-expanded={navOpen}
      aria-controls={presentation.navPresentation === 'grouped' ? revealed.map(groupId).join(' ') : undefined}
      onClick={toggleMore}
    >
      <Menu aria-hidden="true" className="rs-more-icon" />
      <span className="rs-more-long">{presentation.moreLongLabel ?? text.moreLong}</span>
      <span className="rs-more-short">{text.more}</span>
    </button>
  );

  const withMore = (entries: readonly PrimaryNavItem[], from: number) => entries.flatMap((item, index) =>
    from + index === lastWorkspace ? [renderLink(item), more] : [renderLink(item)]);

  const links = presentation.navPresentation === 'grouped'
    ? NAV_GROUP_IDS.map(group => {
      const entries = visible.filter(item => item.group === group);
      if (entries.length === 0) return null;
      const labelId = `rs-shell-nav-group-${group}`;
      const from = visible.indexOf(entries[0]);
      return (
        <div key={group} id={groupId(group)} role="group" aria-labelledby={labelId} data-group={group} className={`rs-slot-group ${classes.group}`}>
          <p id={labelId} className={`rs-slot-group-label ${classes.groupLabel}`}>{NAV_GROUP_LABELS[group]}</p>
          <div className={`rs-slot-group-links ${classes.groupLinks}`}>{withMore(entries, from)}</div>
        </div>
      );
    })
    : withMore(visible, 0);

  return (
    <div ref={rootRef} className={`rs-layout-shell ${classes.root}`} data-rs-layout={presentation.id} data-rs-theme={resolvedTheme}
      data-rs-bridge={usesPageTokens(layout, resolvedTheme) ? '' : undefined} data-nav-open={navOpen ? '' : undefined}>
      <nav aria-label="Primary" className={`rs-slot-nav ${classes.nav}`} onKeyDown={onNavKeyDown}>
        <div className={`rs-slot-nav-inner ${classes.navInner}`}>
          <div className={`rs-slot-nav-row ${classes.navRow}`}>
            <div className={`rs-slot-brand ${classes.brand}`}>
              <LogoSvg size={32} className="rs-slot-logo" />
              <span className={`rs-slot-brand-name ${classes.brandName}`}>RepoSync</span>
              <div className={`rs-slot-links ${classes.links}`}>
                {links}
              </div>
            </div>
            <div className={`rs-slot-account ${classes.account}`}>
              {syncStatus}
              {accountName !== null && (
                <span className="rs-shell-user rs-shell-muted text-sm">
                  {accountName}
                </span>
              )}
              {chooser}
              <button
                onClick={onLogout}
                className="rs-shell-logout rs-shell-action text-sm"
              >
                {text.logout}
              </button>
            </div>
          </div>
        </div>
      </nav>

      <main tabIndex={-1} className={`rs-slot-main ${classes.main}`}>
        <VocabularyProvider vocabulary={vocabulary}>
          <Outlet />
        </VocabularyProvider>
      </main>
    </div>
  );
}

export default function Layout() {
  const statusQuery = useQuery({...errorStatusQuery(),refetchInterval:15000});
  const status=statusQuery.isError ? undefined:statusQuery.data;

  const user = getStoredUser();
  const isAdmin = user?.role === 'admin';
  // #262: presentation only. It never feeds isAdmin, routes, queries, actions or server mode.
  const presentation = useLayoutPreference(user);

  const handleLogout = () => {
    // ProtectedRoute reacts to synchronous retirement, even if server revocation is unavailable.
    void api.logout().catch(() => {});
  };

  return (
    <LayoutFrame
      layout={presentation.layout}
      theme={presentation.theme}
      isAdmin={isAdmin}
      activeConflicts={status?.active_conflicts ?? 0}
      accountName={user ? user.display_name || user.username : null}
      syncStatus={<SyncStatus query={statusQuery} />}
      chooser={<LayoutChooser value={presentation.layout} onChange={presentation.choose}
        theme={presentation.theme} onThemeChange={presentation.chooseTheme} notice={presentation.notice} />}
      onLogout={handleLogout}
    />
  );
}
