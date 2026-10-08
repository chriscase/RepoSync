import React, { useEffect, useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useParams, useNavigate, useLocation, Link } from 'react-router-dom';
import { api, type Repository, type SyncStatus, type SyncRecord, type CommitMapEntry, type AuditEntry, type LatePairPlan, type PairRefreshPlan, type SkipCommitContext } from '../api';
import {
  type BranchPairRemovalNotice as RemovalNotice,
  isNotFoundError,
  removalDestination,
  repoDetailQueryKeys,
  readBranchPairRemovalNotice,
  persistBranchPairRemovalNotice,
  readPersistedBranchPairRemovalNotice,
} from '../branchPairRemoval';
import ImportProgressCard from '../components/ImportProgressCard';
import BranchPairRemovalNotice from '../components/BranchPairRemovalNotice';
import ManagedRemovalPanel from '../components/ManagedRemovalPanel';
import {
  type ManagedRemovalStatus,
  managedRemovalNeedsPoll,
  persistManagedRemovalReceipt,
  readManagedRemovalReceipt,
  clearManagedRemovalReceipt,
} from '../managedRemoval';
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
  allowed_paths: string;
  blocked_patterns: string;
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
    allowed_paths: repo.allowed_paths ? JSON.parse(repo.allowed_paths).join('\n') : '',
    blocked_patterns: repo.blocked_patterns ? JSON.parse(repo.blocked_patterns).join('\n') : '',
  };
}

const inputClass =
  'w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent';

const selectClass =
  'w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent';

