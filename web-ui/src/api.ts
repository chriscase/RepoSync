import { ApiError, clientSession, sessionFetch } from './session';
export { ApiError } from './session';
import { decodeAuditPage, decodeConflictPage, type ConflictFilters } from './listPagination';
import { decodeSyncStatus, type StatusObservation } from './statusObservation';
import type { ErrorObservation, ErrorScope } from './errorAcknowledgement';
import { buildBranchPairDeleteQuery } from './branchPairDeletion';
import type { BranchPairRemovalResult } from './branchPairRemoval';
import type { ManagedRemovalStatus } from './managedRemoval';
import type { RepoImportMode, SnapshotPin, StartRepoImportRequest } from './importBaseline';

const API_BASE = '/api';

export interface RepositorySaveReceipt {
  version: number;
  request_id: string;
  kind: 'create' | 'configuration' | 'credentials';
  repo_id: string;
  configuration_updated_at: string;
  credential_fields: Array<'svn_password' | 'git_token'>;
  propagated_repositories: number;
}

export interface SyncStatus {
  observation?: StatusObservation;
  state: string;
  last_sync_at: string | null;
  last_svn_revision: number | null;
  last_git_hash: string | null;
  total_syncs: number;
  total_conflicts: number;
  active_conflicts: number;
  total_errors: number;
  last_error_at: string | null;
  error_observation: ErrorObservation;
  uptime_secs: number;
}

export interface Conflict {
  id: string;
  file_path: string;
  conflict_type: string;
  svn_content: string | null;
  git_content: string | null;
  base_content: string | null;
  svn_revision: number | null;
  git_hash: string | null;
  status: string;
  resolution: string | null;
  resolved_by: string | null;
  created_at: string;
  resolved_at: string | null;
  repo_id?: string | null;
}

export interface AuditEntry {
  id: number;
  repo_id?: string | null;
  action: string;
  direction: string | null;
  svn_rev: number | null;
  git_sha: string | null;
  author: string | null;
  details: string | null;
  created_at: string;
  success: boolean;
}

export type ConflictListItem = Pick<Conflict,'id'|'file_path'|'conflict_type'|'status'|'svn_revision'|'git_hash'|'created_at'|'resolved_at'|'repo_id'>;
export interface ConflictPage {
  items: ConflictListItem[];
  total:number;page:number;per_page:number;total_pages:number;
}

export interface AuditListResponse {
  entries: AuditEntry[];
  total:number;page:number;per_page:number;total_pages:number;offset:number;
}

export interface AuthorMapping {
  svn_username: string;
  name: string;
  email: string;
  github?: string;
}

/** Server classification of a commit-map row's Git side (#54). */
export type CommitMapTargetState = 'git_commit' | 'filtered_no_target' | 'no_target_unverified' | 'legacy_empty_value';

export interface CommitMapEntry {
  id: number;
  repo_id?: string;
  svn_rev: number;
  /** `null` for a no-target row; read `target_state` for why. */
  git_sha: string | null;
  /** Absent only from servers older than #54. */
  target_state?: CommitMapTargetState;
  direction: string;
  synced_at: string;
  svn_author: string;
  git_author: string;
}

export interface CommitMapResponse {
  entries: CommitMapEntry[];
  total: number;
}

export interface SyncRecord {
  id: string;
  repo_id?: string;
  svn_rev: number | null;
  git_sha: string | null;
  direction: string;
  author: string;
  message: string;
  timestamp: string;
  synced_at: string;
  status: string;
}

export interface SyncRecordResponse {
  entries: SyncRecord[];
  total: number;
}

export interface LatePairPlan {
  review_pin?: string;
  include_parent?: boolean;
  result_git_sha?: string | null;
  result_tree_digest?: string;
  operation_id?: string | null;
  observation?: string;
  updated_at?: string | null;
  handled_steps?: number;
  total_steps?: number;
  action_error?: string;
  mode: string;
  published: boolean;
  admitted: boolean;
  pair_state: string;
  scheduler_active: boolean;
  policy_version: string;
  parent_id: string;
  git_branch: string;
  svn_branch: string;
  git_tip: string | null;
  svn_source_revision: number | null;
  svn_target_revision: number | null;
  verified_baseline: {
    git_sha: string;
    svn_revision: number;
    evidence: string;
  } | null;
  baseline_missing_reason: string | null;
  inherited_work: string[];
  pending_git: { count: number; summary: string[] };
  pending_svn: {
    knowable: boolean;
    parent_head_revision: number | null;
    baseline_revision: number | null;
    note: string;
  };
  conflicts: string[];
  unknowns: string[];
  proposed_svn_copy_source_revision: number | null;
  existing_svn_target: {
    exists: boolean;
    equivalent: boolean;
    uuid: string | null;
    revision: number | null;
    url: string;
    note: string;
  };
  skip_import_requested: boolean;
  skip_import_applied: boolean;
  skip_import_note: string;
}

