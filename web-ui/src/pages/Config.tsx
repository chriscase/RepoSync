import React, { useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { api, type AuthorMapping } from '../api';

export default function Config() {
  const queryClient = useQueryClient();
  const { data: mappings, isLoading } = useQuery({
    queryKey: ['identityMappings'],
    queryFn: api.getIdentityMappings,
  });

  const [newMapping, setNewMapping] = useState<AuthorMapping>({
    svn_username: '',
    name: '',
    email: '',
  });

  const updateMutation = useMutation({
    mutationFn: (updated: AuthorMapping[]) =>
      api.updateIdentityMappings(updated),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ['identityMappings'] }),
  });

  const addMapping = () => {
    if (!newMapping.svn_username || !newMapping.name || !newMapping.email) return;
    const updated = [...(mappings ?? []), newMapping];
    updateMutation.mutate(updated);
    setNewMapping({ svn_username: '', name: '', email: '' });
  };

  const removeMapping = (svnUsername: string) => {
    const updated = (mappings ?? []).filter(
      (m) => m.svn_username !== svnUsername
    );
    updateMutation.mutate(updated);
  };

  if (isLoading) {
    return <div className="text-center py-8 text-gray-500">Loading...</div>;
  }

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold text-gray-100">Configuration</h1>

      {/* Identity Mappings */}
      <div className="bg-gray-800 shadow rounded-lg p-6 border border-gray-700">
        <h2 className="text-lg font-semibold text-gray-100 mb-4">
          Author Identity Mappings
        </h2>
        <p className="text-sm text-gray-400 mb-4">
          Map SVN usernames to Git author identities. These mappings ensure
          commits are attributed to the correct developer on both sides.
        </p>

        {/* Existing Mappings */}
        <table className="min-w-full divide-y divide-gray-700 mb-4">
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
            {(mappings ?? []).map((m: AuthorMapping) => (
              <tr key={m.svn_username} className="hover:bg-gray-700/50">
                <td className="px-4 py-2 font-mono text-sm text-gray-200">
                  {m.svn_username}
                </td>
                <td className="px-4 py-2 text-sm text-gray-300">{m.name}</td>
                <td className="px-4 py-2 text-sm text-gray-300">{m.email}</td>
                <td className="px-4 py-2 text-sm text-gray-500">
                  {m.github ?? '-'}
                </td>
                <td className="px-4 py-2 text-right">
                  <button
                    onClick={() => removeMapping(m.svn_username)}
                    className="text-red-400 hover:text-red-300 text-sm"
                  >
                    Remove
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>

        {/* Add New Mapping */}
        <div className="border-t border-gray-700 pt-4">
          <h3 className="text-sm font-medium text-gray-300 mb-2">
            Add New Mapping
          </h3>
          <div className="flex space-x-2">
            <input
              placeholder="SVN username"
              value={newMapping.svn_username}
              onChange={(e) =>
                setNewMapping({ ...newMapping, svn_username: e.target.value })
              }
              className="flex-1 rounded-md border border-gray-600 bg-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:ring-blue-500 focus:border-blue-500"
            />
            <input
              placeholder="Full Name"
              value={newMapping.name}
              onChange={(e) =>
                setNewMapping({ ...newMapping, name: e.target.value })
              }
              className="flex-1 rounded-md border border-gray-600 bg-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:ring-blue-500 focus:border-blue-500"
            />
            <input
              placeholder="email@company.com"
              value={newMapping.email}
              onChange={(e) =>
                setNewMapping({ ...newMapping, email: e.target.value })
              }
              className="flex-1 rounded-md border border-gray-600 bg-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:ring-blue-500 focus:border-blue-500"
            />
            <button
              onClick={addMapping}
              disabled={updateMutation.isPending}
              className="px-4 py-2 bg-blue-600 text-white rounded-md hover:bg-blue-700 disabled:opacity-50 text-sm font-medium"
            >
              Add
            </button>
          </div>
        </div>
      </div>

      {/* Notifications Section */}
      <NotificationsConfig />
    </div>
  );
}

function NotificationsConfig() {
  const [teamsUrl, setTeamsUrl] = React.useState('');
  const [loading, setLoading] = React.useState(true);
  const [saving, setSaving] = React.useState(false);
  const [testing, setTesting] = React.useState(false);
  const [message, setMessage] = React.useState('');
  const [showSetup, setShowSetup] = React.useState(false);

  React.useEffect(() => {
    const token = localStorage.getItem('session_token');
    fetch('/api/config/notifications', {
      headers: token ? { Authorization: `Bearer ${token}` } : {},
    })
      .then((r) => r.json())
      .then((d) => {
        setTeamsUrl(d.teams_webhook_url || '');
        setLoading(false);
      })
      .catch(() => setLoading(false));
  }, []);

  const save = async () => {
    setSaving(true);
    setMessage('');
    const token = localStorage.getItem('session_token');
    try {
      await fetch('/api/config/notifications', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          ...(token ? { Authorization: `Bearer ${token}` } : {}),
        },
        body: JSON.stringify({ teams_webhook_url: teamsUrl || null }),
      });
      setMessage('Saved');
      setTimeout(() => setMessage(''), 3000);
    } catch {
      setMessage('Save failed');
    }
    setSaving(false);
  };

  const testNotification = async () => {
    setTesting(true);
    setMessage('');
    const token = localStorage.getItem('session_token');
    try {
      const res = await fetch('/api/config/notifications/test', {
        method: 'POST',
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      const d = await res.json();
      setMessage(d.ok ? 'Test notification sent!' : d.error || 'Failed');
    } catch {
      setMessage('Failed to send test');
    }
    setTesting(false);
  };

  if (loading) return null;

  return (
    <div className="bg-gray-800/60 border border-gray-700 rounded-lg p-6">
      <h2 className="text-lg font-semibold text-gray-100 mb-4">
        Notifications — Microsoft Teams
      </h2>

      <button
        onClick={() => setShowSetup(!showSetup)}
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
            <li>Search for <strong>"Post to a channel when a webhook request is received"</strong> and select it</li>
            <li>Name your workflow (e.g., "RepoSync Notifications") and click <strong>Next</strong></li>
            <li>Select the channel to post to and click <strong>Add workflow</strong></li>
            <li>Copy the <strong>webhook URL</strong> and paste it below</li>
          </ol>
          <p className="text-xs text-gray-500 mt-2">
            No admin permissions required — any team member can create a workflow on their channel.
          </p>
        </div>
      )}

      <div className="space-y-3">
        <div>
          <label className="block text-sm text-gray-400 mb-1">Teams Webhook URL</label>
          <div className="flex gap-2">
            <input
              type="text"
              value={teamsUrl}
              onChange={(e) => setTeamsUrl(e.target.value)}
              placeholder="https://prod-XX.westus.logic.azure.com/workflows/..."
              className="flex-1 rounded-md border border-gray-600 bg-gray-700 px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:ring-blue-500 focus:border-blue-500"
            />
            <button
              onClick={save}
              disabled={saving}
              className="px-4 py-2 bg-blue-600 text-white rounded-md hover:bg-blue-700 disabled:opacity-50 text-sm font-medium"
            >
              {saving ? 'Saving...' : 'Save'}
            </button>
            {teamsUrl && (
              <button
                onClick={testNotification}
                disabled={testing}
                className="px-4 py-2 border border-gray-600 text-gray-300 rounded-md hover:border-blue-500 hover:text-white disabled:opacity-50 text-sm font-medium"
              >
                {testing ? 'Sending...' : 'Test'}
              </button>
            )}
          </div>
          {message && (
            <p className={`text-xs mt-1 ${message.includes('fail') ? 'text-red-400' : 'text-green-400'}`}>
              {message}
            </p>
          )}
        </div>
        <p className="text-xs text-gray-500">
          Notifications will be sent for: sync activity, errors, circuit breaker alerts, imports, branch pair changes, and path violations.
          No-change sync cycles are automatically filtered out.
        </p>
      </div>
    </div>
  );
}
