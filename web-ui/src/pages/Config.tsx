import { useViewMutation as useMutation } from '../useViewMutation';
import { sessionFetch } from '../session';
import React, { useEffect, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Check, Minus } from 'lucide-react';
import { api, type AuthorMapping } from '../api';
import { ConfirmModal } from '../components/ConfirmModal';
import { useText } from '../layouts/vocabulary';

const EMPTY_MAPPING: AuthorMapping = { svn_username: '', name: '', email: '' };

export default function Config() {
  const t = useText();
  const queryClient = useQueryClient();
  const { data: mappings, isLoading, error: readError, refetch, dataUpdatedAt, errorUpdatedAt } = useQuery({
    queryKey: ['identityMappings'],
    queryFn: api.getIdentityMappings,
  });

  const [newMapping, setNewMapping] = useState<AuthorMapping>(EMPTY_MAPPING);
  const [confirmRemove, setConfirmRemove] = useState<AuthorMapping | null>(null);

  const updateMutation = useMutation({
    mutationFn: (updated: AuthorMapping[]) =>
      api.updateIdentityMappings(updated),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ['identityMappings'] }),
  });

  // G11.1: every edit sends the whole list, so no edit is built from a list
  // that did not load. Editing stays off until a read succeeds.
  const listKnown = !readError && mappings !== undefined;

  // A removal is never confirmed against a list that stopped being known.
  useEffect(() => {
    if (!listKnown) setConfirmRemove(null);
  }, [listKnown]);

  const addMapping = () => {
    if (!listKnown || !mappings) return;
    if (!newMapping.svn_username || !newMapping.name || !newMapping.email) return;
    const updated = [...mappings, newMapping];
    updateMutation.mutate(updated, { onSuccess: () => setNewMapping(EMPTY_MAPPING) });
  };

  const askRemove = (mapping: AuthorMapping) => {
    updateMutation.reset();
    setConfirmRemove(mapping);
  };

  const removeMapping = (svnUsername: string) => {
    if (!listKnown || !mappings) return;
    const updated = mappings.filter(
      (m) => m.svn_username !== svnUsername
    );
    updateMutation.mutate(updated, { onSuccess: () => setConfirmRemove(null) });
  };

  // The first read keeps the page-level loading line. A retry after a failed
  // read loads inside the section, so the rest of the page stays mounted.
  if (isLoading && dataUpdatedAt === 0 && errorUpdatedAt === 0) {
    return <div className="text-center py-8 text-gray-300">Loading...</div>;
  }

  return (
    <div className="space-y-6">
      <h1 className="rs-title text-2xl font-bold text-gray-100">{t('config.title')}</h1>

      {/* Identity Mappings */}
      <IdentityMappingsView
        mappings={listKnown ? mappings : undefined}
        loading={isLoading}
        readError={readError ? readError.message : ''}
        saving={updateMutation.isPending}
        saveError={!confirmRemove && updateMutation.error ? updateMutation.error.message : ''}
        newMapping={newMapping}
        onNewMapping={setNewMapping}
        onAdd={addMapping}
        onRemove={askRemove}
        onRetry={() => void refetch()}
      />
      <RemoveMappingConfirm
        mapping={confirmRemove}
        busy={updateMutation.isPending}
        error={confirmRemove && updateMutation.error ? updateMutation.error.message : ''}
        onConfirm={() => {
          if (confirmRemove) removeMapping(confirmRemove.svn_username);
        }}
        onCancel={() => setConfirmRemove(null)}
      />

      {/* Notifications Section */}
      <NotificationsConfig />
    </div>
  );
}

/** G11.1: Remove asks first; nothing is sent until the removal is confirmed. */
export function RemoveMappingConfirm({ mapping, busy, error, onConfirm, onCancel }: {
  mapping: AuthorMapping | null;
  busy: boolean;
  error: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <ConfirmModal
      isOpen={!!mapping}
      title="Remove this mapping?"
      message={mapping ? `SVN username ${mapping.svn_username}, Git author ${mapping.name} <${mapping.email}>.` : ''}
      confirmLabel="Remove mapping"
      cancelLabel="Cancel"
      variant="danger"
      busy={busy}
      error={error || undefined}
      onConfirm={onConfirm}
      onCancel={onCancel}
    />
  );
}

export interface IdentityMappingsViewProps {
  /** undefined when the list is not known: still loading, or the latest read failed. */
  mappings: AuthorMapping[] | undefined;
  loading: boolean;
  readError: string;
  saving: boolean;
  saveError: string;
  newMapping: AuthorMapping;
  onNewMapping: (mapping: AuthorMapping) => void;
  onAdd: () => void;
  onRemove: (mapping: AuthorMapping) => void;
  onRetry: () => void;
}