export interface PairRefreshPlan {
  mode: string;
  operation: string;
  executed: boolean;
  published: boolean;
  durable_job_started: boolean;
  policy_version: string;
  pair_id: string;
  parent_id: string;
  pair_generation: number;
  generation_source: string;
  pins_complete: boolean;
  plan_id: string;
  plan_digest: string;
  approval: { eligible: boolean; reason: string; binds_to: string };
  git: {
    pair_branch: string;
    parent_branch: string;
    pair_tip: string | null;
    parent_tip: string | null;
    pair_local_tip: string | null;
    parent_local_tip: string | null;
  };
  svn: {
    uuid: string | null;
    pair_path: string;
    parent_path: string;
    pair_revision: number | null;
    parent_revision: number | null;
  };
  pending: {
    pair_git: { knowable: boolean; rewritten: boolean; count: number; shas: string[]; note: string };
    parent_git: { knowable: boolean; rewritten: boolean; count: number; shas: string[]; note: string };
    pair_svn: { knowable: boolean; count: number; mapped_revision: number | null; head_revision: number | null };
    parent_svn: { knowable: boolean; count: number; mapped_revision: number | null; head_revision: number | null };
  };
  conflicts: string[];
  intended_result: {
    summary: string;
    discards_unsynced_work: boolean;
    preserves_published_git_commits: boolean;
    preserves_published_svn_revisions: boolean;
    merge_echo_treatment: string;
  };
  reanchor_status: string;
  execute_status: string;
}

export interface SkipCommitPending {
  sha: string;
  subject: string;
  excluded: boolean;
}

export interface SkipCommitContext {
  pinned_cursor: string;
  observed_remote_tip: string | null;
  observed_bridge_tip: string | null;
  pending_commits: SkipCommitPending[];
}

export interface SkipCommitRequest {
  pinned_cursor: string;
  selected_commits: string[];
  expected_remote_tip: string;
  expected_bridge_tip?: string;
  reason?: string;
}

export interface ConfigResponse {
  daemon: { poll_interval_secs: number; log_level: string; data_dir: string };
  svn: { url: string; username: string; password: string; trunk_path: string };
  github: { api_url: string; repo: string; token: string; default_branch: string };
  web: { listen: string; auth_mode: string };
  sync: { mode: string; auto_merge: boolean; sync_tags: boolean };
}

// Multi-user auth types
export interface User {
  id: string;
  username: string;
  display_name: string;
  email: string;
  role: string;
  enabled: boolean;
  created_at: string;
}

export interface CredentialSummary {
  id: string;
  service: string;
  server_url: string;
  username: string;
  created_at: string;
  updated_at: string;
}

export interface CreateUserRequest {
  username: string;
  display_name: string;
  email: string;
  password: string;
  role: string;
}

export interface UpdateUserRequest {
  display_name?: string;
  email?: string;
  role?: string;
  enabled?: boolean;
  password?: string;
  current_password?: string;
}

export interface StoreCredentialRequest {
  service: string;
  server_url: string;
  username: string;
  value: string;
}

export interface LdapConfig {
  enabled: boolean;
  url: string;
  base_dn: string;
  search_filter: string;
  display_name_attr: string;
  email_attr: string;
  group_attr: string;
  bind_dn: string;
  bind_password_set: boolean;
}

export interface SaveLdapConfigRequest {
  enabled: boolean;
  url: string;
  base_dn: string;
  search_filter: string;
  display_name_attr: string;
  email_attr: string;
  group_attr: string;
  bind_dn?: string;
  bind_password?: string;
}

export interface LoginResponse {
  token: string;
  user?: User;
}

