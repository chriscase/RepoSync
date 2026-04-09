import React, { useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useParams, useNavigate, Link } from 'react-router-dom';
import { api, type Repository, type SyncStatus, type SyncRecord, type CommitMapEntry, type AuditEntry } from '../api';
import ImportProgressCard from '../components/ImportProgressCard';
import ServerMonitor from '../components/ServerMonitor';
import {
  ArrowLeft, RefreshCw, Settings, Database, GitBranch, Clock,
  Activity, Zap, Save, X, Trash2, Power, AlertTriangle, CheckCircle,
  ChevronDown, Plus,
} from 'lucide-react';

import { getStoredUser } from '../utils/auth';
import { formatTimeAgo } from '../utils/time';

type EditForm = {
  name: string;
  svn_url: string;
  svn_branch: string;
  svn_username: string;
  svn_password: string;
  git_provider: string;
  git_api_url: string;
  git_repo: string;
  git_branch: string;
  git_token: string;
  sync_mode: string;
  poll_interval_secs: number;
  lfs_threshold_mb: number;
  auto_merge: boolean;
};

function repoToForm(repo: Repository): EditForm {
  return {
    name: repo.name,
    svn_url: repo.svn_url,
    svn_branch: repo.svn_branch,
    svn_username: repo.svn_username,
    svn_password: '',
    git_provider: repo.git_provider,
    git_api_url: repo.git_api_url,
    git_repo: repo.git_repo,
    git_branch: repo.git_branch,
    git_token: '',
    sync_mode: repo.sync_mode,
    poll_interval_secs: repo.poll_interval_secs,
    lfs_threshold_mb: repo.lfs_threshold_mb,
    auto_merge: repo.auto_merge,
  };
}

const inputClass =
  'w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent';

const selectClass =
  'w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent';