export function IdentityMappingsView({ mappings, loading, readError, saving, saveError, newMapping, onNewMapping, onAdd, onRemove, onRetry }: IdentityMappingsViewProps) {
  const t = useText();
  const editingOff = mappings === undefined;
  return (
    <div className="rs-sec bg-gray-800 shadow rounded-lg p-6 border border-gray-700">
      <h2 className="rs-heading text-lg font-semibold text-gray-100 mb-4">
        {t('config.identity')}
      </h2>
      <p className="text-sm text-gray-400 mb-4">
        Map SVN usernames to Git author identities. These mappings ensure
        commits are attributed to the correct developer on both sides.
      </p>

      {loading && (
        <p role="status" className="mb-4 text-sm text-gray-300">Loading...</p>
      )}

      {readError && (
        <div role="alert" className="rs-bn rs-bn--danger mb-4 bg-red-900/40 border border-red-700 text-red-300 px-4 py-3 rounded text-sm space-y-1">
          <p className="font-medium">Mappings could not be loaded</p>
          <p>{readError}</p>
          <p>They are unknown, so editing is turned off. Saving now would replace the whole list.</p>
          <button
            type="button"
            onClick={onRetry}
            className="rs-btn rs-btn--quiet mt-1 px-3 py-1 border border-gray-600 text-gray-300 rounded-md hover:border-blue-500 rs-hover-text-strong text-sm font-medium"
          >
            Retry
          </button>
        </div>
      )}

      {saveError && (
        <div role="alert" className="rs-bn rs-bn--danger mb-4 bg-red-900/40 border border-red-700 text-red-300 px-4 py-3 rounded text-sm space-y-1">
          <p className="font-medium">The mappings were not saved</p>
          <p>{saveError}</p>
        </div>
      )}

      {/* Existing Mappings */}
      {mappings !== undefined && (
        <div className="overflow-x-auto mb-4" role="region" aria-label="Author identity mappings" tabIndex={0}>
        <table className="rs-tbl min-w-full divide-y divide-gray-700">
          <thead className="bg-gray-700/50">
            <tr>
              <th className="px-4 py-2 text-left text-xs font-medium text-gray-400 uppercase">
                SVN Username
              </th>
              <th className="px-4 py-2 text-left text-xs font-medium text-gray-400 uppercase">
                Git Name
              </th>
              <th className="px-4 py-2 text-left text-xs font-medium text-gray-400 uppercase">
                Git Email
              </th>
              <th className="px-4 py-2 text-left text-xs font-medium text-gray-400 uppercase">
                GitHub
              </th>
              <th className="px-4 py-2 text-right text-xs font-medium text-gray-400 uppercase">
                Action
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-gray-700">
            {mappings.length === 0 && (
              <tr>
                <td colSpan={5} className="px-4 py-3 text-sm text-gray-400">
                  No mappings yet
                </td>
              </tr>
            )}
            {mappings.map((m: AuthorMapping) => (
              <tr key={m.svn_username} className="hover:bg-gray-700/50">
                <td className="px-4 py-2 font-mono text-sm text-gray-200">
                  {m.svn_username}
                </td>
                <td className="px-4 py-2 text-sm text-gray-300">{m.name}</td>
                <td className="px-4 py-2 text-sm text-gray-300">{m.email}</td>
                <td className="px-4 py-2 text-sm text-gray-300">
                  {m.github ?? '-'}
                </td>
                <td className="px-4 py-2 text-right">
                  <button
                    type="button"
                    onClick={() => onRemove(m)}
                    disabled={saving}
                    className="text-red-400 hover:text-red-300 text-sm disabled:opacity-50"
                  >
                    Remove
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        </div>
      )}

      {/* Add New Mapping */}
      <div className="border-t border-gray-700 pt-4">
        <h3 className="rs-heading text-sm font-medium text-gray-300 mb-2">
          Add New Mapping
        </h3>
        <div className="flex space-x-2">
          <input
            aria-label="Mapping SVN username" placeholder="SVN username"
            value={newMapping.svn_username}
            disabled={editingOff}
            onChange={(e) =>
              onNewMapping({ ...newMapping, svn_username: e.target.value })
            }
            className="rs-in flex-1 rounded-md border border-gray-600 bg-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:ring-blue-500 focus:border-blue-500 disabled:opacity-50"
          />
          <input
            aria-label="Mapping full name" placeholder="Full Name"
            value={newMapping.name}
            disabled={editingOff}
            onChange={(e) =>
              onNewMapping({ ...newMapping, name: e.target.value })
            }
            className="rs-in flex-1 rounded-md border border-gray-600 bg-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:ring-blue-500 focus:border-blue-500 disabled:opacity-50"
          />
          <input
            aria-label="Mapping email" placeholder="email@company.com"
            value={newMapping.email}
            disabled={editingOff}
            onChange={(e) =>
              onNewMapping({ ...newMapping, email: e.target.value })
            }
            className="rs-in flex-1 rounded-md border border-gray-600 bg-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:ring-blue-500 focus:border-blue-500 disabled:opacity-50"
          />
          <button
            type="button"
            onClick={onAdd}
            disabled={saving || editingOff}
            className="rs-btn rs-btn--primary px-4 py-2 bg-blue-600 rs-text-on-accent rounded-md hover:bg-blue-700 disabled:opacity-50 text-sm font-medium"
          >
            Add
          </button>
        </div>
      </div>
    </div>
  );
}

/** The Teams webhook as this page draws it: a state, not an address. */
export type WebhookRead =
  | { state: 'loading' }
  | { state: 'failed'; error: string }
  | { state: 'set' }
  | { state: 'not_set' };

async function postNotifications(teamsWebhookUrl: string | null): Promise<void> {
  const token = localStorage.getItem('session_token');
  const response = await sessionFetch('/api/config/notifications', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify({ teams_webhook_url: teamsWebhookUrl }),
  });
  if (!response.ok) throw new Error(`Save failed (${response.status})`);
  await response.text();
}

function NotificationsConfig() {
  const [read, setRead] = React.useState<WebhookRead>({ state: 'loading' });
  const [teamsUrl, setTeamsUrl] = React.useState('');
  const [replacing, setReplacing] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const [testing, setTesting] = React.useState(false);
  const [message, setMessage] = React.useState<{type: 'success' | 'error'; text: string} | null>(null);
  const [showSetup, setShowSetup] = React.useState(false);
  const [confirmingRemove, setConfirmingRemove] = React.useState(false);
  const [removing, setRemoving] = React.useState(false);
  const [removeError, setRemoveError] = React.useState('');

  const load = React.useCallback(() => {
    setRead({ state: 'loading' });
    const token = localStorage.getItem('session_token');
    sessionFetch('/api/config/notifications', {
      headers: token ? { Authorization: `Bearer ${token}` } : {},
    })
      .then(async (r) => {
        if (!r.ok) throw new Error(`API error ${r.status}: ${await r.text()}`);
        return r.json();
      })
      .then((d) => {
        setRead(d.teams_webhook_url ? { state: 'set' } : { state: 'not_set' });
      })
      .catch((err) => setRead({ state: 'failed', error: err instanceof Error ? err.message : String(err) }));
  }, []);

  React.useEffect(() => {
    load();
  }, [load]);

  const save = async () => {
    // G11.2: Save never runs on an unknown setting, and never with a blank
    // field; removing the webhook is the separate, confirmed Remove step.
    if (read.state !== 'set' && read.state !== 'not_set') return;
    if (!teamsUrl.trim()) return;
    setSaving(true);
    setMessage(null);
    try {
      await postNotifications(teamsUrl || null);
      setRead({ state: 'set' });
      setReplacing(false);
      setTeamsUrl('');
      setMessage({type: 'success', text: 'Saved'});
      setTimeout(() => setMessage(null), 3000);
    } catch {
      setMessage({type: 'error', text: 'Save failed'});
    }
    setSaving(false);
  };

  const remove = async () => {
    if (read.state !== 'set') return;
    setRemoving(true);
    setRemoveError('');
    setMessage(null);
    try {
      await postNotifications(null);
      setRead({ state: 'not_set' });
      setReplacing(false);
      setTeamsUrl('');
      setConfirmingRemove(false);
      setMessage({type: 'success', text: 'Removed'});
      setTimeout(() => setMessage(null), 3000);
    } catch (err) {
      setRemoveError(err instanceof Error ? err.message : 'Save failed');
    }
    setRemoving(false);
  };

  const testNotification = async () => {
    setTesting(true);
    setMessage(null);
    const token = localStorage.getItem('session_token');
    try {
      const res = await sessionFetch('/api/config/notifications/test', {
        method: 'POST',
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      const d = await res.json();
      setMessage({type: d.ok ? 'success' : 'error', text: d.ok ? 'Test notification sent!' : d.error || 'Failed'});
    } catch {
      setMessage({type: 'error', text: 'Failed to send test'});
    }
    setTesting(false);
  };

  return (
    <NotificationsView
      read={read}
      showSetup={showSetup}
      onToggleSetup={() => setShowSetup(!showSetup)}
      teamsUrl={teamsUrl}
      onTeamsUrl={setTeamsUrl}
      replacing={replacing}
      saving={saving}
      testing={testing}
      message={message}
      confirmingRemove={confirmingRemove}
      removing={removing}
      removeError={removeError}
      onRetry={load}
      onSave={save}
      onReplace={() => {
        setMessage(null);
        setTeamsUrl('');
        setReplacing(true);
      }}
      onCancelReplace={() => {
        setTeamsUrl('');
        setReplacing(false);
      }}
      onTest={testNotification}
      onAskRemove={() => {
        setRemoveError('');
        setConfirmingRemove(true);
      }}
      onConfirmRemove={remove}
      onCancelRemove={() => setConfirmingRemove(false)}
    />
  );
}

/** S23: the webhook's state is a word and a mark. */
export function WebhookStateMark({ set }: { set: boolean }) {
  return (
    <span
      className={`rs-mark inline-flex items-center gap-1 text-xs font-medium px-2 py-0.5 rounded ${
        set
          ? 'bg-green-900/50 text-green-400 border border-green-700'
          : 'bg-gray-700 text-gray-200 border border-gray-600'
      }`}
    >
      {set ? <Check className="h-3 w-3" aria-hidden="true" /> : <Minus className="h-3 w-3" aria-hidden="true" />}
      {set ? 'Set' : 'Not set'}
    </span>
  );
}

export interface NotificationsViewProps {
  read: WebhookRead;
  showSetup: boolean;
  onToggleSetup: () => void;
  teamsUrl: string;
  onTeamsUrl: (value: string) => void;
  replacing: boolean;
  saving: boolean;
  testing: boolean;
  message: { type: 'success' | 'error'; text: string } | null;
  confirmingRemove: boolean;
  removing: boolean;
  removeError: string;
  onRetry: () => void;
  onSave: () => void;
  onReplace: () => void;
  onCancelReplace: () => void;
  onTest: () => void;
  onAskRemove: () => void;
  onConfirmRemove: () => void;
  onCancelRemove: () => void;
}

/**
 * G11.2: the webhook is shown as Set or Not set. Replace opens a blank field,
 * Remove is confirmed, and after a failed read nothing can be saved. The test
 * request carries no URL, so Test appears only in the Set state.
 */
export function NotificationsView(props: NotificationsViewProps) {
  const { read, showSetup, teamsUrl, replacing, saving, testing, message } = props;
  const t = useText();
  const feedbackId = React.useId();
  const known = read.state === 'set' || read.state === 'not_set';
  const isSet = read.state === 'set';
  const formOpen = read.state === 'not_set' || (isSet && replacing);

  return (
    <div role="group" aria-label="Microsoft Teams notification settings" aria-describedby={message ? feedbackId : undefined} className="rs-sec bg-gray-800/60 border border-gray-700 rounded-lg p-6">
      <h2 className="rs-heading text-lg font-semibold text-gray-100 mb-4">
        {t('config.notifications')}
      </h2>

      <button
        type="button"
        onClick={props.onToggleSetup}
        className="text-sm text-blue-400 hover:text-blue-300 mb-4 flex items-center gap-1"
      >
        {showSetup ? '▼' : '▶'} Setup Instructions
      </button>

      {showSetup && (
        <div className="bg-gray-900/50 border border-gray-700 rounded-lg p-4 mb-4 text-sm text-gray-300 space-y-2">
          <p className="font-medium text-gray-200">How to create a Teams Webhook:</p>
          <ol className="list-decimal list-inside space-y-1 ml-2">
            <li>Open <strong>Microsoft Teams</strong> and go to the channel where you want notifications</li>
            <li>Click the <strong>••• (More options)</strong> next to the channel name</li>
            <li>Select <strong>Workflows</strong></li>
            <li>Search for <strong>"webhook"</strong> and select <strong>"Send webhook alerts to a channel"</strong></li>
            <li>Name your workflow (e.g., "RepoSync Notifications") and click <strong>Next</strong></li>
            <li>Select the channel to post to and click <strong>Add workflow</strong></li>
            <li>Copy the <strong>webhook URL</strong> and paste it below</li>
          </ol>
          <p className="text-xs text-gray-300 mt-2">
            No admin permissions required — any team member can create a workflow on their channel.
          </p>
        </div>
      )}

      <div className="space-y-3">
        {read.state === 'loading' && (
          <p role="status" className="text-sm text-gray-300">Loading...</p>
        )}

        {read.state === 'failed' && (
          <div role="alert" className="rs-bn rs-bn--danger bg-red-900/40 border border-red-700 text-red-300 px-4 py-3 rounded text-sm space-y-1">
            <p className="font-medium">The notification setting could not be read</p>
            <p>{read.error}</p>
            <p>It is unknown, so saving is turned off and an existing webhook is not cleared.</p>
            <button
              type="button"
              onClick={props.onRetry}
              className="rs-btn rs-btn--quiet mt-1 px-3 py-1 border border-gray-600 text-gray-300 rounded-md hover:border-blue-500 rs-hover-text-strong text-sm font-medium"
            >
              Retry
            </button>
          </div>
        )}

        {known && (
          <div>
            <div className="flex items-center gap-2 mb-1">
              {formOpen ? (
                <label className="block text-sm text-gray-400" htmlFor="teams-webhook-url">Teams Webhook URL</label>
              ) : (
                <span className="block text-sm text-gray-400">Teams Webhook URL</span>
              )}
              <WebhookStateMark set={isSet} />
            </div>
            {formOpen ? (
              <div className="flex gap-2">
                <input
                  type="text"
                  id="teams-webhook-url" value={teamsUrl}
                  aria-describedby={message ? feedbackId : undefined}
                  onChange={(e) => props.onTeamsUrl(e.target.value)}
                  placeholder="https://prod-XX.westus.logic.azure.com/workflows/..."
                  className="rs-in flex-1 rounded-md border border-gray-600 bg-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:ring-blue-500 focus:border-blue-500"
                />
                <button
                  type="button"
                  onClick={props.onSave}
                  aria-describedby={message ? feedbackId : undefined}
                  disabled={saving || !teamsUrl.trim()}
                  className="rs-btn rs-btn--primary px-4 py-2 bg-blue-600 rs-text-on-accent rounded-md hover:bg-blue-700 disabled:opacity-50 text-sm font-medium"
                >
                  {saving ? 'Saving...' : 'Save'}
                </button>
                {isSet && (
                  <button
                    type="button"
                    onClick={props.onCancelReplace}
                    disabled={saving}
                    className="rs-btn rs-btn--quiet px-4 py-2 border border-gray-600 text-gray-300 rounded-md hover:border-blue-500 rs-hover-text-strong disabled:opacity-50 text-sm font-medium"
                  >
                    Cancel
                  </button>
                )}
              </div>
            ) : (
              // The test request carries no URL, so Test is offered only here, in
              // the Set state, and never beside a typed value that is not saved.
              <div className="flex flex-wrap gap-2">
                <button
                  type="button"
                  onClick={props.onReplace}
                  // A test result belongs to the Set state; Replace waits until a running test reports.
                  disabled={testing}
                  className="rs-btn rs-btn--quiet px-4 py-2 border border-gray-600 text-gray-300 rounded-md hover:border-blue-500 rs-hover-text-strong disabled:opacity-50 text-sm font-medium"
                >
                  Replace
                </button>
                <button
                  type="button"
                  onClick={props.onTest}
                  aria-describedby={message ? feedbackId : undefined}
                  disabled={testing}
                  className="rs-btn rs-btn--quiet px-4 py-2 border border-gray-600 text-gray-300 rounded-md hover:border-blue-500 rs-hover-text-strong disabled:opacity-50 text-sm font-medium"
                >
                  {testing ? 'Sending...' : 'Test'}
                </button>
                <button
                  type="button"
                  onClick={props.onAskRemove}
                  disabled={props.removing || testing}
                  className="rs-btn rs-btn--quiet px-4 py-2 text-sm font-medium rounded-md text-red-300 bg-red-900/40 hover:bg-red-900/60 border border-red-700 disabled:opacity-50"
                >
                  Remove
                </button>
              </div>
            )}
            {message && (
              <p id={feedbackId} role={message.type === 'error' ? 'alert' : 'status'} className={`text-xs mt-1 ${message.type === 'error' ? 'text-red-400' : 'text-green-400'}`}>
                {message.text}
              </p>
            )}
          </div>
        )}
        <p className="text-xs text-gray-300">
          Notifications will be sent for: sync activity, errors, circuit breaker alerts, imports, branch pair changes, and path violations.
          No-change sync cycles are automatically filtered out.
        </p>
      </div>

      <ConfirmModal
        isOpen={props.confirmingRemove && isSet}
        title="Remove the Teams webhook?"
        message="The saved Teams webhook URL will be removed. To set it again you will need the webhook URL from Teams."
        confirmLabel="Remove webhook"
        cancelLabel="Cancel"
        variant="danger"
        busy={props.removing}
        error={props.removeError || undefined}
        onConfirm={props.onConfirmRemove}
        onCancel={props.onCancelRemove}
      />
    </div>
  );
}