async function fetchJson<T>(url: string, options?: RequestInit): Promise<T> {
  const res = await sessionFetch(`${API_BASE}${url}`, { ...options, headers: { 'Content-Type': 'application/json', ...options?.headers } });
  if (!res.ok) {
    const text = await res.text();
    throw new ApiError(res.status, `API error ${res.status}: ${text}`);
  }
  return res.json();
}

export const api = {
  getStatus: (repoId?: string) => {
    const params = repoId ? `?repo_id=${encodeURIComponent(repoId)}` : '';
    return fetchJson<unknown>(`/status${params}`).then(value=>decodeSyncStatus(value,repoId));
  },

  getHealth: () => fetchJson<{ ok: boolean }>('/status/health'),

  getSystemMetrics: () => fetchJson<SystemMetrics>('/status/system'),

  getConflicts: (status?: string, repoId?: string) => {
    const params = new URLSearchParams();
    if (status) params.append('status', status);
    if (repoId) params.append('repo_id', repoId);
    const qs = params.toString();
    return fetchJson<Conflict[]>(`/conflicts${qs ? `?${qs}` : ''}`);
  },

  getConflictsPage: (filters:ConflictFilters) => {
    const params=new URLSearchParams({page:String(filters.page),per_page:String(filters.perPage)});
    if(filters.status)params.set('status',filters.status);
    if(filters.repoId)params.set('repo_id',filters.repoId);
    return fetchJson<unknown>(`/conflicts?${params}`).then(value=>decodeConflictPage(value,filters));
  },

  getConflict: (id: string) => fetchJson<Conflict>(`/conflicts/${id}`),

  resolveConflict: (id: string, resolution: string, mergedContent?: string) =>
    fetchJson<{ ok: boolean }>(`/conflicts/${id}/resolve`, {
      method: 'POST',
      body: JSON.stringify({ resolution, merged_content: mergedContent }),
    }),

  deferConflict: (id: string) =>
    fetchJson<{ ok: boolean }>(`/conflicts/${id}/defer`, { method: 'POST' }),

  getAuditLog: (limit = 50, page?: number, success?: boolean, repoId?: string, action?: string) => {
    const params = new URLSearchParams({ limit: String(limit) });
    if (page !== undefined) params.append('page', String(page));
    if (success !== undefined) params.append('success', String(success));
    if (repoId) params.append('repo_id', repoId);
    if(action)params.append('action',action);
    return fetchJson<unknown>(`/audit?${params.toString()}`).then(value=>decodeAuditPage(value,{page:page ?? 1,perPage:limit,success,repoId,action}));
  },

  acknowledgeErrors: (scope: ErrorScope, observation: ErrorObservation) =>
    fetchJson<{ ok: boolean; acknowledged: number }>('/status/acknowledge-errors', {
      method: 'POST',
      body: JSON.stringify({ scope, observation }),
    }),

  getIdentityMappings: () => fetchJson<AuthorMapping[]>('/config/identity'),

  updateIdentityMappings: (mappings: AuthorMapping[]) =>
    fetchJson<{ ok: boolean }>('/config/identity', {
      method: 'PUT',
      body: JSON.stringify({ mappings }),
    }),

  getConfig: () => fetchJson<ConfigResponse>('/config'),

  getCommitMap: (limit = 100, repoId?: string) => {
    const params = new URLSearchParams({ limit: String(limit) });
    if (repoId) params.append('repo_id', repoId);
    return fetchJson<CommitMapResponse>(`/commit-map?${params}`);
  },

  getSyncRecords: (limit = 100, repoId?: string) => {
    const params = new URLSearchParams({ limit: String(limit) });
    if (repoId) params.append('repo_id', repoId);
    return fetchJson<SyncRecordResponse>(`/sync-records?${params}`);
  },

  seedData: () =>
    fetchJson<{ ok: boolean; message: string }>('/seed', { method: 'POST' }),

  testSvnConnection: (data: { url: string; username: string; password?: string }) =>
    fetchJson<{ ok: boolean; message: string }>('/setup/test-svn', {
      method: 'POST',
      body: JSON.stringify(data),
    }),

  testGitConnection: (data: { api_url: string; repo: string; provider: string; token?: string }) =>
    fetchJson<{ ok: boolean; message: string }>('/setup/test-git', {
      method: 'POST',
      body: JSON.stringify(data),
    }),

  applyConfig: (data: Record<string, unknown>) =>
    fetchJson<{ ok: boolean; message: string; warnings: string[] }>('/setup/apply', {
      method: 'POST',
      body: JSON.stringify(data),
    }),

  startImport: () =>
    fetchJson<{
      ok: boolean;
      message: string;
      operation_id?: string;
      lifecycle?: ImportStatus['lifecycle'];
    }>('/setup/import', {
      method: 'POST',
    }),

  getSetupConfig: () => fetchJson<WizardSetupConfig>('/setup/config'),

  getImportStatus: () =>
    fetchJson<ImportStatus>('/setup/import/status'),

  cancelImport: () =>
    fetchJson<{
      ok: boolean;
      message: string;
      operation_id?: string;
      lifecycle?: ImportStatus['lifecycle'];
    }>('/setup/import/cancel', {
      method: 'POST',
    }),

  resetAndReimport: () =>
    fetchJson<{ ok: boolean; message: string }>('/setup/reset-reimport', {
      method: 'POST',
    }),

  // Repositories (multi-repo)
  getRepos: () => fetchJson<Repository[]>('/repos'),
  createRepo: (data: Partial<Repository>, requestId?: string, signal?: AbortSignal) =>
    fetchJson<Repository>('/repos', { method: 'POST', signal, body: JSON.stringify({ ...data, save_request_id: requestId }) }),
  startRepoImport: (id: string, body: StartRepoImportRequest = {}) =>
    fetchJson<{
      ok: boolean;
      operation_id?: string;
      import_mode?: RepoImportMode;
      starting_revision?: number | null;
      history_boundary?: string | null;
      lifecycle?: ImportStatus['lifecycle'];
    }>(`/repos/${id}/import`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  getRepo: (id: string) => fetchJson<Repository>(`/repos/${id}`),
  updateRepo: (id: string, data: Partial<Repository>, options?: { requestId?: string; expectedUpdatedAt?: string; signal?: AbortSignal }) =>
    fetchJson<Repository>(`/repos/${id}`, { method: 'PUT', signal: options?.signal, body: JSON.stringify({ ...data, save_request_id: options?.requestId, expected_updated_at: options?.expectedUpdatedAt }) }),
  /** Legacy root DELETE — disable only (non-destructive). Prefer `disableRepo`. */
  deleteRepo: (id: string) =>
    fetchJson<{
      ok: boolean;
      action: string;
      message: string;
      enabled: boolean;
    }>(`/repos/${id}`, { method: 'DELETE' }),

  disableRepo: (id: string) =>
    fetchJson<{
      ok: boolean;
      action: string;
      message: string;
      preservation?: string;
      enabled: boolean;
      remote_git: string;
      remote_svn: string;
      managed_removal: boolean;
    }>(`/repos/${id}/disable`, { method: 'POST' }),

  reviewManagedRemoval: (id: string, options: { delete_git: boolean; delete_svn: boolean }, signal?: AbortSignal) =>
    fetchJson<import('./managedRemoval').ReviewedRemovalPreview>(`/repos/${id}/removal/review`, { method: 'POST', signal, body: JSON.stringify(options) }),

  approveManagedRemoval: async (id: string, review: import('./managedRemoval').ReviewedRemovalPreview, owner?: { assertCurrent(): void }): Promise<ManagedRemovalStatus> => {
    const session = clientSession(); const origin = session.snapshot();
    const journal = await import('./managedRemoval');
    session.assertCurrent(origin); owner?.assertCurrent();
    journal.retainPendingRemovalReview(id, review);
    const res = await sessionFetch(`${API_BASE}/repos/${id}/removal/approve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${localStorage.getItem('session_token') || ''}` },
      body: JSON.stringify({ delete_git: review.delete_git, delete_svn: review.delete_svn, review_pin: review.review_pin, request_id: `ui-remove-v2-${review.review_pin}` }),
    });
    const body = await res.json() as ManagedRemovalStatus & { error?: string; request_rejected?: boolean };
    session.assertCurrent(origin);
    if ((!res.ok && res.status !== 202 && res.status !== 409) || !body.operation_id) {
      if (res.status === 409 && body.request_rejected === true) journal.clearPendingRemovalReview(id, review.review_pin);
      throw new Error(body.error || body.message || `API error ${res.status}`);
    }
    journal.persistManagedRemovalReceipt(journal.managedRemovalReceiptFromStatus(id, body), true);
    journal.clearPendingRemovalReview(id, review.review_pin);
    return body;
  },

  continueManagedRemoval: async (id: string, operationId: string): Promise<ManagedRemovalStatus> => {
    const session = clientSession(); const origin = session.snapshot();
    const res = await sessionFetch(`${API_BASE}/repos/${id}/removal/continue`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${localStorage.getItem('session_token') || ''}` },
      body: JSON.stringify({ operation_id: operationId }),
    });
    const body = await res.json() as ManagedRemovalStatus & { error?: string };
    if ((!res.ok && res.status !== 202 && res.status !== 409) || !body.operation_id) throw new Error(body.error || body.message || `API error ${res.status}`);
    if (body.operation_id !== operationId || body.protocol !== 2) throw new Error('Continuation returned a different removal identity');
    const journal = await import('./managedRemoval'); session.assertCurrent(origin);
    journal.persistManagedRemovalReceipt(journal.managedRemovalReceiptFromStatus(id, body), true);
    return body;
  },

  lookupRemovalRequest: async (id: string, reviewPin: string): Promise<ManagedRemovalStatus | null> => {
    const params = new URLSearchParams({ request_id: `ui-remove-v2-${reviewPin}`, review_pin: reviewPin });
    const res = await sessionFetch(`${API_BASE}/repos/${id}/removal/request?${params}`, {
      headers: { Authorization: `Bearer ${localStorage.getItem('session_token') || ''}` },
    });
    if (res.status === 404) return null;
    const body = await res.json() as ManagedRemovalStatus & { error?: string };
    if ((!res.ok && res.status !== 202 && res.status !== 409) || !body.operation_id) throw new Error(body.error || body.message || `API error ${res.status}`);
    if (body.protocol !== 2 || body.request_id !== `ui-remove-v2-${reviewPin}` || body.review_pin !== reviewPin) throw new Error('Removal lookup returned a different request identity');
    return body;
  },

  removeManagedRepo: async (
    id: string,
    remoteOpts?: { delete_git?: boolean; delete_svn?: boolean },
  ): Promise<ManagedRemovalStatus> => {
    const token = localStorage.getItem('session_token');
    const params = new URLSearchParams({
      explicit_remote_deletion_opts: 'true',
      delete_git: String(remoteOpts?.delete_git ?? false),
      delete_svn: String(remoteOpts?.delete_svn ?? false),
    });
    const res = await sessionFetch(`${API_BASE}/repos/${id}/remove?${params}`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
        'Idempotency-Key': `ui-remove-${id}-${Date.now()}`,
      },
    });
    const text = await res.text();
    let body = {} as ManagedRemovalStatus & { error?: string };
    if (text) {
      try {
        body = JSON.parse(text) as ManagedRemovalStatus & { error?: string };
      } catch {
        throw new Error(text || `API error ${res.status}`);
      }
    }
    const message = body.error || body.message || text || `API error ${res.status}`;
    if (!res.ok && res.status !== 409 && res.status !== 202) {
      throw new Error(message);
    }
    if (!body.operation_id) {
      if (res.status === 409 && body.state === 'blocked') {
        throw new Error(message);
      }
      throw new Error(message);
    }
    return body;
  },

  restoreManagedRepo: async (id: string): Promise<{
    ok: boolean;
    action: string;
    state: string;
    repo_id: string;
    enabled: boolean;
    message: string;
  }> => {
    const token = localStorage.getItem('session_token');
    const res = await sessionFetch(`${API_BASE}/repos/${id}/restore`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
    });
    const text = await res.text();
    let body = {} as {
      ok?: boolean;
      action?: string;
      state?: string;
      repo_id?: string;
      enabled?: boolean;
      message?: string;
      error?: string;
    };
    if (text) {
      body = JSON.parse(text) as typeof body;
    }
    const message = body.error || body.message || text || `API error ${res.status}`;
    if (!res.ok) {
      throw new Error(message);
    }
    return {
      ok: body.ok ?? true,
      action: body.action ?? 'managed_restore',
      state: body.state ?? 'restored',
      repo_id: body.repo_id ?? id,
      enabled: body.enabled ?? false,
      message: body.message ?? message,
    };
  },

  getRemovalDependencyPreview: async (id: string): Promise<import('./managedRemoval').RemovalPreviewResponse> => {
    const token = localStorage.getItem('session_token');
    const res = await sessionFetch(`${API_BASE}/repos/${id}/removal/preview`, {
      headers: token ? { Authorization: `Bearer ${token}` } : {},
    });
    const text = await res.text();
    let body = {} as import('./managedRemoval').RemovalPreviewResponse & { error?: string };
    if (text) {
      body = JSON.parse(text) as import('./managedRemoval').RemovalPreviewResponse & { error?: string };
    }
    const message = (body as { error?: string }).error || text || `API error ${res.status}`;
    if (!res.ok && res.status !== 409 && res.status !== 202) {
      throw new Error(message);
    }
    if (!body.dependency_preview) {
      throw new Error(message);
    }
    return body;
  },

  getManagedRemoval: async (
    id: string,
    operationId?: string,
  ): Promise<ManagedRemovalStatus | null> => {
    const token = localStorage.getItem('session_token');
    const params = operationId
      ? `?operation_id=${encodeURIComponent(operationId)}`
      : '';
    const res = await sessionFetch(`${API_BASE}/repos/${id}/removal${params}`, {
      headers: token ? { Authorization: `Bearer ${token}` } : {},
    });
    if (res.status === 404) {
      return null;
    }
    const text = await res.text();
    let body = {} as ManagedRemovalStatus & { error?: string };
    if (text) {
      body = JSON.parse(text) as ManagedRemovalStatus & { error?: string };
    }
    const message = body.error || body.message || text || `API error ${res.status}`;
    if (!res.ok && res.status !== 409 && res.status !== 202) {
      throw new Error(message);
    }
    return body;
  },
  createBranchPair: (repoId: string, data: {
    reconciliation?: boolean;
    include_parent?: boolean;
    review_pin?: string;
    operation_id?: string;
    action?: 'preview' | 'publish' | 'status' | 'resume' | 'cancel';
    svn_branch: string;
    git_branch: string;
    skip_import?: boolean;
    compatibility_skip_import?: boolean;
    dry_run?: boolean;
    preview?: boolean;
    auto_create_svn_branch?: boolean;
    auto_create_git_branch?: boolean;
  }) =>
    fetchJson<LatePairPlan>(`/repos/${repoId}/branches`, { method: 'POST', body: JSON.stringify(data) }),

  listBranchPairs: (repoId: string) =>
    fetchJson<Repository[]>(`/repos/${repoId}/branches`),

  previewPairRefresh: (repoId: string, data: {
    operation?: 'update_pair_from_parent' | 'reanchor' | 'recreate';
    execute?: boolean;
    dry_run?: boolean;
  }) =>
    fetchJson<PairRefreshPlan>(`/repos/${repoId}/refresh`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),

  deleteBranchPair: async (
    repoId: string,
    opts?: { delete_git?: boolean; delete_svn?: boolean },
  ): Promise<BranchPairRemovalResult> => {
    const params = buildBranchPairDeleteQuery(opts);
    const token = localStorage.getItem('session_token');
    const res = await sessionFetch(`${API_BASE}/repos/${repoId}/branch-pair?${params.toString()}`, {
      method: 'DELETE',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
    });
    const text = await res.text();
    let body: BranchPairRemovalResult & { error?: string } = {};
    if (text) {
      try {
        body = JSON.parse(text) as BranchPairRemovalResult & { error?: string };
      } catch {
        body = { message: text };
      }
    }
    const message = body.error || body.message || text || `API error ${res.status}`;
    if (!res.ok) {
      throw new Error(message);
    }
    return body;
  },

  triggerRepoSync: (id: string) =>
    fetchJson<{ ok: boolean }>(`/repos/${id}/sync`, { method: 'POST' }),

  getSkipCommitContext: (repoId: string) =>
    fetchJson<{ ok: boolean; context: SkipCommitContext }>(
      `/repos/${repoId}/skip-commit/context`
    ),

  skipCommit: (repoId: string, body: SkipCommitRequest) =>
    fetchJson<{
      ok: boolean;
      message: string;
      old_sha: string;
      new_sha: string;
      excluded_commits: string[];
      remaining_pending: string[];
    }>(`/repos/${repoId}/skip-commit`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  retryRepo: (repoId: string) =>
    fetchJson<{ ok: boolean; message: string }>(
      `/repos/${repoId}/retry`, { method: 'POST' }
    ),
  getRepoCredentials: (id: string) =>
    fetchJson<{ svn_password_set: boolean; git_token_set: boolean }>(`/repos/${id}/credentials`),
  getRepoSaveRequest: (requestId: string, repoId?: string, signal?: AbortSignal) =>
    fetchJson<RepositorySaveReceipt>(repoId
      ? `/repos/${encodeURIComponent(repoId)}/save-requests/${encodeURIComponent(requestId)}`
      : `/repository-save-requests/${encodeURIComponent(requestId)}`, { signal }),

  saveRepoCredentials: (id: string, data: { svn_password?: string; git_token?: string }, requestId?: string, expectedUpdatedAt?: string, signal?: AbortSignal) =>
    fetchJson<{ ok: boolean; save_receipt: RepositorySaveReceipt }>(`/repos/${id}/credentials`, {
      method: 'POST', signal,
      body: JSON.stringify({ ...data, save_request_id: requestId, expected_updated_at: expectedUpdatedAt }),
    }),

  // Auth - public info (no auth required) for login page LDAP hints
  getAuthInfo: async (): Promise<{ ldap_enabled: boolean; ldap_domain: string | null }> => {
    const res = await sessionFetch(`${API_BASE}/auth/info`, {}, false);
    return res.json();
  },

  // Auth - multi-user login with real error messages
  login: async (username: string, password: string): Promise<LoginResponse> => {
    const session = clientSession(); const origin = session.snapshot();
    const res = await sessionFetch(`${API_BASE}/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    }, false);
    if (!res.ok) {
      const data = await res.json().catch(() => ({ error: 'Login failed' }));
      throw new Error(data.error || `Login failed (${res.status})`);
    }
    const result: LoginResponse = await res.json();
    session.signIn(result, origin);
    return result;
  },

  logout: () => {
    // Local retirement is immediate. A late revocation response cannot clear a new login.
    const token = clientSession().end();
    return sessionFetch(`${API_BASE}/auth/logout`, {
      method: 'POST', headers: { 'Content-Type': 'application/json', ...(token ? { Authorization: `Bearer ${token}` } : {}) },
      body: JSON.stringify({ token: token ?? '' }),
    }, false).then(async response => { if (!response.ok) throw new ApiError(response.status, 'Server logout could not be confirmed'); return response.json(); });
  },

  getMe: () => fetchJson<User>('/auth/me'),

  // Users (admin)
  getUsers: () => fetchJson<User[]>('/users'),

  createUser: (data: CreateUserRequest) =>
    fetchJson<User>('/users', {
      method: 'POST',
      body: JSON.stringify(data),
    }),

  updateUser: (id: string, data: UpdateUserRequest) =>
    fetchJson<User>(`/users/${id}`, {
      method: 'PUT',
      body: JSON.stringify(data),
    }),

  deleteUser: (id: string) =>
    fetchJson<{ ok: boolean }>(`/users/${id}`, {
      method: 'DELETE',
    }),

  // Credentials
  getUserCredentials: (userId: string) =>
    fetchJson<CredentialSummary[]>(`/users/${userId}/credentials`),

  storeCredential: (userId: string, data: StoreCredentialRequest) =>
    fetchJson<CredentialSummary>(`/users/${userId}/credentials`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),

  deleteCredential: (userId: string, credId: string) =>
    fetchJson<{ ok: boolean }>(`/users/${userId}/credentials/${credId}`, {
      method: 'DELETE',
    }),

  testCredential: (userId: string, credId: string) =>
    fetchJson<{ ok: boolean; message: string }>(`/users/${userId}/credentials/${credId}/test`, {
      method: 'POST',
    }),

  // LDAP configuration (admin)
  getLdapConfig: () => fetchJson<LdapConfig>('/admin/ldap'),

  saveLdapConfig: (config: SaveLdapConfigRequest) =>
    fetchJson<{ ok: boolean; message: string }>('/admin/ldap', {
      method: 'PUT',
      body: JSON.stringify(config),
    }),

  testLdapConnection: (config: SaveLdapConfigRequest) =>
    fetchJson<{ ok: boolean; message: string }>('/admin/ldap/test', {
      method: 'POST',
      body: JSON.stringify(config),
    }),
};

