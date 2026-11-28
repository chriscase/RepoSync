import { useId } from 'react';
import { clientSession } from '../session';
import { useState, useEffect, FormEvent } from 'react';
import { Navigate } from 'react-router-dom';
import { api } from '../api';

/** The one read of sign-in options made when the page opens. */
export type DirectoryRead =
  | { state: 'pending' }
  | { state: 'failed' }
  | { state: 'loaded'; info: unknown };

interface DirectoryHint {
  network: boolean;
  domain: string | null;
}

const NO_DIRECTORY_HINT: DirectoryHint = { network: false, domain: null };

/**
 * The directory hint comes only from a read that completed and has directory sign-in
 * switched on. A pending, failed or malformed read shows no hint and the plain wording.
 */
function directoryHint(read: DirectoryRead): DirectoryHint {
  if (read.state !== 'loaded' || typeof read.info !== 'object' || read.info === null) return NO_DIRECTORY_HINT;
  const { ldap_enabled: enabled, ldap_domain: domain } = read.info as { ldap_enabled?: unknown; ldap_domain?: unknown };
  if (enabled !== true) return NO_DIRECTORY_HINT;
  return { network: true, domain: typeof domain === 'string' && domain.trim() !== '' ? domain : null };
}

/** The "Corporate login enabled" banner, shown only for a directory hint with a domain. */
export function DirectorySignInHint({ read }: { read: DirectoryRead }) {
  const hint = directoryHint(read);
  if (!hint.network || !hint.domain) return null;
  return (
    <div className="rs-bn rs-bn--info bg-blue-900/30 border border-blue-700/50 text-blue-300 px-4 py-3 rounded-md text-sm">
      <div className="flex items-center space-x-2">
        <svg className="w-4 h-4 flex-shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z" />
        </svg>
        <div>
          <span className="font-medium">Corporate login enabled</span>
          <span className="text-blue-400 ml-1">({hint.domain})</span>
        </div>
      </div>
      <p className="mt-1 text-xs text-blue-400/80 ml-6">
        Sign in with your network credentials
      </p>
    </div>
  );
}

export default function Login() {
  const fieldId = useId();
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [directoryRead, setDirectoryRead] = useState<DirectoryRead>({ state: 'pending' });
  const ldapEnabled = directoryHint(directoryRead).network;

  // Fetch LDAP info on mount (public endpoint, no auth needed)
  useEffect(() => {
    let current = true;
    api.getAuthInfo()
      .then((info) => {
        if (current) setDirectoryRead({ state: 'loaded', info });
      })
      .catch(() => {
        // Ignore — older backend may not have this endpoint
        if (current) setDirectoryRead({ state: 'failed' });
      });
    return () => {
      current = false;
    };
  }, []);

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    setError('');
    setLoading(true);
    try {
      await api.login(username, password);
    } catch (err) {
      // Show the real error message from the API
      const message = err instanceof Error ? err.message : 'Login failed';
      setError(message);
      // Clear password but keep username so user can retry
      setPassword('');
    } finally {
      setLoading(false);
    }
  };

  if (clientSession().snapshot().token) return <Navigate to="/" replace />;

  return (
    <div className="min-h-screen flex items-center justify-center bg-gray-900">
      <div className="rs-sec max-w-md w-full space-y-8 p-8 bg-gray-800 rounded-lg shadow-lg border border-gray-700">
        <div>
          <h2 className="rs-heading text-center text-3xl font-bold text-gray-100 font-display tracking-wider">
            RepoSync
          </h2>
          <p className="mt-2 text-center text-sm text-gray-400">
            Sign in to the sync dashboard
          </p>
        </div>

        {/* LDAP info banner */}
        <DirectorySignInHint read={directoryRead} />

        <form className="mt-8 space-y-6" onSubmit={handleSubmit}>
          {error && (
            <div className="rs-bn rs-bn--danger bg-red-900/40 border border-red-700 text-red-300 px-4 py-3 rounded" id={`${fieldId}-error`} role="alert">
              <div className="flex items-start space-x-2">
                <svg className="w-5 h-5 flex-shrink-0 mt-0.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                </svg>
                <span>{error}</span>
              </div>
            </div>
          )}
          <div className="space-y-4">
            <div>
              <label htmlFor="username" className="sr-only">
                Username
              </label>
              <input
                id="username"
                name="username"
                type="text"
                required
                autoFocus
                className="rs-in appearance-none rounded-md relative block w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-400 text-gray-100 focus:outline-none focus:ring-blue-500 focus:border-blue-500 focus:z-10 sm:text-sm"
                placeholder={ldapEnabled ? 'Network username' : 'Username'}
                value={username}
                onChange={(e) => setUsername(e.target.value)} aria-describedby={error ? `${fieldId}-error` : undefined}
              />
            </div>
            <div>
              <label htmlFor="password" className="sr-only">
                Password
              </label>
              <input
                id="password"
                name="password"
                type="password"
                required
                className="rs-in appearance-none rounded-md relative block w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-400 text-gray-100 focus:outline-none focus:ring-blue-500 focus:border-blue-500 focus:z-10 sm:text-sm"
                placeholder={ldapEnabled ? 'Network password' : 'Password'}
                value={password}
                onChange={(e) => setPassword(e.target.value)} aria-describedby={error ? `${fieldId}-error` : undefined}
              />
            </div>
          </div>
          <button
            type="submit"
            disabled={loading}
            className="rs-btn rs-btn--primary w-full flex justify-center py-2 px-4 border border-transparent text-sm font-medium rounded-md rs-text-on-accent bg-blue-600 hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-offset-gray-800 focus:ring-blue-500 disabled:opacity-50"
          >
            {loading ? 'Signing in...' : 'Sign in'}
          </button>
        </form>
      </div>
    </div>
  );
}
