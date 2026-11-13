import type { ReactNode } from 'react';
import { Outlet, NavLink } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import { api } from '../api';
import { errorStatusQuery } from '../errorStatus';
import SyncStatus from './SyncStatus';
import { getStoredUser } from '../utils/auth';
import { LogoSvg } from './Logo';
import LayoutChooser from '../layouts/LayoutChooser';
import { LAYOUT_CATALOG, NAV_GROUP_IDS, NAV_GROUP_LABELS, resolveTheme, type LayoutId, type NavGroupId } from '../layouts/layoutCatalog';
import { useLayoutPreference } from '../layouts/layoutPreference';
import '../layouts/layoutTokens.css';

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

export interface LayoutFrameProps {
  readonly layout: LayoutId;
  readonly isAdmin: boolean;
  readonly activeConflicts: number;
  readonly accountName: string | null;
  readonly syncStatus: ReactNode;
  readonly chooser: ReactNode;
  readonly onLogout: () => void;
}

/**
 * #262: one fixed shell tree for every layout. A layout changes classes, data
 * attributes and how the navigation entries are grouped, never the element
 * types or keys on the path to <main>/<Outlet>, so switching keeps the routed
 * page, its queries, drafts, focus and scroll.
 */
export function LayoutFrame({ layout, isAdmin, activeConflicts, accountName, syncStatus, chooser, onLogout }: LayoutFrameProps) {
  const presentation = LAYOUT_CATALOG[layout];
  const classes = presentation.classes;
  const visible = PRIMARY_NAVIGATION.filter(item => !item.adminOnly || isAdmin);

  const navLinkClass = ({ isActive }: { isActive: boolean }) =>
    `rs-shell-link ${classes.link}${isActive ? ' rs-shell-link--active' : ''}`;

  const renderLink = (item: PrimaryNavItem) => (
    <NavLink key={item.to} to={item.to} end={item.end} className={navLinkClass}>
      {item.label}
      {item.conflictCount && activeConflicts > 0 && (
        <span className="ml-2 inline-flex items-center justify-center px-2 py-1 text-xs font-bold leading-none rs-shell-alert rounded-full">
          {activeConflicts}
        </span>
      )}
    </NavLink>
  );

  const links = presentation.navPresentation === 'grouped'
    ? NAV_GROUP_IDS.map(groupId => {
      const entries = visible.filter(item => item.group === groupId);
      if (entries.length === 0) return null;
      const labelId = `rs-shell-nav-group-${groupId}`;
      return (
        <div key={groupId} role="group" aria-labelledby={labelId} className={classes.group}>
          <p id={labelId} className={classes.groupLabel}>{NAV_GROUP_LABELS[groupId]}</p>
          <div className={classes.groupLinks}>{entries.map(renderLink)}</div>
        </div>
      );
    })
    : visible.map(renderLink);

  return (
    <div className={`rs-layout-shell ${classes.root}`} data-rs-layout={presentation.id} data-rs-theme={resolveTheme(presentation)}>
      <nav aria-label="Primary" className={classes.nav}>
        <div className={classes.navInner}>
          <div className={classes.navRow}>
            <div className={classes.brand}>
              <LogoSvg size={32} />
              <span className={classes.brandName}>RepoSync</span>
              <div className={classes.links}>
                {links}
              </div>
            </div>
            <div className={classes.account}>
              {syncStatus}
              {accountName !== null && (
                <span className="rs-shell-muted text-sm">
                  {accountName}
                </span>
              )}
              {chooser}
              <button
                onClick={onLogout}
                className="rs-shell-action text-sm"
              >
                Logout
              </button>
            </div>
          </div>
        </div>
      </nav>

      <main tabIndex={-1} className={classes.main}>
        <Outlet />
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
      isAdmin={isAdmin}
      activeConflicts={status?.active_conflicts ?? 0}
      accountName={user ? user.display_name || user.username : null}
      syncStatus={<SyncStatus query={statusQuery} />}
      chooser={<LayoutChooser value={presentation.layout} onChange={presentation.choose} notice={presentation.notice} />}
      onLogout={handleLogout}
    />
  );
}