export interface SystemMetrics {
  disk_free_bytes: number;
  disk_total_bytes: number;
  disk_usage_percent: number;
  mem_used_bytes: number;
  mem_total_bytes: number;
  mem_usage_percent: number;
  cpu_load_1m: number;
  cpu_load_5m: number;
  cpu_load_15m: number;
  git_push_active: boolean;
  git_push_pid: number | null;
  git_push_elapsed_secs: number | null;
  data_dir_size_bytes: number;
  net_bytes_sent: number;
  net_bytes_recv: number;
  net_up_bytes_per_sec: number;
  net_down_bytes_per_sec: number;
  svn_active: boolean;
}

export interface VerificationResult {
  ok: boolean;
  mismatched_files: string[];
  missing_files: string[];
  extra_files: string[];
  message: string;
}

export interface WizardSetupConfig {
  // SVN
  svn_url: string;
  svn_username: string;
  svn_layout: string;
  svn_trunk_path: string;
  svn_password_set: boolean;

  // Git
  git_provider: string;
  git_api_url: string;
  git_repo: string;
  git_branch: string;
  git_token_set: boolean;

  // Sync
  sync_mode: string;
  auto_merge: boolean;
  sync_tags: boolean;
  lfs_threshold: number;

  // Identity
  email_domain: string;

