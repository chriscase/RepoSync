import { dashboardListLink } from '../listPagination';
import { useStatusObservation } from '../useStatusObservation';
import RepositoryStatusSummary, { StatusObservationPanel } from '../components/StatusObservation';
import { errorStatusQuery, errorStatusView, refreshErrorStatus } from '../errorStatus';
import ErrorAcknowledgement from '../components/ErrorAcknowledgement';
import { errorScope } from '../errorAcknowledgement';
import { useCallback, useEffect, useId, useMemo, useState, useSyncExternalStore } from 'react';
import { useQuery, useQueryClient, type QueryState } from '@tanstack/react-query';
import { Link, useNavigate, useSearchParams } from 'react-router-dom';
import { UIForgeActivityStream } from '@appforgeapps/uiforge';
import { api, type CommitMapEntry, type Repository, type SyncStatus } from '../api';
import { formatTimeAgo } from '../utils/time';
import ImportProgressCard from '../components/ImportProgressCard';
import ServerMonitor from '../components/ServerMonitor';
import { HealthMark } from '../components/SyncStatus';
import { statusObservationView, type EffectiveHealth } from '../statusObservation';
import { Database, GitBranch, ArrowRight } from 'lucide-react';
import { RepoBadge, DirectionBadge } from '../components/Badges';
import { renderAuditEvent, renderAuditIcon, renderSyncRecordEvent, renderSyncRecordIcon } from '../components/ActivityEventRenderers';
import { auditEntryToActivityEvent, syncRecordToActivityEvent } from '../utils/activityAdapter';
import { useText } from '../layouts/vocabulary';

