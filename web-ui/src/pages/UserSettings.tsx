import { useId } from 'react';
import { clientSession } from '../session';
import { useState, useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Check, Minus } from 'lucide-react';
import { api, type User, type CredentialSummary, type StoreCredentialRequest } from '../api';
import { ConfirmModal } from '../components/ConfirmModal';
import { useText } from '../layouts/vocabulary';

// ---------------------------------------------------------------------------
// Credential Card
// ---------------------------------------------------------------------------

interface CredentialCardProps {
  title: string;
  service: string;
  userId: string;
  existing: CredentialSummary | undefined;
  showUsername: boolean;
  onSaved: () => Promise<unknown> | void;
}

function CredentialCard({ title, service, userId, existing, showUsername, onSaved }: CredentialCardProps) {
  const [serverUrl, setServerUrl] = useState('');
  const [username, setUsername] = useState('');
  const [value, setValue] = useState('');
  const [replacing, setReplacing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState('');
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

  useEffect(() => {
    if (existing) {
      setServerUrl(existing.server_url);
      setUsername(existing.username);
    }
  }, [existing]);

  const handleSave = async () => {
    // A blank value is never sent: replacing a saved secret needs a new one.
    if (saving || !serverUrl || !value.trim()) return;
    setError('');
    setNotice('');
    setSaving(true);
    try {
      const data: StoreCredentialRequest = {
        service,
        server_url: serverUrl,
        username: showUsername ? username : '',
        value,
      };
      await api.storeCredential(userId, data);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save credential');
      setSaving(false);
      return;
    }
    setValue('');
    setReplacing(false);
    setNotice('Saved');
    try {
      await onSaved();
    } finally {
      setSaving(false);
    }
  };

  const startReplace = () => {
    setError('');
    setNotice('');
    setTestResult(null);
    setValue('');
    setReplacing(true);
  };

  const cancelReplace = () => {
    setError('');
    setValue('');
    setReplacing(false);
    if (existing) {
      setServerUrl(existing.server_url);
      setUsername(existing.username);
    }
  };

  const askDelete = () => {
    setDeleteError('');
    setNotice('');
    setConfirmingDelete(true);
  };

  const handleDelete = async () => {
    if (!existing) return;
    setError('');
    setDeleteError('');
    setDeleting(true);
    try {
      await api.deleteCredential(userId, existing.id);
    } catch (err) {
      setDeleteError(err instanceof Error ? err.message : 'Failed to delete credential');
      setDeleting(false);
      return;
    }
    setServerUrl('');
    setUsername('');
    setValue('');
    setReplacing(false);
    setTestResult(null);
    try {
      await onSaved();
    } finally {
      setDeleting(false);
      setConfirmingDelete(false);
      setNotice('Deleted');
    }
  };

  const handleTest = async () => {
    if (!existing) return;
    setTestResult(null);
    setNotice('');
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

  return (
    <CredentialCardView
      title={title}
      service={service}
      existing={existing}
      showUsername={showUsername}
      serverUrl={serverUrl}
      username={username}
      value={value}
      replacing={replacing}
      saving={saving}
      deleting={deleting}
      testing={testing}
      confirmingDelete={confirmingDelete}
      testResult={testResult}
      error={error}
      deleteError={deleteError}
      notice={notice}
      onServerUrl={setServerUrl}
      onUsername={setUsername}
      onValue={setValue}
      onSave={handleSave}
      onReplace={startReplace}
      onCancelReplace={cancelReplace}
      onTest={handleTest}
      onAskDelete={askDelete}
      onConfirmDelete={handleDelete}
      onCancelDelete={() => setConfirmingDelete(false)}
    />
  );
}

/** S23: a saved secret's state is a word and a mark, never a placeholder. */
export function CredentialStateMark({ configured }: { configured: boolean }) {
  return (
    <span
      className={`rs-mark inline-flex items-center gap-1 text-xs font-medium px-2 py-1 rounded ${
        configured
          ? 'bg-green-900/50 text-green-400 border border-green-700'
          : 'bg-gray-700 text-gray-200 border border-gray-600'
      }`}
    >
      {configured ? <Check className="h-3 w-3" aria-hidden="true" /> : <Minus className="h-3 w-3" aria-hidden="true" />}
      {configured ? 'Configured' : 'Not configured'}
    </span>
  );
}

export interface CredentialCardViewProps {
  title: string;
  service: string;
  existing: CredentialSummary | undefined;
  showUsername: boolean;
  serverUrl: string;
  username: string;
  value: string;
  replacing: boolean;
  saving: boolean;
  deleting: boolean;
  testing: boolean;
  confirmingDelete: boolean;
  testResult: { ok: boolean; message: string } | null;
  error: string;
  deleteError: string;
  notice: string;
  onServerUrl: (value: string) => void;
  onUsername: (value: string) => void;
  onValue: (value: string) => void;
  onSave: () => void;
  onReplace: () => void;
  onCancelReplace: () => void;
  onTest: () => void;
  onAskDelete: () => void;
  onConfirmDelete: () => void;
  onCancelDelete: () => void;
}

/**
 * G08.4: a saved secret is never shown and never edited in place. Replace opens
 * the fields with the secret blank, and Save stays unavailable until it is filled.
 * Delete asks for confirmation first.
 */
export function CredentialCardView(props: CredentialCardViewProps) {
  const { title, service, existing, showUsername, serverUrl, username, value, replacing, saving, deleting, testing, error } = props;
  const fieldId = useId();
  const valueLabel = service === 'svn_password' ? 'Password' : 'Token';
  const noun = valueLabel.toLowerCase();
  const formOpen = !existing || replacing;
  const saveUnavailable = saving || !serverUrl || !value.trim();
  const testFailed = props.testResult ? props.testResult.ok !== true : false;

  return (
    <div className="rs-sec bg-gray-800 rounded-lg border border-gray-700 p-5">
      <div className="flex items-center justify-between mb-4">
        <h3 className="rs-heading text-lg font-semibold text-gray-100">{title}</h3>
        <CredentialStateMark configured={!!existing} />
      </div>

      {error && (
        <div className="rs-bn rs-bn--danger mb-3 bg-red-900/40 border border-red-700 text-red-300 px-3 py-2 rounded text-sm" id={`${fieldId}-error`} role="alert">
          {error}
        </div>
      )}

      {formOpen ? (
        <div className="space-y-3">
          <div>
            <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-1`}>Server URL</label>
            <input
              type="text"
              value={serverUrl}
              onChange={(e) => props.onServerUrl(e.target.value)}
              placeholder="https://svn.example.com or https://github.com"
              className="rs-in w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-500 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-1`} aria-describedby={error ? `${fieldId}-error` : undefined}
            />
          </div>

          {showUsername && (
            <div>
              <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-2`}>Username</label>
              <input
                type="text"
                value={username}
                onChange={(e) => props.onUsername(e.target.value)}
                placeholder="Username"
                className="rs-in w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-500 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-2`} aria-describedby={error ? `${fieldId}-error` : undefined}
              />
            </div>
          )}

          <div>
            <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-3`}>
              {valueLabel}
            </label>
            <input
              type="password"
              value={value}
              onChange={(e) => props.onValue(e.target.value)}
              placeholder={`Enter ${noun}`}
              className="rs-in w-full px-3 py-2 border border-gray-600 bg-gray-700 placeholder-gray-500 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-3`} aria-describedby={[error ? `${fieldId}-error` : '', existing ? `${fieldId}-3-help` : ''].filter(Boolean).join(' ') || undefined}
            />
            {existing && (
              <p id={`${fieldId}-3-help`} className="mt-1 text-xs text-gray-400">
                Enter the new {noun}. Save stays unavailable while this field is blank.
              </p>
            )}
          </div>
        </div>
      ) : (
        <dl className="space-y-3">
          <div>
            <dt className="block text-sm font-medium text-gray-400 mb-1">Server URL</dt>
            <dd className="text-sm text-gray-100 font-mono break-all">{existing.server_url}</dd>
          </div>
          {showUsername && (
            <div>
              <dt className="block text-sm font-medium text-gray-400 mb-1">Username</dt>
              <dd className="text-sm text-gray-100 break-all">{existing.username}</dd>
            </div>
          )}
        </dl>
      )}

      {props.testResult && (
        <div
          role={testFailed ? 'alert' : 'status'}
          className={`rs-bn ${testFailed ? 'rs-bn--danger' : 'rs-bn--ok'} mt-3 px-3 py-2 rounded text-sm border ${
            testFailed
              ? 'bg-red-900/40 border-red-700 text-red-300'
              : 'bg-green-900/40 border-green-700 text-green-300'
          }`}
        >
          {props.testResult.message}
        </div>
      )}

      <p role="status" className={props.notice ? 'mt-3 text-sm text-green-400' : undefined}>{props.notice}</p>

      <div className="mt-4 flex flex-wrap items-center gap-2">
        {formOpen ? (
          <>
            <button
              type="button"
              onClick={props.onSave}
              disabled={saveUnavailable}
              className="rs-btn rs-btn--primary px-4 py-2 text-sm font-medium rounded-md rs-text-on-accent bg-blue-600 hover:bg-blue-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-blue-500"
            >
              {saving ? 'Saving...' : 'Save'}
            </button>
            {existing && (
              <button
                type="button"
                onClick={props.onCancelReplace}
                disabled={saving}
                className="rs-btn rs-btn--quiet px-4 py-2 text-sm font-medium rounded-md text-gray-100 bg-gray-600 hover:bg-gray-500 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-gray-400"
              >
                Cancel
              </button>
            )}
          </>
        ) : (
          <>
            <button
              type="button"
              onClick={props.onReplace}
              className="rs-btn rs-btn--quiet px-4 py-2 text-sm font-medium rounded-md text-gray-100 bg-gray-600 hover:bg-gray-500 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-gray-400"
            >
              Replace
            </button>
            <button
              type="button"
              onClick={props.onTest}
              disabled={testing}
              className="px-4 py-2 text-sm font-medium rounded-md text-gray-100 bg-gray-600 hover:bg-gray-500 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-gray-400"
            >
              {testing ? 'Testing...' : 'Test Connection'}
            </button>
            <button
              type="button"
              onClick={props.onAskDelete}
              disabled={deleting}
              className="rs-btn rs-btn--quiet px-4 py-2 text-sm font-medium rounded-md text-red-300 bg-red-900/40 hover:bg-red-900/60 border border-red-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-red-500"
            >
              {deleting ? 'Deleting...' : 'Delete'}
            </button>
          </>
        )}
      </div>

      {existing && (
        <ConfirmModal
          isOpen={props.confirmingDelete}
          title={`Delete your ${title}?`}
          message={`The saved ${noun} for ${existing.server_url} will be deleted. This cannot be undone.`}
          confirmLabel={`Delete ${noun}`}
          cancelLabel="Cancel"
          variant="danger"
          busy={deleting}
          error={props.deleteError || undefined}
          onConfirm={props.onConfirmDelete}
          onCancel={props.onCancelDelete}
        />
      )}
    </div>
  );
}

