import { useId } from 'react';
import { clientSession } from '../session';
import { useState, useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api, type User, type CredentialSummary, type StoreCredentialRequest } from '../api';

// ---------------------------------------------------------------------------
// Credential Card
// ---------------------------------------------------------------------------

interface CredentialCardProps {
  title: string;
  service: string;
  userId: string;
  existing: CredentialSummary | undefined;
  showUsername: boolean;
  onSaved: () => void;
}

function CredentialCard({ title, service, userId, existing, showUsername, onSaved }: CredentialCardProps) {
  const fieldId = useId();
  const [serverUrl, setServerUrl] = useState('');
  const [username, setUsername] = useState('');
  const [value, setValue] = useState('');
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [error, setError] = useState('');

  useEffect(() => {
    if (existing) {
      setServerUrl(existing.server_url);
      setUsername(existing.username);
    }
  }, [existing]);

  const handleSave = async () => {
    setError('');
    setSaving(true);
    try {
      const data: StoreCredentialRequest = {
        service,
        server_url: serverUrl,
        username: showUsername ? username : '',
        value,
      };
      await api.storeCredential(userId, data);
      setValue('');
      onSaved();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save credential');
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!existing) return;
    setError('');
    setDeleting(true);
    try {
      await api.deleteCredential(userId, existing.id);
      setServerUrl('');
      setUsername('');
      setValue('');
      onSaved();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete credential');
    } finally {
      setDeleting(false);
    }
  };

  const handleTest = async () => {
    if (!existing) return;
    setTestResult(null);
    setTesting(true);
    try {
      const result = await api.testCredential(userId, existing.id);
      setTestResult(result);
    } catch (err) {
      setTestResult({ ok: false, message: err instanceof Error ? err.message : 'Test failed' });
    } finally {
      setTesting(false);
    }
  };

  const valueLabel = service === 'svn_password' ? 'Password' : 'Token';

  return (
    <div className="bg-gray-800 rounded-lg border border-gray-700 p-5">
      <div className="flex items-center justify-between mb-4">
        <h3 className="text-lg font-semibold text-gray-100">{title}</h3>
        <span
          className={`text-xs font-medium px-2 py-1 rounded ${
            existing
              ? 'bg-green-900/50 text-green-400 border border-green-700'
              : 'bg-gray-700 text-gray-200 border border-gray-600'
          }`}
        >
          {existing ? 'Configured' : 'Not configured'}
        </span>
      </div>

      {error && (
        <div className="mb-3 bg-red-900/40 border border-red-700 text-red-300 px-3 py-2 rounded text-sm" id={`${fieldId}-error`} role="alert">
          {error}
        </div>
      )}

      <div className="space-y-3">
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-1`}>Server URL</label>
          <input
            type="text"
            value={serverUrl}
            onChange={(e) => setServerUrl(e.target.value)}
            placeholder="https://svn.example.com or https://github.com"
            className="w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-500 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-1`} aria-describedby={error ? `${fieldId}-error` : undefined}
          />
        </div>

        {showUsername && (
          <div>
            <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-2`}>Username</label>
            <input
              type="text"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder="Username"
              className="w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-500 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-2`} aria-describedby={error ? `${fieldId}-error` : undefined}
            />
          </div>
        )}

        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-3`}>
            {valueLabel} {existing && '(leave blank to keep current)'}
          </label>
          <input
            type="password"
            value={value}
            onChange={(e) => setValue(e.target.value)}
            placeholder={existing ? `Current ${valueLabel.toLowerCase()} is set` : `Enter ${valueLabel.toLowerCase()}`}
            className="w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-500 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-3`} aria-describedby={error ? `${fieldId}-error` : undefined}
          />
        </div>
      </div>

      {testResult && (
        <div
          className={`mt-3 px-3 py-2 rounded text-sm border ${
            testResult.ok
              ? 'bg-green-900/40 border-green-700 text-green-300'
              : 'bg-red-900/40 border-red-700 text-red-300'
          }`}
        >
          {testResult.message}
        </div>
      )}

      <div className="mt-4 flex items-center gap-2">
        <button
          onClick={handleSave}
          disabled={saving || !serverUrl}
          className="px-4 py-2 text-sm font-medium rounded-md text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-blue-500"
        >
          {saving ? 'Saving...' : 'Save'}
        </button>
        {existing && (
          <>
            <button
              onClick={handleTest}
              disabled={testing}
              className="px-4 py-2 text-sm font-medium rounded-md text-gray-100 bg-gray-600 hover:bg-gray-500 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-gray-400"
            >
              {testing ? 'Testing...' : 'Test Connection'}
            </button>
            <button
              onClick={handleDelete}
              disabled={deleting}
              className="px-4 py-2 text-sm font-medium rounded-md text-red-300 bg-red-900/40 hover:bg-red-900/60 border border-red-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-red-500"
            >
              {deleting ? 'Deleting...' : 'Delete'}
            </button>
          </>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Profile Section
// ---------------------------------------------------------------------------

function ProfileSection({ user }: { user: User }) {
  const fieldId = useId();
  const [displayName, setDisplayName] = useState(user.display_name);
  const [email, setEmail] = useState(user.email);
  const [currentPassword, setCurrentPassword] = useState('');
  const [newPassword, setNewPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [saving, setSaving] = useState(false);
  const [changingPassword, setChangingPassword] = useState(false);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');

  const isLdap = user.role === 'ldap';

  const handleSaveProfile = async () => {
    const session = clientSession(); const origin = session.snapshot();
    setSaving(true);
    setError('');
    setMessage('');
    try {
      const updated = await api.updateUser(user.id, { display_name: displayName, email });
      session.updateUser(updated, origin);
      setMessage('Profile updated successfully');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to update profile');
    } finally {
      setSaving(false);
    }
  };

  const handleChangePassword = async () => {
    if (!currentPassword) {
      setError('Current password is required');
      return;
    }
    if (newPassword !== confirmPassword) {
      setError('Passwords do not match');
      return;
    }
    if (newPassword.length < 8) {
      setError('Password must be at least 8 characters');
      return;
    }
    setChangingPassword(true);
    setError('');
    setMessage('');
    try {
      await api.updateUser(user.id, { password: newPassword, current_password: currentPassword });
      setCurrentPassword('');
      setNewPassword('');
      setConfirmPassword('');
      setMessage('Password changed successfully');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to change password');
    } finally {
      setChangingPassword(false);
    }
  };

  return (
    <div className="bg-gray-800 rounded-lg border border-gray-700 p-6">
      <h2 className="text-xl font-semibold text-gray-100 mb-6">My Profile</h2>

      {message && (
        <div className="mb-4 bg-green-900/40 border border-green-700 text-green-300 px-4 py-3 rounded text-sm">
          {message}
        </div>
      )}
      {error && (
        <div className="mb-4 bg-red-900/40 border border-red-700 text-red-300 px-4 py-3 rounded text-sm" id={`${fieldId}-error`} role="alert">
          {error}
        </div>
      )}

      <div className="grid grid-cols-1 md:grid-cols-2 gap-4 mb-6">
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-4`}>Username</label>
          <input
            type="text"
            value={user.username}
            disabled
            className="w-full px-3 py-2 border border-gray-600 bg-gray-900 text-gray-300 rounded-md sm:text-sm cursor-not-allowed" id={`${fieldId}-4`} aria-describedby={error ? `${fieldId}-error` : undefined}
          />
        </div>
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-5`}>Role</label>
          <input
            type="text"
            value={user.role}
            disabled
            className="w-full px-3 py-2 border border-gray-600 bg-gray-900 text-gray-300 rounded-md sm:text-sm cursor-not-allowed" id={`${fieldId}-5`} aria-describedby={error ? `${fieldId}-error` : undefined}
          />
        </div>
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-6`}>Display Name</label>
          <input
            type="text"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            disabled={isLdap}
            className={`w-full px-3 py-2 border border-gray-600 rounded-md sm:text-sm ${
              isLdap
                ? 'bg-gray-900 text-gray-300 cursor-not-allowed'
                : 'bg-gray-700 text-gray-100 focus:outline-none focus:ring-blue-500 focus:border-blue-500'
            }`} id={`${fieldId}-6`} aria-describedby={error ? `${fieldId}-error` : undefined}
          />
        </div>
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-7`}>Email</label>
          <input
            type="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            disabled={isLdap}
            className={`w-full px-3 py-2 border border-gray-600 rounded-md sm:text-sm ${
              isLdap
                ? 'bg-gray-900 text-gray-300 cursor-not-allowed'
                : 'bg-gray-700 text-gray-100 focus:outline-none focus:ring-blue-500 focus:border-blue-500'
            }`} id={`${fieldId}-7`} aria-describedby={error ? `${fieldId}-error` : undefined}
          />
        </div>
      </div>

      {!isLdap && (
        <button
          onClick={handleSaveProfile}
          disabled={saving}
          className="px-4 py-2 text-sm font-medium rounded-md text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-blue-500"
        >
          {saving ? 'Saving...' : 'Save Profile'}
        </button>
      )}

      {/* Change Password - local users only */}
      {!isLdap && (
        <div className="mt-8 pt-6 border-t border-gray-700">
          <h3 className="text-lg font-semibold text-gray-100 mb-4">Change Password</h3>
          <div className="grid grid-cols-1 md:grid-cols-3 gap-4 mb-4">
            <div>
              <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-8`}>Current Password</label>
              <input
                type="password"
                value={currentPassword}
                onChange={(e) => setCurrentPassword(e.target.value)}
                className="w-full px-3 py-2 border border-gray-600 bg-gray-700 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-8`} aria-describedby={error ? `${fieldId}-error` : undefined}
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-9`}>New Password</label>
              <input
                type="password"
                value={newPassword}
                onChange={(e) => setNewPassword(e.target.value)}
                className="w-full px-3 py-2 border border-gray-600 bg-gray-700 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-9`} aria-describedby={error ? `${fieldId}-error` : undefined}
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-10`}>Confirm Password</label>
              <input
                type="password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                className="w-full px-3 py-2 border border-gray-600 bg-gray-700 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-10`} aria-describedby={error ? `${fieldId}-error` : undefined}
              />
            </div>
          </div>
          <button
            onClick={handleChangePassword}
            disabled={changingPassword || !newPassword || !confirmPassword}
            className="px-4 py-2 text-sm font-medium rounded-md text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-blue-500"
          >
            {changingPassword ? 'Changing...' : 'Change Password'}
          </button>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main Settings Page
// ---------------------------------------------------------------------------

export default function UserSettings() {
  const [user, setUser] = useState<User | null>(null);
  const queryClient = useQueryClient();

  useEffect(() => {
    const stored = localStorage.getItem('user');
    if (stored) {
      try {
        setUser(JSON.parse(stored));
      } catch {
        // ignore parse errors
      }
    }
  }, []);

  // Try to fetch user from API if not in localStorage
  const { data: meData } = useQuery({
    queryKey: ['me'],
    queryFn: api.getMe,
    enabled: !user,
    retry: false,
  });

  const currentUser = user || meData;

  const { data: credentials, isLoading: credsLoading } = useQuery({
    queryKey: ['credentials', currentUser?.id],
    queryFn: () => api.getUserCredentials(currentUser!.id),
    enabled: !!currentUser?.id,
    retry: false,
  });

  const handleCredentialSaved = () => {
    if (currentUser) {
      queryClient.invalidateQueries({ queryKey: ['credentials', currentUser.id] });
    }
  };

  if (!currentUser) {
    return (
      <div className="text-center py-12 text-gray-400">
        <p>Unable to load user information. Please log in again.</p>
      </div>
    );
  }

  const findCredential = (service: string) =>
    credentials?.find((c) => c.service === service);

  return (
    <div className="max-w-4xl mx-auto space-y-8">
      <div>
        <h1 className="text-2xl font-bold text-gray-100">Settings</h1>
        <p className="mt-1 text-gray-400">Manage your profile and credentials</p>
      </div>

      {/* Profile */}
      <ProfileSection user={currentUser} />

      {/* Credentials */}
      <div>
        <h2 className="text-xl font-semibold text-gray-100 mb-4">My Credentials</h2>
        {credsLoading ? (
          <div className="text-center py-8 text-gray-400">Loading credentials...</div>
        ) : (
          <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
            <CredentialCard
              title="SVN Password"
              service="svn_password"
              userId={currentUser.id}
              existing={findCredential('svn_password')}
              showUsername={true}
              onSaved={handleCredentialSaved}
            />
            <CredentialCard
              title="SVN Token"
              service="svn_token"
              userId={currentUser.id}
              existing={findCredential('svn_token')}
              showUsername={false}
              onSaved={handleCredentialSaved}
            />
            <CredentialCard
              title="Git Token"
              service="git_token"
              userId={currentUser.id}
              existing={findCredential('git_token')}
              showUsername={false}
              onSaved={handleCredentialSaved}
            />
          </div>
        )}
      </div>
    </div>
  );
}
