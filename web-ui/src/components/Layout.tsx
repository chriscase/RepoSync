import { Outlet, NavLink } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import { api } from '../api';
import { errorStatusQuery } from '../errorStatus';
import SyncStatus from './SyncStatus';
import { getStoredUser } from '../utils/auth';
import { LogoSvg } from './Logo';

export default function Layout() {
  const statusQuery = useQuery({...errorStatusQuery(),refetchInterval:15000});
  const status=statusQuery.isError ? undefined:statusQuery.data;

  const user = getStoredUser();
  const isAdmin = user?.role === 'admin';

  const handleLogout = () => {
    // ProtectedRoute reacts to synchronous retirement, even if server revocation is unavailable.
    void api.logout().catch(() => {});
  };

  const navLinkClass = ({ isActive }: { isActive: boolean }) =>
    `px-3 py-2 rounded-md text-sm font-medium transition-colors ${
      isActive
        ? 'bg-blue-600 text-white'
        : 'text-gray-400 hover:bg-gray-700 hover:text-white'
    }`;

  return (
    <div className="min-h-screen bg-gray-900">
      <nav className="bg-gray-950 border-b border-gray-800">
        <div className="px-6 sm:px-8 lg:px-12">
          <div className="flex items-center justify-between h-16">
            <div className="flex items-center">
              <LogoSvg size={32} />
              <span className="text-white font-bold text-lg font-display tracking-wider">RepoSync</span>
              <div className="ml-10 flex items-baseline space-x-4">
                <NavLink to="/" end className={navLinkClass}>
                  Dashboard
                </NavLink>
                <NavLink to="/repos" className={navLinkClass}>
                  Repositories
                </NavLink>
                <NavLink to="/conflicts" className={navLinkClass}>
                  Conflicts
                  {status && status.active_conflicts > 0 && (
                    <span className="ml-2 inline-flex items-center justify-center px-2 py-1 text-xs font-bold leading-none text-red-100 bg-red-600 rounded-full">
                      {status.active_conflicts}
                    </span>
                  )}
                </NavLink>
                <NavLink to="/audit" className={navLinkClass}>
                  Audit Log
                </NavLink>
                <NavLink to="/settings" className={navLinkClass}>
                  Settings
                </NavLink>
                {isAdmin && (
                  <NavLink to="/users" className={navLinkClass}>
                    Users
                  </NavLink>
                )}
                {isAdmin && (
                  <NavLink to="/ldap" className={navLinkClass}>
                    LDAP
                  </NavLink>
                )}
                {isAdmin && (
                  <NavLink to="/config" className={navLinkClass}>
                    Config
                  </NavLink>
                )}
                {isAdmin && (
                  <NavLink to="/setup" className={navLinkClass}>
                    Setup
                  </NavLink>
                )}
              </div>
            </div>
            <div className="flex items-center space-x-4">
              <SyncStatus query={statusQuery} />
              {user && (
                <span className="text-sm text-gray-400">
                  {user.display_name || user.username}
                </span>
              )}
              <button
                onClick={handleLogout}
                className="text-gray-400 hover:text-white text-sm"
              >
                Logout
              </button>
            </div>
          </div>
        </div>
      </nav>

      <main className="py-6 px-6 sm:px-8 lg:px-12">
        <Outlet />
      </main>
    </div>
  );
}