/** A failed credential read is shown as failed, never as "Not configured". */
export function CredentialsReadFailure({ message, stale, onRetry }: { message: string; stale: boolean; onRetry: () => void }) {
  return (
    <div role="alert" className="rs-bn rs-bn--danger mb-4 bg-red-900/40 border border-red-700 text-red-300 px-4 py-3 rounded text-sm space-y-1">
      <p className="font-medium">Credentials could not be loaded</p>
      <p>{message}</p>
      <p>
        {stale
          ? 'The cards below show the last state that loaded, which may be out of date.'
          : 'Their state is unknown, so the cards are not shown.'}
      </p>
      <button
        type="button"
        onClick={onRetry}
        className="rs-btn rs-btn--quiet mt-1 px-3 py-1 text-sm font-medium rounded-md text-gray-100 bg-gray-600 hover:bg-gray-500 focus:outline-none focus:ring-2 focus:ring-gray-400"
      >
        Retry
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Profile Section
// ---------------------------------------------------------------------------

function ProfileSection({ user }: { user: User }) {
  const t = useText();
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
    <div className="rs-sec bg-gray-800 rounded-lg border border-gray-700 p-6">
      <h2 className="rs-heading text-xl font-semibold text-gray-100 mb-6">{t('account.profile')}</h2>

      {message && (
        <div className="rs-bn rs-bn--ok mb-4 bg-green-900/40 border border-green-700 text-green-300 px-4 py-3 rounded text-sm">
          {message}
        </div>
      )}
      {error && (
        <div className="rs-bn rs-bn--danger mb-4 bg-red-900/40 border border-red-700 text-red-300 px-4 py-3 rounded text-sm" id={`${fieldId}-error`} role="alert">
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
            className="rs-in w-full px-3 py-2 border border-gray-600 bg-gray-900 text-gray-300 rounded-md sm:text-sm cursor-not-allowed" id={`${fieldId}-4`} aria-describedby={error ? `${fieldId}-error` : undefined}
          />
        </div>
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-5`}>Role</label>
          <input
            type="text"
            value={user.role}
            disabled
            className="rs-in w-full px-3 py-2 border border-gray-600 bg-gray-900 text-gray-300 rounded-md sm:text-sm cursor-not-allowed" id={`${fieldId}-5`} aria-describedby={error ? `${fieldId}-error` : undefined}
          />
        </div>
        <div>
          <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-6`}>Display Name</label>
          <input
            type="text"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            disabled={isLdap}
            className={`rs-in w-full px-3 py-2 border border-gray-600 rounded-md sm:text-sm ${
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
            className={`rs-in w-full px-3 py-2 border border-gray-600 rounded-md sm:text-sm ${
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
          className="rs-btn rs-btn--primary px-4 py-2 text-sm font-medium rounded-md rs-text-on-accent bg-blue-600 hover:bg-blue-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-blue-500"
        >
          {saving ? 'Saving...' : 'Save Profile'}
        </button>
      )}

      {/* Change Password - local users only */}
      {!isLdap && (
        <div className="mt-8 pt-6 border-t border-gray-700">
          <h3 className="rs-heading text-lg font-semibold text-gray-100 mb-4">{t('account.changePassword')}</h3>
          <div className="grid grid-cols-1 md:grid-cols-3 gap-4 mb-4">
            <div>
              <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-8`}>Current Password</label>
              <input
                type="password"
                value={currentPassword}
                onChange={(e) => setCurrentPassword(e.target.value)}
                className="rs-in w-full px-3 py-2 border border-gray-600 bg-gray-700 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-8`} aria-describedby={error ? `${fieldId}-error` : undefined}
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-9`}>New Password</label>
              <input
                type="password"
                value={newPassword}
                onChange={(e) => setNewPassword(e.target.value)}
                className="rs-in w-full px-3 py-2 border border-gray-600 bg-gray-700 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-9`} aria-describedby={error ? `${fieldId}-error` : undefined}
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-gray-400 mb-1" htmlFor={`${fieldId}-10`}>Confirm Password</label>
              <input
                type="password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                className="rs-in w-full px-3 py-2 border border-gray-600 bg-gray-700 text-gray-100 rounded-md focus:outline-none focus:ring-blue-500 focus:border-blue-500 sm:text-sm" id={`${fieldId}-10`} aria-describedby={error ? `${fieldId}-error` : undefined}
              />
            </div>
          </div>
          <button
            onClick={handleChangePassword}
            disabled={changingPassword || !newPassword || !confirmPassword}
            className="rs-btn rs-btn--primary px-4 py-2 text-sm font-medium rounded-md rs-text-on-accent bg-blue-600 hover:bg-blue-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-blue-500"
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
  const t = useText();
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

  const { data: credentials, isPending: credsLoading, error: credsError, refetch: refetchCredentials } = useQuery({
    queryKey: ['credentials', currentUser?.id],
    queryFn: () => api.getUserCredentials(currentUser!.id),
    enabled: !!currentUser?.id,
    retry: false,
  });

  const handleCredentialSaved = () => {
    if (currentUser) {
      return queryClient.invalidateQueries({ queryKey: ['credentials', currentUser.id] });
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
        <h1 className="rs-title text-2xl font-bold text-gray-100">{t('account.title')}</h1>
        <p className="mt-1 text-gray-400">Manage your profile and credentials</p>
      </div>

      {/* Profile */}
      <ProfileSection user={currentUser} />

      {/* Credentials */}
      <div>
        <h2 className="rs-heading text-xl font-semibold text-gray-100 mb-4">{t('account.credentials')}</h2>
        {credsLoading ? (
          <div className="text-center py-8 text-gray-400" role="status">Loading credentials...</div>
        ) : (
          <>
            {credsError && (
              <CredentialsReadFailure
                message={credsError.message}
                stale={credentials !== undefined}
                onRetry={() => void refetchCredentials()}
              />
            )}
            {credentials !== undefined && (
              <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
                <CredentialCard
                  title={t('account.credential.svnPassword')}
                  service="svn_password"
                  userId={currentUser.id}
                  existing={findCredential('svn_password')}
                  showUsername={true}
                  onSaved={handleCredentialSaved}
                />
                <CredentialCard
                  title={t('account.credential.svnToken')}
                  service="svn_token"
                  userId={currentUser.id}
                  existing={findCredential('svn_token')}
                  showUsername={false}
                  onSaved={handleCredentialSaved}
                />
                <CredentialCard
                  title={t('account.credential.gitToken')}
                  service="git_token"
                  userId={currentUser.id}
                  existing={findCredential('git_token')}
                  showUsername={false}
                  onSaved={handleCredentialSaved}
                />
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
