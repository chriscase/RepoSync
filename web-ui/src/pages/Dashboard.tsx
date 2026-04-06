import { LoadingSpinner } from '../components/LoadingSpinner';
import { useState, useMemo } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from 'react-router-dom';
import { UIForgeActivityStream } from '@appforgeapps/uiforge';
import { api, type CommitMapEntry, type Repository } from '../api';
import { formatTimeAgo } from '../utils/time';
import ImportProgressCard from '../components/ImportProgressCard';
import ServerMonitor from '../components/ServerMonitor';
import { Database, GitBranch, ArrowRight } from 'lucide-react';
import { RepoBadge, DirectionBadge } from '../components/Badges';
import { renderAuditEvent, renderAuditIcon, renderSyncRecordEvent, renderSyncRecordIcon } from '../components/ActivityEventRenderers';
import { auditEntryToActivityEvent, syncRecordToActivityEvent } from '../utils/activityAdapter';

export default function Dashboard() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [selectedRepoId, setSelectedRepoId] = useState<string>('all');

  const activeRepoId = selectedRepoId !== 'all' ? selectedRepoId : undefined;

  const { data: status, isLoading: statusLoading, isError, error } = useQuery({
    queryKey: ['status', activeRepoId],
    queryFn: () => api.getStatus(activeRepoId),
    refetchInterval: 5000,
  });

  const { data: recentActivity } = useQuery({
    queryKey: ['audit', 'recent', activeRepoId],
    queryFn: () => api.getAuditLog(20, undefined, undefined, activeRepoId),
    staleTime: 30000,
    refetchInterval: 10000,
  });

  const { data: syncRecords } = useQuery({
    queryKey: ['sync-records', activeRepoId],
    queryFn: () => api.getSyncRecords(20, activeRepoId),
    staleTime: 30000,
    refetchInterval: 10000,
  });

  const { data: commitMap } = useQuery({
    queryKey: ['commit-map', activeRepoId],
    queryFn: () => api.getCommitMap(15, activeRepoId),
    staleTime: 30000,
    refetchInterval: 10000,
  });

  const { data: repos } = useQuery({
    queryKey: ['repos'],
    queryFn: api.getRepos,
  });

  const repoName = repos && repos.length === 1 ? repos[0].name : 'Unknown';

  const repoNameMap = useMemo(() => {
    const m = new Map<string, string>();
    if (repos) repos.forEach(r => m.set(r.id, r.name));
    return m;
  }, [repos]);

  // Convert audit entries and sync records to UIForge ActivityEvent format
  // (must be called before any early returns to satisfy rules-of-hooks)
  const activityEvents = useMemo(
    () => (recentActivity?.entries ?? []).map(e => auditEntryToActivityEvent(e, e.repo_id ? repoNameMap.get(e.repo_id) || 'Unknown' : repoName)),
    [recentActivity, repoName, repoNameMap],
  );
  const syncRecordEvents = useMemo(
    () => (syncRecords?.entries ?? []).map(r => syncRecordToActivityEvent(r, r.repo_id ? repoNameMap.get(r.repo_id) || 'Unknown' : repoName)),
    [syncRecords, repoName, repoNameMap],
  );

  if (statusLoading) {
    return <LoadingSpinner />;
  }

  if (isError) {
    return (
      <div className="text-center py-8 text-red-400">
        Error loading status: {error?.message ?? 'Unknown error'}
      </div>
    );
  }

  const formatUptime = (secs: number) => {
    const days = Math.floor(secs / 86400);
    const hours = Math.floor((secs % 86400) / 3600);
    const mins = Math.floor((secs % 3600) / 60);
    if (days > 0) return `${days}d ${hours}h ${mins}m`;
    if (hours > 0) return `${hours}h ${mins}m`;
    return `${mins}m`;
  };

  const selectedRepo = selectedRepoId !== 'all'
    ? repos?.find(r => r.id === selectedRepoId)
    : null;

  const repoContextLabel = selectedRepo ? selectedRepo.name : 'All Repositories';

  const cmEntries = commitMap?.entries ?? [];

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold text-gray-100">Dashboard</h1>
        {repos && repos.length > 1 && (
          <select
            value={selectedRepoId}
            onChange={(e) => setSelectedRepoId(e.target.value)}
            className="bg-gray-800 border border-gray-600 text-gray-200 rounded-md px-3 py-1.5 text-sm"
          >
            <option value="all">All Repositories</option>
            {repos.map(r => (
              <option key={r.id} value={r.id}>{r.name}</option>
            ))}
          </select>
        )}
      </div>

      {/* Import Progress — show per-repo when filtered, or scan all repos for active/recent imports */}
      {activeRepoId ? (
        <ImportProgressCard repoId={activeRepoId} repoName={selectedRepo?.name} />
      ) : (
        repos && repos.filter((r: Repository) => !r.parent_id).map((r: Repository) => (
          <ImportProgressCard key={r.id} repoId={r.id} repoName={r.name} hideIfIdle />
        ))
      )}

      {/* Repositories Overview */}
      {repos && repos.length > 0 && (() => {
        const parents = repos.filter((r: Repository) => !r.parent_id);
        const childMap = new Map<string, Repository[]>();
        for (const r of repos) {
          if (r.parent_id) {
            const list = childMap.get(r.parent_id) || [];
            list.push(r);
            childMap.set(r.parent_id, list);
          }
        }
        return (
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <h2 className="text-lg font-semibold text-gray-100">Repositories</h2>
              <button
                onClick={() => navigate('/repos')}
                className="text-sm text-blue-400 hover:text-blue-300 transition-colors"
              >
                Manage Repositories &rarr;
              </button>
            </div>
            <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-3">
            {parents.map((repo: Repository) => {
              const children = childMap.get(repo.id) || [];
              return (
              <div key={repo.id} className="bg-gray-800/60 border border-gray-700 rounded-lg overflow-hidden hover:border-blue-500/50 transition-colors">
                <button
                  onClick={() => navigate(`/repos/${repo.id}`)}
                  className="w-full p-4 text-left"
                >
                  <div className="flex items-start gap-3">
                    <svg className="w-7 h-7 flex-shrink-0 mt-0.5" viewBox="0 0 32 32" fill="none">
                      <path d="M8 14 A8 8 0 0 1 24 14" stroke="#3b82f6" strokeWidth="2.5" strokeLinecap="round" fill="none" />
                      <path d="M22 10 L24 14 L20 14" fill="#3b82f6" />
                      <path d="M24 18 A8 8 0 0 1 8 18" stroke="#a855f7" strokeWidth="2.5" strokeLinecap="round" fill="none" />
                      <path d="M10 22 L8 18 L12 18" fill="#a855f7" />
                    </svg>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2 mb-1">
                        <span className="font-semibold text-sm text-gray-100 truncate">{repo.name}</span>
                        <span className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${repo.enabled ? 'bg-green-900/60 text-green-300' : 'bg-gray-700 text-gray-400'}`}>
                          {repo.enabled ? 'Enabled' : 'Disabled'}
                        </span>
                      </div>
                      <div className="flex items-center gap-1 text-xs text-gray-400 mb-0.5">
                        <Database className="w-3 h-3 text-blue-400" />
                        <span className="truncate">{repo.svn_url}</span>
                      </div>
                      <div className="flex items-center gap-1 text-xs text-gray-400">
                        <GitBranch className="w-3 h-3 text-purple-400" />
                        <span className="truncate">{repo.git_repo}</span>
                      </div>
                      <div className="flex items-center gap-3 mt-2 text-xs text-gray-500">
                        {repo.enabled ? (
                          <span className="flex items-center gap-1">
                            <span className="relative w-2 h-2"><span className="absolute inset-0 rounded-full bg-green-400 animate-ping opacity-40" /><span className="relative block w-2 h-2 rounded-full bg-green-400" /></span>
                            <span className="text-green-300">Active</span>
                          </span>
                        ) : (
                          <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-full bg-gray-500" /><span>Disabled</span></span>
                        )}
                        <span>{formatTimeAgo(repo.updated_at)}</span>
                      </div>
                    </div>
                  </div>
                </button>
                {/* Branch pairs inside card */}
                {children.length > 0 && (
                  <div className="border-t border-gray-700/50 bg-gray-900/30 px-4 py-2 space-y-0.5">
                    <div className="flex items-center gap-1 text-[10px] uppercase tracking-wider text-gray-500 mb-1">
                      <GitBranch className="w-2.5 h-2.5" />
                      <span>Branches</span>
                    </div>
                    {children.map((child: Repository) => (
                      <button
                        key={child.id}
                        onClick={() => navigate(`/repos/${child.id}`)}
                        className="w-full flex items-center gap-2 px-2 py-1 rounded hover:bg-gray-700/40 transition-colors text-left"
                      >
                        <GitBranch className="w-3 h-3 text-purple-400 flex-shrink-0" />
                        <span className="text-xs font-medium text-gray-300 truncate">{child.name.split(' / ').pop()}</span>
                        <span className="text-[10px] text-blue-400/60">{child.svn_branch}</span>
                        <ArrowRight className="w-2.5 h-2.5 text-gray-600 flex-shrink-0" />
                        <span className="text-[10px] text-purple-400/60">{child.git_branch}</span>
                        <span className="ml-auto flex items-center gap-1.5 flex-shrink-0">
                          <span className="text-[10px] text-gray-600">{formatTimeAgo(child.updated_at)}</span>
                          {child.enabled ? <span className="w-1.5 h-1.5 rounded-full bg-green-400" /> : <span className="w-1.5 h-1.5 rounded-full bg-gray-500" />}
                        </span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
              );
            })}
            </div>
          </div>
        );
      })()}

      {/* Status Cards */}
      <div className="mb-1">
        <span className="text-xs text-blue-400">{repoContextLabel}</span>
      </div>
      <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-6 gap-4">
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
          title="Total Syncs"
          value={String(status?.total_syncs ?? 0)}
          color="blue"
          onClick={() => navigate('/audit')}
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
          onClick={() => navigate('/audit?success=false')}
          subtitle={
            status?.last_error_at
              ? `Last: ${formatTimeAgo(status.last_error_at)}`
              : 'No recent errors'
          }
          onClear={
            (status?.total_errors ?? 0) > 0
              ? async () => {
                  await api.resetErrors();
                  queryClient.invalidateQueries({ queryKey: ['status'] });
                }
              : undefined
          }
        />
        <StatusCard
          title="Uptime"
          value={status ? formatUptime(status.uptime_secs) : '-'}
          color="gray"
        />
        <StatusCard
          title="Last Sync"
          value={status?.last_sync_at ? formatTimeAgo(status.last_sync_at) : 'Never'}
          color="gray"
        />
      </div>

      {/* Sync Position — only meaningful for a single repo */}
      {activeRepoId && (
      <div className="bg-gray-800 shadow rounded-lg p-6 border border-gray-700">
        <h2 className="text-lg font-semibold text-gray-100 mb-4">
          Sync Position
          {selectedRepo && (
            <span className="ml-2 text-sm font-normal text-blue-400">— {selectedRepo.name}</span>
          )}
        </h2>
        <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
          <div>
            <span className="text-sm text-gray-400">Last SVN Revision</span>
            <p className="text-lg font-mono text-gray-100">
              {status?.last_svn_revision != null
                ? `r${status.last_svn_revision}`
                : 'Not synced'}
            </p>
          </div>
          <div>
            <span className="text-sm text-gray-400">Last Git Hash</span>
            <p className="text-lg font-mono truncate text-gray-100">
              {status?.last_git_hash
                ? status.last_git_hash.substring(0, 12)
                : 'Not synced'}
            </p>
          </div>
          <div>
            <span className="text-sm text-gray-400">Last Sync</span>
            <p className="text-lg text-gray-100">
              {status?.last_sync_at
                ? new Date(status.last_sync_at).toLocaleString()
                : 'Never'}
            </p>
          </div>
          <div>
            <span className="text-sm text-gray-400">Total Conflicts</span>
            <p className="text-lg text-gray-100">
              {status?.total_conflicts ?? 0}
            </p>
          </div>
        </div>
      </div>
      )}

      {/* Sync Records */}
      <div className="bg-gray-800 shadow rounded-lg border border-gray-700">
        <div className="p-6 pb-3">
          <h2 className="text-lg font-semibold text-gray-100">
            Recent Sync Records
            {selectedRepo && (
              <span className="ml-2 text-sm font-normal text-blue-400">— {selectedRepo.name}</span>
            )}
          </h2>
          <p className="text-sm text-gray-400 mt-1">
            Individual commits synced between SVN and Git (click to expand)
          </p>
          {!selectedRepo && repos && repos.length > 1 && (
            <p className="text-xs text-gray-500 mt-1">
              Showing data from all repositories. Select a specific repository to filter.
            </p>
          )}
        </div>
        <div className="px-6 pb-6">
          <UIForgeActivityStream
            events={syncRecordEvents}
            theme="dark"
            density="compact"
            enableGrouping={true}
            groupingThreshold={3}
            showTimeline={true}
            showDateSeparators={true}
            renderEvent={renderSyncRecordEvent}
            renderIcon={renderSyncRecordIcon}
            maxHeight="500px"
            emptyMessage="No sync records yet"
          />
        </div>
      </div>

      {/* Commit Map */}
      <div className="bg-gray-800 shadow rounded-lg border border-gray-700">
        <div className="p-6 pb-3">
          <h2 className="text-lg font-semibold text-gray-100">
            Commit Map (SVN &harr; Git)
            {selectedRepo && (
              <span className="ml-2 text-sm font-normal text-blue-400">— {selectedRepo.name}</span>
            )}
          </h2>
          <p className="text-sm text-gray-400 mt-1">
            Bidirectional mapping between SVN revisions and Git commits
          </p>
        </div>
        {cmEntries.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="min-w-full divide-y divide-gray-700">
              <thead>
                <tr>
                  <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase">Repository</th>
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
                    <td className="px-6 py-3">
                      <RepoBadge name={cm.repo_id ? repoNameMap.get(cm.repo_id) || 'Unknown' : repoName} />
                    </td>
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

      {/* Recent Activity */}
      <div className="bg-gray-800 shadow rounded-lg p-6 border border-gray-700">
        <h2 className="text-lg font-semibold text-gray-100 mb-4">
          Recent Activity
          {selectedRepo && (
            <span className="ml-2 text-sm font-normal text-blue-400">— {selectedRepo.name}</span>
          )}
        </h2>
        <UIForgeActivityStream
          events={activityEvents}
          theme="dark"
          density="compact"
          enableGrouping={true}
          groupingThreshold={2}
          showDateSeparators={false}
          showTimeline={true}
          responsive={true}
          renderEvent={renderAuditEvent}
          renderIcon={renderAuditIcon}
          maxHeight="600px"
          emptyMessage="No activity yet"
        />
      </div>

      {/* Server Monitor */}
      <ServerMonitor />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

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
      role={onClick ? 'button' : undefined}
      tabIndex={onClick ? 0 : undefined}
      onKeyDown={onClick ? (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onClick(); } } : undefined}
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