  // Server
  listen: string;
  auth_mode: string;
  poll_interval: number;
  log_level: string;
  data_dir: string;
  admin_password_set: boolean;
}

/** #56 one stored policy column as read. `list` with no entries is a stored `[]`. */
export type RepositoryPolicyFieldRead =
  | { state: 'unset' }
  | { state: 'list'; entries: string[] }
  | { state: 'malformed'; stored_bytes: number; reason: string };

/** #56 `path_policy` on repository reads; never inferred from the write-only raw fields. */
export interface RepositoryPathPolicyRead {
  version: 1;
  allowed_paths: RepositoryPolicyFieldRead;
  blocked_patterns: RepositoryPolicyFieldRead;
  enforced_direction: 'git_to_svn';
  svn_to_git_filtering: 'not_applied';
  path_rewriting: 'none_read_only';
}

export interface Repository {
  save_receipt?: RepositorySaveReceipt;
  id: string;
  name: string;
  parent_id: string | null;
  svn_url: string;
  svn_branch: string;
  svn_username: string;
  git_provider: string;
  git_api_url: string;
  git_repo: string;
  git_branch: string;
  sync_mode: string;
  poll_interval_secs: number;
  lfs_threshold_mb: number;
  auto_merge: boolean;
  enabled: boolean;
  created_by: string | null;
  created_at: string;
  updated_at: string;
  /** Write-only PUT field: JSON array text, or null to clear. Reads use `path_policy`. */
  allowed_paths?: string | null;
  /** Write-only PUT field: JSON array text, or null to clear. Reads use `path_policy`. */
  blocked_patterns?: string | null;
  /** #56 typed policy read; absent from servers without #56. */
  path_policy?: RepositoryPathPolicyRead;
  consecutive_errors: number;
  sync_status: string;
  initializing?: boolean;
  import_mode?: RepoImportMode | string | null;
  starting_revision?: number | null;
  history_boundary?: string | null;
  snapshot_pin?: SnapshotPin | null;
}