export default function RepoDetail() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const location = useLocation();
  const queryClient = useQueryClient();
  const user = getStoredUser();
  const isAdmin = user?.role === 'admin';
  const routedNotice = readBranchPairRemovalNotice(location.state);
  const [localNotice, setLocalNotice] = useState<RemovalNotice | null>(null);
  const [retiredId, setRetiredId] = useState<string | null>(null);

  const [syncTriggered, setSyncTriggered] = useState(false);
  const [editing, setEditing] = useState(false);
  const [form, setForm] = useState<EditForm | null>(null);
  const [showDisableConfirm, setShowDisableConfirm] = useState(false);
  const [showRemoveConfirm, setShowRemoveConfirm] = useState(false);
  const [removalReceipt, setRemovalReceipt] = useState(readManagedRemovalReceipt());
  const [expandedAuditGroups, setExpandedAuditGroups] = useState<Set<number>>(new Set());
  const [expandedDetails, setExpandedDetails] = useState<Set<number>>(new Set());
  const [svnTestResult, setSvnTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [gitTestResult, setGitTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [svnTesting, setSvnTesting] = useState(false);
  const [gitTesting, setGitTesting] = useState(false);
  const [showBranchModal, setShowBranchModal] = useState(false);
  const [removalTarget, setRemovalTarget] = useState<Repository | null>(null);
  const [removeConfirmText, setRemoveConfirmText] = useState('');
  const [removeBranchOpts, setRemoveBranchOpts] = useState({ delete_git: false, delete_svn: false });
  const [branchForm, setBranchForm] = useState({
    svn_branch: '',
    git_branch: '',
    skip_import: false,
    auto_create_svn_branch: true,
    auto_create_git_branch: true,
  });
  const [branchSuccess, setBranchSuccess] = useState(false);
  const [branchPlan, setBranchPlan] = useState<LatePairPlan | null>(null);
  const [refreshPlan, setRefreshPlan] = useState<PairRefreshPlan | null>(null);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const [showSkipModal, setShowSkipModal] = useState(false);
  const [skipSelectedCount, setSkipSelectedCount] = useState(0);
  const [skipError, setSkipError] = useState<string | null>(null);

  const repoQuery = useQuery({
    queryKey: ['repo', id],
    queryFn: () => api.getRepo(id!),
    enabled: !!id && id !== retiredId,
    retry: (failureCount, error) => !isNotFoundError(error) && failureCount < 2,
    refetchInterval: (query) => (isNotFoundError(query.state.error) ? false : 5000),
    refetchOnWindowFocus: (query) => !isNotFoundError(query.state.error),
    refetchOnReconnect: (query) => !isNotFoundError(query.state.error),
  });
  const { data: repo, isLoading, isError, error } = repoQuery;
  const repoMissing = isNotFoundError(error);
  const detailLive = !!id && id !== retiredId && !repoMissing;

  // Status query scoped to this repo. Stop it once this detail is gone.
  const { data: status } = useQuery<SyncStatus>({
    queryKey: ['status', id],
    queryFn: () => fetch(`/api/status?repo_id=${id}`, { headers: { Authorization: `Bearer ${localStorage.getItem('session_token')}` } }).then(r => r.json()),
    refetchInterval: (query) => (detailLive && !isNotFoundError(query.state.error) ? 5000 : false),
    retry: (failureCount, queryError) => !isNotFoundError(queryError) && failureCount < 2,
    enabled: detailLive,
  });

  // Sync records
  const detailRetry = (failureCount: number, queryError: unknown) =>
    !isNotFoundError(queryError) && failureCount < 2;

  const { data: syncRecords } = useQuery({
    queryKey: ['sync-records', id],
    queryFn: () => api.getSyncRecords(20, id),
    enabled: detailLive,
    refetchInterval: detailLive ? 5000 : false,
    retry: detailRetry,
  });

  // Commit map
  const { data: commitMap } = useQuery({
    queryKey: ['commit-map', id],
    queryFn: () => api.getCommitMap(15, id),
    enabled: detailLive,
    refetchInterval: detailLive ? 5000 : false,
    retry: detailRetry,
  });

  // Audit log
  const { data: auditLog } = useQuery({
    queryKey: ['audit', id],
    queryFn: () => api.getAuditLog(10, undefined, undefined, id),
    enabled: detailLive,
    refetchInterval: detailLive ? 5000 : false,
    retry: detailRetry,
  });

  // Credential status
  const { data: credStatus } = useQuery({
    queryKey: ['repo-credentials', id],
    queryFn: () => api.getRepoCredentials(id!),
    enabled: detailLive,
    refetchInterval: detailLive ? 5000 : false,
    retry: detailRetry,
  });

  // Branch pairs
  const { data: branchPairs } = useQuery({
    queryKey: ['branch-pairs', id],
    queryFn: () => api.listBranchPairs(id!),
    enabled: detailLive,
    refetchInterval: detailLive ? 5000 : false,
    retry: detailRetry,
  });

  const skipContextEnabled = detailLive && isAdmin && status?.state === 'error_paused';
  const { data: skipContextResponse } = useQuery({
    queryKey: ['skip-commit-context', id],
    queryFn: () => api.getSkipCommitContext(id!),
    enabled: skipContextEnabled,
    refetchInterval: skipContextEnabled ? 5000 : false,
    retry: detailRetry,
  });
  const skipContext: SkipCommitContext | null = skipContextResponse?.context ?? null;

  const skipMutation = useMutation({
    mutationFn: (selected: string[]) => {
      if (!skipContext?.observed_remote_tip) {
        throw new Error('Observed remote tip is unavailable for exact skip');
      }
      return api.skipCommit(id!, {
        pinned_cursor: skipContext.pinned_cursor,
        selected_commits: selected,
        expected_remote_tip: skipContext.observed_remote_tip,
        expected_bridge_tip: skipContext.observed_bridge_tip ?? undefined,
        reason: 'operator_exact_skip',
      });
    },
    onSuccess: () => {
      setShowSkipModal(false);
      setSkipSelectedCount(0);
      setSkipError(null);
      queryClient.invalidateQueries({ queryKey: ['repo-status', id] });
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
      queryClient.invalidateQueries({ queryKey: ['status', id] });
      queryClient.invalidateQueries({ queryKey: ['skip-commit-context', id] });
    },
    onError: (error: Error) => {
      setSkipError(error.message);
    },
  });

  const branchMutation = useMutation({
    mutationFn: (data: { svn_branch: string; git_branch: string; skip_import: boolean; auto_create_svn_branch: boolean; auto_create_git_branch: boolean }) =>
      api.createBranchPair(id!, {
        ...data,
        dry_run: true,
        preview: true,
      }),
    onSuccess: (plan) => {
      setBranchPlan(plan);
      setBranchSuccess(true);
    },
  });

  const refreshMutation = useMutation({
    mutationFn: (args: { repoId: string; operation: 'update_pair_from_parent' | 'reanchor' }) =>
      api.previewPairRefresh(args.repoId, { operation: args.operation, execute: false }),
    onSuccess: (plan) => {
      setRefreshPlan(plan);
      setRefreshError(null);
    },
    onError: (err: Error) => {
      setRefreshPlan(null);
      setRefreshError(err.message);
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

  const disableMutation = useMutation({
    mutationFn: () => api.disableRepo(id!),
    onSuccess: () => {
      setShowDisableConfirm(false);
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
      queryClient.invalidateQueries({ queryKey: ['repos'] });
    },
  });

  const removalSubject = removalTarget ?? repo;
  const removalSubjectId = removalSubject?.id ?? id;

  const removeMutation = useMutation({
    mutationFn: async () => {
      const target = removalTarget ?? repo;
      if (!target?.id) {
        throw new Error('No repository selected for removal');
      }
      const remoteOpts = target.parent_id ? removeBranchOpts : undefined;
      const result = await api.removeManagedRepo(target.id, remoteOpts);
      return { result, target };
    },
    onSuccess: async ({ result, target }) => {
      setShowRemoveConfirm(false);
      setRemovalTarget(null);
      setRemoveConfirmText('');
      const receipt = {
        repoId: target.id,
        operationId: result.operation_id,
        state: result.state,
        message: result.message,
        recovery: result.recovery,
      };
      persistManagedRemovalReceipt(receipt);
      setRemovalReceipt(receipt);
      queryClient.setQueryData(['managed-removal', target.id], result);
      queryClient.invalidateQueries({ queryKey: ['managed-removal', target.id] });
      queryClient.invalidateQueries({ queryKey: ['repos'] });
      if (target.id !== id) {
        queryClient.invalidateQueries({ queryKey: ['branch-pairs', id] });
      }
      if (result.state === 'completed' && result.registration_listed === false) {
        const notice: RemovalNotice = {
          outcome: 'completed',
          message: result.message,
          warnings: [],
          operationId: result.operation_id,
          name: target.name,
        };
        let parentExists: boolean | null = null;
        if (target.parent_id) {
          try {
            await api.getRepo(target.parent_id);
            parentExists = true;
          } catch (lookupError) {
            parentExists = isNotFoundError(lookupError) ? false : null;
          }
        }
        const destination = removalDestination({
          viewedId: id!,
          targetId: target.id,
          parentId: target.parent_id,
          parentExists,
        });
        if (target.id === id) {
          for (const queryKey of repoDetailQueryKeys(target.id)) {
            await queryClient.cancelQueries({ queryKey });
          }
          setRetiredId(target.id);
          if (target.parent_id) {
            persistBranchPairRemovalNotice(notice);
            navigate(destination ?? '/repos', { replace: true, state: { branchPairRemoval: notice } });
          } else {
            navigate('/repos', { replace: true });
          }
        } else {
          persistBranchPairRemovalNotice(notice);
          setLocalNotice(notice);
        }
      }
    },
  });

  const restoreMutation = useMutation({
    mutationFn: () => api.restoreManagedRepo(id!),
    onSuccess: () => {
      clearManagedRemovalReceipt();
      setRemovalReceipt(null);
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
      queryClient.invalidateQueries({ queryKey: ['repos'] });
      queryClient.invalidateQueries({ queryKey: ['managed-removal', id] });
    },
  });

  const removalPreviewQuery = useQuery({
    queryKey: ['removal-preview', removalSubjectId],
    queryFn: () => api.getRemovalDependencyPreview(removalSubjectId!),
    enabled: !!removalSubjectId && isAdmin && showRemoveConfirm && detailLive,
    retry: false,
  });
  const removalDependencyPreview = removalPreviewQuery.data?.dependency_preview;

  const removalStatusQuery = useQuery<ManagedRemovalStatus | null>({
    queryKey: ['managed-removal', id],
    queryFn: () => api.getManagedRemoval(id!),
    enabled:
      !!id
      && isAdmin
      && detailLive
      && (showRemoveConfirm
        || removeMutation.isPending
        || removeMutation.isSuccess
        || removalReceipt?.repoId === id),
    retry: false,
    refetchInterval: (query) => {
      const state = query.state.data?.state;
      return managedRemovalNeedsPoll(state) ? 2000 : false;
    },
  });

  const removalPanelStatus: ManagedRemovalStatus | undefined = removalStatusQuery.data
    ?? (removalReceipt && removalReceipt.repoId === id
      ? {
          ok: removalReceipt.state === 'completed',
          action: 'managed_remove',
          state: removalReceipt.state,
          operation_id: removalReceipt.operationId,
          message: removalReceipt.message,
          remote_git: 'untouched',
          remote_svn: 'untouched',
          restore_supported: removalReceipt.recovery?.restore_supported ?? false,
          retryable: removalReceipt.state !== 'completed',
          registration_listed: removalReceipt.state !== 'completed',
          recovery: removalReceipt.recovery,
        }
      : undefined);

  const removalPreviewReady = !!removalDependencyPreview
    && !removalPreviewQuery.isLoading
    && !removalPreviewQuery.isFetching
    && !removalPreviewQuery.isError;
  const childRemovalConfirm = !!(removalSubject?.parent_id);
  const removalConfirmLabel = childRemovalConfirm ? 'Remove branch pair' : 'Remove from RepoSync';

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
    // Save repo config (exclude credential fields, convert path rules to JSON)
    const { svn_password, git_token, allowed_paths, blocked_patterns, ...repoData } = form;
    const pathData = {
      ...repoData,
      allowed_paths: allowed_paths.trim()
        ? JSON.stringify(allowed_paths.split('\n').map(s => s.trim()).filter(Boolean))
        : null,
      blocked_patterns: blocked_patterns.trim()
        ? JSON.stringify(blocked_patterns.split('\n').map(s => s.trim()).filter(Boolean))
        : null,
    };
    updateMutation.mutate(pathData);
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
    } catch (e: unknown) {
      setSvnTestResult({ ok: false, message: e instanceof Error ? e.message : String(e) });
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
    } catch (e: unknown) {
      setGitTestResult({ ok: false, message: e instanceof Error ? e.message : String(e) });
    } finally {
      setGitTesting(false);
    }
  }

  function setField<K extends keyof EditForm>(key: K, value: EditForm[K]) {
    setForm((prev) => (prev ? { ...prev, [key]: value } : prev));
  }

  useEffect(() => {
    setLocalNotice(null);
    if (removalReceipt && removalReceipt.repoId !== id) {
      setRemovalReceipt(null);
    }
  }, [id, removalReceipt]);

  useEffect(() => {
    if (!retiredId || retiredId === id) return;
    for (const queryKey of repoDetailQueryKeys(retiredId)) {
      queryClient.removeQueries({ queryKey });
    }
    setRetiredId(null);
  }, [id, retiredId, queryClient]);

  const removalNotice = localNotice ?? routedNotice ?? readPersistedBranchPairRemovalNotice();

  if (repoMissing) {
    return (
        <div className="max-w-lg mx-auto py-16 text-center space-y-4" data-testid="repo-not-found">
          {removalNotice && <BranchPairRemovalNotice notice={removalNotice} />}
          <h1 className="text-xl font-semibold text-gray-100">Repository not found</h1>
          <p className="text-sm text-gray-400">
            This repository or branch pair is no longer available. It may have been removed.
          </p>
          <Link
            to="/repos"
            data-testid="repo-not-found-repositories"
            className="inline-flex items-center justify-center px-4 py-2 rounded-lg bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium"
          >
            Back to repositories
          </Link>
        </div>
    );
  }

  if (!repo && (isLoading || retiredId === id)) {
    return <div className="text-center py-8 text-gray-400">Loading repository...</div>;
  }

  if (isError || !repo) {
    return (
      <div className="text-center py-8 space-y-4">
        <p className="text-red-400">
          Error loading repository: {error?.message ?? 'Not found'}
        </p>
        <Link to="/repos" className="text-sm text-blue-400 hover:text-blue-300">
          Back to repositories
        </Link>
      </div>
    );
  }

  const records = syncRecords?.entries ?? [];
  const cmEntries = commitMap?.entries ?? [];

  return (
    <div className="space-y-6" data-testid="repo-detail" data-repo-id={id}>
      {removalNotice && <BranchPairRemovalNotice notice={removalNotice} />}
      {isAdmin && removalPanelStatus && (
        <ManagedRemovalPanel
          status={removalPanelStatus}
          onRetry={
            removalPanelStatus.retryable
              ? () => removeMutation.mutate()
              : undefined
          }
          retryPending={removeMutation.isPending}
          onRestore={
            removalPanelStatus.recovery?.restore_supported
              ? () => restoreMutation.mutate()
              : undefined
          }
          restorePending={restoreMutation.isPending}
        />
      )}
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
          <h1 className="text-2xl font-bold text-gray-100" data-testid="repo-detail-heading">{repo.name}</h1>
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
          {(repo?.allowed_paths || repo?.blocked_patterns) && (
            <a
              href={`/api/repos/${id}/hooks/pre-commit`}
              download="pre-commit"
              className="inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-gray-600 text-gray-300 hover:text-white hover:border-gray-500 text-sm font-medium transition-colors"
              title="Download Git pre-commit hook script"
            >
              <GitBranch className="w-4 h-4" />
              Git Hook
            </a>
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
            <div className="col-span-full grid grid-cols-2 gap-4">
              <div>
                <label className="block text-sm text-gray-400 mb-1">Allowed SVN Paths <span className="text-gray-600">(one per line)</span></label>
                <textarea
                  className="w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 font-mono placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                  rows={3}
                  value={form.allowed_paths}
                  onChange={(e) => setField('allowed_paths', e.target.value)}
                  placeholder={"source/\nconfig/"}
                />
                <p className="text-xs text-gray-500 mt-1">Files must be under these path prefixes. Leave empty for no restriction.</p>
              </div>
              <div>
                <label className="block text-sm text-gray-400 mb-1">Blocked Patterns <span className="text-gray-600">(one per line)</span></label>
                <textarea
                  className="w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 font-mono placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                  rows={3}
                  value={form.blocked_patterns}
                  onChange={(e) => setField('blocked_patterns', e.target.value)}
                  placeholder={"*.exe\ntemp/"}
                />
                <p className="text-xs text-gray-500 mt-1">Files matching these patterns will be excluded from SVN sync.</p>
              </div>
            </div>
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

      {/* Circuit Breaker: Error Paused Banner */}
      {status?.state === 'error_paused' && isAdmin && (
        <div className="bg-red-900/30 border border-red-700 rounded-lg p-4 flex items-center justify-between">
          <div>
            <p className="text-red-300 font-medium">Sync Paused — Permanent Error</p>
            <p className="text-sm text-red-400 mt-1">
              The sync engine encountered repeated permanent errors and has been automatically paused.
              Skip selected pending commits using observed tips; this does not adopt live HEAD or skip unselected work.
              {skipContext?.observed_remote_tip && (
                <span className="block mt-1 text-red-300/80 font-mono text-xs">
                  cursor {skipContext.pinned_cursor.slice(0, 8)} · remote {skipContext.observed_remote_tip.slice(0, 8)}
                  {skipContext.observed_bridge_tip ? ` · bridge ${skipContext.observed_bridge_tip.slice(0, 8)}` : ''}
                </span>
              )}
            </p>
          </div>
          <div className="flex gap-2 ml-4 flex-shrink-0">
            <button
              type="button"
              disabled={!skipContext?.observed_remote_tip || (skipContext.pending_commits.filter((c) => !c.excluded).length === 0)}
              title="Exclude selected pending commits at observed tips. Does not adopt live HEAD."
              data-testid="skip-commit-button"
              onClick={() => {
                setSkipError(null);
                setSkipSelectedCount(0);
                setShowSkipModal(true);
              }}
              className="px-3 py-1.5 rounded-lg bg-yellow-600 hover:bg-yellow-700 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm font-medium transition-colors"
            >
              Skip Commit
            </button>
            <button
              onClick={async () => {
                await api.retryRepo(id!);
                queryClient.invalidateQueries({ queryKey: ['repo-status', id] });
                queryClient.invalidateQueries({ queryKey: ['repo', id] });
              }}
              className="px-3 py-1.5 rounded-lg bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium transition-colors"
            >
              Retry
            </button>
          </div>
        </div>
      )}

      {showSkipModal && skipContext && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" data-testid="skip-commit-modal">
          <div className="bg-gray-800 border border-gray-600 rounded-lg p-6 max-w-lg w-full mx-4 shadow-xl">
            <h3 className="text-lg font-semibold text-gray-100 mb-2">Skip selected pending commits</h3>
            <p className="text-sm text-gray-400 mb-4">
              Select the oldest contiguous pending prefix to exclude. Unselected commits remain pending.
              Observed remote tip: <span className="font-mono text-gray-300">{skipContext.observed_remote_tip?.slice(0, 12) ?? 'unavailable'}</span>
            </p>
            <div className="space-y-2 mb-4 max-h-48 overflow-y-auto">
              {skipContext.pending_commits.filter((c) => !c.excluded).map((commit, index) => (
                <label key={commit.sha} className="flex items-start gap-2 text-sm text-gray-200">
                  <input
                    type="checkbox"
                    checked={index < skipSelectedCount}
                    onChange={() => setSkipSelectedCount(index + 1)}
                    data-testid={`skip-commit-select-${index}`}
                  />
                  <span>
                    <span className="font-mono text-xs text-gray-400">{commit.sha.slice(0, 8)}</span>
                    {' '}{commit.subject}
                  </span>
                </label>
              ))}
            </div>
            {skipError && <p className="text-sm text-red-400 mb-3">{skipError}</p>}
            <div className="flex justify-end gap-2">
              <button
                type="button"
                onClick={() => { setShowSkipModal(false); setSkipError(null); }}
                className="px-3 py-1.5 rounded-lg bg-gray-700 hover:bg-gray-600 text-sm text-gray-200"
              >
                Cancel
              </button>
              <button
                type="button"
                disabled={skipSelectedCount === 0 || skipMutation.isPending}
                data-testid="skip-commit-confirm"
                onClick={() => {
                  const pending = skipContext.pending_commits.filter((c) => !c.excluded);
                  const selected = pending.slice(0, skipSelectedCount).map((c) => c.sha);
                  skipMutation.mutate(selected);
                }}
                className="px-3 py-1.5 rounded-lg bg-yellow-600 hover:bg-yellow-700 disabled:opacity-50 text-sm text-white font-medium"
              >
                {skipMutation.isPending ? 'Skipping…' : 'Skip selected'}
              </button>
            </div>
          </div>
        </div>
      )}

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
          onClick={() => navigate(`/audit?success=false&repo_id=${id}`)}
          onClear={
            (status?.total_errors ?? 0) > 0
              ? async () => {
                  await api.resetErrors(id);
                  queryClient.invalidateQueries({ queryKey: ['repo-status', id] });
                }
              : undefined
          }
        />
      </div>

      {/* Import Progress */}
      {detailLive && <ImportProgressCard repoId={id} repoName={repo?.name} />}

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

      {repo?.parent_id && (
        <div className="bg-gray-800 shadow rounded-lg border border-gray-700 p-6">
          <h2 className="text-lg font-semibold text-gray-100">Update pair from parent</h2>
          <p className="text-sm text-gray-400 mt-1">
            Read-only preview. Published Git commits and SVN revisions stay. Unsynced work on either side is reported and is not discarded. Running the refresh is not available in this slice.
          </p>
          <p className="text-sm text-amber-200/90 mt-2">
            Re-anchor / recreate is a separate mode and is NOT IMPLEMENTED. It does not reset, force-push, or delete the old SVN path.
          </p>
          <div className="flex flex-wrap gap-2 mt-4">
            <button
              data-testid="preview-update-from-parent"
              onClick={() => refreshMutation.mutate({ repoId: id!, operation: 'update_pair_from_parent' })}
              disabled={refreshMutation.isPending}
              className="inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium bg-blue-600 hover:bg-blue-700 disabled:opacity-50 text-white"
            >
              <RefreshCw className="w-3.5 h-3.5" />
              {refreshMutation.isPending ? 'Previewing...' : 'Preview update from parent'}
            </button>
            <button
              data-testid="reanchor-not-implemented"
              onClick={() => refreshMutation.mutate({ repoId: id!, operation: 'reanchor' })}
              disabled={refreshMutation.isPending}
              className="inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium border border-amber-700 text-amber-200 hover:bg-amber-900/30 disabled:opacity-50"
            >
              Re-anchor pair (not implemented)
            </button>
          </div>
        </div>
      )}

      {(refreshPlan || refreshError) && (
        <div className="bg-gray-800 shadow rounded-lg border border-gray-700 p-6" data-testid="pair-refresh-result">
          {refreshError && (
            <div className="bg-amber-900/30 border border-amber-700 rounded-lg p-3 text-amber-100 text-sm">
              {refreshError}
            </div>
          )}
          {refreshPlan && (
            <div className="text-sm text-gray-300 space-y-2">
              <p className="text-gray-100 font-medium">Update-pair preview · not executed</p>
              <p className="font-mono text-xs text-gray-400 break-all">plan {refreshPlan.plan_digest}</p>
              <p>
                Git {refreshPlan.git.pair_branch} {refreshPlan.git.pair_tip?.slice(0, 12) || 'unpinned'}
                {' · parent '}
                {refreshPlan.git.parent_branch} {refreshPlan.git.parent_tip?.slice(0, 12) || 'unpinned'}
              </p>
              <p>
                SVN {refreshPlan.svn.uuid || 'uuid unpinned'} · {refreshPlan.svn.pair_path} r{refreshPlan.svn.pair_revision ?? '?'}
                {' · parent '}
                {refreshPlan.svn.parent_path} r{refreshPlan.svn.parent_revision ?? '?'}
              </p>
              <p>
                Generation {refreshPlan.pair_generation} · {refreshPlan.policy_version} · pending Git pair {refreshPlan.pending.pair_git.count} / parent {refreshPlan.pending.parent_git.count}
                {' · pending SVN pair '}{refreshPlan.pending.pair_svn.count} / parent {refreshPlan.pending.parent_svn.count}
              </p>
              {refreshPlan.pending.pair_git.rewritten && (
                <p className="text-amber-200">Pair lineage is rewritten. Those commits are not counted as new work and are not discarded.</p>
              )}
              {refreshPlan.conflicts.length > 0 && (
                <p className="text-amber-200">Conflicts: {refreshPlan.conflicts.join(', ')}. Resolution is not in this slice.</p>
              )}
              <p>{refreshPlan.intended_result.summary}</p>
              <p className="text-xs text-gray-500">
                Discards unsynced work: {String(refreshPlan.intended_result.discards_unsynced_work)}. Execute: {refreshPlan.execute_status}. Re-anchor: {refreshPlan.reanchor_status}.
              </p>
            </div>
          )}
        </div>
      )}

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
        {branchSuccess && branchPlan && (
          <div className="mx-6 mb-3 bg-blue-900/30 border border-blue-700 rounded-lg p-3 text-blue-200 text-sm">
            Preview only — pair stays preparing. Git tip {branchPlan.git_tip?.slice(0, 8) || 'unknown'},
            SVN baseline r{branchPlan.svn_source_revision ?? '?'},
            {branchPlan.pending_git.count} pending Git commit{branchPlan.pending_git.count === 1 ? '' : 's'}.
            Replay/publish is not enabled in this slice.
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
                  {isAdmin && <th className="px-6 py-3 text-right text-xs font-medium text-gray-400 uppercase"></th>}
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-700">
                {(branchPairs ?? []).map((bp) => (
                  <tr
                    key={bp.id}
                    data-testid={`branch-pair-row-${bp.id}`}
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
                    {isAdmin && (
                      <td className="px-6 py-3 text-right">
                        <div className="inline-flex items-center gap-2">
                          <button
                            data-testid={`preview-refresh-${bp.id}`}
                            onClick={(e) => {
                              e.stopPropagation();
                              refreshMutation.mutate({ repoId: bp.id, operation: 'update_pair_from_parent' });
                            }}
                            className="text-xs text-blue-300 hover:text-blue-200"
                            title="Read-only update-from-parent preview"
                          >
                            Preview update
                          </button>
                          <button
                            data-testid={`reanchor-${bp.id}`}
                            onClick={(e) => {
                              e.stopPropagation();
                              refreshMutation.mutate({ repoId: bp.id, operation: 'reanchor' });
                            }}
                            className="text-xs text-amber-300 hover:text-amber-200"
                            title="Re-anchor is NOT IMPLEMENTED"
                          >
                            Re-anchor
                          </button>
                          <button
                            data-testid={`delete-child-pair-${bp.id}`}
                            onClick={(e) => {
                              e.stopPropagation();
                              setRemovalTarget(bp);
                              setRemoveConfirmText('');
                              setRemoveBranchOpts({ delete_git: false, delete_svn: false });
                              removeMutation.reset();
                              setShowRemoveConfirm(true);
                            }}
                            className="text-gray-500 hover:text-red-400 transition-colors p-1"
                            title="Delete branch pair"
                          >
                            <Trash2 className="w-4 h-4" />
                          </button>
                        </div>
                      </td>
                    )}
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
              <h2 className="text-lg font-semibold text-gray-100">Preview Branch Pair</h2>
              <button
                onClick={() => { setShowBranchModal(false); setBranchForm({ svn_branch: '', git_branch: '', skip_import: false, auto_create_svn_branch: true, auto_create_git_branch: true }); setBranchPlan(null); branchMutation.reset(); }}
                className="text-gray-400 hover:text-gray-200 transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            <div className="p-6 space-y-4">
              {branchMutation.isError && (
                <div className="bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm">
                  Pairing refused: {branchMutation.error?.message}
                </div>
              )}
              {branchPlan && (
                <div className="bg-gray-900/70 border border-gray-600 rounded-lg p-3 text-xs text-gray-300 space-y-1 font-mono">
                  <div>mode: {branchPlan.mode} · state: {branchPlan.pair_state}</div>
                  <div>git tip: {branchPlan.git_tip || 'unknown'}</div>
                  <div>SVN source: r{branchPlan.svn_source_revision ?? 'unknown'} · copy from r{branchPlan.proposed_svn_copy_source_revision ?? 'n/a'}</div>
                  <div>pending Git: {branchPlan.pending_git.count} · published: {String(branchPlan.published)} · scheduler: {String(branchPlan.scheduler_active)}</div>
                  {branchPlan.existing_svn_target.exists && (
                    <div className="text-amber-300">existing SVN target is not equivalent</div>
                  )}
                  {branchPlan.skip_import_note && <div className="text-amber-200">{branchPlan.skip_import_note}</div>}
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
                  const invalid = /[^a-zA-Z0-9._/-]/.test(v);
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
                  const invalid = /[^a-zA-Z0-9._/-]/.test(v);
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
                <label className="block text-sm text-gray-400 mb-2">Pairing mode</label>
                <div className="space-y-2">
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="importMode"
                      checked={!branchForm.skip_import}
                      onChange={() => setBranchForm(prev => ({ ...prev, skip_import: false }))}
                      className="text-blue-600"
                    />
                    <span className="text-sm text-gray-300">Preview from verified SVN baseline <span className="text-gray-500">(this slice)</span></span>
                  </label>
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="importMode"
                      checked={branchForm.skip_import}
                      onChange={() => setBranchForm(prev => ({ ...prev, skip_import: true }))}
                      className="text-blue-600"
                    />
                    <span className="text-sm text-gray-300">Start from now <span className="text-amber-400">(unsafe — refused)</span></span>
                  </label>
                </div>
              </div>
            </div>
            <div className="flex items-center justify-end gap-3 p-6 border-t border-gray-700">
              <button
                onClick={() => { setShowBranchModal(false); setBranchForm({ svn_branch: '', git_branch: '', skip_import: false, auto_create_svn_branch: true, auto_create_git_branch: true }); setBranchPlan(null); branchMutation.reset(); }}
                className="px-4 py-2 rounded-lg border border-gray-600 text-gray-300 hover:text-white text-sm font-medium transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={() => branchMutation.mutate(branchForm)}
                disabled={branchMutation.isPending || !branchForm.svn_branch.trim() || !branchForm.git_branch.trim()
                  || /[^a-zA-Z0-9._/-]/.test(branchForm.git_branch) || /[^a-zA-Z0-9._/-]/.test(branchForm.svn_branch)
                  || branchForm.git_branch.includes('..') || branchForm.svn_branch.includes('..')
                  || branchForm.git_branch.includes('//') || branchForm.git_branch.startsWith('/') || branchForm.git_branch.endsWith('/')
                  || branchForm.git_branch.startsWith('-') || branchForm.git_branch === 'HEAD' || branchForm.git_branch.endsWith('.lock')
                }
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-purple-600 hover:bg-purple-700 disabled:opacity-50 text-white text-sm font-medium transition-colors"
              >
                <GitBranch className="w-4 h-4" />
                {branchMutation.isPending ? 'Previewing...' : 'Preview Pair'}
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
                <p className="text-sm text-gray-500 mt-0.5">Unavailable during safe import cancellation. Request a reviewed recovery plan for an existing repository.</p>
              </div>
            </div>
            {!repo?.parent_id && (
              <div className="flex items-center justify-between">
                <div>
                  <p className="text-sm text-gray-300 font-medium">Pause / disable sync</p>
                  <p className="text-sm text-gray-500 mt-0.5">
                    Stops scheduling. Registration, mappings, secrets, local files, and remotes stay.
                  </p>
                </div>
                <button
                  data-testid="pause-disable-repo"
                  onClick={() => setShowDisableConfirm(true)}
                  className="inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-amber-700 text-amber-300 hover:bg-amber-900/30 text-sm font-medium transition-colors"
                >
                  <Power className="w-4 h-4" />
                  Pause / disable
                </button>
              </div>
            )}
            <div className="flex items-center justify-between">
              <div>
                <p className="text-sm text-gray-300 font-medium">
                  {repo?.parent_id ? 'Remove branch pair' : 'Remove from RepoSync'}
                </p>
                <p className="text-sm text-gray-500 mt-0.5">
                  {repo?.parent_id
                    ? 'Remove this pair from RepoSync. Remote Git/SVN deletion is opt-in in the dialog.'
                    : 'Stop work, clean only owned local data, and drop the registration from active views. Remotes and history stay unless you separately authorize branch deletion.'}
                </p>
              </div>
              {repo?.parent_id ? (
                <button
                  data-testid="delete-viewed-branch-pair"
                  onClick={() => {
                    setRemovalTarget(null);
                    setRemoveConfirmText('');
                    setRemoveBranchOpts({ delete_git: false, delete_svn: false });
                    removeMutation.reset();
                    setShowRemoveConfirm(true);
                  }}
                  className="inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-red-700 text-red-400 hover:bg-red-900/30 text-sm font-medium transition-colors"
                >
                  <Trash2 className="w-4 h-4" />
                  Remove branch pair
                </button>
              ) : (
                <button
                  data-testid="remove-from-reposync"
                  onClick={() => {
                    setRemovalTarget(null);
                    setRemoveConfirmText('');
                    setRemoveBranchOpts({ delete_git: false, delete_svn: false });
                    removeMutation.reset();
                    setShowRemoveConfirm(true);
                  }}
                  className="inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-red-700 text-red-400 hover:bg-red-900/30 text-sm font-medium transition-colors"
                >
                  <Trash2 className="w-4 h-4" />
                  Remove from RepoSync
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {showDisableConfirm && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4">
          <div className="bg-gray-800 border border-gray-700 rounded-lg p-6 max-w-md w-full shadow-xl">
            <h3 className="text-lg font-semibold text-gray-100 mb-2">Pause / disable sync</h3>
            <p className="text-sm text-gray-400 mb-6">
              Disable <span className="font-semibold text-gray-200">{repo.name}</span>? This does not remove
              data or touch remotes. You can re-enable later.
            </p>
            {disableMutation.isError && (
              <div className="bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm mb-4">
                Failed to disable: {disableMutation.error?.message}
              </div>
            )}
            <div className="flex items-center justify-end gap-3">
              <button
                onClick={() => setShowDisableConfirm(false)}
                className="px-4 py-2 rounded-lg border border-gray-600 text-gray-300 hover:text-white text-sm font-medium transition-colors"
              >
                Cancel
              </button>
              <button
                data-testid="confirm-pause-disable"
                onClick={() => disableMutation.mutate()}
                disabled={disableMutation.isPending}
                className="px-4 py-2 rounded-lg bg-amber-600 hover:bg-amber-700 disabled:opacity-50 text-white text-sm font-medium transition-colors"
              >
                {disableMutation.isPending ? 'Disabling…' : 'Pause / disable'}
              </button>
            </div>
          </div>
        </div>
      )}

      {showRemoveConfirm && removalSubject && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4" data-testid="managed-remove-modal">
          <div className="bg-gray-800 border border-gray-700 rounded-lg p-6 max-w-md w-full shadow-xl">
            <h3 className="text-lg font-semibold text-gray-100 mb-2">{removalConfirmLabel}</h3>
            <p className="text-sm text-gray-400 mb-4">
              Remove <span className="font-semibold text-gray-200">{removalSubject.name}</span> from active listings and
              clean only RepoSync-owned local data. Remote Git and SVN history are not deleted unless you opt in below.
              {childRemovalConfirm
                ? ' Managed removal keeps parent and sibling data unless preview shows otherwise.'
                : ' Completed removals can be restored while recovery metadata remains.'}
            </p>
            {childRemovalConfirm && (
              <>
                <div className="text-xs text-gray-400 border border-gray-600 rounded p-2 space-y-1 mb-4" data-testid="branch-remote-deletion-preview">
                  <p>Git ref: <span className="font-mono text-gray-200">{removalSubject.git_branch}</span> — {removeBranchOpts.delete_git ? 'will be deleted on remote when authorized' : 'left on remote'}</p>
                  <p>SVN path: <span className="font-mono text-gray-200">{removalSubject.svn_branch}</span> — {removeBranchOpts.delete_svn ? 'will be deleted on remote when authorized (history retained)' : 'left on remote'}</p>
                </div>
                <div className="space-y-2 mb-4">
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input type="checkbox" checked={removeBranchOpts.delete_git}
                      data-testid="delete-git-opt"
                      onChange={(e) => setRemoveBranchOpts((p) => ({ ...p, delete_git: e.target.checked }))}
                      className="rounded border-gray-600 bg-gray-700 text-red-600" />
                    <span className="text-sm text-gray-300">Delete Git branch <span className="text-gray-500 font-mono">({removalSubject.git_branch})</span></span>
                  </label>
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input type="checkbox" checked={removeBranchOpts.delete_svn}
                      data-testid="delete-svn-opt"
                      onChange={(e) => setRemoveBranchOpts((p) => ({ ...p, delete_svn: e.target.checked }))}
                      className="rounded border-gray-600 bg-gray-700 text-red-600" />
                    <span className="text-sm text-gray-300">Delete SVN branch <span className="text-gray-500 font-mono">({removalSubject.svn_branch})</span></span>
                  </label>
                </div>
                <div className="mb-4">
                  <p className="text-sm text-gray-400 mb-2">
                    Type <span className="font-mono text-yellow-300">{removalSubject.git_branch}</span> to confirm:
                  </p>
                  <input
                    type="text"
                    data-testid="delete-branch-confirm-input"
                    className="w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-red-500"
                    value={removeConfirmText}
                    onChange={(e) => setRemoveConfirmText(e.target.value)}
                    placeholder={removalSubject.git_branch}
                  />
                </div>
              </>
            )}
            {removalPreviewQuery.isLoading && (
              <p className="text-sm text-gray-400 mb-4" data-testid="removal-preview-loading">
                Loading dependency preview…
              </p>
            )}
            {removalDependencyPreview && (
              <div
                className="text-sm text-gray-300 mb-4 space-y-2 border border-gray-600 rounded-lg p-3 bg-gray-900/40"
                data-testid="managed-removal-dependency-preview"
              >
                {removalDependencyPreview.parent_removal_blocked && (
                  <p className="text-amber-300" data-testid="child-dependency-refusal-preview">
                    {removalDependencyPreview.block_reason}
                  </p>
                )}
                {removalDependencyPreview.children.length > 0 && (
                  <div>
                    <p className="text-gray-400 font-medium">Child branch pairs</p>
                    <ul className="list-disc list-inside text-gray-300">
                      {removalDependencyPreview.children.map((child) => (
                        <li key={child.id}>
                          {child.name} ({child.git_branch} / {child.svn_branch})
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
                {removalDependencyPreview.credentials.length > 0 && (
                  <div>
                    <p className="text-gray-400 font-medium">Credentials</p>
                    <ul className="list-disc list-inside text-gray-300 text-xs font-mono">
                      {removalDependencyPreview.credentials.map((cred) => (
                        <li key={cred.key}>
                          {cred.key} — {cred.action}
                          {cred.retained_for_repo_ids.length > 0
                            ? ` (other registrations keep their own keys: ${cred.retained_for_repo_ids.join(', ')})`
                            : ''}
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
                <p className="text-gray-400 text-xs">
                  Local path removed: <span className="font-mono text-gray-200">{removalDependencyPreview.managed_local_path}</span>
                  {removalDependencyPreview.sibling_local_paths_preserved.length > 0 && (
                    <>
                      {' '}
                      · preserved:{' '}
                      {removalDependencyPreview.sibling_local_paths_preserved.join(', ')}
                    </>
                  )}
                </p>
                {removalDependencyPreview.shared_git_registrations.length > 0 && (
                  <div>
                    <p className="text-gray-400 font-medium">Shared remote registrations</p>
                    <ul className="list-disc list-inside text-gray-300 text-xs">
                      {removalDependencyPreview.shared_git_registrations.map((other) => (
                        <li key={other.id}>
                          {other.name} ({other.relationship})
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </div>
            )}
            {removalPreviewQuery.isError && (
              <div className="text-sm text-red-300 mb-4 space-y-2">
                <p data-testid="removal-preview-error">
                  Could not load dependency preview: {removalPreviewQuery.error?.message}
                </p>
                <button
                  type="button"
                  data-testid="removal-preview-retry"
                  onClick={() => removalPreviewQuery.refetch()}
                  className="px-3 py-1.5 rounded-md border border-red-700 text-red-200 text-xs"
                >
                  Retry preview
                </button>
              </div>
            )}
            {removeMutation.isError && (
              <div className="bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm mb-4">
                {removeMutation.error?.message}
              </div>
            )}
            <div className="flex items-center justify-end gap-3">
              <button
                data-testid="cancel-managed-remove"
                onClick={() => {
                  setShowRemoveConfirm(false);
                  setRemovalTarget(null);
                  setRemoveConfirmText('');
                }}
                className="px-4 py-2 rounded-lg border border-gray-600 text-gray-300 hover:text-white text-sm font-medium transition-colors"
              >
                Cancel
              </button>
              <button
                data-testid={childRemovalConfirm ? 'confirm-delete-branch-pair' : 'confirm-remove-from-reposync'}
                onClick={() => removeMutation.mutate()}
                disabled={
                  removeMutation.isPending
                  || !removalPreviewReady
                  || removalDependencyPreview?.parent_removal_blocked
                  || (childRemovalConfirm && removeConfirmText !== removalSubject.git_branch)
                }
                className="px-4 py-2 rounded-lg bg-red-600 hover:bg-red-700 disabled:opacity-50 text-white text-sm font-medium transition-colors"
              >
                {removeMutation.isPending ? 'Removing…' : removalConfirmLabel}
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
  onClear,
}: {
  title: string;
  value: string;
  color: string;
  onClick?: () => void;
  subtitle?: string;
  onClear?: () => void;
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
      <div className="flex items-center justify-between">
        <p className="text-sm text-gray-400">{title}</p>
        {onClear && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onClear();
            }}
            className="text-xs text-gray-400 hover:text-red-400 transition-colors px-1.5 py-0.5 rounded border border-gray-600 hover:border-red-500/50"
          >
            Clear
          </button>
        )}
      </div>
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