export default function RepoDetail() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const user = getStoredUser();
  const isAdmin = user?.role === 'admin';

  const [syncTriggered, setSyncTriggered] = useState(false);
  const [editing, setEditing] = useState(false);
  const [form, setForm] = useState<EditForm | null>(null);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [showReimportConfirm, setShowReimportConfirm] = useState(false);
  const [reimportConfirmText, setReimportConfirmText] = useState('');
  const [expandedAuditGroups, setExpandedAuditGroups] = useState<Set<number>>(new Set());
  const [expandedDetails, setExpandedDetails] = useState<Set<number>>(new Set());
  const [svnTestResult, setSvnTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [gitTestResult, setGitTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [svnTesting, setSvnTesting] = useState(false);
  const [gitTesting, setGitTesting] = useState(false);
  const [showBranchModal, setShowBranchModal] = useState(false);
  const [branchForm, setBranchForm] = useState({
    svn_branch: '',
    git_branch: '',
    skip_import: true,
    auto_create_svn_branch: true,
    auto_create_git_branch: true,
  });
  const [branchSuccess, setBranchSuccess] = useState(false);

  const { data: repo, isLoading, isError, error } = useQuery({
    queryKey: ['repo', id],
    queryFn: () => api.getRepo(id!),
    enabled: !!id,
  });

  // Status query scoped to this repo
  const { data: status } = useQuery<SyncStatus>({
    queryKey: ['status', id],
    queryFn: () => fetch(`/api/status?repo_id=${id}`, { headers: { Authorization: `Bearer ${localStorage.getItem('session_token')}` } }).then(r => r.json()),
    refetchInterval: 5000,
    enabled: !!id,
  });

  // Sync records
  const { data: syncRecords } = useQuery({
    queryKey: ['sync-records', id],
    queryFn: () => api.getSyncRecords(20, id),
    enabled: !!id,
  });

  // Commit map
  const { data: commitMap } = useQuery({
    queryKey: ['commit-map', id],
    queryFn: () => api.getCommitMap(15, id),
    enabled: !!id,
  });

  // Audit log
  const { data: auditLog } = useQuery({
    queryKey: ['audit', id],
    queryFn: () => api.getAuditLog(10, undefined, undefined, id),
    enabled: !!id,
  });

  // Credential status
  const { data: credStatus } = useQuery({
    queryKey: ['repo-credentials', id],
    queryFn: () => api.getRepoCredentials(id!),
    enabled: !!id,
  });

  // Branch pairs
  const { data: branchPairs } = useQuery({
    queryKey: ['branch-pairs', id],
    queryFn: () => api.listBranchPairs(id!),
    enabled: !!id,
  });

  const branchMutation = useMutation({
    mutationFn: (data: { svn_branch: string; git_branch: string; skip_import: boolean }) =>
      api.createBranchPair(id!, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['branch-pairs', id] });
      queryClient.invalidateQueries({ queryKey: ['repos'] });
      setShowBranchModal(false);
      setBranchForm({ svn_branch: '', git_branch: '', skip_import: true, auto_create_svn_branch: true, auto_create_git_branch: true });
      setBranchSuccess(true);
      setTimeout(() => setBranchSuccess(false), 3000);
    },
  });

  const syncMutation = useMutation({
    mutationFn: () => api.triggerRepoSync(id!),
    onSuccess: () => {
      setSyncTriggered(true);
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
      queryClient.invalidateQueries({ queryKey: ['status', id] });
      setTimeout(() => setSyncTriggered(false), 3000);
    },
  });

  const updateMutation = useMutation({
    mutationFn: (data: Partial<Repository>) => api.updateRepo(id!, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
      queryClient.invalidateQueries({ queryKey: ['repos'] });
      setEditing(false);
      setForm(null);
    },
  });

  const toggleMutation = useMutation({
    mutationFn: (enabled: boolean) => api.updateRepo(id!, { enabled }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
      queryClient.invalidateQueries({ queryKey: ['repos'] });
    },
  });

  const deleteMutation = useMutation({
    mutationFn: () => api.deleteRepo(id!),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['repos'] });
      navigate('/repos');
    },
  });

  const reimportMutation = useMutation({
    mutationFn: async () => {
      const token = localStorage.getItem('session_token');
      const res = await fetch(`/api/repos/${id}/import?reset=true`, {
        method: 'POST',
        headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}) },
      });
      if (!res.ok) throw new Error((await res.json()).message || 'Reset failed');
      return res.json();
    },
    onSuccess: () => {
      setShowReimportConfirm(false);
      setReimportConfirmText('');
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
    },
  });

  const auditEntries = auditLog?.entries ?? [];

  function groupAuditEntries(items: AuditEntry[]): { key: number; entries: AuditEntry[] }[] {
    const groups: { key: number; entries: AuditEntry[] }[] = [];
    for (const entry of items) {
      const last = groups[groups.length - 1];
      if (last && last.entries[0].action === entry.action && last.entries[0].success === entry.success) {
        last.entries.push(entry);
      } else {
        groups.push({ key: entry.id, entries: [entry] });
      }
    }
    return groups;
  }
  const auditGroups = groupAuditEntries(auditEntries);

  function startEdit() {
    if (!repo) return;
    setForm(repoToForm(repo));
    setEditing(true);
  }

  function cancelEdit() {
    setEditing(false);
    setForm(null);
  }

  async function handleSave() {
    if (!form) return;
    // Save repo config (exclude credential fields)
    const { svn_password, git_token, ...repoData } = form;
    updateMutation.mutate(repoData);
    // Save credentials if provided
    if (svn_password || git_token) {
      const credData: { svn_password?: string; git_token?: string } = {};
      if (svn_password) credData.svn_password = svn_password;
      if (git_token) credData.git_token = git_token;
      try {
        await api.saveRepoCredentials(id!, credData);
        queryClient.invalidateQueries({ queryKey: ['repo-credentials', id] });
      } catch (e) {
        // update mutation error will show in UI
      }
    }
  }

  async function handleTestSvn() {
    setSvnTesting(true);
    setSvnTestResult(null);
    try {
      // Prefer unsaved form values (so the user can test edits before saving).
      // Any empty field falls through to the saved DB value on the server side.
      const token = localStorage.getItem('session_token');
      const body: Record<string, string> = {};
      if (form) {
        if (form.svn_url) body.svn_url = form.svn_url;
        if (form.svn_branch) body.svn_branch = form.svn_branch;
        if (form.svn_username) body.svn_username = form.svn_username;
        if (form.svn_password) body.svn_password = form.svn_password;
      }
      const res = await fetch(`/api/repos/${id}/test-svn`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          ...(token ? { Authorization: `Bearer ${token}` } : {}),
        },
        body: JSON.stringify(body),
      });
      const result = await res.json();
      setSvnTestResult(result);
    } catch (e: any) {
      setSvnTestResult({ ok: false, message: e.message });
    } finally {
      setSvnTesting(false);
    }
  }

  async function handleTestGit() {
    setGitTesting(true);
    setGitTestResult(null);
    try {
      // Prefer unsaved form values (so the user can test edits before saving).
      const token = localStorage.getItem('session_token');
      const body: Record<string, string> = {};
      if (form) {
        if (form.git_api_url) body.git_api_url = form.git_api_url;
        if (form.git_repo) body.git_repo = form.git_repo;
        if (form.git_token) body.git_token = form.git_token;
      }
      const res = await fetch(`/api/repos/${id}/test-git`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          ...(token ? { Authorization: `Bearer ${token}` } : {}),
        },
        body: JSON.stringify(body),
      });
      const result = await res.json();
      setGitTestResult(result);
    } catch (e: any) {
      setGitTestResult({ ok: false, message: e.message });
    } finally {
      setGitTesting(false);
    }
  }

  function setField<K extends keyof EditForm>(key: K, value: EditForm[K]) {
    setForm((prev) => (prev ? { ...prev, [key]: value } : prev));
  }

  if (isLoading) {
    return <div className="text-center py-8 text-gray-400">Loading repository...</div>;
  }

  if (isError || !repo) {
    return (
      <div className="text-center py-8 text-red-400">
        Error loading repository: {error?.message ?? 'Not found'}
      </div>
    );
  }

  const records = syncRecords?.entries ?? [];
  const cmEntries = commitMap?.entries ?? [];

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <Link
            to="/repos"
            className="inline-flex items-center gap-1 text-sm text-gray-400 hover:text-gray-200 transition-colors"
          >
            <ArrowLeft className="w-4 h-4" />
            Back to Repositories
          </Link>
        </div>
      </div>

      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-bold text-gray-100">{repo.name}</h1>
          <span
            className={`inline-flex items-center px-2.5 py-0.5 rounded text-xs font-medium ${
              repo.enabled
                ? 'bg-green-900/50 text-green-300'
                : 'bg-gray-700 text-gray-400'
            }`}
          >
            {repo.enabled ? 'Enabled' : 'Disabled'}
          </span>
        </div>
        <div className="flex items-center gap-3">
          {/* Enable/Disable toggle */}
          <button
            onClick={() => toggleMutation.mutate(!repo.enabled)}
            disabled={toggleMutation.isPending}
            className={`inline-flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
              repo.enabled
                ? 'border border-yellow-600 text-yellow-300 hover:bg-yellow-900/30'
                : 'border border-green-600 text-green-300 hover:bg-green-900/30'
            } disabled:opacity-50`}
          >
            <Power className="w-4 h-4" />
            {repo.enabled ? 'Disable' : 'Enable'}
          </button>

          {!editing ? (
            <button
              onClick={startEdit}
              className="inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-gray-600 hover:border-gray-500 text-gray-300 hover:text-white text-sm font-medium transition-colors"
            >
              <Settings className="w-4 h-4" />
              Edit
            </button>
          ) : (
            <>
              <button
                onClick={handleSave}
                disabled={updateMutation.isPending}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-blue-600 hover:bg-blue-700 disabled:opacity-50 text-white text-sm font-medium transition-colors"
              >
                <Save className="w-4 h-4" />
                {updateMutation.isPending ? 'Saving...' : 'Save'}
              </button>
              <button
                onClick={cancelEdit}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-gray-600 hover:border-gray-500 text-gray-300 hover:text-white text-sm font-medium transition-colors"
              >
                <X className="w-4 h-4" />
                Cancel
              </button>
            </>
          )}
          <button
            onClick={() => syncMutation.mutate()}
            disabled={syncMutation.isPending || syncTriggered}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-blue-600 hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm font-medium transition-colors"
          >
            <RefreshCw className={`w-4 h-4 ${syncMutation.isPending ? 'animate-spin' : ''}`} />
            {syncTriggered ? 'Sync Triggered' : 'Trigger Sync'}
          </button>
        </div>
      </div>

      {/* Mutation errors */}
      {syncMutation.isError && (
        <div className="bg-red-900/30 border border-red-700 rounded-lg p-4 text-red-300 text-sm">
          Failed to trigger sync: {syncMutation.error?.message}
        </div>
      )}
      {updateMutation.isError && (
        <div className="bg-red-900/30 border border-red-700 rounded-lg p-4 text-red-300 text-sm">
          Failed to save: {updateMutation.error?.message}
        </div>
      )}

      {/* Config Cards - SVN and Git side by side */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        {/* SVN Config */}
        <div className="bg-gray-800/60 border border-gray-700 rounded-lg p-5">
          <div className="flex items-center gap-2 mb-4">
            <Database className="w-5 h-5 text-blue-400" />
            <h2 className="text-lg font-semibold text-gray-100">SVN Configuration</h2>
          </div>
          <div className="space-y-3">
            {editing && form ? (
              <>
                <FieldInput label="Name" value={form.name} onChange={(v) => setField('name', v)} />
                <FieldInput label="SVN URL" value={form.svn_url} onChange={(v) => setField('svn_url', v)} />
                <FieldInput label="Branch" value={form.svn_branch} onChange={(v) => setField('svn_branch', v)} />
                <FieldInput label="Username" value={form.svn_username} onChange={(v) => setField('svn_username', v)} />
                <div>
                  <label className="block text-sm text-gray-400 mb-1">SVN Password</label>
                  <input
                    type="password"
                    className={inputClass}
                    value={form.svn_password}
                    onChange={(e) => setField('svn_password', e.target.value)}
                    placeholder={credStatus?.svn_password_set ? '\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF (saved)' : 'Enter password'}
                  />
                </div>
                <button
                  type="button"
                  onClick={handleTestSvn}
                  disabled={svnTesting}
                  className="inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium border border-blue-600 text-blue-300 hover:bg-blue-900/30 disabled:opacity-50 transition-colors"
                >
                  {svnTesting ? 'Testing...' : 'Test SVN Connection'}
                </button>
                {svnTestResult && (
                  <div className={`text-xs px-2 py-1.5 rounded ${svnTestResult.ok ? 'bg-green-900/30 text-green-300' : 'bg-red-900/30 text-red-300'}`}>
                    {svnTestResult.message}
                  </div>
                )}
              </>
            ) : (
              <>
                <ConfigRow label="Name" value={repo.name} />
                <ConfigRow label="URL" value={repo.svn_url} />
                <ConfigRow label="Branch" value={repo.svn_branch} />
                <ConfigRow label="Username" value={repo.svn_username} />
                <ConfigRow label="Password" value={credStatus?.svn_password_set ? '\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF (saved)' : 'Not set'} />
              </>
            )}
          </div>
        </div>

        {/* Git Config */}
        <div className="bg-gray-800/60 border border-gray-700 rounded-lg p-5">
          <div className="flex items-center gap-2 mb-4">
            <GitBranch className="w-5 h-5 text-purple-400" />
            <h2 className="text-lg font-semibold text-gray-100">Git Configuration</h2>
          </div>
          <div className="space-y-3">
            {editing && form ? (
              <>
                <FieldSelect
                  label="Provider"
                  value={form.git_provider}
                  options={[
                    { value: 'github', label: 'GitHub' },
                    { value: 'gitea', label: 'Gitea' },
                  ]}
                  onChange={(v) => setField('git_provider', v)}
                />
                <FieldInput label="API URL" value={form.git_api_url} onChange={(v) => setField('git_api_url', v)} />
                <FieldInput label="Repository" value={form.git_repo} onChange={(v) => setField('git_repo', v)} placeholder="owner/repo" />
                <FieldInput label="Default Branch" value={form.git_branch} onChange={(v) => setField('git_branch', v)} />
                <div>
                  <label className="block text-sm text-gray-400 mb-1">Git Token</label>
                  <input
                    type="password"
                    className={inputClass}
                    value={form.git_token}
                    onChange={(e) => setField('git_token', e.target.value)}
                    placeholder={credStatus?.git_token_set ? '\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF (saved)' : 'Enter token'}
                  />
                </div>
                <button
                  type="button"
                  onClick={handleTestGit}
                  disabled={gitTesting}
                  className="inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium border border-purple-600 text-purple-300 hover:bg-purple-900/30 disabled:opacity-50 transition-colors"
                >
                  {gitTesting ? 'Testing...' : 'Test Git Connection'}
                </button>
                {gitTestResult && (
                  <div className={`text-xs px-2 py-1.5 rounded ${gitTestResult.ok ? 'bg-green-900/30 text-green-300' : 'bg-red-900/30 text-red-300'}`}>
                    {gitTestResult.message}
                  </div>
                )}
              </>
            ) : (
              <>
                <ConfigRow label="Provider" value={repo.git_provider} />
                <ConfigRow label="API URL" value={repo.git_api_url} />
                <ConfigRow label="Repository" value={repo.git_repo} />
                <ConfigRow label="Branch" value={repo.git_branch} />
                <ConfigRow label="Token" value={credStatus?.git_token_set ? '\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF (saved)' : 'Not set'} />
              </>
            )}
          </div>
        </div>
      </div>

      {/* Sync Settings */}
      <div className="bg-gray-800/60 border border-gray-700 rounded-lg p-5">
        <div className="flex items-center gap-2 mb-4">
          <Activity className="w-5 h-5 text-green-400" />
          <h2 className="text-lg font-semibold text-gray-100">Sync Settings</h2>
        </div>
        {editing && form ? (
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
            <FieldSelect
              label="Sync Mode"
              value={form.sync_mode}
              options={[
                { value: 'direct', label: 'Direct' },
                { value: 'pr', label: 'Pull Request' },
              ]}
              onChange={(v) => setField('sync_mode', v)}
            />
            <FieldNumber label="Poll Interval (s)" value={form.poll_interval_secs} onChange={(v) => setField('poll_interval_secs', v)} min={10} />
            <FieldNumber label="LFS Threshold (MB)" value={form.lfs_threshold_mb} onChange={(v) => setField('lfs_threshold_mb', v)} min={0} />
            <FieldToggle label="Auto Merge" checked={form.auto_merge} onChange={(v) => setField('auto_merge', v)} />
          </div>
        ) : (
          <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            <div>
              <span className="text-sm text-gray-400">Sync Mode</span>
              <p className="text-lg text-gray-100 capitalize">{repo.sync_mode}</p>
            </div>
            <div>
              <span className="text-sm text-gray-400">Poll Interval</span>
              <p className="text-lg text-gray-100">{repo.poll_interval_secs}s</p>
            </div>
            <div>
              <span className="text-sm text-gray-400">LFS Threshold</span>
              <p className="text-lg text-gray-100">{repo.lfs_threshold_mb} MB</p>
            </div>
            <div>
              <span className="text-sm text-gray-400">Auto Merge</span>
              <p className="text-lg text-gray-100">{repo.auto_merge ? 'Yes' : 'No'}</p>
            </div>
          </div>
        )}
      </div>

      {/* Quick Stats */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
        <StatCard
          icon={<Zap className="w-5 h-5 text-yellow-400" />}
          label="LFS Threshold"
          value={`${repo.lfs_threshold_mb} MB`}
        />
        <StatCard
          icon={<Clock className="w-5 h-5 text-blue-400" />}
          label="Created"
          value={new Date(repo.created_at).toLocaleDateString()}
        />
        <StatCard
          icon={<Activity className="w-5 h-5 text-green-400" />}
          label="Last Updated"
          value={formatTimeAgo(repo.updated_at)}
        />
        <StatCard
          icon={<Database className="w-5 h-5 text-purple-400" />}
          label="Created By"
          value={repo.created_by ?? 'System'}
        />
      </div>

      {/* ===== NEW DASHBOARD SECTIONS ===== */}

      {/* Status Cards Row */}
      <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-5 gap-4">
        <StatusCard
          title="Sync State"
          value={status?.state ?? 'unknown'}
          color={
            status?.state === 'idle'
              ? 'green'
              : status?.state === 'error'
                ? 'red'
                : 'yellow'
          }
        />
        <StatusCard
          title="Last Sync"
          value={status?.last_sync_at ? formatTimeAgo(status.last_sync_at) : 'Never'}
          color="gray"
        />
        <StatusCard
          title="Total Syncs"
          value={String(status?.total_syncs ?? 0)}
          color="blue"
        />
        <StatusCard
          title="Active Conflicts"
          value={String(status?.active_conflicts ?? 0)}
          color={status?.active_conflicts ? 'red' : 'green'}
          onClick={() => navigate('/conflicts')}
        />
        <StatusCard
          title="Errors (24h)"
          value={String(status?.total_errors ?? 0)}
          color={status?.total_errors ? 'red' : 'gray'}
          subtitle={
            status?.last_error_at
              ? `Last: ${formatTimeAgo(status.last_error_at)}`
              : 'No recent errors'
          }
        />
      </div>

      {/* Import Progress */}
      <ImportProgressCard repoId={id} repoName={repo?.name} />

      {/* Sync Records */}
      <div className="bg-gray-800 shadow rounded-lg border border-gray-700">
        <div className="p-6 pb-3">
          <h2 className="text-lg font-semibold text-gray-100">
            Sync Records
            <span className="ml-2 text-sm font-normal text-blue-400">&mdash; {repo.name}</span>
          </h2>
          <p className="text-sm text-gray-400 mt-1">Recent commits synced for this repository (click to expand)</p>
        </div>
        {records.length > 0 ? (
          <div className="divide-y divide-gray-700">
            {records.map((record) => (
              <SyncRecordRow key={record.id} record={record} />
            ))}
          </div>
        ) : (
          <p className="text-gray-400 text-sm px-6 pb-6">No sync records yet</p>
        )}
      </div>

      {/* Commit Map */}
      <div className="bg-gray-800 shadow rounded-lg border border-gray-700">
        <div className="p-6 pb-3">
          <h2 className="text-lg font-semibold text-gray-100">
            Commit Map (SVN &harr; Git)
            <span className="ml-2 text-sm font-normal text-blue-400">&mdash; {repo.name}</span>
          </h2>
          <p className="text-sm text-gray-400 mt-1">Bidirectional mapping between SVN revisions and Git commits</p>
        </div>
        {cmEntries.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="min-w-full divide-y divide-gray-700">
              <thead>
                <tr>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">SVN Rev</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Git SHA</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Direction</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">SVN Author</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Git Author</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Synced At</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-700">
                {cmEntries.map((cm: CommitMapEntry) => (
                  <tr key={cm.id} className="hover:bg-gray-700/50">
                    <td className="px-6 py-3 text-sm font-mono text-blue-400">r{cm.svn_rev}</td>
                    <td className="px-6 py-3 text-sm font-mono text-purple-400 truncate max-w-[200px]">
                      {cm.git_sha.substring(0, 12)}
                    </td>
                    <td className="px-6 py-3">
                      <DirectionBadge direction={cm.direction} />
                    </td>
                    <td className="px-6 py-3 text-sm text-gray-300">{cm.svn_author}</td>
                    <td className="px-6 py-3 text-sm text-gray-300 truncate max-w-[200px]">{cm.git_author}</td>
                    <td className="px-6 py-3 text-sm text-gray-400">
                      {new Date(cm.synced_at).toLocaleString()}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <p className="text-gray-400 text-sm px-6 pb-6">No commit mappings yet</p>
        )}
      </div>

      {/* Audit Log (compact) */}
      <div className="bg-gray-800 shadow rounded-lg border border-gray-700">
        <div className="p-6 pb-3">
          <h2 className="text-lg font-semibold text-gray-100">
            Recent Audit Log
            <span className="ml-2 text-sm font-normal text-blue-400">&mdash; {repo.name}</span>
          </h2>
          <p className="text-sm text-gray-400 mt-1">Last 10 audit entries for this repository</p>
        </div>
        {auditGroups.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="min-w-full divide-y divide-gray-700">
              <thead>
                <tr>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Status</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Action</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Author</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Details</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Timestamp</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-700">
                {auditGroups.map((group) => {
                  const latest = group.entries[0];
                  const isGroup = group.entries.length > 1;
                  const isGrpExpanded = expandedAuditGroups.has(group.key);
                  const isDetExpanded = expandedDetails.has(latest.id);
                  const detailText = latest.details || latest.action;
                  const isLong = detailText.length > 80;
                  return (
                    <React.Fragment key={group.key}>
                      <tr
                        className={`hover:bg-gray-700/50 ${isGroup ? 'cursor-pointer' : ''}`}
                        onClick={() => {
                          if (!isGroup) return;
                          setExpandedAuditGroups(prev => {
                            const next = new Set(prev);
                            if (next.has(group.key)) next.delete(group.key); else next.add(group.key);
                            return next;
                          });
                        }}
                      >
                        <td className="px-6 py-3">
                          <div className="flex items-center space-x-1">
                            {isGroup && <span className="text-gray-500 text-xs">{isGrpExpanded ? '\u25BE' : '\u25B8'}</span>}
                            {latest.success ? (
                              <CheckCircle className="w-4 h-4 text-green-400" />
                            ) : (
                              <AlertTriangle className="w-4 h-4 text-red-400" />
                            )}
                          </div>
                        </td>
                        <td className="px-6 py-3">
                          <div className="flex items-center space-x-2">
                            <ActionBadge action={latest.action} />
                            {isGroup && (
                              <span className="text-xs bg-gray-600 text-gray-300 rounded-full px-2 py-0.5">
                                &times;{group.entries.length}
                              </span>
                            )}
                          </div>
                        </td>
                        <td className="px-6 py-3 text-sm text-gray-300">{latest.author ?? '-'}</td>
                        <td className="px-6 py-3 text-sm text-gray-400">
                          <div>
                            <span
                              className={isLong ? 'cursor-pointer hover:text-gray-200' : ''}
                              onClick={(e) => {
                                if (!isLong) return;
                                e.stopPropagation();
                                setExpandedDetails(prev => {
                                  const next = new Set(prev);
                                  if (next.has(latest.id)) next.delete(latest.id); else next.add(latest.id);
                                  return next;
                                });
                              }}
                            >
                              {isDetExpanded ? detailText : (isLong ? `${detailText.substring(0, 80)}...` : detailText)}
                            </span>
                            {isDetExpanded && (
                              <pre className="mt-2 p-3 bg-gray-900 rounded text-xs font-mono text-gray-300 whitespace-pre-wrap break-words max-w-lg">
                                {detailText}
                              </pre>
                            )}
                          </div>
                        </td>
                        <td className="px-6 py-3 text-sm text-gray-500">
                          {new Date(latest.created_at).toLocaleString()}
                        </td>
                      </tr>
                      {isGroup && isGrpExpanded && group.entries.slice(1).map((entry: AuditEntry) => (
                        <tr key={entry.id} className="bg-gray-900/40">
                          <td className="px-6 py-2"></td>
                          <td className="px-6 py-2"><ActionBadge action={entry.action} /></td>
                          <td className="px-6 py-2 text-sm text-gray-400">{entry.author ?? '-'}</td>
                          <td className="px-6 py-2 text-sm text-gray-500 truncate max-w-[300px]">{entry.details || entry.action}</td>
                          <td className="px-6 py-2 text-sm text-gray-600">{new Date(entry.created_at).toLocaleString()}</td>
                        </tr>
                      ))}
                    </React.Fragment>
                  );
                })}
              </tbody>
            </table>
          </div>
        ) : (
          <p className="text-gray-400 text-sm px-6 pb-6">No audit entries yet</p>
        )}
      </div>

      {/* Branch Pairs */}
      <div className="bg-gray-800 shadow rounded-lg border border-gray-700">
        <div className="p-6 pb-3 flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold text-gray-100 flex items-center gap-2">
              <GitBranch className="w-5 h-5 text-purple-400" />
              Branch Pairs
            </h2>
            <p className="text-sm text-gray-400 mt-1">SVN branches synced to separate Git branches</p>
          </div>
          {isAdmin && (
            <button
              onClick={() => setShowBranchModal(true)}
              className="inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium bg-purple-600 hover:bg-purple-700 text-white transition-colors"
            >
              <Plus className="w-3.5 h-3.5" />
              Add Branch Pair
            </button>
          )}
        </div>
        {branchSuccess && (
          <div className="mx-6 mb-3 bg-green-900/30 border border-green-700 rounded-lg p-3 text-green-300 text-sm">
            Branch pair created successfully.
          </div>
        )}
        {(branchPairs ?? []).length > 0 ? (
          <div className="overflow-x-auto">
            <table className="min-w-full divide-y divide-gray-700">
              <thead>
                <tr>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Name</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">SVN Branch</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Git Branch</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Status</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Last Sync</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-700">
                {(branchPairs ?? []).map((bp) => (
                  <tr
                    key={bp.id}
                    onClick={() => navigate(`/repos/${bp.id}`)}
                    className="hover:bg-gray-700/50 cursor-pointer transition-colors"
                  >
                    <td className="px-6 py-3 text-sm text-gray-200 font-medium">{bp.name}</td>
                    <td className="px-6 py-3 text-sm font-mono text-blue-400">{bp.svn_branch}</td>
                    <td className="px-6 py-3 text-sm font-mono text-purple-400">{bp.git_branch}</td>
                    <td className="px-6 py-3">
                      <span className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
                        bp.enabled ? 'bg-green-900/50 text-green-300' : 'bg-gray-700 text-gray-400'
                      }`}>
                        {bp.enabled ? 'Active' : 'Disabled'}
                      </span>
                    </td>
                    <td className="px-6 py-3 text-sm text-gray-400">{formatTimeAgo(bp.updated_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <p className="text-gray-400 text-sm px-6 pb-6">No branch pairs configured</p>
        )}
      </div>

      {/* Add Branch Pair Modal */}
      {showBranchModal && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4">
          <div className="bg-gray-800 border border-gray-700 rounded-lg shadow-xl w-full max-w-md">
            <div className="flex items-center justify-between p-6 border-b border-gray-700">
              <h2 className="text-lg font-semibold text-gray-100">Create Branch</h2>
              <button
                onClick={() => { setShowBranchModal(false); setBranchForm({ svn_branch: '', git_branch: '', skip_import: true, auto_create_svn_branch: true, auto_create_git_branch: true }); branchMutation.reset(); }}
                className="text-gray-400 hover:text-gray-200 transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            <div className="p-6 space-y-4">
              {branchMutation.isError && (
                <div className="bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm">
                  Failed to create branch: {branchMutation.error?.message}
                </div>
              )}

              {/* Parent context */}
              <div className="bg-gray-900/50 border border-gray-700/50 rounded-lg px-3 py-2 text-xs text-gray-400">
                <span className="text-gray-500">Branching from:</span>{' '}
                <span className="text-blue-400 font-mono">{repo?.svn_branch || 'trunk'}</span>
                {' \u2192 '}
                <span className="text-purple-400 font-mono">{repo?.git_branch || 'main'}</span>
              </div>

              <div>
                <label className="block text-sm text-gray-400 mb-1">Branch Name <span className="text-gray-600 text-xs">({branchForm.git_branch.length}/200)</span></label>
                <input
                  type="text"
                  className={inputClass}
                  value={branchForm.git_branch}
                  maxLength={200}
                  onChange={(e) => {
                    const name = e.target.value;
                    setBranchForm(prev => ({
                      ...prev,
                      git_branch: name,
                      svn_branch: name ? `branches/${name}` : '',
                    }));
                  }}
                  placeholder="e.g., fix-123, dev/james-wilson"
                />
                {(() => {
                  const v = branchForm.git_branch;
                  if (!v) return null;
                  const invalid = /[^a-zA-Z0-9._\/-]/.test(v);
                  const hasTraversal = v.includes('..');
                  const hasDoubleSlash = v.includes('//');
                  const badEdges = v.startsWith('/') || v.endsWith('/') || v.startsWith('-');
                  const reserved = v === 'HEAD' || v.startsWith('refs/') || v.endsWith('.lock');
                  const err = invalid ? 'Invalid characters (use letters, numbers, . _ - / only)'
                    : hasTraversal ? 'Must not contain ".."'
                    : hasDoubleSlash ? 'Must not contain "//"'
                    : badEdges ? 'Must not start/end with "/" or start with "-"'
                    : reserved ? 'Reserved name not allowed'
                    : null;
                  return err ? <p className="mt-1 text-xs text-red-400">{err}</p> : null;
                })()}
              </div>
              <div>
                <label className="block text-sm text-gray-400 mb-1">SVN Branch Path <span className="text-gray-600">(auto-derived, editable)</span></label>
                <input
                  type="text"
                  className={inputClass}
                  value={branchForm.svn_branch}
                  maxLength={200}
                  onChange={(e) => setBranchForm(prev => ({ ...prev, svn_branch: e.target.value }))}
                  placeholder="branches/fix-123"
                />
                {(() => {
                  const v = branchForm.svn_branch;
                  if (!v) return null;
                  const invalid = /[^a-zA-Z0-9._\/-]/.test(v);
                  const hasTraversal = v.includes('..');
                  const err = invalid ? 'Invalid characters' : hasTraversal ? 'Must not contain ".."' : null;
                  return err ? <p className="mt-1 text-xs text-red-400">{err}</p> : null;
                })()}
              </div>

              {/* Auto-create options */}
              <div className="space-y-2">
                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={branchForm.auto_create_svn_branch}
                    onChange={(e) => setBranchForm(prev => ({ ...prev, auto_create_svn_branch: e.target.checked }))}
                    className="rounded border-gray-600 bg-gray-700 text-blue-600"
                  />
                  <span className="text-sm text-gray-300">Create SVN branch <span className="text-gray-500">(svn copy)</span></span>
                </label>
                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={branchForm.auto_create_git_branch}
                    onChange={(e) => setBranchForm(prev => ({ ...prev, auto_create_git_branch: e.target.checked }))}
                    className="rounded border-gray-600 bg-gray-700 text-purple-600"
                  />
                  <span className="text-sm text-gray-300">Create Git branch <span className="text-gray-500">(from {repo?.git_branch || 'main'})</span></span>
                </label>
              </div>

              {/* Import mode */}
              <div>
                <label className="block text-sm text-gray-400 mb-2">Import Mode</label>
                <div className="space-y-2">
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="importMode"
                      checked={branchForm.skip_import}
                      onChange={() => setBranchForm(prev => ({ ...prev, skip_import: true }))}
                      className="text-blue-600"
                    />
                    <span className="text-sm text-gray-300">Start from now <span className="text-gray-500">(recommended)</span></span>
                  </label>
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="importMode"
                      checked={!branchForm.skip_import}
                      onChange={() => setBranchForm(prev => ({ ...prev, skip_import: false }))}
                      className="text-blue-600"
                    />
                    <span className="text-sm text-gray-300">Import full history</span>
                  </label>
                </div>
              </div>
            </div>
            <div className="flex items-center justify-end gap-3 p-6 border-t border-gray-700">
              <button
                onClick={() => { setShowBranchModal(false); setBranchForm({ svn_branch: '', git_branch: '', skip_import: true, auto_create_svn_branch: true, auto_create_git_branch: true }); branchMutation.reset(); }}
                className="px-4 py-2 rounded-lg border border-gray-600 text-gray-300 hover:text-white text-sm font-medium transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={() => branchMutation.mutate(branchForm)}
                disabled={branchMutation.isPending || !branchForm.svn_branch.trim() || !branchForm.git_branch.trim()
                  || /[^a-zA-Z0-9._\/-]/.test(branchForm.git_branch) || /[^a-zA-Z0-9._\/-]/.test(branchForm.svn_branch)
                  || branchForm.git_branch.includes('..') || branchForm.svn_branch.includes('..')
                  || branchForm.git_branch.includes('//') || branchForm.git_branch.startsWith('/') || branchForm.git_branch.endsWith('/')
                  || branchForm.git_branch.startsWith('-') || branchForm.git_branch === 'HEAD' || branchForm.git_branch.endsWith('.lock')
                }
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-purple-600 hover:bg-purple-700 disabled:opacity-50 text-white text-sm font-medium transition-colors"
              >
                <GitBranch className="w-4 h-4" />
                {branchMutation.isPending ? 'Creating...' : 'Create Branch'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Server Monitor */}
      <ServerMonitor />

      {/* Danger Zone - admin only */}
      {isAdmin && (
        <div className="border-t border-gray-700 pt-6">
          <h3 className="text-sm font-medium text-red-400 mb-4">Danger Zone</h3>
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <div>
                <p className="text-sm text-gray-300 font-medium">Reset & Reimport</p>
                <p className="text-sm text-gray-500 mt-0.5">Wipe the Git repository and re-sync all SVN history from the beginning.</p>
              </div>
              <button
                onClick={() => setShowReimportConfirm(true)}
                disabled={reimportMutation.isPending}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-yellow-700 text-yellow-400 hover:bg-yellow-900/30 text-sm font-medium transition-colors disabled:opacity-50"
              >
                <RefreshCw className="w-4 h-4" />
                Reset & Reimport
              </button>
            </div>
            <div className="flex items-center justify-between">
              <div>
                <p className="text-sm text-gray-300 font-medium">Delete Repository</p>
                <p className="text-sm text-gray-500 mt-0.5">Permanently remove this repository configuration.</p>
              </div>
              <button
                onClick={() => setShowDeleteConfirm(true)}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-red-700 text-red-400 hover:bg-red-900/30 text-sm font-medium transition-colors"
              >
                <Trash2 className="w-4 h-4" />
                Delete Repository
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Delete confirmation modal */}
      {showDeleteConfirm && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4">
          <div className="bg-gray-800 border border-gray-700 rounded-lg p-6 max-w-md w-full shadow-xl">
            <h3 className="text-lg font-semibold text-gray-100 mb-2">Delete Repository</h3>
            <p className="text-sm text-gray-400 mb-6">
              Are you sure you want to delete <span className="font-semibold text-gray-200">{repo.name}</span>? This cannot be undone.
            </p>
            {deleteMutation.isError && (
              <div className="bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm mb-4">
                Failed to delete: {deleteMutation.error?.message}
              </div>
            )}
            <div className="flex items-center justify-end gap-3">
              <button
                onClick={() => setShowDeleteConfirm(false)}
                className="px-4 py-2 rounded-lg border border-gray-600 text-gray-300 hover:text-white text-sm font-medium transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={() => deleteMutation.mutate()}
                disabled={deleteMutation.isPending}
                className="px-4 py-2 rounded-lg bg-red-600 hover:bg-red-700 disabled:opacity-50 text-white text-sm font-medium transition-colors"
              >
                {deleteMutation.isPending ? 'Deleting...' : 'Delete'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Reset & Reimport confirmation modal */}
      {showReimportConfirm && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4">
          <div className="bg-gray-800 border border-gray-700 rounded-lg p-6 max-w-md w-full shadow-xl">
            <h3 className="text-lg font-semibold text-gray-100 mb-2">Reset & Reimport</h3>
            <p className="text-sm text-gray-400 mb-2">
              This will <span className="text-red-400 font-semibold">wipe the entire Git repository</span> and re-sync all SVN history from revision 0. The remote repository will be force-pushed with a clean slate.
            </p>
            <p className="text-sm text-gray-400 mb-4">
              Type <span className="font-mono text-yellow-300">{repo.name}</span> to confirm:
            </p>
            <input
              type="text"
              className="w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-yellow-500 focus:border-transparent mb-4"
              value={reimportConfirmText}
              onChange={(e) => setReimportConfirmText(e.target.value)}
              placeholder={repo.name}
            />
            {reimportMutation.isError && (
              <div className="bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm mb-4">
                Failed: {reimportMutation.error?.message}
              </div>
            )}
            <div className="flex items-center justify-end gap-3">
              <button
                onClick={() => { setShowReimportConfirm(false); setReimportConfirmText(''); reimportMutation.reset(); }}
                className="px-4 py-2 rounded-lg border border-gray-600 text-gray-300 hover:text-white text-sm font-medium transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={() => reimportMutation.mutate()}
                disabled={reimportMutation.isPending || reimportConfirmText !== repo.name}
                className="px-4 py-2 rounded-lg bg-yellow-600 hover:bg-yellow-700 disabled:opacity-50 text-white text-sm font-medium transition-colors"
              >
                {reimportMutation.isPending ? 'Resetting...' : 'Reset & Reimport'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/* ---- Sub-components ---- */

function ConfigRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-4">
      <span className="text-sm text-gray-400 flex-shrink-0">{label}</span>
      <span className="text-sm text-gray-200 truncate text-right font-mono">{value}</span>
    </div>
  );
}

function FieldInput({
  label,
  value,
  onChange,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <div>
      <label className="block text-sm text-gray-400 mb-1">{label}</label>
      <input
        type="text"
        className={inputClass}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
      />
    </div>
  );
}

function FieldNumber({
  label,
  value,
  onChange,
  min,
}: {
  label: string;
  value: number;
  onChange: (v: number) => void;
  min?: number;
}) {
  return (
    <div>
      <label className="block text-sm text-gray-400 mb-1">{label}</label>
      <input
        type="number"
        className={inputClass}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        min={min}
      />
    </div>
  );
}

function FieldSelect({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: { value: string; label: string }[];
  onChange: (v: string) => void;
}) {
  return (
    <div>
      <label className="block text-sm text-gray-400 mb-1">{label}</label>
      <select className={selectClass} value={value} onChange={(e) => onChange(e.target.value)}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </div>
  );
}

function FieldToggle({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div>
      <label className="block text-sm text-gray-400 mb-1">{label}</label>
      <button
        type="button"
        onClick={() => onChange(!checked)}
        className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors mt-1 ${
          checked ? 'bg-blue-600' : 'bg-gray-600'
        }`}
      >
        <span
          className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
            checked ? 'translate-x-6' : 'translate-x-1'
          }`}
        />
      </button>
    </div>
  );
}

function StatCard({ icon, label, value }: { icon: React.ReactNode; label: string; value: string }) {
  return (
    <div className="bg-gray-800/60 border border-gray-700 rounded-lg p-4">
      <div className="flex items-center gap-2 mb-2">
        {icon}
        <span className="text-sm text-gray-400">{label}</span>
      </div>
      <p className="text-lg font-semibold text-gray-100 capitalize">{value}</p>
    </div>
  );
}

function StatusCard({
  title,
  value,
  color,
  onClick,
  subtitle,
}: {
  title: string;
  value: string;
  color: string;
  onClick?: () => void;
  subtitle?: string;
}) {
  const colorClasses: Record<string, string> = {
    green: 'bg-green-900/30 border-green-700',
    red: 'bg-red-900/30 border-red-700',
    yellow: 'bg-yellow-900/30 border-yellow-700',
    blue: 'bg-blue-900/30 border-blue-700',
    gray: 'bg-gray-800 border-gray-700',
  };

  const clickableClasses = onClick
    ? 'cursor-pointer hover:border-blue-500/50 transition-colors'
    : '';

  return (
    <div
      className={`rounded-lg border p-4 ${colorClasses[color] ?? colorClasses.gray} ${clickableClasses}`}
      onClick={onClick}
    >
      <p className="text-sm text-gray-400">{title}</p>
      <p className="text-2xl font-bold capitalize text-gray-100">{value}</p>
      {subtitle && <p className="text-xs text-gray-500 mt-1">{subtitle}</p>}
    </div>
  );
}

function SyncRecordRow({ record }: { record: SyncRecord }) {
  const [expanded, setExpanded] = useState(false);

  const statusColor =
    record.status === 'applied'
      ? 'text-green-400'
      : record.status === 'failed'
        ? 'text-red-400'
        : 'text-yellow-400';

  return (
    <div>
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full px-6 py-3 flex items-center justify-between hover:bg-gray-700/50 text-left transition-colors"
      >
        <div className="flex items-center space-x-3 min-w-0">
          <span className={`text-xs font-bold uppercase ${statusColor}`}>
            {record.status === 'applied' ? '\u2713' : record.status === 'failed' ? '\u2717' : '\u25CB'}
          </span>
          <DirectionBadge direction={record.direction} />
          <span className="text-sm text-gray-200 truncate">{record.message}</span>
        </div>
        <div className="flex items-center space-x-3 flex-shrink-0 ml-4">
          <span className="text-sm text-gray-400">{record.author}</span>
          {record.svn_rev && (
            <span className="text-xs font-mono text-blue-400">r{record.svn_rev}</span>
          )}
          {record.git_sha && (
            <span className="text-xs font-mono text-purple-400">{record.git_sha.substring(0, 8)}</span>
          )}
          <span className="text-xs text-gray-500">
            {new Date(record.synced_at).toLocaleString()}
          </span>
          <ChevronDown
            className={`w-4 h-4 text-gray-400 transition-transform ${expanded ? 'rotate-180' : ''}`}
          />
        </div>
      </button>
      {expanded && (
        <div className="px-6 pb-4 bg-gray-850">
          <div className="bg-gray-900 rounded-lg p-4 border border-gray-700">
            <div className="grid grid-cols-2 md:grid-cols-4 gap-4 mb-4 text-sm">
              <div>
                <span className="text-gray-500 text-xs uppercase">Record ID</span>
                <p className="font-mono text-gray-300 truncate">{record.id}</p>
              </div>
              <div>
                <span className="text-gray-500 text-xs uppercase">SVN Revision</span>
                <p className="font-mono text-blue-400">{record.svn_rev ? `r${record.svn_rev}` : 'N/A'}</p>
              </div>
              <div>
                <span className="text-gray-500 text-xs uppercase">Git SHA</span>
                <p className="font-mono text-purple-400">{record.git_sha || 'N/A'}</p>
              </div>
              <div>
                <span className="text-gray-500 text-xs uppercase">Status</span>
                <p className={statusColor + ' font-medium capitalize'}>{record.status}</p>
              </div>
            </div>
            <div className="grid grid-cols-2 md:grid-cols-3 gap-4 text-sm">
              <div>
                <span className="text-gray-500 text-xs uppercase">Author</span>
                <p className="text-gray-300">{record.author}</p>
              </div>
              <div>
                <span className="text-gray-500 text-xs uppercase">Committed</span>
                <p className="text-gray-300">{new Date(record.timestamp).toLocaleString()}</p>
              </div>
              <div>
                <span className="text-gray-500 text-xs uppercase">Synced At</span>
                <p className="text-gray-300">{new Date(record.synced_at).toLocaleString()}</p>
              </div>
            </div>
            <div className="mt-4">
              <span className="text-gray-500 text-xs uppercase">Commit Message</span>
              <div className="mt-1 bg-gray-800 rounded p-3 border border-gray-700">
                <pre className="text-sm text-gray-200 whitespace-pre-wrap font-mono">{record.message}</pre>
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function DirectionBadge({ direction }: { direction: string }) {
  const isToGit = direction === 'svn_to_git';
  return (
    <span
      className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
        isToGit
          ? 'bg-blue-900/50 text-blue-300'
          : 'bg-purple-900/50 text-purple-300'
      }`}
    >
      {isToGit ? 'SVN \u2192 Git' : 'Git \u2192 SVN'}
    </span>
  );
}

function ActionBadge({ action }: { action: string }) {
  const colors: Record<string, string> = {
    sync_cycle: 'bg-cyan-900/50 text-cyan-300',
    conflict_detected: 'bg-red-900/50 text-red-300',
    conflict_resolved: 'bg-green-900/50 text-green-300',
    sync_error: 'bg-red-900/50 text-red-300',
    webhook_received: 'bg-yellow-900/50 text-yellow-300',
    daemon_started: 'bg-emerald-900/50 text-emerald-300',
    auth_login: 'bg-indigo-900/50 text-indigo-300',
    config_updated: 'bg-orange-900/50 text-orange-300',
  };

  const label = action.replace(/_/g, ' ');

  return (
    <span
      className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
        colors[action] ?? 'bg-gray-700 text-gray-300'
      }`}
    >
      {label}
    </span>
  );
}
