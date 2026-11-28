import { useId } from 'react';
import Dialog from '../components/Dialog';
import { useViewMutation as useMutation, useViewOwnership } from '../useViewMutation';
import { clientSession, sessionFetch } from '../session';
import { canConfirmObservedSkip, runObservedSkip, latestScopedSkipObservation, type ScopedSkipObservation } from '../statusActions';
import { useStatusObservation } from '../useStatusObservation';
import RepositoryStatusSummary, { StatusObservationPanel } from '../components/StatusObservation';
import { useRepositorySave } from '../useRepositorySave';
import { samePublicConfiguration, retainUnsavedCredentials, repositoryEditorKey, type PublicConfiguration } from '../repositorySave';
import RepositorySaveNotice from '../components/RepositorySaveNotice';
import { errorStatusQuery, errorStatusView, refreshErrorStatus } from '../errorStatus';
import ErrorAcknowledgement from '../components/ErrorAcknowledgement';
import { errorScope } from '../errorAcknowledgement';
import React, { useEffect, useRef, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useParams, useNavigate, useLocation, useSearchParams, Link } from 'react-router-dom';
import { api, type Repository, type SyncStatus, type SyncRecord, type CommitMapEntry, type AuditEntry, type LatePairPlan, type PairRefreshPlan, type SkipCommitContext, type SkipCommitPending } from '../api';
import {
  type BranchPairRemovalNotice as RemovalNotice,
  removeBranchPairForView,
  shouldRetireBranchPair,
  isNotFoundError,
  repoDetailQueryKeys,
  readBranchPairRemovalNotice,
  persistBranchPairRemovalNotice,
  readPersistedBranchPairRemovalNotice,
} from '../branchPairRemoval';
import ImportProgressCard from '../components/ImportProgressCard';
import ImportHistoryNotice from '../components/ImportHistoryNotice';
import {
  DEFAULT_REPO_IMPORT_MODE,
  hasVerifiedSnapshotBaseline,
  importBaselineFailureNotice,
  type RepoImportMode,
} from '../importBaseline';
import type { ImportStatus } from '../api';
import BranchPairRemovalNotice from '../components/BranchPairRemovalNotice';
import ManagedRemovalPanel from '../components/ManagedRemovalPanel';
import {
  type ManagedRemovalReceipt,
  type ManagedRemovalStatus,
  managedRemovalNeedsPoll,
  managedRemovalReceiptFromStatus,
  managedRemovalStatusFromReceipt,
  removalDependencyPreviewConfirmReady,
  reviewedRemovalReady,
  removalReviewForDisplay,
  readPendingRemovalReview,
  clearPendingRemovalReview,
  shouldPersistPolledManagedRemovalReceipt,
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
import { useText } from '../layouts/vocabulary';

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
  'rs-in w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent';

const selectClass =
  'rs-in w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent';

/*
 * #264 page regions (contracts/page-regions.md; decisions D7, D8, D12). One region tree in
 * every layout and in today's order. Each region names the sections it shows in; the page
 * names the current section. Layout styles decide what is shown; nothing here branches on
 * the layout, and changing section never unmounts a region.
 */
const REPO_SECTIONS = [
  { id: 'overview', label: 'Overview' },
  { id: 'activity', label: 'Activity' },
  { id: 'branches', label: 'Branch pairs' },
  { id: 'operations', label: 'Operations' },
  { id: 'settings', label: 'Settings' },
] as const;
type RepoSection = (typeof REPO_SECTIONS)[number]['id'];
const EVERY_SECTION = REPO_SECTIONS.map(section => section.id).join(' ');

/** G04.31: absent or unknown values mean Overview. */
function normaliseRepoSection(value: string | null): RepoSection {
  return REPO_SECTIONS.find(section => section.id === value)?.id ?? 'overview';
}

/** The search string for a section link: keeps any other parameter and sets `section`. */
function sectionSearch(search: string, section: RepoSection): string {
  const params = new URLSearchParams(search);
  params.set('section', section);
  return `?${params.toString()}`;
}

function region(name: string, sections: string) {
  return { 'data-rs-region': name, 'data-rs-sections': sections };
}

/**
 * Whether the repository section navigation is displayed, from its computed `display` (null
 * before it renders). Only then does the page show one section at a time, so only then do
 * Edit and Replace switch to Settings, and only then does a section change not scroll. Where
 * it is hidden (Classic, Individual, Team) every section is on the page, and Edit leaves the
 * URL, the history and the scroll position as they were.
 */
function sectionNavDisplayed(display: string | null | undefined): boolean {
  return display != null && display !== 'none';
}

/** D7: the first displayed region of a section, not counting the head and the status line. */
function firstSectionRegion(root: ParentNode, section: RepoSection): HTMLElement | null {
  const candidates = Array.from(root.querySelectorAll<HTMLElement>(`[data-rs-region][data-rs-sections~="${section}"]`));
  return candidates.find(element => {
    const name = element.getAttribute('data-rs-region');
    return name !== 'head' && name !== 'status-line' && element.getClientRects().length > 0;
  }) ?? null;
}

export default function RepoDetail() {
  const {id}=useParams<{id:string}>();
  const key=repositoryEditorKey({owner:getStoredUser()?.id ?? 'legacy',targetId:id,session:clientSession().snapshot().token,epoch:clientSession().snapshot().epoch});
  return <RepositoryDetailEditor key={key} />;
}

function RepositoryDetailEditor() {
  const t = useText();
  const fieldId = useId();
  const view = useViewOwnership();
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const location = useLocation();
  const [searchParams] = useSearchParams();
  const sectionParam = searchParams.get('section');
  const section = normaliseRepoSection(sectionParam);
  const pageRef = useRef<HTMLDivElement>(null);
  const sectionNavRef = useRef<HTMLElement>(null);
  // D7: a first load without a parameter does not scroll; every later change of section does.
  const pendingSectionScroll = useRef<RepoSection | null>(sectionParam === null ? null : section);
  const previousSection = useRef<RepoSection>(section);
  const queryClient = useQueryClient();
  const user = getStoredUser();
  const isAdmin = user?.role === 'admin';
  const routedNotice = readBranchPairRemovalNotice(location.state);
  const importPrefs = (location.state as { importPrefs?: { importMode: RepoImportMode; svnRevision: string } } | null)
    ?.importPrefs;
  const [localNotice, setLocalNotice] = useState<RemovalNotice | null>(null);
  const [retiredId, setRetiredId] = useState<string | null>(null);

  const [syncTriggered, setSyncTriggered] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const saveFlow=useRepositorySave(id,editing);
  const [saveMessage,setSaveMessage]=useState<string | null>(null);
  const [saveError,setSaveError]=useState<string | null>(null);
  const editVersion=useRef(0);
  const restoredSave=useRef<string | null>(null);

  const [form, setForm] = useState<EditForm | null>(null);
  const currentForm=useRef(form);currentForm.current=form;
  const [showDisableConfirm, setShowDisableConfirm] = useState(false);
  const [showRemoveConfirm, setShowRemoveConfirm] = useState(false);
  const [removalReceipt, setRemovalReceipt] = useState<ManagedRemovalReceipt | null>(null);
  const [expandedAuditGroups, setExpandedAuditGroups] = useState<Set<number>>(new Set());
  const [expandedDetails, setExpandedDetails] = useState<Set<number>>(new Set());
  const [svnTestResult, setSvnTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [gitTestResult, setGitTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [svnTesting, setSvnTesting] = useState(false);
  const [credentialFocus, setCredentialFocus] = useState<{ field: CredentialField; request: number } | null>(null);
  const [gitTesting, setGitTesting] = useState(false);
  const [showBranchModal, setShowBranchModal] = useState(false);
  const [removalTarget, setRemovalTarget] = useState<Repository | null>(null);
  const [branchPairDeleteTarget, setBranchPairDeleteTarget] = useState<Repository | null>(null);
  const [branchPairDeleteError, setBranchPairDeleteError] = useState<string | null>(null);
  const [removeConfirmText, setRemoveConfirmText] = useState('');
  const [removeBranchOpts, setRemoveBranchOpts] = useState({ delete_git: false, delete_svn: false });
  const [branchForm, setBranchForm] = useState({
    svn_branch: '',
    git_branch: '',
    skip_import: false,
    auto_create_svn_branch: true,
    auto_create_git_branch: true,
  });
  const gitBranchValidation = (() => {
    const v = branchForm.git_branch;
    const invalid = /[^a-zA-Z0-9._/-]/.test(v);
    const traversal = v.includes('..');
    const doubleSlash = v.includes('//');
    const badEdges = v.startsWith('/') || v.endsWith('/') || v.startsWith('-');
    const reserved = v === 'HEAD' || v.startsWith('refs/') || v.endsWith('.lock');
    return {
      error: !v ? null : invalid ? 'Invalid characters (use letters, numbers, . _ - / only)'
        : traversal ? 'Must not contain ".."'
        : doubleSlash ? 'Must not contain "//"'
        : badEdges ? 'Must not start/end with "/" or start with "-"'
        : reserved ? 'Reserved name not allowed' : null,
      // Preserve the existing preview refusal contract, independently of presentation.
      previewRefused: !v.trim() || invalid || traversal || doubleSlash || badEdges || v === 'HEAD' || v.endsWith('.lock'),
    };
  })();
  const svnBranchValidation = (() => {
    const v = branchForm.svn_branch;
    const invalid = /[^a-zA-Z0-9._/-]/.test(v);
    const traversal = v.includes('..');
    return {
      error: !v ? null : invalid ? 'Invalid characters' : traversal ? 'Must not contain ".."' : null,
      previewRefused: !v.trim() || invalid || traversal,
    };
  })();
  const [branchSuccess, setBranchSuccess] = useState(false);
  const [branchPlan, setBranchPlan] = useState<LatePairPlan | null>(null);
  const [refreshPlan, setRefreshPlan] = useState<PairRefreshPlan | null>(null);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const [showSkipModal, setShowSkipModal] = useState(false);
  const [showResumeConfirm, setShowResumeConfirm] = useState(false);
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
  const archived = repo?.status === 'archived';
  const repoMissing = isNotFoundError(error);
  const detailLive = !!id && id !== retiredId && !repoMissing;

  // Status query scoped to this repo. Stop it once this detail is gone.
  const statusQuery = useQuery<SyncStatus>({
    ...errorStatusQuery(id),
    refetchInterval: (query) => (detailLive && !isNotFoundError(query.state.error) ? 5000 : false),
    retry: (failureCount, queryError) => !isNotFoundError(queryError) && failureCount < 2,
    enabled: detailLive,
  });
  const {data:status,isError:statusUnavailable}=statusQuery;
  const statusView=useStatusObservation(statusQuery);
  const errorCard = errorStatusView(status, statusUnavailable);

  // Sync records
  const detailRetry = (failureCount: number, queryError: unknown) =>
    !isNotFoundError(queryError) && failureCount < 2;

  const syncRecordsQuery = useQuery({
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
  const credentialsQuery = useQuery({
    queryKey: ['repo-credentials', id],
    queryFn: () => api.getRepoCredentials(id!),
    enabled: detailLive,
    refetchInterval: detailLive ? 5000 : false,
    retry: detailRetry,
  });
  const credStatus = credentialsQuery.data;

  // Branch pairs
  const needsImportStatusForNotice =
    detailLive
    && repo?.import_mode === 'snapshot'
    && !hasVerifiedSnapshotBaseline({
      import_mode: repo?.import_mode,
      starting_revision: repo?.starting_revision,
      history_boundary: repo?.history_boundary,
      snapshot_pin: repo?.snapshot_pin,
    });

  const { data: importStatus } = useQuery<ImportStatus>({
    queryKey: ['import-status', id],
    queryFn: async () => {
      const token = localStorage.getItem('session_token');
      const res = await sessionFetch(`/api/repos/${id}/import/status`, {
        headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}) },
      });
      if (!res.ok) throw new Error(`Import status unavailable (${res.status})`);
      return res.json();
    },
    enabled: Boolean(needsImportStatusForNotice),
    refetchInterval: needsImportStatusForNotice ? 5000 : false,
  });

  const baselineNoticeFields = repo
    ? {
        import_mode: repo.import_mode,
        starting_revision: repo.starting_revision,
        history_boundary: repo.history_boundary,
        snapshot_pin: repo.snapshot_pin,
        phase: importStatus?.phase,
        lifecycle: importStatus?.lifecycle,
        outcome_detail: importStatus?.outcome_detail,
      }
    : null;

  const showBaselineNotice =
    baselineNoticeFields
    && (hasVerifiedSnapshotBaseline(baselineNoticeFields)
      || Boolean(importBaselineFailureNotice(baselineNoticeFields)));

  const { data: branchPairs } = useQuery({
    queryKey: ['branch-pairs', id],
    queryFn: () => api.listBranchPairs(id!),
    enabled: detailLive,
    refetchInterval: detailLive ? 5000 : false,
    retry: detailRetry,
  });

  const skipState=useRef<ScopedSkipObservation>({repoId:id,detailLive,administrator:isAdmin,query:statusQuery});
  skipState.current={repoId:id,detailLive,administrator:isAdmin,query:statusQuery};
  const skipContextEnabled = !archived && !!id && canConfirmObservedSkip(skipState.current,id);
  const { data: skipContextResponse } = useQuery({
    queryKey: ['skip-commit-context', id],
    queryFn: () => api.getSkipCommitContext(id!),
    enabled: skipContextEnabled,
    refetchInterval: skipContextEnabled ? 5000 : false,
    retry: detailRetry,
  });
  const skipContext: SkipCommitContext | null = skipContextResponse?.context ?? null;

  useEffect(()=>{if(!skipContextEnabled){setShowSkipModal(false);setSkipSelectedCount(0);}},[skipContextEnabled]);

  const skipMutation = useMutation({
    mutationFn: (selected: string[]) => runObservedSkip(()=>latestScopedSkipObservation(queryClient,skipState.current),id!,async () => {
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
    }),
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

  const [branchIncludeParent, setBranchIncludeParent] = useState(false);
  const branchPlanMatches = !!branchPlan && branchPlan.git_branch === branchForm.git_branch
    && branchPlan.svn_branch === branchForm.svn_branch && branchPlan.include_parent === branchIncludeParent;
  // #264 D11 and the interaction contract: "Stop operation…" asks once before the same cancel request.
  const [confirmStopOperation, setConfirmStopOperation] = useState(false);
  const [stopOperationError, setStopOperationError] = useState<string | null>(null);
  const branchMutation = useMutation({
    mutationFn: (data: { svn_branch: string; git_branch: string; skip_import: boolean; auto_create_svn_branch: boolean; auto_create_git_branch: boolean;
      action?: 'preview' | 'publish' | 'status' | 'resume' | 'cancel'; review_pin?: string; operation_id?: string }) =>
      api.createBranchPair(id!, {
        ...data,
        reconciliation: true,
        include_parent: branchIncludeParent,
        dry_run: !data.action || data.action === 'preview' || data.action === 'status',
        preview: !data.action || data.action === 'preview' || data.action === 'status',
      }),
    onSuccess: (plan) => {
      setBranchPlan(plan);
      setBranchSuccess(true);
      if (plan.operation_id) setBranchIncludeParent(plan.include_parent ?? false);
      for (const queryKey of repoDetailQueryKeys(id!)) queryClient.invalidateQueries({ queryKey });
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
    onSuccess: (reply) => {
      setSyncTriggered(syncReplyLabel(reply));
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
      queryClient.invalidateQueries({ queryKey: ['status', id] });
      setTimeout(() => setSyncTriggered(null), 3000);
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
  function openDisableConfirm() {
    disableMutation.reset();
    setShowDisableConfirm(true);
  }

  // G04.9: the existing retry call, now sent only from its confirmation.
  const resumeMutation = useMutation({
    mutationFn: () => api.retryRepo(id!),
    onSuccess: () => {
      setShowResumeConfirm(false);
      queryClient.invalidateQueries({ queryKey: ['repo-status', id] });
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
      queryClient.invalidateQueries({ queryKey: ['status', id] });
    },
  });

  // G04.7: the same hook request, sent with the session instead of as a plain link.
  const hookDownload = useMutation({
    mutationFn: async () => saveTextDownload('pre-commit', await fetchPreCommitHook(id!)),
  });

  const removalSubject = removalTarget ?? repo;
  const removalSubjectId = removalSubject?.id ?? id;
  const branchPairDeleteSubject = branchPairDeleteTarget;
  const branchPairConfirmation = branchPairDeleteSubject
    ? (removeBranchOpts.delete_svn && !removeBranchOpts.delete_git ? branchPairDeleteSubject.svn_branch : branchPairDeleteSubject.git_branch)
    : '';

  const reviewedRemovalTarget = branchPairDeleteTarget ?? removalTarget ?? repo;
  const reviewedRemovalOptions = reviewedRemovalTarget?.parent_id
    ? { ...removeBranchOpts, archive_pair: removeBranchOpts.delete_git !== removeBranchOpts.delete_svn } : { delete_git: false, delete_svn: false, archive_pair: false };
  const reviewedRemovalQuery = useQuery({
    queryKey: ['reviewed-removal', id, reviewedRemovalTarget?.id, reviewedRemovalOptions.delete_git, reviewedRemovalOptions.delete_svn, reviewedRemovalOptions.archive_pair],
    queryFn: ({ signal }) => api.reviewManagedRemoval(reviewedRemovalTarget!.id, reviewedRemovalOptions, signal),
    enabled: isAdmin && detailLive && !!reviewedRemovalTarget?.id && (showRemoveConfirm || !!branchPairDeleteTarget),
    staleTime: 0,
    retry: false,
  });
  const reviewedRemovalConfirmReady = reviewedRemovalReady(reviewedRemovalQuery.data, reviewedRemovalTarget?.id, reviewedRemovalOptions,
    reviewedRemovalQuery.isFetching || reviewedRemovalQuery.isError);
  const pendingRemovalReview = (() => {
    try { return { review: reviewedRemovalTarget?.id ? readPendingRemovalReview(reviewedRemovalTarget.id) : null, error: null }; }
    catch (error) { return { review: null, error: error instanceof Error ? error.message : String(error) }; }
  })();
  const branchRemovalStatusQuery = useQuery({
    queryKey: ['managed-removal-target', id, branchPairDeleteSubject?.id],
    queryFn: () => api.getManagedRemoval(branchPairDeleteSubject!.id),
    enabled: isAdmin && detailLive && !!branchPairDeleteSubject?.id,
    retry: false,
    refetchInterval: query => managedRemovalNeedsPoll(query.state.data?.state) ? 2000 : false,
  });

  const removeMutation = useMutation({
    mutationFn: async () => {
      setBranchPairDeleteError(null);
      const target = branchPairDeleteTarget ?? removalTarget ?? repo;
      if (!target?.id || !id) {
        throw new Error('No repository selected for removal');
      }
      const pending = readPendingRemovalReview(target.id);
      if (pending) {
        view.assertCurrent();
        const matching = await api.lookupRemovalRequest(target.id, pending.review_pin);
        view.assertCurrent();
        if (matching) {
          persistManagedRemovalReceipt(managedRemovalReceiptFromStatus(target.id, matching), true);
          clearPendingRemovalReview(target.id, pending.review_pin);
        }
        const result = matching
          ? matching.state === 'completed' ? matching : await api.continueManagedRemoval(target.id, matching.operation_id)
          : await api.approveManagedRemoval(target.id, pending, view);
        return { kind: 'managed' as const, result, target };
      }
      const retained = readManagedRemovalReceipt(target.id);
      if (retained && retained.protocol !== 2 && retained.state !== 'completed') {
        view.assertCurrent();
        const result = await api.removeManagedRepo(target.id, { delete_git: false, delete_svn: false });
        return { kind: 'managed' as const, result, target };
      }
      if (retained?.protocol === 2 && retained.state !== 'completed') {
        view.assertCurrent();
        const result = await api.continueManagedRemoval(target.id, retained.operationId);
        return { kind: 'managed' as const, result, target };
      }
      view.assertCurrent();
      const observed = await api.getManagedRemoval(target.id);
      view.assertCurrent();
      if (observed?.protocol === 2 && observed.state !== 'completed') {
        const result = await api.continueManagedRemoval(target.id, observed.operation_id);
        return { kind: 'managed' as const, result, target };
      }
      if (!reviewedRemovalConfirmReady || reviewedRemovalQuery.data?.repo_id !== target.id) {
        throw new Error('A fresh exact-target removal review is required');
      }
      const review = reviewedRemovalQuery.data;
      if (target.parent_id) {
        const options = {
          delete_git: review.delete_git,
          delete_svn: review.delete_svn,
        };
        const { result, destination, outcome } = await removeBranchPairForView(
          { viewedId: id, target: { id: target.id, parent_id: target.parent_id }, options },
          view,
          { lookupParent: api.getRepo, remove: () => api.approveManagedRemoval(target.id, review, view) },
        );
        return { kind: 'branch_pair' as const, result: result as ManagedRemovalStatus, target, destination, outcome };
      }
      view.assertCurrent();
      const result = await api.approveManagedRemoval(target.id, review, view);
      return { kind: 'managed' as const, result, target };
    },
    onError: (error) => {
      if (branchPairDeleteTarget) {
        setBranchPairDeleteError(error instanceof Error ? error.message : String(error));
      }
    },
    onSuccess: async (payload) => {
      const durableReceipt = managedRemovalReceiptFromStatus(payload.target.id, payload.result);
      persistManagedRemovalReceipt(durableReceipt);
      setRemovalReceipt(durableReceipt);
      queryClient.setQueryData(['managed-removal', payload.target.id, payload.result.operation_id], payload.result);
      queryClient.invalidateQueries({ queryKey: ['managed-removal-target', id, payload.target.id] });
      if (!payload.result.ok) {
        setBranchPairDeleteError(payload.result.message);
        queryClient.invalidateQueries({ queryKey: ['repos'] });
        return;
      }
      setShowRemoveConfirm(false);
      setRemovalTarget(null);
      setBranchPairDeleteTarget(null);
      setBranchPairDeleteError(null);
      setRemoveConfirmText('');
      queryClient.invalidateQueries({ queryKey: ['repos'] });

      if (payload.kind === 'branch_pair') {
        const { result, target, destination, outcome } = payload;
        const notice: RemovalNotice = {
          outcome: outcome === 'in_progress' ? 'in_progress' : 'completed',
          message: result.message
            || (outcome === 'in_progress'
              ? 'The removal request was accepted and has not finished.'
              : `Branch pair ${target.name} was removed.`),
          warnings: result.warnings ?? [],
          operationId: result.operation_id,
          name: target.name,
        };
        const leaveViewedPair = target.id === id
          && shouldRetireBranchPair(result);
        if (leaveViewedPair) {
          for (const queryKey of repoDetailQueryKeys(target.id)) {
            await queryClient.cancelQueries({ queryKey });
          }
          if (!view.isCurrent()) return;
          setRetiredId(target.id);
          setLocalNotice(null);
          persistBranchPairRemovalNotice(notice);
          navigate(destination ?? '/repos', {
            replace: true,
            state: { branchPairRemoval: notice },
          });
          return;
        }
        if (target.id !== id) {
          queryClient.invalidateQueries({ queryKey: ['branch-pairs', id] });
        }
        queryClient.invalidateQueries({ queryKey: ['repo', target.id] });
        queryClient.invalidateQueries({ queryKey: ['status', target.id] });
        persistBranchPairRemovalNotice(notice);
        setLocalNotice(notice);
        return;
      }

      const { result, target } = payload;
      const receipt = managedRemovalReceiptFromStatus(target.id, result);
      persistManagedRemovalReceipt(receipt);
      setRemovalReceipt(receipt);
      queryClient.setQueryData(['managed-removal', target.id, result.operation_id], result);
      queryClient.invalidateQueries({ queryKey: ['managed-removal', target.id] });
      const registrationGone = result.registration_listed === false;
      const leaveViewedPair = target.id === id
        && `${result.state}`.toLowerCase() === 'completed'
        && registrationGone;
      if (leaveViewedPair) {
        for (const queryKey of repoDetailQueryKeys(target.id)) {
          await queryClient.cancelQueries({ queryKey });
        }
        if (!view.isCurrent()) return;
        setRetiredId(target.id);
        navigate('/repos', { replace: true });
      }
    },
  });

  const restoreMutation = useMutation({
    mutationFn: () => api.restoreManagedRepo(id!),
    onSuccess: () => {
      removeMutation.reset();
      clearManagedRemovalReceipt(id!);
      setRemovalReceipt(null);
      queryClient.removeQueries({ queryKey: ['managed-removal', id] });
      queryClient.invalidateQueries({ queryKey: ['repo', id] });
      queryClient.invalidateQueries({ queryKey: ['repos'] });
    },
  });

  const removalPreviewQuery = useQuery({
    queryKey: ['removal-preview', removalSubjectId],
    queryFn: () => api.getRemovalDependencyPreview(removalSubjectId!),
    enabled: !!removalSubjectId && isAdmin && showRemoveConfirm && detailLive,
    retry: false,
  });

  const branchPairPreviewQuery = useQuery({
    queryKey: ['removal-preview', branchPairDeleteSubject?.id],
    queryFn: () => api.getRemovalDependencyPreview(branchPairDeleteSubject!.id),
    enabled:
      !!branchPairDeleteSubject?.id
      && isAdmin
      && detailLive
      && !!branchPairDeleteSubject.parent_id,
    retry: false,
  });
  const displayedBranchReview = pendingRemovalReview.error ? undefined : removalReviewForDisplay(
    pendingRemovalReview.review, reviewedRemovalQuery.data, branchPairDeleteSubject?.id,
    reviewedRemovalOptions, reviewedRemovalQuery.isFetching || reviewedRemovalQuery.isError,
  );
  const displayedBranchChoices = displayedBranchReview ?? reviewedRemovalOptions;
  const branchPairDependencyPreview = displayedBranchReview?.dependency_preview;
  const branchPairPreviewReady = removalDependencyPreviewConfirmReady({
    isLoading: branchPairPreviewQuery.isLoading,
    isFetching: branchPairPreviewQuery.isFetching,
    isError: branchPairPreviewQuery.isError,
    dependencyPreview: branchPairDependencyPreview,
  });
  const removalDependencyPreview = removalPreviewQuery.data?.dependency_preview;

  useEffect(() => {
    const target = branchPairDeleteSubject?.id;
    const status = branchRemovalStatusQuery.data;
    if (!target || !status || !view.isCurrent()) return;
    const stored = readManagedRemovalReceipt(target);
    let pending;
    try { pending = readPendingRemovalReview(target); } catch { return; }
    const matchesPending = pending && status.protocol === 2 && status.review_pin === pending.review_pin && status.request_id === `ui-remove-v2-${pending.review_pin}`;
    if (!matchesPending && !shouldPersistPolledManagedRemovalReceipt(stored, status)) return;
    try {
      persistManagedRemovalReceipt(managedRemovalReceiptFromStatus(target, status), !!matchesPending);
      if (matchesPending && pending) clearPendingRemovalReview(target, pending.review_pin);
    } catch (error) {
      setBranchPairDeleteError(`The original request is retained because its result could not be saved: ${error instanceof Error ? error.message : String(error)}`);
      return;
    }
    if (status.state === 'completed' && status.registration_listed === false) queryClient.invalidateQueries({ queryKey: ['branch-pairs', id] });
  }, [branchPairDeleteSubject?.id, branchRemovalStatusQuery.data, id, queryClient, view]);

  useEffect(() => {
    if (!id || !isAdmin) return;
    let pending;
    try { pending = readPendingRemovalReview(id); }
    catch (error) {
      setLocalNotice({ outcome: 'in_progress', message: error instanceof Error ? error.message : String(error), warnings: ['Retained request evidence is unavailable. No automatic action was taken.'] });
      return;
    }
    if (!pending) return;
    const captured = pending;
    let cancelled = false;
    void (async () => {
      try {
        const status = await api.lookupRemovalRequest(id, captured.review_pin);
        if (cancelled || !view.isCurrent() || readPendingRemovalReview(id)?.review_pin !== captured.review_pin) return;
        if (!status) {
          setLocalNotice({ outcome: 'in_progress', message: 'No matching server operation was found for the retained request. It remains available for explicit recovery; no automatic retry was sent.', warnings: [] });
          return;
        }
        const receipt = managedRemovalReceiptFromStatus(id, status);
        persistManagedRemovalReceipt(receipt, true);
        clearPendingRemovalReview(id, captured.review_pin);
        setRemovalReceipt(receipt);
        setLocalNotice({ outcome: status.state === 'completed' && (status.registration_listed === false || status.archive_pair) ? 'completed' : 'in_progress', message: status.state === 'completed' && status.registration_listed && !status.archive_pair ? 'The historical removal completed, but this registration currently exists or was restored. No new removal was started.' : status.message, operationId: status.operation_id, warnings: [] });
      } catch (error) {
        if (!cancelled && view.isCurrent()) setLocalNotice({ outcome: 'in_progress', message: error instanceof Error ? error.message : String(error), warnings: ['The original request is retained. Outcome verification is unavailable; no automatic retry was sent.'] });
      }
    })();
    return () => { cancelled = true; };
  }, [id, isAdmin, view]);

  const trackedRemovalOperationId =
    removalReceipt && removalReceipt.repoId === id ? removalReceipt.operationId : undefined;

  const removalStatusQuery = useQuery<ManagedRemovalStatus | null>({
    queryKey: ['managed-removal', id, trackedRemovalOperationId ?? 'latest'],
    queryFn: () => api.getManagedRemoval(id!, trackedRemovalOperationId),
    enabled:
      !!id
      && isAdmin
      && id !== retiredId
      && !branchPairDeleteTarget
      && (showRemoveConfirm
        || removeMutation.isPending
        || removalReceipt?.repoId === id),
    retry: false,
    refetchInterval: (query) => {
      const state = query.state.data?.state;
      return managedRemovalNeedsPoll(state) ? 2000 : false;
    },
  });

  const removalPanelStatus: ManagedRemovalStatus | undefined = (() => {
    const live = removalStatusQuery.data;
    if (live && (!trackedRemovalOperationId || live.operation_id === trackedRemovalOperationId)) {
      return live;
    }
    if (removalReceipt && removalReceipt.repoId === id) {
      return managedRemovalStatusFromReceipt(removalReceipt);
    }
    return live ?? undefined;
  })();

  const childRemovalConfirm = !!(removalSubject?.parent_id);
  const removalPreviewReady = removalDependencyPreviewConfirmReady({
    isLoading: removalPreviewQuery.isLoading,
    isFetching: removalPreviewQuery.isFetching,
    isError: removalPreviewQuery.isError,
    dependencyPreview: removalDependencyPreview,
  });
  const removalConfirmLabel = childRemovalConfirm ? 'Remove branch pair' : 'Remove from RepoSync';
  const displayedRemovalStatus = removeMutation.data && removeMutation.data.target.id === reviewedRemovalTarget?.id
    ? removeMutation.data.result
    : branchPairDeleteSubject ? branchRemovalStatusQuery.data : removalPanelStatus;

  const reviewedRemovalPanel = ((branchPairDeleteSubject || showRemoveConfirm) && (
        <div className="border border-gray-600 rounded p-3 text-sm space-y-2" role="status" data-testid="reviewed-removal-targets">
          {reviewedRemovalQuery.isFetching && <p>Observing exact removal targets…</p>}
          {reviewedRemovalQuery.isError && <p role="alert">Removal review unavailable: {reviewedRemovalQuery.error.message}. No new deletion is authorized.</p>}
          {pendingRemovalReview.error && <p role="alert">{pendingRemovalReview.error}</p>}
          {branchPairDeleteError && <p role="alert">{branchPairDeleteError}</p>}
          {pendingRemovalReview.review && <div role="alert">
            <p>A previously approved removal request has no retained outcome receipt. Recover that same request before approving different work. Original disposition: {pendingRemovalReview.review.archive_pair ? 'archive the pair permanently, preserving the survivor and local data' : 'remove the registration and clean owned local data (not archive)'}. Git deletion: {String(pendingRemovalReview.review.delete_git)}; SVN deletion: {String(pendingRemovalReview.review.delete_svn)}.</p>
            <button type="button" disabled={removeMutation.isPending} onClick={() => removeMutation.mutate()}>Recover retained removal request</button>
          </div>}
          {!pendingRemovalReview.review && reviewedRemovalConfirmReady && reviewedRemovalQuery.data && <>
            <p>Review for {reviewedRemovalQuery.data.repo_id}</p>
            <p className="break-all">Git: {reviewedRemovalQuery.data.git ? `${reviewedRemovalQuery.data.git.remote_url} · ${reviewedRemovalQuery.data.git.reference} · ${reviewedRemovalQuery.data.git.expected_sha ?? 'already absent'}` : 'preserved'}</p>
            <p className="break-all">SVN: {reviewedRemovalQuery.data.svn ? `${reviewedRemovalQuery.data.svn.url} · UUID ${reviewedRemovalQuery.data.svn.uuid} · r${reviewedRemovalQuery.data.svn.base_revision} · ${reviewedRemovalQuery.data.svn.expected_present ? 'present' : 'already absent'}` : 'preserved'}</p>
            <p>{reviewedRemovalQuery.data.retention}</p>
          </>}
          <button type="button" disabled={reviewedRemovalQuery.isFetching || removeMutation.isPending} onClick={() => reviewedRemovalQuery.refetch()}>Refresh exact-target review</button>
          {displayedRemovalStatus && (
            <ManagedRemovalPanel status={displayedRemovalStatus} onRetry={displayedRemovalStatus.retryable ? () => removeMutation.mutate() : undefined} retryPending={removeMutation.isPending} />
          )}
        </div>
      ));

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

  useEffect(()=>{
    const previous=saveFlow.initial;
    if(editing && previous && repo && restoredSave.current!==previous.configRequestId) {
      try {setForm(repoToForm({...repo,...(previous.draftConfiguration ?? previous.configuration)}));restoredSave.current=previous.configRequestId;}
      catch {setSaveError('The recovery draft could not be read. Keep the existing repository ID and review its settings.');}
    }
  },[editing,saveFlow.initial,repo]);
  useEffect(()=>{
    if(saveFlow.attempt?.configurationReceipt) {
      void queryClient.invalidateQueries({queryKey:['repo',id]});
      void queryClient.invalidateQueries({queryKey:['repos']});
    }
  },[saveFlow.attempt?.configurationReceipt,id,queryClient]);

  function editConfiguration(value: EditForm): PublicConfiguration {
    const {svn_password,git_token,allowed_paths,blocked_patterns,...data}=value;
    void svn_password;void git_token;
    return {...data,
      allowed_paths:allowed_paths.trim() ? JSON.stringify(allowed_paths.split('\n').map(v=>v.trim()).filter(Boolean)) : null,
      blocked_patterns:blocked_patterns.trim() ? JSON.stringify(blocked_patterns.split('\n').map(v=>v.trim()).filter(Boolean)) : null};
  }
  /** A navigation (Back works) that keeps the route's state; nothing is remounted. */
  function showSection(next: RepoSection) {
    if (next === section) return;
    navigate({ pathname: location.pathname, search: sectionSearch(location.search, next) }, { state: location.state });
  }
  /** The section navigation's computed display, or null before it renders. */
  function readSectionNavDisplay(): string | null {
    const nav = sectionNavRef.current;
    return nav ? getComputedStyle(nav).display : null;
  }
  function startEdit() {
    if(!repo || archived)return;editVersion.current++;restoredSave.current=null;
    setForm(repoToForm(repo));setEditing(true);setSaveError(null);setSaveMessage(null);
    // The form is in Settings: show that section only where the section navigation is displayed.
    if (sectionNavDisplayed(readSectionNavDisplay())) showSection('settings');
  }
  async function loadCurrentSettingsForReview() {
    const generation=editVersion.current;
    try {
      const current=await saveFlow.loadCurrentForReview();
      if(!saveFlow.viewActive() || generation!==editVersion.current)return;
      setForm(repoToForm(current));setSaveError(null);setSaveMessage('Review these current settings. Required credential fields were cleared; choose a new save explicitly.');
      void queryClient.invalidateQueries({queryKey:['repo',id]});
    } catch(cause) {if(saveFlow.viewActive() && generation===editVersion.current)setSaveError(cause instanceof Error ? cause.message : 'Current settings could not be read.');}
  }
  function useReviewedSettings() {
    if(!currentForm.current)return;
    try {saveFlow.useReviewedSettings(editConfiguration(currentForm.current),{svn_password:currentForm.current.svn_password || undefined,git_token:currentForm.current.git_token || undefined});setSaveError(null);setSaveMessage('New attempt prepared on the same repository. Save will submit only the reviewed snapshot.');}
    catch(cause) {setSaveError(cause instanceof Error ? cause.message : 'Reviewed recovery could not be prepared.');}
  }
  function cancelEdit() {
    if(currentForm.current)saveFlow.rememberDraft(editConfiguration(currentForm.current));
    editVersion.current++;setEditing(false);setForm(null);setSaveError(null);setSaveMessage(null);
  }
  async function handleSave() {
    if(!currentForm.current || !repo)return;
    const generation=editVersion.current;
    const submitted={...currentForm.current};setSaveError(null);setSaveMessage(null);
    try {
      const result=await saveFlow.save(editConfiguration(submitted),{svn_password:submitted.svn_password || undefined,git_token:submitted.git_token || undefined},repo.updated_at);
      if(!result.viewActive || editVersion.current!==generation)return;
      void queryClient.invalidateQueries({queryKey:['repo',id]});void queryClient.invalidateQueries({queryKey:['repos']});
      void queryClient.invalidateQueries({queryKey:['repo-credentials',id]});
      const live=currentForm.current;
      if(!live || !samePublicConfiguration(editConfiguration(live),result.configuration)
        || (live.svn_password || '')!==(result.credentials.svn_password || '')
        || (live.git_token || '')!==(result.credentials.git_token || '')) {
        setForm(previous=>previous ? {...previous,...retainUnsavedCredentials(previous,result.credentials)} : previous);
        setSaveMessage('The earlier save completed. Newer entered changes remain unsaved; save again after reviewing the refreshed settings.');return;
      }
      saveFlow.finish(result.attempt.configRequestId);setEditing(false);setForm(null);
    } catch(cause) {
      if(saveFlow.viewActive() && editVersion.current===generation)setSaveError(cause instanceof Error ? cause.message : 'Save is not confirmed; recovery data was retained.');
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
      const res = await sessionFetch(`/api/repos/${id}/test-svn`, {
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
      const res = await sessionFetch(`/api/repos/${id}/test-git`, {
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
    const next=currentForm.current ? {...currentForm.current,[key]:value} : null;
    currentForm.current=next;setForm(next);
    if(next)saveFlow.rememberDraft(editConfiguration(next));
  }

  /**
   * The credentials summary sends nothing: it opens the existing field in its own region.
   * Like Edit, it changes the section only where the section navigation is displayed.
   */
  function replaceCredential(field: CredentialField) {
    if (!editing) startEdit();
    setCredentialFocus(previous => ({ field, request: (previous?.request ?? 0) + 1 }));
  }
  useEffect(() => {
    if (!credentialFocus || !editing || !form) return;
    const input = document.getElementById(`${fieldId}-${credentialFocus.field === 'svn' ? 1 : 2}`);
    if (input) { input.focus(); input.scrollIntoView({ block: 'center' }); }
    setCredentialFocus(null);
  }, [credentialFocus, editing, form, fieldId]);

  useEffect(() => {
    setLocalNotice(null);
    if (!id) {
      setRemovalReceipt(null);
      return;
    }
    setRemovalReceipt(readManagedRemovalReceipt(id));
  }, [id]);

  useEffect(() => {
    if (!id || !removalStatusQuery.data) return;
    const status = removalStatusQuery.data;
    if (trackedRemovalOperationId && status.operation_id !== trackedRemovalOperationId) {
      return;
    }
    const stored = readManagedRemovalReceipt(id);
    if (!shouldPersistPolledManagedRemovalReceipt(stored, status)) {
      return;
    }
    const receipt = managedRemovalReceiptFromStatus(id, status);
    persistManagedRemovalReceipt(receipt);
    setRemovalReceipt(receipt);
  }, [id, removalStatusQuery.data, trackedRemovalOperationId]);

  useEffect(() => {
    if (!retiredId || retiredId === id) return;
    for (const queryKey of repoDetailQueryKeys(retiredId)) {
      queryClient.removeQueries({ queryKey });
    }
    setRetiredId(null);
  }, [id, retiredId, queryClient]);

  const pageReady = !repoMissing && !!repo && !isError;
  useEffect(() => {
    if (previousSection.current !== section) {
      previousSection.current = section;
      pendingSectionScroll.current = section;
    }
    const target = pendingSectionScroll.current;
    const root = pageRef.current;
    const nav = sectionNavRef.current;
    // Until the page itself renders, keep the request for when it does.
    if (!target || !root || !nav) return;
    pendingSectionScroll.current = null;
    // Where the section navigation is shown, the layout shows the section; nothing scrolls.
    if (sectionNavDisplayed(getComputedStyle(nav).display)) return;
    firstSectionRegion(root, target)?.scrollIntoView({ block: 'start' });
  }, [section, pageReady]);

  const removalNotice = localNotice ?? routedNotice ?? readPersistedBranchPairRemovalNotice();

  if (repoMissing) {
    return (
        <div className="rs-page max-w-lg mx-auto py-16 text-center space-y-4" data-testid="repo-not-found" data-rs-page="repo" data-rs-section={section}>
          {removalNotice && <BranchPairRemovalNotice notice={removalNotice} />}
          {isAdmin && (
            <section data-testid="removed-registration-recovery" className="text-left space-y-3">
              <p className="text-sm text-gray-400">Retained removal evidence. Actions require a fresh operation-status read.</p>
              {removalStatusQuery.isFetching && <p role="status">Verifying the retained removal receipt…</p>}
              {removalStatusQuery.isError && <p role="alert">Current removal verification is unavailable: {removalStatusQuery.error.message}. Recovery actions remain disabled.</p>}
              {removalPanelStatus && <ManagedRemovalPanel status={removalPanelStatus}
                retryPending={removeMutation.isPending}
                onRestore={removalStatusQuery.data?.recovery?.restore_supported && removalStatusQuery.data.state === 'completed' && removalStatusQuery.data.registration_listed === false && !removalStatusQuery.isFetching && !removalStatusQuery.isError ? () => restoreMutation.mutate() : undefined}
                restorePending={restoreMutation.isPending} />}
              {removalPanelStatus && removalPanelStatus.state !== 'completed' && <p role="alert">An incomplete operation with a missing registration needs evidence reconciliation. No cleanup retry is sent from this missing-registration view.</p>}
              {restoreMutation.isError && <p role="alert">Restore failed: {restoreMutation.error.message}</p>}
            </section>
          )}
          <h1 className="rs-title text-xl font-semibold text-gray-100">Repository not found</h1>
          <p className="text-sm text-gray-400">
            This repository or branch pair is no longer available. It may have been removed.
          </p>
          <Link
            to="/repos"
            data-testid="repo-not-found-repositories"
            className="inline-flex items-center justify-center px-4 py-2 rounded-lg bg-blue-600 hover:bg-blue-700 rs-text-on-accent text-sm font-medium"
          >
            Back to repositories
          </Link>
        </div>
    );
  }

  if (!repo && (isLoading || retiredId === id)) {
    return <div className="rs-page text-center py-8 text-gray-400" data-rs-page="repo" data-rs-section={section}>Loading repository...</div>;
  }

  if (isError || !repo) {
    return (
      <div className="rs-page text-center py-8 space-y-4" data-rs-page="repo" data-rs-section={section}>
        <p className="text-red-400">
          Error loading repository: {error?.message ?? 'Not found'}
        </p>
        <Link to="/repos" className="text-sm text-blue-400 hover:text-blue-300">
          Back to repositories
        </Link>
      </div>
    );
  }

  const cmEntries = commitMap?.entries ?? [];
  // G04.9: shown only while the scoped status is fresh and reports the pause (unchanged condition).
  const pausedBannerShown = !archived && statusView.canAct && status?.state === 'error_paused' && isAdmin;
  const syncSwitch = headerSyncSwitch(repo);

  return (
    <div ref={pageRef} className="rs-page space-y-6" data-testid="repo-detail" data-repo-id={id} data-rs-page="repo" data-rs-section={section}>
      {removalNotice && <BranchPairRemovalNotice notice={removalNotice} />}
      {branchPairDeleteError && (
        <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm" role="alert">
          {branchPairDeleteError}
        </div>
      )}
      {isAdmin && !repo.parent_id && removalPanelStatus && (
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
      <div className="flex items-center justify-between" {...region('head', EVERY_SECTION)}>
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

      <div className="flex items-center justify-between" {...region('head', EVERY_SECTION)}>
        <div className="flex items-center gap-3">
          <h1 className="rs-title text-2xl font-bold text-gray-100" data-testid="repo-detail-heading">{repo.name}</h1>
          <span
            className={`rs-mark inline-flex items-center px-2.5 py-0.5 rounded text-xs font-medium ${
              repo.enabled
                ? 'bg-blue-900/50 text-blue-300'
                : 'bg-gray-700 text-gray-400'
            }`}
          >
            {archived ? 'Archived · sync permanently stopped' : repo.enabled ? 'Sync configured enabled' : 'Sync configured disabled'}
          </span>
        </div>
        <div className="flex items-center gap-3">
          {/* G04.23: a repository pauses through the explicit disable call and its dialog; a branch
              pair and turning sync on keep the one-click registration update (PUT enabled). */}
          <button
            data-testid="header-sync-switch"
            onClick={() => (syncSwitch === 'explicit-pause' ? openDisableConfirm() : toggleMutation.mutate(!repo.enabled))}
            disabled={archived || toggleMutation.isPending || (syncSwitch === 'explicit-pause' && disableMutation.isPending)}
            title={syncSwitch === 'explicit-pause'
              ? 'Opens Pause / disable sync, which uses the explicit disable API.'
              : 'Quick sync on/off via registration update. Use Pause / disable in Danger Zone for the explicit disable API.'}
            className={`rs-btn rs-btn--quiet inline-flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
              repo.enabled
                ? 'border border-yellow-600 text-yellow-300 hover:bg-yellow-900/30'
                : 'border border-green-600 text-green-300 hover:bg-green-900/30'
            } disabled:opacity-50`}
          >
            <Power className="w-4 h-4" />
            {syncSwitch === 'explicit-pause' ? t('repo.header.pause') : repo.enabled ? 'Turn sync off' : 'Turn sync on'}
          </button>

          {!editing ? (
            <button
              disabled={archived}
              onClick={startEdit}
              className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-gray-600 hover:border-gray-500 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors"
            >
              <Settings className="w-4 h-4" />
              Edit
            </button>
          ) : (
            <>
              <button
                onClick={handleSave}
                disabled={archived || saveFlow.pending}
                className="rs-btn rs-btn--primary inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-blue-600 hover:bg-blue-700 disabled:opacity-50 rs-text-on-accent text-sm font-medium transition-colors"
              >
                <Save className="w-4 h-4" />
                {saveFlow.pending ? 'Saving...' : 'Save'}
              </button>
              <button
                onClick={cancelEdit}
                className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-gray-600 hover:border-gray-500 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors"
              >
                <X className="w-4 h-4" />
                Cancel
              </button>
            </>
          )}
          {(repo?.allowed_paths || repo?.blocked_patterns) && (
            <button
              type="button"
              data-testid="download-pre-commit-hook"
              onClick={() => hookDownload.mutate()}
              disabled={hookDownload.isPending}
              aria-busy={hookDownload.isPending || undefined}
              className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-gray-600 text-gray-300 rs-hover-text-strong hover:border-gray-500 disabled:opacity-50 text-sm font-medium transition-colors"
              title="Download Git pre-commit hook script"
            >
              <GitBranch className="w-4 h-4" />
              {t('repo.header.hook')}
            </button>
          )}
          <button
            onClick={() => syncMutation.mutate()}
            disabled={archived || syncMutation.isPending || syncTriggered !== null || !statusView.canAct}
            className="rs-btn rs-btn--primary inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-blue-600 hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed rs-text-on-accent text-sm font-medium transition-colors"
          >
            <RefreshCw className={`w-4 h-4 ${syncMutation.isPending ? 'animate-spin' : ''}`} />
            {syncTriggered ?? 'Trigger Sync'}
          </button>
        </div>
      </div>

      {archived && <div role="status" data-testid="archived-pair-notice" className="rs-bn border border-blue-700 rounded-lg p-4 text-sm text-blue-200" {...region('head', EVERY_SECTION)}>
        This pair is permanently archived. The surviving remote branch, pair record, local data and history are retained. Sync, edits, remote deletion and reattachment are unavailable.
      </div>}
      <RepositoryStatusLine view={statusView} />
      <RepositorySectionNav current={section} navRef={sectionNavRef} />

      {/* Mutation errors */}
      {syncMutation.isError && (
        <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-4 text-red-300 text-sm" role="alert">
          Failed to trigger sync: {syncMutation.error?.message}
        </div>
      )}
      {toggleMutation.isError && (
        <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-4 text-red-300 text-sm" role="alert" data-testid="sync-switch-error">
          {toggleMutation.variables ? 'Sync could not be turned on' : 'Sync could not be turned off'}: {toggleMutation.error?.message}
        </div>
      )}
      {hookDownload.isError && (
        <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-4 text-red-300 text-sm" role="alert" data-testid="hook-download-error">
          The hook could not be downloaded: {hookDownload.error?.message}
        </div>
      )}
      {editing && <RepositorySaveNotice attempt={saveFlow.attempt} error={saveError ?? saveFlow.error} pending={saveFlow.pending} onLoadCurrent={loadCurrentSettingsForReview} onUseReviewed={useReviewedSettings} reviewed={!!saveFlow.reviewedCurrent} />}
      {editing && saveMessage && <p role="status" className="text-yellow-300 mb-3">{saveMessage}</p>}

      {/* Config Cards - SVN and Git side by side */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4" data-rs-sections="settings">
        {/* SVN Config */}
        <div className="rs-sec bg-gray-800/60 border border-gray-700 rounded-lg p-5" {...region('svn', 'settings')}>
          <div className="flex items-center gap-2 mb-4">
            <Database className="w-5 h-5 text-blue-400" />
            <h2 className="rs-heading text-lg font-semibold text-gray-100">{t('repo.section.svn')}</h2>
          </div>
          <div className="space-y-3">
            {editing && form ? (
              <>
                <FieldInput label="Name" value={form.name} onChange={(v) => setField('name', v)} />
                <FieldInput label={t('field.svnUrl')} value={form.svn_url} onChange={(v) => setField('svn_url', v)} />
                <FieldInput label="Branch" value={form.svn_branch} onChange={(v) => setField('svn_branch', v)} />
                <FieldInput label="Username" value={form.svn_username} onChange={(v) => setField('svn_username', v)} />
                <div>
                  <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-1`}>{t('repo.field.svnPassword')}</label>
                  <input
                    type="password"
                    className={inputClass}
                    value={form.svn_password}
                    onChange={(e) => setField('svn_password', e.target.value)}
                    placeholder={credStatus?.svn_password_set ? '\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF (saved)' : 'Enter password'} id={`${fieldId}-1`}
                  />
                </div>
                <button
                  type="button"
                  onClick={handleTestSvn}
                  disabled={svnTesting}
                  className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium border border-blue-600 text-blue-300 hover:bg-blue-900/30 disabled:opacity-50 transition-colors"
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
                <ConfigRow label={t('field.svnPassword')} value={credStatus?.svn_password_set ? '\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF (saved)' : 'Not set'} />
              </>
            )}
          </div>
        </div>

        {/* Git Config */}
        <div className="rs-sec bg-gray-800/60 border border-gray-700 rounded-lg p-5" {...region('git', 'settings')}>
          <div className="flex items-center gap-2 mb-4">
            <GitBranch className="w-5 h-5 text-purple-400" />
            <h2 className="rs-heading text-lg font-semibold text-gray-100">{t('repo.section.git')}</h2>
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
                <FieldInput label={t('field.defaultBranch')} value={form.git_branch} onChange={(v) => setField('git_branch', v)} />
                <div>
                  <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-2`}>{t('field.gitToken')}</label>
                  <input
                    type="password"
                    className={inputClass}
                    value={form.git_token}
                    onChange={(e) => setField('git_token', e.target.value)}
                    placeholder={credStatus?.git_token_set ? '\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF (saved)' : 'Enter token'} id={`${fieldId}-2`}
                  />
                </div>
                <button
                  type="button"
                  onClick={handleTestGit}
                  disabled={gitTesting}
                  className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium border border-purple-600 text-purple-300 hover:bg-purple-900/30 disabled:opacity-50 transition-colors"
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
                <ConfigRow label={t('repo.field.gitToken')} value={credStatus?.git_token_set ? '\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF\u25CF (saved)' : 'Not set'} />
              </>
            )}
          </div>
        </div>
      </div>

      <CredentialsSummary read={credentialsQuery} onReplace={replaceCredential} />

      {/* Sync Settings */}
      <div className="rs-sec bg-gray-800/60 border border-gray-700 rounded-lg p-5" {...region('sync', 'settings')}>
        <div className="flex items-center gap-2 mb-4">
          <Activity className="w-5 h-5 text-green-400" />
          <h2 className="rs-heading text-lg font-semibold text-gray-100">{t('repo.section.sync')}</h2>
        </div>
        {editing && form ? (
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
            <FieldNumber label={t('repo.field.pollInterval')} value={form.poll_interval_secs} onChange={(v) => setField('poll_interval_secs', v)} min={10} />
            <FieldNumber label={t('repo.field.lfsThreshold')} value={form.lfs_threshold_mb} onChange={(v) => setField('lfs_threshold_mb', v)} min={0} />
            <FieldToggle label={t('field.autoMerge')} checked={form.auto_merge} onChange={(v) => setField('auto_merge', v)} />
            <div className="col-span-full grid grid-cols-2 gap-4" {...region('policy', 'settings')}>
              <div>
                <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-3`}>Allowed SVN Paths <span className="text-gray-600">(one per line)</span></label>
                <textarea
                  className="w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 font-mono placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                  rows={3}
                  value={form.allowed_paths}
                  onChange={(e) => setField('allowed_paths', e.target.value)}
                  placeholder={"source/\nconfig/"} id={`${fieldId}-3`}
                />
                <p className="text-xs text-gray-300 mt-1">Files must be under these path prefixes. Leave empty for no restriction.</p>
              </div>
              <div>
                <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-4`}>Blocked Patterns <span className="text-gray-600">(one per line)</span></label>
                <textarea
                  className="w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 font-mono placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                  rows={3}
                  value={form.blocked_patterns}
                  onChange={(e) => setField('blocked_patterns', e.target.value)}
                  placeholder={"*.exe\ntemp/"} id={`${fieldId}-4`}
                />
                <p className="text-xs text-gray-300 mt-1">Files matching these patterns will be excluded from SVN sync.</p>
              </div>
            </div>
          </div>
        ) : (
          <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            <div>
              <span className="text-sm text-gray-400">{t('field.pollInterval')}</span>
              <p className="text-lg text-gray-100">{repo.poll_interval_secs}s</p>
            </div>
            <div>
              <span className="text-sm text-gray-400">{t('field.lfsThreshold')}</span>
              <p className="text-lg text-gray-100">{repo.lfs_threshold_mb} MB</p>
            </div>
            <div>
              <span className="text-sm text-gray-400">{t('field.autoMerge')}</span>
              <p className="text-lg text-gray-100">{repo.auto_merge ? 'Yes' : 'No'}</p>
            </div>
          </div>
        )}
      </div>

      {/* Quick Stats */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-4" {...region('about', 'settings')}>
        <StatCard
          icon={<Zap className="w-5 h-5 text-yellow-400" />}
          label={t('field.lfsThreshold')}
          value={`${repo.lfs_threshold_mb} MB`}
        />
        <StatCard
          icon={<Clock className="w-5 h-5 text-blue-400" />}
          label="Created"
          value={new Date(repo.created_at).toLocaleDateString()}
        />
        <StatCard
          icon={<Activity className="w-5 h-5 text-green-400" />}
          label={t('repo.about.lastUpdated')}
          value={formatTimeAgo(repo.updated_at)}
        />
        <StatCard
          icon={<Database className="w-5 h-5 text-purple-400" />}
          label={t('repo.about.createdBy')}
          value={repo.created_by ?? 'System'}
        />
      </div>

      {/* ===== NEW DASHBOARD SECTIONS ===== */}

      {/* Circuit Breaker: Error Paused Banner */}
      {pausedBannerShown && (
        <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-4 flex items-center justify-between" {...region('paused', 'overview')}>
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
              className="rs-btn rs-btn--state px-3 py-1.5 rounded-lg bg-yellow-600 hover:bg-yellow-700 disabled:opacity-50 disabled:cursor-not-allowed rs-text-on-state text-sm font-medium transition-colors"
            >
              {t('repo.paused.skip')}
            </button>
            <button
              type="button"
              data-testid="resume-sync-button"
              onClick={() => {
                resumeMutation.reset();
                setShowResumeConfirm(true);
              }}
              className="rs-btn rs-btn--primary px-3 py-1.5 rounded-lg bg-blue-600 hover:bg-blue-700 rs-text-on-accent text-sm font-medium transition-colors"
            >
              Resume sync…
            </button>
          </div>
        </div>
      )}

      {showSkipModal && skipContextEnabled && skipContext && (
        <Dialog title="Skip selected pending commits" description="Only the selected observed pending prefix will be excluded. Unselected work stays pending." onClose={() => { setShowSkipModal(false); setSkipError(null); }} busy={skipMutation.isPending} testId="skip-commit-modal">
          <div className="rs-dlg rs-sec bg-gray-800 border border-gray-600 rounded-lg p-6 max-w-lg w-full mx-4 shadow-xl">

            <p className="text-sm text-gray-400 mb-4">
              Select the oldest contiguous pending prefix to exclude. Unselected commits remain pending.
              Observed remote tip: <span className="font-mono text-gray-300">{skipContext.observed_remote_tip?.slice(0, 12) ?? 'unavailable'}</span>
            </p>
            <SkipPrefixChoices
              name={`${fieldId}-skip-through`}
              commits={skipContext.pending_commits.filter((c) => !c.excluded)}
              selectedCount={skipSelectedCount}
              onSelect={setSkipSelectedCount}
              disabled={skipMutation.isPending}
            />
            {skipError && <p className="text-sm text-red-400 mb-3" role="alert">{skipError}</p>}
            <div className="flex flex-wrap items-center justify-end gap-2">
              {skipSelectedCount === 0 && <span className="text-xs text-gray-400 mr-auto">Nothing is selected yet.</span>}
              <button
                type="button"
                onClick={() => { setShowSkipModal(false); setSkipError(null); }}
                className="px-3 py-1.5 rounded-lg bg-gray-700 hover:bg-gray-600 text-sm text-gray-200"
               data-dialog-cancel disabled={skipMutation.isPending}>
                Cancel
              </button>
              <button
                type="button"
                disabled={!skipContextEnabled || skipSelectedCount === 0 || skipMutation.isPending}
                data-testid="skip-commit-confirm"
                onClick={() => {
                  if(!id || !canConfirmObservedSkip(latestScopedSkipObservation(queryClient,skipState.current),id)){setSkipError('Status changed; no Skip request was sent.');return;}
                  const pending = skipContext.pending_commits.filter((c) => !c.excluded);
                  const selected = pending.slice(0, skipSelectedCount).map((c) => c.sha);
                  skipMutation.mutate(selected);
                }}
                className="rs-btn rs-btn--state px-3 py-1.5 rounded-lg bg-yellow-700 hover:bg-yellow-800 disabled:opacity-50 text-sm rs-text-on-state font-medium"
              >
                {skipMutation.isPending ? 'Skipping…' : 'Skip selected'}
              </button>
            </div>
          </div>
        </Dialog>
      )}

      {showResumeConfirm && (
        <ResumeSyncDialog
          repoName={repo.name}
          pendingCount={skipContext ? skipContext.pending_commits.filter((c) => !c.excluded).length : null}
          allowed={pausedBannerShown}
          pending={resumeMutation.isPending}
          error={resumeMutation.isError ? resumeMutation.error.message : null}
          onConfirm={() => { if (pausedBannerShown) resumeMutation.mutate(); }}
          onClose={() => { setShowResumeConfirm(false); resumeMutation.reset(); }}
        />
      )}

      {/* The panel's own top margin is replaced by the page spacing, as before. */}
      <div className="[&>section]:mt-0" {...region('status', 'overview')}>
        <StatusObservationPanel view={statusView}/>
      </div>
      {/* Status Cards Row */}
      <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-5 gap-4" {...region('status', 'overview')}>
        <StatusCard
          title="Effective sync health"
          value={statusView.healthLabel}
          color={statusView.health==='idle' ? 'green':'gray'}
        />
        <StatusCard
          title="Last Sync"
          value={status?.last_sync_at ? formatTimeAgo(status.last_sync_at) : status ? 'Not recorded':'Unknown'}
          color="gray"
        />
        <StatusCard
          title="Total Syncs"
          value={statusView.metric('total_syncs')}
          color="blue"
        />
        <StatusCard
          title="Active Conflicts"
          value={statusView.metric('active_conflicts')}
          color={status?.active_conflicts ? 'red' : 'gray'}
          to="/conflicts"
        />
        <StatusCard
          title="Errors (24h)"
          value={errorCard.value}
          color={errorCard.count ? 'red' : 'gray'}
          subtitle={
            errorCard.lastErrorAt
              ? `Last: ${formatTimeAgo(errorCard.lastErrorAt)}`
              : errorCard.description
          }
          to={`/audit?success=false&repo_id=${id}`}
          action={
            !archived && <ErrorAcknowledgement key={id ?? 'all'} scope={errorScope(id)}
              repositoryName={repo?.name ?? id ?? ''} observation={errorCard.observation}
              count={errorCard.count}
              onSuccess={() => { void refreshErrorStatus(queryClient); }} />
          }
        />
      </div>

      {showBaselineNotice && baselineNoticeFields && (
        <div className="empty:hidden" {...region('import', 'overview operations')}>
          <ImportHistoryNotice fields={baselineNoticeFields} />
        </div>
      )}

      {/* Archived pairs retain evidence but cannot start or mutate imports. */}
      {detailLive && !archived && (
        <ImportProgressCard
          repoId={id}
          repoName={repo?.name}
          defaultImportMode={importPrefs?.importMode ?? DEFAULT_REPO_IMPORT_MODE}
          defaultSvnRevision={importPrefs?.svnRevision ?? 'HEAD'}
          regionName="import"
          regionSections="overview operations"
        />
      )}

      {/* Sync Records */}
      <div className="rs-sec bg-gray-800 shadow rounded-lg border border-gray-700" {...region('records', 'overview activity')}>
        <div className="p-6 pb-3">
          <h2 className="rs-heading text-lg font-semibold text-gray-100">
            {t('repo.section.records')}
            <span className="ml-2 text-sm font-normal text-blue-400">&mdash; {repo.name}</span>
          </h2>
          <p className="text-sm text-gray-400 mt-1">Recent commits synced for this repository (click to expand)</p>
        </div>
        <SyncRecordsBody read={syncRecordsQuery} />
      </div>

      {/* Commit Map */}
      <div className="rs-sec bg-gray-800 shadow rounded-lg border border-gray-700" {...region('commit-map', 'activity')}>
        <div className="p-6 pb-3">
          <h2 className="rs-heading text-lg font-semibold text-gray-100">
            {t('repo.section.commitMap')}
            <span className="ml-2 text-sm font-normal text-blue-400">&mdash; {repo.name}</span>
          </h2>
          <p className="text-sm text-gray-400 mt-1">Bidirectional mapping between SVN revisions and Git commits</p>
        </div>
        {cmEntries.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="rs-tbl min-w-full divide-y divide-gray-700">
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
      <div className="rs-sec bg-gray-800 shadow rounded-lg border border-gray-700" {...region('audit', 'activity')}>
        <div className="p-6 pb-3">
          <h2 className="rs-heading text-lg font-semibold text-gray-100">
            {t('repo.section.audit')}
            <span className="ml-2 text-sm font-normal text-blue-400">&mdash; {repo.name}</span>
          </h2>
          <p className="text-sm text-gray-400 mt-1">Last 10 audit entries for this repository</p>
        </div>
        {auditGroups.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="rs-tbl min-w-full divide-y divide-gray-700">
              <thead>
                <tr>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Sync configuration</th>
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

                      >
                        <td className="px-6 py-3">
                          <div className="flex items-center space-x-1">
                            {isGroup && <button type="button" onClick={() => {
                          if (!isGroup) return;
                          setExpandedAuditGroups(prev => {
                            const next = new Set(prev);
                            if (next.has(group.key)) next.delete(group.key); else next.add(group.key);
                            return next;
                          });
                        }} aria-expanded={isGrpExpanded} aria-label={`Audit group: ${latest.action}, ${group.entries.length} entries`} className="text-blue-300 underline">{isGrpExpanded ? 'Hide group' : 'Show group'}</button>}
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
                              <span className="rs-mark text-xs bg-gray-600 text-gray-300 rounded-full px-2 py-0.5">
                                &times;{group.entries.length}
                              </span>
                            )}
                          </div>
                        </td>
                        <td className="px-6 py-3 text-sm text-gray-300">{latest.author ?? '-'}</td>
                        <td className="px-6 py-3 text-sm text-gray-400">
                          <div>
                            {isLong ? <button type="button" onClick={(e) => {
                                if (!isLong) return;
                                e.stopPropagation();
                                setExpandedDetails(prev => {
                                  const next = new Set(prev);
                                  if (next.has(latest.id)) next.delete(latest.id); else next.add(latest.id);
                                  return next;
                                });
                              }} aria-expanded={isDetExpanded} className="text-left text-blue-300 underline">{isDetExpanded ? detailText : (isLong ? `${detailText.substring(0, 80)}...` : detailText)}</button> : <span>{detailText}</span>}
                            {isDetExpanded && (
                              <pre className="mt-2 p-3 bg-gray-900 rounded text-xs font-mono text-gray-300 whitespace-pre-wrap break-words max-w-lg">
                                {detailText}
                              </pre>
                            )}
                          </div>
                        </td>
                        <td className="px-6 py-3 text-sm text-gray-300">
                          {new Date(latest.created_at).toLocaleString()}
                        </td>
                      </tr>
                      {isGroup && isGrpExpanded && group.entries.slice(1).map((entry: AuditEntry) => (
                        <tr key={entry.id} className="bg-gray-900/40">
                          <td className="px-6 py-2"></td>
                          <td className="px-6 py-2"><ActionBadge action={entry.action} /></td>
                          <td className="px-6 py-2 text-sm text-gray-400">{entry.author ?? '-'}</td>
                          <td className="px-6 py-2 text-sm text-gray-300 truncate max-w-[300px]">{entry.details || entry.action}</td>
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

      {repo?.parent_id && !archived && (
        <div className="rs-sec bg-gray-800 shadow rounded-lg border border-gray-700 p-6" {...region('refresh', 'branches')}>
          <h2 className="rs-heading text-lg font-semibold text-gray-100">Update pair from parent</h2>
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
              className="rs-btn rs-btn--primary inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium bg-blue-600 hover:bg-blue-700 disabled:opacity-50 rs-text-on-accent"
            >
              <RefreshCw className="w-3.5 h-3.5" />
              {refreshMutation.isPending ? 'Previewing...' : 'Preview update from parent'}
            </button>
            <button
              {...region('reanchor', 'branches')}
              data-testid="reanchor-not-implemented"
              onClick={() => refreshMutation.mutate({ repoId: id!, operation: 'reanchor' })}
              disabled={refreshMutation.isPending}
              className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium border border-amber-700 text-amber-200 hover:bg-amber-900/30 disabled:opacity-50"
            >
              Re-anchor pair (not implemented)
            </button>
          </div>
        </div>
      )}

      {(refreshPlan || refreshError) && (
        <div className="rs-sec bg-gray-800 shadow rounded-lg border border-gray-700 p-6" data-testid="pair-refresh-result" {...region('refresh', 'branches')}>
          {refreshError && (
            <div className="rs-bn rs-bn--warn bg-amber-900/30 border border-amber-700 rounded-lg p-3 text-amber-100 text-sm">
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
              <p className="text-xs text-gray-300">
                Discards unsynced work: {String(refreshPlan.intended_result.discards_unsynced_work)}. Execute: {refreshPlan.execute_status}. Re-anchor: {refreshPlan.reanchor_status}.
              </p>
            </div>
          )}
        </div>
      )}

      {/* Branch Pairs */}
      <div className="rs-sec bg-gray-800 shadow rounded-lg border border-gray-700" {...region('pairs', 'overview branches')}>
        <div className="p-6 pb-3 flex items-center justify-between">
          <div>
            <h2 className="rs-heading text-lg font-semibold text-gray-100 flex items-center gap-2">
              <GitBranch className="w-5 h-5 text-purple-400" />
              {t('repo.section.pairs')}
            </h2>
            <p className="text-sm text-gray-400 mt-1">SVN branches synced to separate Git branches</p>
          </div>
          {isAdmin && !archived && (
            <button
              onClick={() => setShowBranchModal(true)}
              className="rs-btn rs-btn--state inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium bg-purple-600 hover:bg-purple-700 rs-text-on-state transition-colors"
            >
              <Plus className="w-3.5 h-3.5" />
              Add Branch Pair
            </button>
          )}
        </div>
        {branchSuccess && branchPlan && (
          <div className="rs-bn rs-bn--info mx-6 mb-3 bg-blue-900/30 border border-blue-700 rounded-lg p-3 text-blue-200 text-sm">
            {branchPlan.published ? 'Pair publication completed.' : branchPlan.operation_id ? <>Pair operation: {latePairStateWord(branchPlan.pair_state)}{latePairStateWord(branchPlan.pair_state) !== branchPlan.pair_state && <> <span className="font-mono text-xs">({branchPlan.pair_state})</span></>}.</> : 'Preview only; no repository changes.'}
            {' '}Git tip {branchPlan.git_tip?.slice(0, 8) || 'unknown'}, baseline r{branchPlan.verified_baseline?.svn_revision ?? '?'},
            {' '}{branchPlan.pending_git.count} pending Git commits. Scheduler {branchPlan.scheduler_active ? 'active' : 'inactive'}.
          </div>
        )}
        {(branchPairs ?? []).length > 0 ? (
          <div className="overflow-x-auto">
            <table className="rs-tbl min-w-full divide-y divide-gray-700">
              <thead>
                <tr>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Name</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">SVN Branch</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Git Branch</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Sync configuration</th>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Health / freshness</th>
                  {isAdmin && <th className="px-6 py-3 text-right text-xs font-medium text-gray-400 uppercase"></th>}
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-700">
                {(branchPairs ?? []).map((bp) => (
                  <tr
                    key={bp.id}
                    data-testid={`branch-pair-row-${bp.id}`}
                    className="hover:bg-gray-700/50 cursor-pointer transition-colors"
                  >
                    <td className="px-6 py-3 text-sm text-gray-200 font-medium"><Link to={`/repos/${bp.id}`} className="text-blue-300 underline">{bp.name}</Link></td>
                    <td className="px-6 py-3 text-sm font-mono text-blue-400">{bp.svn_branch}</td>
                    <td className="px-6 py-3 text-sm font-mono text-purple-400">{bp.git_branch}</td>
                    <td className="px-6 py-3">
                      <span className={`rs-mark inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
                        bp.enabled ? 'bg-blue-900/50 text-blue-300' : 'bg-gray-700 text-gray-400'
                      }`}>
                        {bp.sync_status === 'archived' ? 'Archived' : bp.enabled ? 'Configured enabled' : 'Configured disabled'}
                      </span>
                    </td>
                    <td className="px-6 py-3 text-sm text-gray-400"><RepositoryStatusSummary repoId={bp.id}/></td>
                    {isAdmin && (
                      <td className="px-6 py-3 text-right">
                        <div className="inline-flex items-center gap-2">
                          <button
                            data-testid={`preview-refresh-${bp.id}`}
                            disabled={bp.sync_status === 'archived'}
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
                            disabled={bp.sync_status === 'archived'}
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
                            disabled={bp.sync_status === 'archived'}
                            onClick={(e) => {
                              e.stopPropagation();
                              setBranchPairDeleteTarget(bp);
                              setBranchPairDeleteError(null);
                              setRemoveConfirmText('');
                              setRemoveBranchOpts({ delete_git: false, delete_svn: false });
                              removeMutation.reset();
                            }}
                            className="text-gray-300 hover:text-red-400 transition-colors p-1"
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
        <Dialog title="Review Branch Pair" description="Capture both sides, inspect the intended result, then explicitly publish or recover the pinned operation." onClose={() => { setShowBranchModal(false); setBranchForm({ svn_branch: '', git_branch: '', skip_import: false, auto_create_svn_branch: true, auto_create_git_branch: true }); setBranchPlan(null); branchMutation.reset(); }} busy={branchMutation.isPending}>
          <div className="rs-dlg bg-gray-800 border border-gray-700 rounded-lg shadow-xl w-full max-w-md">
            <div className="flex items-center justify-between p-6 border-b border-gray-700">

              <button
                onClick={() => { setShowBranchModal(false); setBranchForm({ svn_branch: '', git_branch: '', skip_import: false, auto_create_svn_branch: true, auto_create_git_branch: true }); setBranchPlan(null); branchMutation.reset(); }}
                className="text-gray-400 hover:text-gray-200 transition-colors"
               aria-label="Close dialog" data-dialog-cancel disabled={branchMutation.isPending}>
                <X className="w-5 h-5" />
              </button>
            </div>
            <div className="p-6 space-y-4">
              {branchMutation.isError && (
                <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm" role="alert">
                  Pairing refused: {branchMutation.error?.message}
                </div>
              )}
              {branchPlan && (
                <div className="bg-gray-900/70 border border-gray-600 rounded-lg p-3 text-xs text-gray-300 space-y-1 font-mono">
                  <div role="status">mode: {branchPlan.mode} · state: {branchPlan.pair_state}</div>
                  <div>intended Git result: {branchPlan.result_git_sha || 'blocked'}</div>
                  <div>result digest: {branchPlan.result_tree_digest || 'unknown'}</div>
                  <div>handled steps: {branchPlan.handled_steps ?? 0}/{branchPlan.total_steps ?? 0}</div>
                  <div>{branchPlan.observation}</div>
                  {branchPlan.conflicts.map((conflict) => <div key={conflict} role="alert" className="text-red-300">{conflict}</div>)}
                  {branchPlan.unknowns.map((unknown) => <div key={unknown} role="alert" className="text-amber-300">{unknown}</div>)}
                  {branchPlan.action_error && <div role="alert">{branchPlan.action_error}</div>}
                  <div>git tip: {branchPlan.git_tip || 'unknown'}</div>
                  <div>SVN source: r{branchPlan.svn_source_revision ?? 'unknown'} · copy from r{branchPlan.proposed_svn_copy_source_revision ?? 'n/a'}</div>
                  <div>pending Git: {branchPlan.pending_git.count} · published: {String(branchPlan.published)} · scheduler: {String(branchPlan.scheduler_active)}</div>
                  {branchPlan.existing_svn_target.exists && (
                    <div className="text-amber-300">Independent SVN work will be reconciled against the verified baseline</div>
                  )}
                  {branchPlan.skip_import_note && <div className="text-amber-200">{branchPlan.skip_import_note}</div>}
                </div>
              )}

              {/* Parent context */}
              <div className="bg-gray-900/50 border border-gray-700/50 rounded-lg px-3 py-2 text-xs text-gray-400">
                <span className="text-gray-300">Branching from:</span>{' '}
                <span className="text-blue-400 font-mono">{repo?.svn_branch || 'trunk'}</span>
                {' \u2192 '}
                <span className="text-purple-400 font-mono">{repo?.git_branch || 'main'}</span>
              </div>

              <div>
                <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-5`}>Branch Name <span className="text-gray-300 text-xs">({branchForm.git_branch.length}/200)</span></label>
                <input
                  type="text"
                  className={inputClass}
                  disabled={branchMutation.isPending || !!branchPlan?.operation_id}
                  value={branchForm.git_branch}
                  maxLength={200}
                  onChange={(e) => {
                    setBranchPlan(null);
                    const name = e.target.value;
                    setBranchForm(prev => ({
                      ...prev,
                      git_branch: name,
                      svn_branch: name ? `branches/${name}` : '',
                    }));
                  }}
                  placeholder="e.g., fix-123, dev/james-wilson" id={`${fieldId}-5`}
                  aria-invalid={gitBranchValidation.error ? true : undefined}
                  aria-describedby={gitBranchValidation.error ? `${fieldId}-git-branch-error` : undefined}
                />
                {gitBranchValidation.error && <p id={`${fieldId}-git-branch-error`} role="alert" className="mt-1 text-xs text-red-400">{gitBranchValidation.error}</p>}
              </div>
              <div>
                <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-6`}>SVN Branch Path <span className="text-gray-300">(auto-derived, editable)</span></label>
                <input
                  type="text"
                  className={inputClass}
                  disabled={branchMutation.isPending || !!branchPlan?.operation_id}
                  value={branchForm.svn_branch}
                  maxLength={200}
                  onChange={(e) => { setBranchPlan(null); setBranchForm(prev => ({ ...prev, svn_branch: e.target.value })); }}
                  placeholder="branches/fix-123" id={`${fieldId}-6`}
                  aria-invalid={svnBranchValidation.error ? true : undefined}
                  aria-describedby={svnBranchValidation.error ? `${fieldId}-svn-branch-error` : undefined}
                />
                {svnBranchValidation.error && <p id={`${fieldId}-svn-branch-error`} role="alert" className="mt-1 text-xs text-red-400">{svnBranchValidation.error}</p>}
              </div>

              <label className="flex items-center gap-2 text-sm text-gray-300">
                <input type="checkbox" checked={branchIncludeParent}
                  disabled={branchMutation.isPending || !!branchPlan?.operation_id}
                  onChange={(event) => { setBranchIncludeParent(event.target.checked); setBranchPlan(null); }} />
                Include parent SVN advancement in this reviewed result
              </label>
              <p className="text-xs text-gray-400">The Git branch must already exist. A missing SVN target is copied at the proven baseline; an existing related target keeps its independent work.</p>

              {/* Import mode */}
              <div>
                <p id="pairing-mode-label" className="block text-sm text-gray-400 mb-2">Pairing mode</p>
                <div role="radiogroup" aria-labelledby="pairing-mode-label" className="space-y-2">
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="importMode"
                      disabled={branchMutation.isPending || !!branchPlan?.operation_id}
                      checked={!branchForm.skip_import}
                      onChange={() => { setBranchPlan(null); setBranchForm(prev => ({ ...prev, skip_import: false })); }}
                      className="text-blue-600"
                    />
                    <span className="text-sm text-gray-300">Reconcile from the verified SVN baseline</span>
                  </label>
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="radio"
                      name="importMode"
                      disabled={branchMutation.isPending || !!branchPlan?.operation_id}
                      checked={branchForm.skip_import}
                      onChange={() => { setBranchPlan(null); setBranchForm(prev => ({ ...prev, skip_import: true })); }}
                      className="text-blue-600"
                    />
                    <span className="text-sm text-gray-300">Start from now <span className="text-amber-400">(unsafe — refused)</span></span>
                  </label>
                </div>
              </div>
            </div>
            <div className="flex flex-wrap items-center justify-end gap-3 p-6 border-t border-gray-700">
              <button
                onClick={() => { setShowBranchModal(false); setBranchForm({ svn_branch: '', git_branch: '', skip_import: false, auto_create_svn_branch: true, auto_create_git_branch: true }); setBranchPlan(null); branchMutation.reset(); }}
                className="rs-btn rs-btn--quiet px-4 py-2 rounded-lg border border-gray-600 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors"
               data-dialog-cancel disabled={branchMutation.isPending}>
                Cancel
              </button>
              <button
                onClick={() => branchMutation.mutate({ ...branchForm, action: 'preview' })}
                disabled={branchMutation.isPending || !!branchPlan?.operation_id || gitBranchValidation.previewRefused || svnBranchValidation.previewRefused}
                className="rs-btn rs-btn--state inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-purple-600 hover:bg-purple-700 disabled:opacity-50 rs-text-on-state text-sm font-medium transition-colors"
              >
                <GitBranch className="w-4 h-4" />
                {branchMutation.isPending ? 'Working...' : 'Preview Pair'}
              </button>
              {!branchPlan?.operation_id && <button
                onClick={() => branchMutation.mutate({ ...branchForm, action: 'publish', review_pin: branchPlan?.review_pin })}
                disabled={branchMutation.isPending || !branchPlanMatches || !branchPlan?.review_pin || !branchPlan.admitted || branchPlan.conflicts.length > 0 || branchForm.skip_import}
                className="px-4 py-2 rounded bg-green-700 disabled:opacity-50 rs-text-on-state text-sm">Publish reviewed result</button>}
              <button disabled={branchMutation.isPending || gitBranchValidation.previewRefused || svnBranchValidation.previewRefused}
                onClick={() => branchMutation.mutate({ ...branchForm, action: 'status', operation_id: branchPlan?.operation_id ?? undefined })}
                className="rs-btn rs-btn--quiet px-3 py-2 rounded border border-gray-600 text-sm">Inspect operation</button>
              {branchPlan?.operation_id && !branchPlan.published && branchPlan.pair_state !== 'cancelled' && <>
                <button disabled={branchMutation.isPending}
                  onClick={() => branchMutation.mutate({ ...branchForm, action: 'resume', operation_id: branchPlan.operation_id!, review_pin: branchPlan.review_pin })}
                  className="rs-btn rs-btn--quiet px-3 py-2 rounded border border-gray-600 text-sm">Reconcile / resume</button>
                <button disabled={branchMutation.isPending}
                  onClick={() => { setStopOperationError(null); setConfirmStopOperation(true); }}
                  className="rs-btn rs-btn--quiet px-3 py-2 rounded border border-gray-600 text-sm">Stop operation…</button>
                <StopOperationConfirm open={confirmStopOperation} pending={branchMutation.isPending}
                  onCancel={() => setConfirmStopOperation(false)}
                  error={stopOperationError}
                  onConfirm={() => branchMutation.mutate({ ...branchForm, action: 'cancel', operation_id: branchPlan.operation_id! }, {
                    onSuccess: () => setConfirmStopOperation(false),
                    onError: error => setStopOperationError(error instanceof Error ? error.message : String(error)),
                  })} />
                <p className="text-xs text-amber-200">Cancellation after intent retains the hold; it does not undo remote work.</p>
              </>}
            </div>
          </div>
        </Dialog>
      )}

      {/* Server Monitor */}
      <ServerMonitor {...region('server', 'overview')} />

      {/* Danger Zone - admin only; archives are terminal read-only records. */}
      {isAdmin && !archived && (
        <div className="border-t border-gray-700 pt-6" {...region('stop-remove', 'settings')}>
          <h3 className="rs-heading text-sm font-medium text-red-400 mb-4">{t('repo.section.stopOrRemove')}</h3>
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <div>
                <p className="text-sm text-gray-300 font-medium">Reset & Reimport</p>
                <p className="text-sm text-gray-300 mt-0.5">Unavailable during safe import cancellation. Request a reviewed recovery plan for an existing repository.</p>
              </div>
            </div>
            {!repo?.parent_id && (
              <div className="flex items-center justify-between">
                <div>
                  <p className="text-sm text-gray-300 font-medium">Pause / disable sync (explicit API)</p>
                  <p className="text-sm text-gray-300 mt-0.5">
                    Calls POST /disable (same contract as legacy DELETE). Stops scheduling; registration,
                    mappings, secrets, local files, and remotes stay. The header “Turn sync off” opens
                    this same dialog; “Turn sync on” only updates enabled on the registration.
                  </p>
                </div>
                <button
                  data-testid="pause-disable-repo"
                  onClick={openDisableConfirm}
                  className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-amber-700 text-amber-300 hover:bg-amber-900/30 text-sm font-medium transition-colors"
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
                <p className="text-sm text-gray-300 mt-0.5">
                  {repo?.parent_id
                    ? 'Remove this pair from RepoSync. Remote Git/SVN deletion is opt-in in the dialog.'
                    : 'Stop work, clean only owned local data, and drop the registration from active views. Remotes and history stay unless you separately authorize branch deletion.'}
                </p>
              </div>
              {repo?.parent_id ? (
                <button
                  data-testid="delete-viewed-branch-pair"
                  onClick={() => {
                    if (!repo) return;
                    setBranchPairDeleteTarget(repo);
                    setBranchPairDeleteError(null);
                    setRemoveConfirmText('');
                    setRemoveBranchOpts({ delete_git: false, delete_svn: false });
                    removeMutation.reset();
                  }}
                  className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-red-700 text-red-400 hover:bg-red-900/30 text-sm font-medium transition-colors"
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
                  className="rs-btn rs-btn--quiet inline-flex items-center gap-2 px-4 py-2 rounded-lg border border-red-700 text-red-400 hover:bg-red-900/30 text-sm font-medium transition-colors"
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
        <Dialog title="Pause / disable sync" description="Disable syncing without removing data or changing remotes." onClose={() => setShowDisableConfirm(false)} busy={disableMutation.isPending}>
          <div className="rs-dlg rs-sec bg-gray-800 border border-gray-700 rounded-lg p-6 max-w-md w-full shadow-xl">

            <p className="text-sm text-gray-400 mb-6">
              Disable <span className="font-semibold text-gray-200">{repo.name}</span>? This does not remove
              data or touch remotes. You can re-enable later.
            </p>
            {disableMutation.isError && (
              <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm mb-4" role="alert">
                Failed to disable: {disableMutation.error?.message}
              </div>
            )}
            <div className="flex items-center justify-end gap-3">
              <button
                onClick={() => setShowDisableConfirm(false)}
                className="rs-btn rs-btn--quiet px-4 py-2 rounded-lg border border-gray-600 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors"
               data-dialog-cancel disabled={disableMutation.isPending}>
                Cancel
              </button>
              <button
                data-testid="confirm-pause-disable"
                onClick={() => disableMutation.mutate()}
                disabled={disableMutation.isPending}
                className="rs-btn rs-btn--state px-4 py-2 rounded-lg bg-amber-600 hover:bg-amber-700 disabled:opacity-50 rs-text-on-state text-sm font-medium transition-colors"
              >
                {disableMutation.isPending ? 'Disabling…' : 'Pause / disable'}
              </button>
            </div>
          </div>
        </Dialog>
      )}

      {isAdmin && repo.parent_id && removalPanelStatus && (
        <ManagedRemovalPanel status={removalPanelStatus} onRetry={removalPanelStatus.retryable ? () => removeMutation.mutate() : undefined} retryPending={removeMutation.isPending} />
      )}

      {branchPairDeleteSubject && (
        <Dialog title={pendingRemovalReview.review && !pendingRemovalReview.review.archive_pair ? 'Recover original removal request' : removePairWording(displayedBranchChoices).title} description={displayedBranchChoices.archive_pair ? 'Review the exact branch and deletion side. Deleting one side archives the pair permanently and preserves the other side.' : 'Review the exact removal request. The registration and owned local data will be removed; selected remote deletions remain explicit.'} onClose={() => {
                  setBranchPairDeleteTarget(null);
                  setBranchPairDeleteError(null);
                  setRemoveConfirmText('');
                  removeMutation.reset();
                }} busy={removeMutation.isPending} testId="delete-branch-modal">
          <div className="rs-dlg bg-gray-800 border border-gray-700 rounded-lg shadow-xl w-full max-w-md">
            <div className="p-6 space-y-4">
              {reviewedRemovalPanel}
              {removeMutation.isError && (
                <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm" data-testid="delete-branch-error" role="alert">
                  {removeMutation.error?.message}
                </div>
              )}
              <p className="text-sm text-gray-300">
                {displayedBranchChoices.archive_pair ? 'Archive branch pair ' : 'Remove branch pair '}<span className="font-semibold text-gray-100">{branchPairDeleteSubject.name}</span>{' '}
                {displayedBranchChoices.archive_pair ? 'after deleting the selected side. The other branch, pair record, local data and history are retained. Sync stops permanently; reattachment is not supported.' : 'from RepoSync. Remote ref/path deletion is opt-in below.'}
              </p>
              <div className="text-xs text-gray-400 border border-gray-600 rounded p-2 space-y-1" data-testid="branch-remote-deletion-preview">
                <p>Git ref: <span className="font-mono text-gray-200">{branchPairDeleteSubject.git_branch}</span> — {displayedBranchChoices.delete_git ? 'will be deleted on remote when authorized' : 'left on remote'}</p>
                <p>SVN path: <span className="font-mono text-gray-200">{branchPairDeleteSubject.svn_branch}</span> — {displayedBranchChoices.delete_svn ? 'will be deleted on remote when authorized (history retained)' : 'left on remote'}</p>
              </div>
              {!pendingRemovalReview.review && (branchPairPreviewQuery.isLoading || reviewedRemovalQuery.isFetching) && (
                <p className="text-sm text-gray-400" data-testid="branch-removal-preview-loading">
                  Loading dependency preview…
                </p>
              )}
              {branchPairDependencyPreview && (
                <div
                  className="text-sm text-gray-300 space-y-2 border border-gray-600 rounded-lg p-3 bg-gray-900/40"
                  data-testid="branch-managed-removal-dependency-preview"
                >
                  {branchPairDependencyPreview.credentials.length > 0 && (
                    <div>
                      <p className="text-gray-400 font-medium">Credentials</p>
                      <ul className="list-disc list-inside text-gray-300 text-xs font-mono">
                        {branchPairDependencyPreview.credentials.map((cred) => (
                          <li key={cred.key}>
                            {cred.key} — {cred.action === 'retain_for_archive' ? 'retained with archived pair' : cred.action}
                            {cred.action === 'never_deleted_by_managed_remove'
                              ? (cred.retained_for_repo_ids.length > 0
                                ? ` (global credential — still used by registrations: ${cred.retained_for_repo_ids.join(', ')})`
                                : ' (global credential — not removed by managed removal)')
                              : ''}
                            {cred.action !== 'never_deleted_by_managed_remove'
                              && cred.retained_for_repo_ids.length > 0
                              ? ` (other registrations keep their own keys: ${cred.retained_for_repo_ids.join(', ')})`
                              : ''}
                            {cred.action !== 'retain_for_archive' && cred.inheriting_repo_ids.length > 0
                              ? ` (child registrations inherit this key and would lose access: ${cred.inheriting_repo_ids.join(', ')})`
                              : ''}
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                  <p className="text-gray-400 text-xs">
                    {displayedBranchChoices.archive_pair ? 'Local path retained:' : 'Local path removed:'}{' '}
                    <span className="font-mono text-gray-200">
                      {branchPairDependencyPreview.managed_local_path}
                    </span>
                  </p>
                </div>
              )}
              {branchPairPreviewQuery.isError && (
                <div className="text-sm text-red-300 space-y-2" role="alert">
                  <p data-testid="branch-removal-preview-error">
                    Could not load dependency preview: {branchPairPreviewQuery.error?.message}
                  </p>
                  <p
                    className="text-amber-200 text-xs"
                    data-testid="branch-removal-preview-unavailable"
                  >
                    Dependency preview is unavailable. Removal is blocked until current dependency and exact-target evidence are available.
                  </p>
                  <button
                    type="button"
                    data-testid="branch-removal-preview-retry"
                    onClick={() => branchPairPreviewQuery.refetch()}
                    className="rs-btn rs-btn--quiet px-3 py-1.5 rounded-md border border-red-700 text-red-200 text-xs"
                  >
                    Retry preview
                  </button>
                </div>
              )}
              <div className="space-y-2">
                <label className="flex items-center gap-2 cursor-pointer">
                  <input type="checkbox" checked={displayedBranchChoices.delete_git}
                    disabled={removeMutation.isPending || !!pendingRemovalReview.review}
                    data-testid="delete-git-opt"
                    onChange={(e) => { setRemoveConfirmText(''); setRemoveBranchOpts((p) => ({ ...p, delete_git: e.target.checked })); }}
                    className="rounded border-gray-600 bg-gray-700 text-red-600" />
                  <span className="text-sm text-gray-300">Delete Git branch <span className="text-gray-300 font-mono">({branchPairDeleteSubject.git_branch})</span></span>
                </label>
                <label className="flex items-center gap-2 cursor-pointer">
                  <input type="checkbox" checked={displayedBranchChoices.delete_svn}
                    disabled={removeMutation.isPending || !!pendingRemovalReview.review}
                    data-testid="delete-svn-opt"
                    onChange={(e) => { setRemoveConfirmText(''); setRemoveBranchOpts((p) => ({ ...p, delete_svn: e.target.checked })); }}
                    className="rounded border-gray-600 bg-gray-700 text-red-600" />
                  <span className="text-sm text-gray-300">Delete SVN branch <span className="text-gray-300 font-mono">({branchPairDeleteSubject.svn_branch})</span></span>
                </label>
              </div>
              <div>
                <label htmlFor="branch-removal-name" className="block text-sm text-gray-400 mb-2">
                  Type <span className="font-mono text-yellow-300">{branchPairConfirmation}</span> to confirm:
                </label>
                <input
                  type="text"
                  id="branch-removal-name" data-testid="delete-branch-confirm-input"
                  className="rs-in w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-red-500"
                  value={removeConfirmText}
                  onChange={(e) => setRemoveConfirmText(e.target.value)}
                  placeholder={branchPairConfirmation}
                />
              </div>
            </div>
            <div className="flex items-center justify-end gap-3 p-6 border-t border-gray-700">
              <button
                data-testid="cancel-delete-branch-pair"
                onClick={() => {
                  setBranchPairDeleteTarget(null);
                  setBranchPairDeleteError(null);
                  setRemoveConfirmText('');
                  removeMutation.reset();
                }}
                className="rs-btn rs-btn--quiet px-4 py-2 rounded-lg border border-gray-600 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors"
               data-dialog-cancel disabled={removeMutation.isPending}>
                Cancel
              </button>
              <button
                data-testid="confirm-delete-branch-pair"
                onClick={() => removeMutation.mutate()}
                disabled={
                  removeMutation.isPending
                  || removeConfirmText !== branchPairConfirmation
                  || !branchPairPreviewReady
                  || !reviewedRemovalConfirmReady
                  || !!pendingRemovalReview.review || !!pendingRemovalReview.error
                  || branchPairDependencyPreview?.parent_removal_blocked
                }
                className="rs-btn rs-btn--danger inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-red-600 hover:bg-red-700 disabled:opacity-50 rs-text-on-danger text-sm font-medium transition-colors"
              >
                <Trash2 className="w-4 h-4" />
                {pendingRemovalReview.review ? 'Recover original request above' : removeMutation.isPending ? removePairWording(removeBranchOpts).pending : removePairWording(removeBranchOpts).confirm}
              </button>
            </div>
          </div>
        </Dialog>
      )}

      {showRemoveConfirm && removalSubject && !branchPairDeleteSubject && (
        <Dialog title={removalConfirmLabel} description="Remove the managed registration and owned local data. Remote history is preserved." onClose={() => {
                  setShowRemoveConfirm(false);
                  setRemovalTarget(null);
                  setRemoveConfirmText('');
                }} busy={removeMutation.isPending} testId="managed-remove-modal">
          <div className="rs-dlg rs-sec bg-gray-800 border border-gray-700 rounded-lg p-6 max-w-md w-full shadow-xl">
            {reviewedRemovalPanel}

            <p className="text-sm text-gray-400 mb-4">
              Remove <span className="font-semibold text-gray-200">{removalSubject.name}</span> from active listings and
              clean only RepoSync-owned local data. Remote Git and SVN history are not modified by this action.
              Completed removals can be restored while recovery metadata remains.
            </p>
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
                          {cred.key} — {cred.action === 'retain_for_archive' ? 'retained with archived pair' : cred.action}
                          {cred.action === 'never_deleted_by_managed_remove'
                            ? (cred.retained_for_repo_ids.length > 0
                              ? ` (global credential — still used by registrations: ${cred.retained_for_repo_ids.join(', ')})`
                              : ' (global credential — not removed by managed removal)')
                            : ''}
                          {cred.action !== 'never_deleted_by_managed_remove'
                            && cred.retained_for_repo_ids.length > 0
                            ? ` (other registrations keep their own keys: ${cred.retained_for_repo_ids.join(', ')})`
                            : ''}
                          {cred.inheriting_repo_ids.length > 0
                            ? ` (child registrations inherit this key and would lose access: ${cred.inheriting_repo_ids.join(', ')})`
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
              <div className="text-sm text-red-300 mb-4 space-y-2" role="alert">
                <p data-testid="removal-preview-error">
                  Could not load dependency preview: {removalPreviewQuery.error?.message}
                </p>
                <p className="text-amber-200 text-xs" data-testid="removal-preview-unavailable">
                  Dependency preview is unavailable. Removal is blocked until current shared-credential, child-dependency and exact-target evidence are available.
                </p>
                <button
                  type="button"
                  data-testid="removal-preview-retry"
                  onClick={() => removalPreviewQuery.refetch()}
                  className="rs-btn rs-btn--quiet px-3 py-1.5 rounded-md border border-red-700 text-red-200 text-xs"
                >
                  Retry preview
                </button>
              </div>
            )}
            {removeMutation.isError && (
              <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm mb-4" role="alert">
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
                className="rs-btn rs-btn--quiet px-4 py-2 rounded-lg border border-gray-600 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors"
               data-dialog-cancel disabled={removeMutation.isPending}>
                Cancel
              </button>
              <button
                data-testid="confirm-remove-from-reposync"
                onClick={() => removeMutation.mutate()}
                disabled={
                  removeMutation.isPending
                  || !removalPreviewReady
                  || !reviewedRemovalConfirmReady
                  || !!pendingRemovalReview.review || !!pendingRemovalReview.error
                  || removalDependencyPreview?.parent_removal_blocked
                }
                className="rs-btn rs-btn--danger px-4 py-2 rounded-lg bg-red-600 hover:bg-red-700 disabled:opacity-50 rs-text-on-danger text-sm font-medium transition-colors"
              >
                {removeMutation.isPending ? 'Removing…' : removalConfirmLabel}
              </button>
            </div>
          </div>
        </Dialog>
      )}

    </div>
  );
}

/* ---- #264 derived regions and section navigation ---- */

type StatusView = ReturnType<typeof useStatusObservation>;
type Tone = 'ok' | 'run' | 'warn' | 'danger' | 'muted';
const toneClasses: Record<Tone, string> = {
  ok: 'bg-green-900/50 text-green-300',
  run: 'bg-blue-900/50 text-blue-300',
  warn: 'bg-yellow-900/50 text-yellow-300',
  danger: 'bg-red-900/50 text-red-300',
  muted: 'bg-gray-700 text-gray-300',
};
const markClass = 'rs-mark inline-flex items-center px-2.5 py-0.5 rounded text-xs font-medium';

/** What the head button shows for a few seconds after a Sync Now reply. */
function syncReplyLabel(reply: { queued?: boolean; retry_after_secs?: number | null }): string {
  if (reply.queued !== false) return 'Sync Triggered';
  return reply.retry_after_secs ? `Sync just started, wait ${reply.retry_after_secs}s` : 'Not queued';
}

function observedClock(iso: string): string {
  return `${new Date(iso).toLocaleTimeString()} · ${formatTimeAgo(iso)}`;
}

/** G04.32: read-only, derived from the status read the page already makes. */
function repositoryStatusLine(view: StatusView) {
  const status = view.status;
  const observation = view.observation;
  const health = view.health;
  const tone: Tone = health === 'idle' ? 'ok'
    : health === 'running' || health === 'initializing' || health === 'busy' ? 'run'
      : health === 'paused' || health === 'held' || health === 'conflict' ? 'warn'
        : health === 'error' ? 'danger' : 'muted';
  const lastKnown = view.stale || view.failed ? ' (last known)' : '';
  const observed = observation
    ? view.stale || view.failed
      ? `${view.availability} · last observed ${observedClock(observation.observed_at)}`
      : `Observed ${observedClock(observation.observed_at)}`
    : status ? `${view.availability} · observation time not reported` : view.availability;
  const lastSyncAt = observation ? observation.last_successful_sync_at : status?.last_sync_at ?? null;
  const lastSync = lastSyncAt
    ? `${observedClock(lastSyncAt)}${lastKnown}`
    : !status ? 'Unknown'
      : observation && observation.data_freshness !== 'never_recorded' ? 'Unknown' : 'Not recorded';
  const recorded = status
    ? [status.last_svn_revision !== null ? `r${status.last_svn_revision}` : null, status.last_git_hash ? status.last_git_hash.slice(0, 12) : null]
      .filter((part): part is string => !!part)
    : [];
  const position = !status ? 'Unknown' : recorded.length ? `${recorded.join(' · ')}${lastKnown}` : 'Not recorded';
  return { word: view.healthLabel, tone, observed, lastSync, position };
}

/** G04.32: state, observed time, last sync, position and a link to the evidence. */
export function RepositoryStatusLine({ view }: { view: StatusView }) {
  const location = useLocation();
  const line = repositoryStatusLine(view);
  return (
    <div
      className="rs-sec bg-gray-800/60 border border-gray-700 rounded-lg px-4 py-3 flex flex-wrap items-center gap-x-6 gap-y-2 text-sm"
      {...region('status-line', EVERY_SECTION)}
      data-rs-derived=""
    >
      <div className="flex flex-wrap items-center gap-2">
        <span role="status" aria-atomic="true" className={`${markClass} ${toneClasses[line.tone]}`}>{line.word}</span>
        <span className="text-xs text-gray-400">{line.observed}</span>
      </div>
      <dl className="flex flex-wrap gap-x-6 gap-y-1">
        <div className="flex gap-2">
          <dt className="text-gray-400">Last successful sync</dt>
          <dd className="text-gray-200">{line.lastSync}</dd>
        </div>
        <div className="flex gap-2">
          <dt className="text-gray-400">Position recorded</dt>
          <dd className="text-gray-200 font-mono">{line.position}</dd>
        </div>
      </dl>
      <Link
        to={{ pathname: location.pathname, search: sectionSearch(location.search, 'overview') }}
        state={location.state}
        className="ml-auto text-blue-300 underline"
      >
        What this status proves
      </Link>
    </div>
  );
}

/** G04.31: one link per section; a navigation that keeps the route's state. */
export function RepositorySectionNav({ current, navRef }: { current: RepoSection; navRef?: React.Ref<HTMLElement> }) {
  const location = useLocation();
  return (
    <nav ref={navRef} aria-label="Repository sections" className="rs-page-sections" data-rs-derived="">
      {REPO_SECTIONS.map(item => (
        <Link
          key={item.id}
          to={{ pathname: location.pathname, search: sectionSearch(location.search, item.id) }}
          state={location.state}
          aria-current={item.id === current ? 'page' : undefined}
        >
          {item.label}
        </Link>
      ))}
    </nav>
  );
}

type CredentialField = 'svn' | 'git';
interface CredentialRead {
  data?: { svn_password_set?: unknown; git_token_set?: unknown };
  isError: boolean;
  error?: unknown;
}

/** G04.4 / S23: a word and a mark from the existing credential read; never a placeholder. */
function credentialState(flag: unknown, read: CredentialRead): { word: string; tone: Tone; note: string | null; set: boolean } {
  const failure = read.isError ? (read.error instanceof Error && read.error.message) || 'The credential read failed.' : null;
  if (!read.data) {
    return failure
      ? { word: 'Could not be read', tone: 'danger', note: failure, set: false }
      : { word: 'Loading…', tone: 'muted', note: null, set: false };
  }
  if (typeof flag !== 'boolean') {
    return { word: 'Not reported', tone: 'muted', note: failure ? `The latest read failed: ${failure}` : null, set: false };
  }
  return {
    word: flag ? 'Set' : 'Not set',
    tone: flag ? 'ok' : 'muted',
    note: failure ? `Last known. The latest read failed: ${failure}` : null,
    set: flag,
  };
}

/** Settings: Credentials. Read-only; “Replace…” opens the field in its own region and sends nothing. */
export function CredentialsSummary({ read, onReplace }: { read: CredentialRead; onReplace: (field: CredentialField) => void }) {
  const id = useId();
  const rows = [
    { field: 'svn' as const, label: 'SVN password', state: credentialState(read.data?.svn_password_set, read) },
    { field: 'git' as const, label: 'Git token', state: credentialState(read.data?.git_token_set, read) },
  ];
  return (
    <section
      aria-labelledby={`${id}-heading`}
      className="rs-sec bg-gray-800/60 border border-gray-700 rounded-lg p-5"
      {...region('credentials', 'settings')}
      data-rs-derived=""
    >
      <h2 id={`${id}-heading`} className="rs-heading text-lg font-semibold text-gray-100">Credentials</h2>
      <p className="text-sm text-gray-400 mt-1">Used by RepoSync to read and write this repository. A saved value is never shown.</p>
      <ul className="mt-3 divide-y divide-gray-700">
        {rows.map(row => (
          <li key={row.field} className="py-2 flex flex-wrap items-center gap-x-4 gap-y-1">
            <span id={`${id}-${row.field}`} className="text-sm text-gray-200 min-w-[8rem]">{row.label}</span>
            <span className={`${markClass} ${toneClasses[row.state.tone]}`}>{row.state.word}</span>
            <span className="text-xs text-gray-400">Origin not reported</span>
            <button
              type="button"
              onClick={() => onReplace(row.field)}
              aria-describedby={`${id}-${row.field}`}
              className="rs-btn rs-btn--quiet ml-auto inline-flex items-center px-3 py-1.5 rounded-md text-xs font-medium border border-gray-600 text-gray-300 rs-hover-text-strong hover:border-gray-500 transition-colors"
            >
              {row.state.set ? 'Replace…' : 'Set…'}
            </button>
            {row.state.note && <p className="w-full text-xs text-red-300">{row.state.note}</p>}
          </li>
        ))}
      </ul>
    </section>
  );
}

/* ---- #264 shared changes ---- */

/** G04.23: a repository pauses through the explicit call and its dialog; pairs and “on” stay one click. */
function headerSyncSwitch(repo: Pick<Repository, 'parent_id' | 'enabled'>): 'explicit-pause' | 'turn-off' | 'turn-on' {
  if (!repo.enabled) return 'turn-on';
  return repo.parent_id ? 'turn-off' : 'explicit-pause';
}

type HookFetcher = (url: string) => Promise<Response>;

/** G04.7: the existing hook request, with the session (a plain link cannot send it). */
async function fetchPreCommitHook(repoId: string, fetcher: HookFetcher = url => sessionFetch(url)): Promise<string> {
  const response = await fetcher(`/api/repos/${repoId}/hooks/pre-commit`);
  const body = await response.text();
  if (!response.ok) throw new Error(`API error ${response.status}: ${body}`);
  return body;
}

/** How long the file's object URL stays valid after the click, so the browser can finish the download. */
const DOWNLOAD_URL_LIFETIME_MS = 60_000;

/** Hands the fetched text to the browser as a file; nothing else is sent. */
function saveTextDownload(filename: string, text: string, doc: Document = document): void {
  const url = URL.createObjectURL(new Blob([text], { type: 'text/x-shellscript' }));
  const link = doc.createElement('a');
  link.href = url;
  link.download = filename;
  link.rel = 'noopener';
  link.style.display = 'none';
  doc.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), DOWNLOAD_URL_LIFETIME_MS);
}

function commitCount(count: number): string {
  return `${count} commit${count === 1 ? '' : 's'}`;
}

/** G04.10: what choosing a prefix does; the request is unchanged. */
function skipChoiceHelp(selected: number, total: number): string {
  const remaining = Math.max(0, total - selected);
  return `Skips ${commitCount(selected)}. ${remaining === 0 ? 'Nothing stays pending.' : `${commitCount(remaining)} ${remaining === 1 ? 'stays' : 'stay'} pending.`}`;
}

/** G04.10: the oldest contiguous pending prefix, chosen as radios (“Skip through”). */
export function SkipPrefixChoices({ name, commits, selectedCount, onSelect, disabled = false }: {
  name: string;
  commits: SkipCommitPending[];
  selectedCount: number;
  onSelect: (count: number) => void;
  disabled?: boolean;
}) {
  return (
    <div className="mb-4">
      <p id={`${name}-label`} className="text-sm text-gray-300 mb-2">Skip through</p>
      <div role="radiogroup" aria-labelledby={`${name}-label`} className="space-y-2 max-h-48 overflow-y-auto">
        {commits.map((commit, index) => (
          <label key={commit.sha} className="flex items-start gap-2 text-sm text-gray-200">
            <input
              type="radio"
              name={name}
              value={commit.sha}
              checked={index === selectedCount - 1}
              disabled={disabled}
              onChange={() => onSelect(index + 1)}
              aria-describedby={`${name}-help-${index}`}
              data-testid={`skip-commit-select-${index}`}
            />
            <span>
              <span className="font-mono text-xs text-gray-400">{commit.sha.slice(0, 8)}</span>
              {' '}{commit.subject}
              <span id={`${name}-help-${index}`} className="block text-xs text-gray-400">{skipChoiceHelp(index + 1, commits.length)}</span>
            </span>
          </label>
        ))}
      </div>
    </div>
  );
}

/** G04.9: what resuming does and does not do. */
function resumeSyncConsequences(pendingCount: number | null): string[] {
  const pending = pendingCount === null ? 'pending commits' : `the ${commitCount(pendingCount).replace('commit', 'pending commit')}`;
  return [
    'Clears the error pause so scheduled sync can run again.',
    `It does not skip or change ${pending}. If the same errors keep recurring, sync pauses again.`,
    'It does not prove that the two sides match, and it does not release a hold.',
  ];
}

/**
 * G04.9: “Resume sync…” asks once, then sends the existing retry call; pending and failure are
 * shown. The dialog's description names the repository and says what resuming does and does not do.
 */
export function ResumeSyncDialog({ repoName, pendingCount, allowed, pending, error, onConfirm, onClose }: {
  repoName: string;
  pendingCount: number | null;
  allowed: boolean;
  pending: boolean;
  error: string | null;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Dialog
      title="Resume sync?"
      description={
        <>
          <p>{repoName}</p>
          <ul className="list-disc pl-5 space-y-1 mt-2">
            {resumeSyncConsequences(pendingCount).map(line => <li key={line}>{line}</li>)}
          </ul>
        </>
      }
      onClose={onClose}
      busy={pending}
      testId="resume-sync-modal"
    >
      <div className="rs-dlg rs-sec bg-gray-800 border border-gray-700 rounded-lg p-6 max-w-md w-full shadow-xl">
        {!allowed && (
          <p role="status" className="text-sm text-yellow-300 mb-4">
            Current status no longer reports this pause. No resume request will be sent.
          </p>
        )}
        {error && (
          <div className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-3 text-red-300 text-sm mb-4" role="alert">
            Sync could not be resumed: {error}
          </div>
        )}
        <div className="flex items-center justify-end gap-3">
          <button
            type="button"
            onClick={onClose}
            className="rs-btn rs-btn--quiet px-4 py-2 rounded-lg border border-gray-600 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors"
            data-dialog-cancel
            disabled={pending}
          >
            Cancel
          </button>
          <button
            type="button"
            data-testid="confirm-resume-sync"
            onClick={onConfirm}
            disabled={pending || !allowed}
            className="rs-btn rs-btn--primary px-4 py-2 rounded-lg bg-blue-600 hover:bg-blue-700 disabled:opacity-50 rs-text-on-accent text-sm font-medium transition-colors"
          >
            {pending ? 'Resuming…' : 'Resume sync'}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

/** One-side deletion explicitly archives; zero/both retain the existing removal contract. */
function removePairWording(options: { delete_git: boolean; delete_svn: boolean }) {
  const deleted = options.delete_git && options.delete_svn ? 'Git and SVN branches'
    : options.delete_git ? 'Git branch' : options.delete_svn ? 'SVN branch' : null;
  if (options.delete_git !== options.delete_svn) return { title: `Archive pair and delete ${deleted}`, confirm: 'Archive pair', pending: 'Archiving…' };
  if (!deleted) return { title: 'Remove branch pair', confirm: 'Remove pair', pending: 'Removing…' };
  return { title: `Remove branch pair and delete its ${deleted}`, confirm: `Remove pair and delete ${deleted}`, pending: 'Deleting...' };
}

/** S20: plain words for a late-pairing operation state; the raw value is kept beside it. */
function latePairStateWord(state: string): string {
  const words: Record<string, string> = {
    preview: 'preview only',
    preparing: 'preparing',
    queued: 'queued',
    svn_copy_pending: 'SVN copy pending',
    svn_copied: 'SVN branch copied',
    child_registered: 'pair registered',
    replay_in_progress: 'replaying commits',
    active: 'active',
    completed: 'completed',
    failed: 'failed',
    reconciliation_required: 'needs reconciliation',
    reconciliation_held: 'held for reconciliation',
    cancelled: 'stopped',
  };
  return words[state] ?? 'state not recognised';
}

/** G04.15: a failed read is shown as failed, never as an empty list. */
export function SyncRecordsBody({ read }: { read: { data?: { entries?: SyncRecord[] }; isError: boolean; error?: unknown } }) {
  const failure = read.isError ? (read.error instanceof Error && read.error.message) || 'The sync records read failed.' : null;
  if (!read.data) {
    return failure
      ? <p role="alert" className="text-red-300 text-sm px-6 pb-6" data-testid="sync-records-failed">Sync records could not be loaded: {failure}</p>
      : <p className="text-gray-400 text-sm px-6 pb-6">Loading sync records…</p>;
  }
  const records = read.data.entries ?? [];
  return (
    <>
      {failure && (
        <p role="alert" className="text-red-300 text-sm px-6 pb-3" data-testid="sync-records-stale">
          Sync records could not be refreshed: {failure}. The list below is the last one loaded.
        </p>
      )}
      {records.length > 0 ? (
        <div className="divide-y divide-gray-700" data-rs-list="">
          {records.map((record) => (
            <SyncRecordRow key={record.id} record={record} />
          ))}
        </div>
      ) : (
        <p className="text-gray-400 text-sm px-6 pb-6">No sync records yet</p>
      )}
    </>
  );
}

// Pure helpers, exported for this page's tests only.
// eslint-disable-next-line react-refresh/only-export-components
export { syncReplyLabel, normaliseRepoSection, sectionSearch, sectionNavDisplayed, repositoryStatusLine, credentialState, headerSyncSwitch, fetchPreCommitHook, saveTextDownload, skipChoiceHelp, resumeSyncConsequences, removePairWording, latePairStateWord, REPO_SECTIONS };

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
  const fieldId = useId();
  return (
    <div>
      <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-7`}>{label}</label>
      <input
        type="text"
        className={inputClass}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder} id={`${fieldId}-7`}
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
  const fieldId = useId();
  return (
    <div>
      <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-8`}>{label}</label>
      <input
        type="number"
        className={inputClass}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        min={min} id={`${fieldId}-8`}
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
  const fieldId = useId();
  return (
    <div>
      <label className="block text-sm text-gray-400 mb-1" htmlFor={`${fieldId}-9`}>{label}</label>
      <select className={selectClass} value={value} onChange={(e) => onChange(e.target.value)} id={`${fieldId}-9`}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </div>
  );
}

function FieldToggle({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }) {
  const fieldId = useId();
  return <div>
    <label htmlFor={fieldId} className="block text-sm text-gray-300 mb-1">{label}</label>
    <input id={fieldId} type="checkbox" checked={checked} onChange={event => onChange(event.target.checked)}
      className="h-5 w-5 rounded border-gray-500 accent-blue-500" />
  </div>;
}

function StatCard({ icon, label, value }: { icon: React.ReactNode; label: string; value: string }) {
  return (
    <div className="rs-sec bg-gray-800/60 border border-gray-700 rounded-lg p-4">
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
  to,
  subtitle,
  action,
}: {
  title: string;
  value: string;
  color: string;
  to?: string;
  subtitle?: string;
  action?: React.ReactNode;
}) {
  const colorClasses: Record<string, string> = {
    green: 'bg-green-900/30 border-green-700',
    red: 'bg-red-900/30 border-red-700',
    yellow: 'bg-yellow-900/30 border-yellow-700',
    blue: 'bg-blue-900/30 border-blue-700',
    gray: 'bg-gray-800 border-gray-700',
  };

  const clickableClasses = to
    ? 'cursor-pointer hover:border-blue-500/50 transition-colors'
    : '';

  return (
    <div
      className={`rounded-lg border p-4 ${colorClasses[color] ?? colorClasses.gray} ${clickableClasses}`}
    >
      <div className="flex items-center justify-between">
        {to ? <Link to={to} className="text-sm text-blue-300 underline">{title}</Link> : <p className="text-sm text-gray-400">{title}</p>}
        {action}
      </div>
      <p className="text-2xl font-bold capitalize text-gray-100">{value}</p>
      {subtitle && <p className="text-xs text-gray-300 mt-1">{subtitle}</p>}
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
        aria-expanded={expanded}
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
          <span className="text-xs text-gray-300">
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
                <span className="text-gray-300 text-xs uppercase">Record ID</span>
                <p className="font-mono text-gray-300 truncate">{record.id}</p>
              </div>
              <div>
                <span className="text-gray-300 text-xs uppercase">SVN Revision</span>
                <p className="font-mono text-blue-400">{record.svn_rev ? `r${record.svn_rev}` : 'N/A'}</p>
              </div>
              <div>
                <span className="text-gray-300 text-xs uppercase">Git SHA</span>
                <p className="font-mono text-purple-400">{record.git_sha || 'N/A'}</p>
              </div>
              <div>
                <span className="text-gray-300 text-xs uppercase">Status</span>
                <p className={statusColor + ' font-medium capitalize'}>{record.status}</p>
              </div>
            </div>
            <div className="grid grid-cols-2 md:grid-cols-3 gap-4 text-sm">
              <div>
                <span className="text-gray-300 text-xs uppercase">Author</span>
                <p className="text-gray-300">{record.author}</p>
              </div>
              <div>
                <span className="text-gray-300 text-xs uppercase">Committed</span>
                <p className="text-gray-300">{new Date(record.timestamp).toLocaleString()}</p>
              </div>
              <div>
                <span className="text-gray-300 text-xs uppercase">Synced At</span>
                <p className="text-gray-300">{new Date(record.synced_at).toLocaleString()}</p>
              </div>
            </div>
            <div className="mt-4">
              <span className="text-gray-300 text-xs uppercase">Commit Message</span>
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
      className={`rs-mark inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
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
      className={`rs-mark inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
        colors[action] ?? 'bg-gray-700 text-gray-300'
      }`}
    >
      {label}
    </span>
  );
}

/**
 * #264 (D11, interaction contract): "Stop operation…" opens one confirmation that says what
 * stopping does not undo; confirming sends the same cancel request as before.
 */
export function StopOperationConfirm({ open, pending, error = null, onConfirm, onCancel }: {
  readonly open: boolean; readonly pending: boolean; readonly error?: string | null;
  readonly onConfirm: () => void; readonly onCancel: () => void;
}) {
  if (!open) return null;
  return (
    <Dialog title="Stop this operation?" description="Request a stop for the branch pair operation."
      onClose={onCancel} busy={pending} testId="stop-operation-modal">
      <div className="space-y-3 text-sm text-gray-300">
        <p>Cancellation after intent retains the hold; it does not undo remote work.</p>
        {error && <p role="alert" className="text-red-400">{error}</p>}
        <div className="flex items-center justify-end gap-3 pt-1">
          <button type="button" onClick={onCancel} data-dialog-cancel disabled={pending}
            className="rs-btn rs-btn--quiet px-4 py-2 rounded-lg border border-gray-600 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors">
            Keep running
          </button>
          <button type="button" data-testid="confirm-stop-operation" onClick={onConfirm} disabled={pending}
            className="rs-btn rs-btn--state px-4 py-2 rounded-lg bg-yellow-600 hover:bg-yellow-700 disabled:opacity-50 rs-text-on-state text-sm font-medium transition-colors">
            {pending ? 'Requesting stop…' : 'Stop operation'}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
