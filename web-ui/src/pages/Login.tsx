import { useState, useEffect, FormEvent } from 'react';
import { useNavigate } from 'react-router-dom';
import { api } from '../api';

export default function Login() {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [ldapEnabled, setLdapEnabled] = useState(false);
  const [ldapDomain, setLdapDomain] = useState<string | null>(null);
  const navigate = useNavigate();

  // Fetch LDAP info on mount (public endpoint, no auth needed)
  useEffect(() => {
    api.getAuthInfo()
      .then((info) => {
        setLdapEnabled(info.ldap_enabled);
        setLdapDomain(info.ldap_domain);
      })
      .catch(() => {
        // Ignore — older backend may not have this endpoint
      });
  }, []);

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    setError('');
    setLoading(true);
    try {
      const result = await api.login(username, password);
      localStorage.setItem('session_token', result.token);
      if (result.user) {
        localStorage.setItem('user', JSON.stringify(result.user));
      }
      navigate('/');
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

  return (
    <div className="min-h-screen flex items-center justify-center bg-gray-900">
      <div className="max-w-md w-full space-y-8 p-8 bg-gray-800 rounded-lg shadow-lg border border-gray-700">
        <div>
          <h2 className="text-center text-3xl font-bold text-gray-100 font-display tracking-wider">
            RepoSync
          </h2>
          <p className="mt-2 text-center text-sm text-gray-400">
            Sign in to the sync dashboard
          </p>
        </div>

        {/* LDAP info banner */}
        {ldapEnabled && ldapDomain && (
          <div className="bg-blue-900/30 border border-blue-700/50 text-blue-300 px-4 py-3 rounded-md text-sm">
            <div className="flex items-center space-x-2">
              <svg className="w-4 h-4 flex-shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z" />
              </svg>
              <div>
                <span className="font-medium">Corporate login enabled</span>
                <span className="text-blue-400 ml-1">({ldapDomain})</span>
              </div>
            </div>
            <p className="mt-1 text-xs text-blue-400/80 ml-6">
              Sign in with your network credentials
            </p>
          </div>
        )}

        <form className="mt-8 space-y-6" onSubmit={handleSubmit}>
          {error && (
            <div className="bg-red-900/40 border border-red-700 text-red-300 px-4 py-3 rounded">
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
                className="appearance-none rounded-md relative block w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-400 text-gray-100 focus:outline-none focus:ring-blue-500 focus:border-blue-500 focus:z-10 sm:text-sm"
                placeholder={ldapEnabled ? 'Network username' : 'Username'}
                value={username}
                onChange={(e) => setUsername(e.target.value)}
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
                className="appearance-none rounded-md relative block w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-400 text-gray-100 focus:outline-none focus:ring-blue-500 focus:border-blue-500 focus:z-10 sm:text-sm"
                placeholder={ldapEnabled ? 'Network password' : 'Password'}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </div>
          </div>
          <button
            type="submit"
            disabled={loading}
            className="w-full flex justify-center py-2 px-4 border border-transparent text-sm font-medium rounded-md text-white bg-blue-600 hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-offset-gray-800 focus:ring-blue-500 disabled:opacity-50"
          >
            {loading ? 'Signing in...' : 'Sign in'}
          </button>
        </form>
      </div>
    </div>
  );
}