export default function Dashboard() {
  const t = useText();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [searchParams,setSearchParams]=useSearchParams();
  const selectedRepoId=searchParams.get('repo_id') || 'all';
  const setSelectedRepoId=(value:string)=>{const next=new URLSearchParams(searchParams);if(value==='all')next.delete('repo_id');else next.set('repo_id',value);setSearchParams(next);};

  const activeRepoId = selectedRepoId !== 'all' ? selectedRepoId : undefined;

  const statusQuery = useQuery({
    ...errorStatusQuery(activeRepoId),
    refetchInterval: 5000,
  });

  const {data:status,isError}=statusQuery;
  const statusView=useStatusObservation(statusQuery);
  const errorCard=errorStatusView(status,isError);

  const { data: recentActivity,isPending:activityPending,isError:activityError,refetch:refetchActivity } = useQuery({
    queryKey: ['audit', 'recent', activeRepoId],
    queryFn: () => api.getAuditLog(20, undefined, undefined, activeRepoId),
    staleTime: 30000,
    refetchInterval: 10000,
  });

  const { data: syncRecords, isPending: syncRecordsPending, isError: syncRecordsError, error: syncRecordsFailure, dataUpdatedAt: syncRecordsLoadedAt } = useQuery({
    queryKey: ['sync-records', activeRepoId],
    queryFn: () => api.getSyncRecords(20, activeRepoId),
    staleTime: 30000,
    refetchInterval: 10000,
  });

  const { data: commitMap, isPending: commitMapPending, isError: commitMapError, error: commitMapFailure, dataUpdatedAt: commitMapLoadedAt } = useQuery({
    queryKey: ['commit-map', activeRepoId],
    queryFn: () => api.getCommitMap(15, activeRepoId),
    staleTime: 30000,
    refetchInterval: 10000,
  });

  const { data: repos, isError: reposError, error: reposFailure } = useQuery({
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

  const repoContextLabel = selectedRepo ? selectedRepo.name : activeRepoId ? `Repository ${activeRepoId}`:t('dashboard.scopeAll');

  const cmEntries = commitMap?.entries ?? [];

  return (
    <div className="rs-page space-y-6" data-rs-page="dashboard">
      <div className="flex items-center justify-between" data-rs-region="scope">
        <h1 className="rs-title text-2xl font-bold text-gray-100">Dashboard</h1>
        {(repos || activeRepoId) && (
          <select
            aria-label="Repository scope"
            value={selectedRepoId}
            onChange={(e) => setSelectedRepoId(e.target.value)}
            className="rs-in bg-gray-800 border border-gray-600 text-gray-200 rounded-md px-3 py-1.5 text-sm"
          >
            <option value="all">{t('dashboard.scopeAll')}</option>
            {activeRepoId && !repos?.some(r=>r.id===activeRepoId) && <option value={activeRepoId}>Repository {activeRepoId}</option>}
            {repos?.map(r => (
              <option key={r.id} value={r.id}>{r.name}</option>
            ))}
          </select>
        )}
      </div>

      {/* #264 G02.11: derived from the status reads below; hidden in Classic, Individual and Team */}
      <NeedsAttention repos={repos} reposError={reposError} reposFailure={reposFailure} />

      {/* Import Progress — show per-repo when filtered, or scan all repos for active/recent imports.
          The region is the cards themselves: each card's root carries data-rs-region="imports". */}
      {activeRepoId ? (
        <ImportProgressCard repoId={activeRepoId} repoName={selectedRepo?.name} regionName="imports" />
      ) : (
        repos && repos.filter((r: Repository) => !r.parent_id).map((r: Repository) => (
          <ImportProgressCard key={r.id} repoId={r.id} repoName={r.name} hideIfIdle regionName="imports" />
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
          <div className="space-y-3" data-rs-region="repositories">
            <div className="flex items-center justify-between">
              <h2 className="rs-heading text-lg font-semibold text-gray-100">Repositories</h2>
              <button
                onClick={() => navigate('/repos')}
                className="text-sm text-blue-400 hover:text-blue-300 transition-colors"
              >
                {t('dashboard.manageRepositories')}
              </button>
            </div>
            <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-3">
            {parents.map((repo: Repository) => {
              return (
              <div key={repo.id} className="rs-sec bg-gray-800/60 border border-gray-700 rounded-lg overflow-hidden hover:border-blue-500/50 transition-colors">
                <button
                  onClick={() => navigate(`/repos/${repo.id}`)}
                  className="w-full p-4 text-left"
                >
                  <div className="flex items-start gap-3">
                    <svg className="w-7 h-7 flex-shrink-0 mt-0.5" viewBox="0 0 32 32" fill="none">
                      <path d="M8 14 A8 8 0 0 1 24 14" style={{ stroke: 'var(--rs-svn, #3b82f6)' }} strokeWidth="2.5" strokeLinecap="round" fill="none" />
                      <path d="M22 10 L24 14 L20 14" style={{ fill: 'var(--rs-svn, #3b82f6)' }} />
                      <path d="M24 18 A8 8 0 0 1 8 18" style={{ stroke: 'var(--rs-git, #a855f7)' }} strokeWidth="2.5" strokeLinecap="round" fill="none" />
                      <path d="M10 22 L8 18 L12 18" style={{ fill: 'var(--rs-git, #a855f7)' }} />
                    </svg>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2 mb-1">
                        <span className="font-semibold text-sm text-gray-100 truncate">{repo.name}</span>
                        <span className={`rs-mark px-1.5 py-0.5 rounded text-[10px] font-medium ${repo.enabled ? 'bg-blue-900/60 text-blue-300' : 'bg-gray-700 text-gray-400'}`}>
                          {repo.enabled ? 'Configured enabled' : 'Configured disabled'}
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
                    </div>
                  </div>
                </button>
                <div className="px-5 pb-4"><RepositoryStatusSummary repoId={repo.id}/></div>
                {/* Branch pairs inside card (recursive) */}
                <DashboardBranchTree repoId={repo.id} childMap={childMap} navigate={navigate} depth={0} />
              </div>
              );
            })}
            </div>
          </div>
        );
      })()}

      {/* One region; its own space-y-6 keeps the spacing these three had as direct children of the page */}
      <div data-rs-region="health" className="space-y-6">
      <StatusObservationPanel view={statusView}/>
      {/* Status Cards */}
      <div className="mb-1">
        <span className="text-xs text-blue-400">{repoContextLabel}</span>
      </div>
      <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-6 gap-4">
        <StatusCard
          title="Effective sync health"
          value={statusView.healthLabel}
          color={statusView.health==='idle' ? 'green':'gray'}
        />
        <StatusCard
          title="Total Syncs"
          value={statusView.metric('total_syncs')}
          color="blue"
          navigationHref={dashboardListLink('/audit',activeRepoId)}
        />
        <StatusCard
          title="Active Conflicts"
          value={statusView.metric('active_conflicts')}
          color={status?.active_conflicts ? 'red' : 'gray'}
          navigationHref={dashboardListLink('/conflicts',activeRepoId)}
        />
        <StatusCard
          title="Errors (24h)"
          value={statusView.metric('total_errors')}
          color={status?.total_errors ? 'red' : 'gray'}
          navigationHref={dashboardListLink('/audit',activeRepoId,{success:'false'})}
          subtitle={
            status?.last_error_at
              ? `Last: ${formatTimeAgo(status.last_error_at)}`
              : errorCard.description
          }
          action={
            <ErrorAcknowledgement key={activeRepoId ?? 'all'} scope={errorScope(activeRepoId)}
              repositoryName={repoContextLabel} observation={errorCard.observation}
              count={errorCard.count}
              onSuccess={() => { void refreshErrorStatus(queryClient); }} />
          }
        />
        <StatusCard
          title="Uptime"
          value={status?.observation?.uptime_available ? formatUptime(status.uptime_secs) : 'Unavailable'}
          color="gray"
        />
        <StatusCard
          title="Last Sync"
          value={status?.observation?.scope==='aggregate' ? 'See scoped observations' : status?.last_sync_at ? formatTimeAgo(status.last_sync_at) : status ? 'Not recorded':'Unknown'}
          color="gray"
        />
      </div>
      </div>

      {/* Sync Position — only meaningful for a single repo */}
      {activeRepoId && (
      <div className="rs-sec bg-gray-800 shadow rounded-lg p-6 border border-gray-700" data-rs-region="position">
        <h2 className="rs-heading text-lg font-semibold text-gray-100 mb-4">
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
                : status ? 'Not recorded':'Unknown'}
            </p>
          </div>
          <div>
            <span className="text-sm text-gray-400">Last Git Hash</span>
            <p className="text-lg font-mono truncate text-gray-100">
              {status?.last_git_hash
                ? status.last_git_hash.substring(0, 12)
                : status ? 'Not recorded':'Unknown'}
            </p>
          </div>
          <div>
            <span className="text-sm text-gray-400">Last Sync</span>
            <p className="text-lg text-gray-100">
              {status?.last_sync_at
                ? new Date(status.last_sync_at).toLocaleString()
                : status ? 'Not recorded':'Unknown'}
            </p>
          </div>
          <div>
            <span className="text-sm text-gray-400">Total Conflicts</span>
            <p className="text-lg text-gray-100">
              {statusView.metric('total_conflicts')}
            </p>
          </div>
        </div>
      </div>
      )}

      {/* Sync Records */}
      <div className="rs-sec bg-gray-800 shadow rounded-lg border border-gray-700" data-rs-region="records">
        <div className="p-6 pb-3">
          <h2 className="rs-heading text-lg font-semibold text-gray-100">
            {t('dashboard.records')}
            {selectedRepo && (
              <span className="ml-2 text-sm font-normal text-blue-400">— {selectedRepo.name}</span>
            )}
          </h2>
          <p className="text-sm text-gray-400 mt-1">
            Individual commits synced between SVN and Git (click to expand)
          </p>
          {!selectedRepo && repos && repos.length > 1 && (
            <p className="text-xs text-gray-300 mt-1">
              Showing data from all repositories. Select a specific repository to filter.
            </p>
          )}
        </div>
        <div className="px-6 pb-6">
          {/* #264 G02.7: a failed read is not "No sync records yet"; a failed refresh keeps the last list */}
          {syncRecordsError && !syncRecords ? <ReadFailure title="Sync records could not be loaded." error={syncRecordsFailure} />
          : syncRecordsPending ? <p role="status" className="text-sm text-gray-400">Loading sync records…</p>
          : <>
          {syncRecordsError && (
            <StaleReadNote title="Sync records could not be refreshed." items="records" loadedAt={syncRecordsLoadedAt} error={syncRecordsFailure} />
          )}
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
          </>}
        </div>
      </div>

      {/* Commit Map */}
      <div className="rs-sec bg-gray-800 shadow rounded-lg border border-gray-700" data-rs-region="commit-map">
        <div className="p-6 pb-3">
          <h2 className="rs-heading text-lg font-semibold text-gray-100">
            {t('dashboard.commitMap')}
            {selectedRepo && (
              <span className="ml-2 text-sm font-normal text-blue-400">— {selectedRepo.name}</span>
            )}
          </h2>
          <p className="text-sm text-gray-400 mt-1">
            Bidirectional mapping between SVN revisions and Git commits
          </p>
        </div>
        {/* #264 G02.8: a failed read is not "No commit mappings yet"; a failed refresh keeps the last list */}
        {commitMapError && commitMap && (
          <div className="px-6 pb-3">
            <StaleReadNote title="Commit map could not be refreshed." items="mappings" loadedAt={commitMapLoadedAt} error={commitMapFailure} />
          </div>
        )}
        {commitMapError && !commitMap ? (
          <div className="px-6 pb-6"><ReadFailure title="Commit map could not be loaded." error={commitMapFailure} /></div>
        ) : commitMapPending ? (
          <p role="status" className="text-gray-400 text-sm px-6 pb-6">Loading commit map…</p>
        ) : cmEntries.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="rs-tbl min-w-full divide-y divide-gray-700">
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
      <div className="rs-sec bg-gray-800 shadow rounded-lg p-6 border border-gray-700" data-rs-region="activity">
        <h2 className="rs-heading text-lg font-semibold text-gray-100 mb-4">
          {t('dashboard.activity')}
          {selectedRepo && (
            <span className="ml-2 text-sm font-normal text-blue-400">— {selectedRepo.name}</span>
          )}
        </h2>
        {activityError ? <div role="alert">Error loading recent activity. Results are unknown. <button onClick={()=>void refetchActivity()}>Retry</button></div>
        : activityPending || !recentActivity ? <div role="status">Loading recent activity…</div>
        : <UIForgeActivityStream
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
          emptyMessage="No matching activity"
        />}
      </div>

      {/* Server Monitor */}
      <ServerMonitor data-rs-region="server" />
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
  navigationHref,
  subtitle,
  action,
}: {
  title: string;
  value: string;
  color: string;
  navigationHref?: string;
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

  const clickableClasses = navigationHref
    ? 'cursor-pointer hover:border-blue-500/50 transition-colors'
    : '';

  return (
    <div
      className={`relative rounded-lg border p-4 ${colorClasses[color] ?? colorClasses.gray} ${clickableClasses}`}
    >
      {navigationHref && <Link to={navigationHref} aria-label={`View ${title}`} className="absolute inset-0 rounded-lg focus-visible:outline focus-visible:outline-2 focus-visible:outline-blue-400"/>}
      <div className="flex items-center justify-between">
        <p className="text-sm text-gray-400">{title}</p>
        {action && <div className="relative z-10">{action}</div>}
      </div>
      <p className="text-2xl font-bold capitalize text-gray-100">{value}</p>
      {subtitle && <p className="text-xs text-gray-300 mt-1">{subtitle}</p>}
    </div>
  );
}

/** The client's own error text, which is what a failed read is shown with. */
function readErrorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error ?? 'Unknown error');
}

/** #264 G02.7, G02.8: a failed read is shown as failed, never as an empty list. */
function ReadFailure({ title, error }: { title: string; error: unknown }) {
  return (
    <div role="alert" className="rs-bn rs-bn--danger rounded-md border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">
      <p className="font-medium">{title}</p>
      <p>Results are unknown. This is not the same as none.</p>
      <p className="mt-1 break-words text-gray-300">{readErrorText(error)}</p>
    </div>
  );
}

/**
 * #264 G02.7, G02.8: a failed refresh keeps the last list that loaded, marked as possibly out
 * of date and with the client's error text, as the repository page and the audit log do.
 */
function StaleReadNote({ title, items, loadedAt, error }: { title: string; items: string; loadedAt: number; error: unknown }) {
  return (
    <div role="status" className="rs-bn rs-bn--warn mb-3 rounded-md border border-yellow-700 bg-yellow-900/30 px-4 py-3 text-sm text-yellow-200">
      <p className="font-medium">{title}</p>
      <p>
        {loadedAt
          ? `The ${items} below were last loaded at ${new Date(loadedAt).toLocaleString()} and may be out of date.`
          : `The ${items} below are from an earlier read and may be out of date.`}
      </p>
      <p className="mt-1 break-words text-gray-300">{readErrorText(error)}</p>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Needs attention (#264 G02.11): derived, read-only, links only
// ---------------------------------------------------------------------------

const ATTENTION_HEALTH: readonly EffectiveHealth[] = ['held', 'paused', 'error', 'conflict', 'unknown'];
const STATE_ORDER: readonly EffectiveHealth[] = ['held', 'paused', 'conflict', 'error', 'unknown', 'running', 'initializing', 'busy', 'disabled', 'idle'];
const BANNER_TONE: Partial<Record<EffectiveHealth, string>> = { held: 'held', paused: 'warn', conflict: 'warn', error: 'danger', unknown: 'info' };

type PairReading =
  | { repo: Repository; kind: 'pending' }
  | {
    repo: Repository;
    kind: 'answered' | 'attention';
    /** A current observation: not failed and not stale. */
    current: boolean;
    health: EffectiveHealth;
    label: string;
    lines: string[];
    section: 'overview' | 'operations';
  };

/** The pairs the Repositories region shows, in its order. The page reads the status of each one. */
function pairsInPageOrder(repos: readonly Repository[]): Repository[] {
  const childMap = new Map<string, Repository[]>();
  for (const r of repos) if (r.parent_id) childMap.set(r.parent_id, [...(childMap.get(r.parent_id) ?? []), r]);
  const ordered: Repository[] = [];
  const seen = new Set<string>();
  const visit = (repo: Repository) => {
    if (seen.has(repo.id)) return;
    seen.add(repo.id);
    ordered.push(repo);
    (childMap.get(repo.id) ?? []).forEach(visit);
  };
  repos.filter(r => !r.parent_id).forEach(visit);
  return ordered;
}

/**
 * Reads the per-pair status queries the page already runs (RepositoryStatusSummary) straight from
 * the query cache. It adds no observer, so it never starts, repeats or reconfigures a request.
 */
function useStatusReads(ids: readonly string[]): (QueryState<SyncStatus> | undefined)[] {
  const client = useQueryClient();
  const subscribe = useCallback((onChange: () => void) => client.getQueryCache().subscribe(onChange), [client]);
  const signature = () => ids.map(id => {
    const state = client.getQueryState<SyncStatus>(errorStatusQuery(id).queryKey);
    return state ? `${state.status}:${state.dataUpdatedAt}:${state.errorUpdatedAt}` : '-';
  }).join('|');
  useSyncExternalStore(subscribe, signature, signature);
  return ids.map(id => client.getQueryState<SyncStatus>(errorStatusQuery(id).queryKey));
}

/** Staleness depends on the clock as well as on the reads. */
function useNow(intervalMs = 5000): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs]);
  return now;
}

function readPair(repo: Repository, state: QueryState<SyncStatus> | undefined, now: number): PairReading {
  const view = statusObservationView({ data: state?.data, error: state?.error ?? undefined, isError: state?.status === 'error', dataUpdatedAt: state?.dataUpdatedAt }, now);
  if (!view.failed && !view.status) return { repo, kind: 'pending' };
  // A read that failed or went stale is Unknown, whatever the last-known health was.
  const current = !view.failed && !view.stale;
  const operations = view.observation?.operations ?? [];
  const lines = !current
    ? [`${view.availability}.`, ...(view.failed ? [readErrorText(state?.error)] : [])]
    : [
      // Why a current read is still Unknown.
      ...(!view.observation ? [view.notice] : []),
      ...(view.observation && !view.observation.evidence_complete ? ['Supported durable-read coverage: incomplete / unknown.'] : []),
      ...operations.filter(op => op.held).map(op => `Operation: ${op.kind} — ${op.state} (hold retained).`),
      ...(view.observation?.safe_next_action ? [view.observation.safe_next_action] : []),
    ];
  const importNeedsYou = operations.some(op => op.kind === 'import' && (op.held || ['failed', 'reconciliation_required', 'unknown'].includes(op.state)));
  return {
    repo,
    kind: !current || ATTENTION_HEALTH.includes(view.health) ? 'attention' : 'answered',
    current,
    health: view.health,
    label: view.healthLabel,
    lines,
    section: importNeedsYou ? 'operations' : 'overview',
  };
}

function NeedsAttention({ repos, reposError, reposFailure }: { repos: Repository[] | undefined; reposError: boolean; reposFailure: unknown }) {
  const headingId = useId();
  const now = useNow();
  const pairs = useMemo(() => pairsInPageOrder(repos ?? []), [repos]);
  const states = useStatusReads(pairs.map(pair => pair.id));
  const readings = pairs.map((pair, index) => readPair(pair, states[index], now));
  const total = readings.length;
  const attention = readings.filter(r => r.kind === 'attention');
  const pending = readings.filter(r => r.kind === 'pending').length;
  const current = readings.filter(r => r.kind !== 'pending' && r.current).length;
  const counts = new Map<EffectiveHealth, { label: string; count: number }>();
  for (const r of readings) {
    if (r.kind === 'pending') continue;
    counts.set(r.health, { label: r.label, count: (counts.get(r.health)?.count ?? 0) + 1 });
  }
  const pairWord = (n: number) => (n === 1 ? 'pair' : 'pairs');
  const summary = !repos
    ? (reposError ? 'What needs attention is unknown.' : 'Loading repositories…')
    : total === 0 ? 'Nothing needs attention. No repositories are registered.'
    : attention.length > 0 ? `${attention.length} of ${total} ${pairWord(total)} ${attention.length === 1 ? 'needs' : 'need'} attention.`
    : pending === total ? `Reading status for ${total} ${pairWord(total)}…`
    : pending > 0 ? `Still reading status for ${pending} of ${total} ${pairWord(total)}. None of the others needs attention.`
    : 'Nothing needs attention: no pair is held, paused, in conflict, in error or of unknown status.';

  return (
    <section data-rs-region="attention" data-rs-derived="" aria-labelledby={headingId}
      className="rs-sec bg-gray-800 shadow rounded-lg p-6 border border-gray-700 space-y-3">
      <h2 id={headingId} className="rs-heading text-lg font-semibold text-gray-100">Needs attention</h2>
      {reposError && (
        <div role="alert" className="rs-bn rs-bn--danger rounded-md border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">
          <p className="font-medium">{repos ? 'The repository list could not be refreshed; this is the last list read.' : 'Repositories could not be read.'}</p>
          <p className="mt-1 break-words text-gray-300">{readErrorText(reposFailure)}</p>
        </div>
      )}
      <p role="status" className="text-sm text-gray-200">{summary}</p>
      {total > 0 && (
        <>
          <p className="text-xs text-gray-400">{current} of {total} {pairWord(total)} answered with a current status.</p>
          {counts.size > 0 && (
            <ul aria-label="Pairs by state" className="flex flex-wrap gap-2">
              {STATE_ORDER.filter(health => counts.has(health)).map(health => (
                <li key={health}><HealthMark health={health} label={`${counts.get(health)!.label} ${counts.get(health)!.count}`} /></li>
              ))}
            </ul>
          )}
        </>
      )}
      {attention.length > 0 && (
        <ul aria-label="Pairs that need attention" className="space-y-2">
          {attention.map(r => r.kind === 'pending' ? null : (
            <li key={r.repo.id} className={`rs-bn rs-bn--${BANNER_TONE[r.health] ?? 'info'} rounded-md border border-gray-700 bg-gray-900/40 p-3`}>
              <div className="flex flex-wrap items-center gap-2">
                <HealthMark health={r.health} label={r.label} />
                <Link to={`/repos/${encodeURIComponent(r.repo.id)}?section=${r.section}`}
                  className="text-sm font-semibold text-blue-400 hover:text-blue-300 break-all">{r.repo.name}</Link>
              </div>
              {r.lines.map((line, index) => <p key={index} className="mt-1 break-words text-xs text-gray-300">{line}</p>)}
            </li>
          ))}
        </ul>
      )}
      {total > 0 && (
        <p className="text-xs text-gray-400">
          Counted from the status read this page already makes for each pair. A pair whose read fails or goes stale is counted as Unknown.
        </p>
      )}
    </section>
  );
}

/** Recursive component for rendering nested branch pairs in Dashboard repo cards. */
function DashboardBranchTree({
  repoId,
  childMap,
  navigate,
  depth,
}: {
  repoId: string;
  childMap: Map<string, Repository[]>;
  navigate: (path: string) => void;
  depth: number;
}) {
  const children = childMap.get(repoId) ?? [];
  if (children.length === 0) return null;

  return (
    <div className={`border-t border-gray-700/50 bg-gray-900/30 px-4 py-2 space-y-0.5 ${depth > 0 ? 'border-t-0 pl-6' : ''}`}>
      {depth === 0 && (
        <div className="flex items-center gap-1 text-[10px] uppercase tracking-wider text-gray-300 mb-1">
          <GitBranch className="w-2.5 h-2.5" />
          <span>Branches</span>
        </div>
      )}
      {children.map((child: Repository) => (
        <div key={child.id}>
          <button
            onClick={() => navigate(`/repos/${child.id}`)}
            className="w-full flex items-center gap-2 px-2 py-1 rounded hover:bg-gray-700/40 transition-colors text-left"
          >
            <GitBranch className="w-3 h-3 text-purple-400 flex-shrink-0" />
            <span className="text-xs font-medium text-gray-300 truncate">{child.name.split(' / ').pop()}</span>
            <span className="text-[10px] text-blue-300">{child.svn_branch}</span>
            <ArrowRight className="w-2.5 h-2.5 text-gray-400 flex-shrink-0" />
            <span className="text-[10px] text-purple-300">{child.git_branch}</span>
            <span className="ml-auto flex items-center gap-1.5 flex-shrink-0">
              <span className="text-[10px] text-gray-400">Configuration: {child.enabled ? 'enabled':'disabled'}</span>
            </span>
          </button>
          <RepositoryStatusSummary repoId={child.id}/>
          <DashboardBranchTree repoId={child.id} childMap={childMap} navigate={navigate} depth={depth + 1} />
        </div>
      ))}
    </div>
  );
}

