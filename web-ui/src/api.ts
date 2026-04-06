const API_BASE = '/api';

export interface SyncStatus {
  state: string;
  last_sync_at: string | null;
  last_svn_revision: number | null;
  last_git_hash: string | null;
  total_syncs: number;
  total_conflicts: number;
  active_conflicts: number;
  total_errors: number;
  last_error_at: string | null;
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
}

export interface AuditEntry {
  id: number;
  repo_id?: string;
  action: string;
  direction: string | null;
  svn_rev: number | null;
  git_sha: string | null;
  author: string | null;
  details: string | null;
  created_at: string;
  success: boolean;
}

export interface AuditListResponse {
  entries: AuditEntry[];
  total: number;
}

export interface AuthorMapping {
  svn_username: string;
  name: string;
  email: string;
  github?: string;
}

export interface CommitMapEntry {
  id: number;
  repo_id?: string;
  svn_rev: number;
  git_sha: string;
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
  const token = localStorage.getItem('session_token');
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...(token ? { Authorization: `Bearer ${token}` } : {}),
  };

  const res = await fetch(`${API_BASE}${url}`, { ...options, headers });
  if (res.status === 401) {
    // Clear credentials and redirect to login
    localStorage.removeItem('session_token');
    localStorage.removeItem('user');
    if (window.location.pathname !== '/login') {
      window.location.href = '/login';
    }
    throw new Error('Session expired — please log in again');
  }
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`API error ${res.status}: ${text}`);
  }
  return res.json();
}

export const api = {
  getStatus: (repoId?: string) => {
    const params = repoId ? `?repo_id=${repoId}` : '';
    return fetchJson<SyncStatus>(`/status${params}`);
  },

  getHealth: () => fetchJson<{ ok: boolean }>('/status/health'),

  getSystemMetrics: () => fetchJson<SystemMetrics>('/status/system'),

  getConflicts: (status?: string) => {
    const params = new URLSearchParams();
    if (status) params.append('status', status);
    const qs = params.toString();
    return fetchJson<Conflict[]>(`/conflicts${qs ? `?${qs}` : ''}`);
  },

  getConflict: (id: string) => fetchJson<Conflict>(`/conflicts/${id}`),

  resolveConflict: (id: string, resolution: string, mergedContent?: string) =>
    fetchJson<{ ok: boolean }>(`/conflicts/${id}/resolve`, {
      method: 'POST',
      body: JSON.stringify({ resolution, merged_content: mergedContent }),
    }),

  deferConflict: (id: string) =>
    fetchJson<{ ok: boolean }>(`/conflicts/${id}/defer`, { method: 'POST' }),

  getAuditLog: (limit = 50, page?: number, success?: boolean, repoId?: string) => {
    const params = new URLSearchParams({ limit: String(limit) });
    if (page !== undefined) params.append('page', String(page));
    if (success !== undefined) params.append('success', String(success));
    if (repoId) params.append('repo_id', repoId);
    return fetchJson<AuditListResponse>(`/audit?${params.toString()}`);
  },

  resetErrors: async (): Promise<{ok: boolean, cleared: number}> => {
    const token = localStorage.getItem('session_token');
    const res = await fetch(`${API_BASE}/status/reset-errors`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
    });
    if (!res.ok) throw new Error('Failed to reset errors');
    return res.json();
  },

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
    fetchJson<{ ok: boolean; message: string }>('/setup/import', {
      method: 'POST',
    }),

  getSetupConfig: () => fetchJson<WizardSetupConfig>('/setup/config'),

  getImportStatus: () =>
    fetchJson<ImportStatus>('/setup/import/status'),

  cancelImport: () =>
    fetchJson<{ ok: boolean; message: string }>('/setup/import/cancel', {
      method: 'POST',
    }),

  resetAndReimport: () =>
    fetchJson<{ ok: boolean; message: string }>('/setup/reset-reimport', {
      method: 'POST',
    }),

  // Repositories (multi-repo)
  getRepos: () => fetchJson<Repository[]>('/repos'),
  createRepo: (data: Partial<Repository>) =>
    fetchJson<Repository>('/repos', { method: 'POST', body: JSON.stringify(data) }),
  getRepo: (id: string) => fetchJson<Repository>(`/repos/${id}`),
  updateRepo: (id: string, data: Partial<Repository>) =>
    fetchJson<Repository>(`/repos/${id}`, { method: 'PUT', body: JSON.stringify(data) }),
  deleteRepo: (id: string) =>
    fetchJson<void>(`/repos/${id}`, { method: 'DELETE' }),
  createBranchPair: (repoId: string, data: { svn_branch: string; git_branch: string; skip_import: boolean }) =>
    fetchJson<Repository>(`/repos/${repoId}/branches`, { method: 'POST', body: JSON.stringify(data) }),

  listBranchPairs: (repoId: string) =>
    fetchJson<Repository[]>(`/repos/${repoId}/branches`),

  triggerRepoSync: (id: string) =>
    fetchJson<{ ok: boolean }>(`/repos/${id}/sync`, { method: 'POST' }),
  getRepoCredentials: (id: string) =>
    fetchJson<{ svn_password_set: boolean; git_token_set: boolean }>(`/repos/${id}/credentials`),
  saveRepoCredentials: (id: string, data: { svn_password?: string; git_token?: string }) =>
    fetchJson<{ ok: boolean }>(`/repos/${id}/credentials`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),

  // Auth - public info (no auth required) for login page LDAP hints
  getAuthInfo: async (): Promise<{ ldap_enabled: boolean; ldap_domain: string | null }> => {
    const res = await fetch(`${API_BASE}/auth/info`);
    return res.json();
  },

  // Auth - multi-user login with real error messages
  login: async (username: string, password: string): Promise<LoginResponse> => {
    const res = await fetch(`${API_BASE}/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    });
    if (!res.ok) {
      const data = await res.json().catch(() => ({ error: 'Login failed' }));
      throw new Error(data.error || `Login failed (${res.status})`);
    }
    return res.json();
  },

  logout: () => {
    const token = localStorage.getItem('session_token');
    return fetchJson<{ ok: boolean }>('/auth/logout', {
      method: 'POST',
      body: JSON.stringify({ token: token ?? '' }),
    });
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

export interface Repository {
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
}