export interface ImportStatus {
  phase: 'idle' | 'connecting' | 'importing' | 'verifying' | 'final_push' | 'completed' | 'failed' | 'cancelled';
  current_rev: number;
  total_revs: number;
  commits_created: number;
  current_file_count: number;
  lfs_unique_count: number;
  files_skipped: number;
  batches_pushed: number;
  push_started_at: string | null;
  verification: VerificationResult | null;
  errors: string[];
  log_lines: string[];
  started_at: string | null;
  completed_at: string | null;
  operation_id?: string;
  lifecycle?: 'queued' | 'running' | 'cancel_requested' | 'cancelling' | 'completed' | 'cancelled' | 'failed' | 'reconciliation_required';
  busy?: boolean;
  cancelling?: boolean;
  can_start?: boolean;
  last_local_svn_rev?: number | null;
  last_local_git_sha?: string | null;
  last_confirmed_svn_rev?: number | null;
  last_confirmed_git_sha?: string | null;
  intended_ref?: string | null;
  intended_git_sha?: string | null;
  outcome_detail?: string | null;
  import_mode?: RepoImportMode | string | null;
  starting_revision?: number | null;
  history_boundary?: string | null;
  earlier_history_imported?: boolean | null;
  snapshot_pin?: SnapshotPin | null;
}
