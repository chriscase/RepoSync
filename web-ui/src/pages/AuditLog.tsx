import { auditFilters, auditPageKey, changeListFilter, changeListPage } from '../listPagination';
import { LoadingSpinner } from '../components/LoadingSpinner';
import { useState, useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import { useSearchParams } from 'react-router-dom';
import { UIForgeActivityStream } from '@appforgeapps/uiforge';
import { api, type AuditEntry } from '../api';
import { DirectionBadge } from '../components/Badges';
import { renderAuditEvent, renderAuditIcon } from '../components/ActivityEventRenderers';
import { auditEntryToActivityEvent } from '../utils/activityAdapter';

const ACTION_TYPES = [
  { value: '', label: 'All Actions' },
  { value: 'sync_cycle', label: 'Sync Cycles' },
  { value: 'conflict_detected', label: 'Conflicts Detected' },
  { value: 'conflict_resolved', label: 'Conflicts Resolved' },
  { value: 'sync_error', label: 'Sync Errors' },
  { value: 'webhook_received', label: 'Webhooks' },
  { value: 'daemon_started', label: 'Daemon Events' },
  { value: 'auth_login', label: 'Auth Events' },
  { value: 'config_updated', label: 'Config Changes' },
];

export default function AuditLog() {
  const [searchParams,setSearchParams]=useSearchParams();
  const filters=auditFilters(searchParams);
  const page=filters.page,PAGE_SIZE=filters.perPage;
  const actionFilter=filters.action ?? '';
  const successFilter=filters.success===false ? 'failure':filters.success===true ? 'success':'';
  const setActionFilter=(value:string)=>setSearchParams(changeListFilter(searchParams,'action',value));
  const setSuccessFilter=(value:string)=>setSearchParams(changeListFilter(searchParams,'success',value==='failure' ? 'false':value==='success' ? 'true':''));
  const [expandedId, setExpandedId] = useState<number | null>(null);
  const [viewMode, setViewMode] = useState<'table' | 'stream'>('stream');

  const {data:response,isPending,isError,error,refetch}=useQuery({
    queryKey:auditPageKey(filters),
    queryFn:()=>api.getAuditLog(PAGE_SIZE,page,filters.success,filters.repoId,filters.action),
    refetchInterval:15000,enabled:!filters.error,
  });
  const {data:repos}=useQuery({queryKey:['repos'],queryFn:api.getRepos});
  const entries=response?.entries ?? [];
  const totalCount=response?.total;
  const totalPages=response?.total_pages ?? 0;
  const allEntries=entries;
  const successCount = allEntries.filter((e) => e.success).length;
  const failureCount = allEntries.filter((e) => !e.success).length;

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold text-gray-100">Audit Log</h1>
          <p className="text-sm text-gray-400 mt-1">
            Complete activity trail of all sync operations, events, and changes
          </p>
        </div>
        <div className="flex items-center space-x-3">
          <span className="flex items-center space-x-1 text-sm">
            <span className="w-2 h-2 rounded-full bg-green-400 inline-block" />
            <span className="text-gray-400">{response && !isError ? `${successCount} successes on this page`:'Success count unknown'}</span>
          </span>
          <span className="flex items-center space-x-1 text-sm">
            <span className="w-2 h-2 rounded-full bg-red-400 inline-block" />
            <span className="text-gray-400">{response && !isError ? `${failureCount} failures on this page`:'Failure count unknown'}</span>
          </span>
        </div>
      </div>

      {/* Filters */}
      <div className="flex items-center space-x-3">
        <select aria-label="Repository filter" value={filters.repoId ?? 'all'} onChange={e=>setSearchParams(changeListFilter(searchParams,'repo_id',e.target.value==='all' ? '':e.target.value))} className="rounded-md border border-gray-600 bg-gray-700 text-gray-200 px-3 py-2 text-sm">
          <option value="all">All Repositories</option>
          {filters.repoId && !repos?.some(r=>r.id===filters.repoId) && <option value={filters.repoId}>Repository {filters.repoId}</option>}
          {repos?.map(r=><option key={r.id} value={r.id}>{r.name}</option>)}
        </select>
        <select
          value={actionFilter}
          aria-label="Action filter"
          onChange={(e) => setActionFilter(e.target.value)}
          className="rounded-md border border-gray-600 bg-gray-700 text-gray-200 px-3 py-2 text-sm focus:ring-blue-500 focus:border-blue-500"
        >
          {actionFilter && !ACTION_TYPES.some(a=>a.value===actionFilter) && <option value={actionFilter}>{actionFilter}</option>}
          {ACTION_TYPES.map((at) => (
            <option key={at.value} value={at.value}>{at.label}</option>
          ))}
        </select>
        <div className="flex rounded-md overflow-hidden border border-gray-600">
          {[
            { value: '', label: 'All' },
            { value: 'success', label: 'Success' },
            { value: 'failure', label: 'Failures' },
          ].map((f) => (
            <button
              key={f.value}
              aria-pressed={successFilter===f.value}
              onClick={() => setSuccessFilter(f.value)}
              className={`px-3 py-2 text-sm ${
                successFilter === f.value
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
              }`}
            >
              {f.label}
            </button>
          ))}
        </div>
        <div className="flex rounded-md overflow-hidden border border-gray-600">
          <button
            onClick={() => setViewMode('stream')}
            className={`px-3 py-2 text-sm ${
              viewMode === 'stream'
                ? 'bg-blue-600 text-white'
                : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
            }`}
          >
            Stream
          </button>
          <button
            onClick={() => setViewMode('table')}
            className={`px-3 py-2 text-sm ${
              viewMode === 'table'
                ? 'bg-blue-600 text-white'
                : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
            }`}
          >
            Table
          </button>
        </div>
        <span className="text-sm text-gray-500">
          {response && !isError ? `Showing ${entries.length} of ${totalCount} matching entries`:'Matching total unknown'}
        </span>
      </div>

      {filters.error ? <div role="alert">{filters.error} <button onClick={()=>setSearchParams(new URLSearchParams())}>Clear filters</button></div>
      : isError ? <div role="alert" className="text-red-300">Error loading audit log: {error.message}. Results are unknown. <button onClick={()=>void refetch()}>Retry</button></div>
      : isPending || !response ? <LoadingSpinner/>
      : !entries.length ? <div role="status">{response.total===0 ? 'No matching audit entries':'No audit entries on this page; matching records exist on other pages.'}</div>
      : viewMode === 'stream' ? (
        <AuditStreamView entries={entries} />
      ) : (
        <AuditTableView
          entries={entries}
          expandedId={expandedId}
          onToggleExpand={(id) => setExpandedId(expandedId === id ? null : id)}
        />
      )}

      {/* Pagination */}
      {response && !isError && !filters.error && (
        <div className="flex items-center justify-center space-x-4 py-4">
          <button
            onClick={() => setSearchParams(changeListPage(searchParams,page-1))}
            disabled={page <= 1}
            className={`px-3 py-2 text-sm rounded-md border ${
              page <= 1
                ? 'border-gray-700 text-gray-600 cursor-not-allowed'
                : 'border-gray-600 text-gray-300 hover:bg-gray-700'
            }`}
          >
            &larr; Previous
          </button>
          <span aria-live="polite" aria-atomic="true" className="text-sm text-gray-400">
            {totalPages ? `Page ${page} of ${totalPages}`:'No matching pages'}
          </span>
          <button
            onClick={() => setSearchParams(changeListPage(searchParams,page+1))}
            disabled={page >= totalPages}
            className={`px-3 py-2 text-sm rounded-md border ${
              page >= totalPages
                ? 'border-gray-700 text-gray-600 cursor-not-allowed'
                : 'border-gray-600 text-gray-300 hover:bg-gray-700'
            }`}
          >
            Next &rarr;
          </button>
          {page>1 && !entries.length && <button onClick={()=>setSearchParams(changeListPage(searchParams,1))}>Return to first page</button>}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Stream View (UIForgeActivityStream)
// ---------------------------------------------------------------------------

function AuditStreamView({ entries }: { entries: AuditEntry[] }) {
  const activityEvents = useMemo(
    () => entries.map(e => auditEntryToActivityEvent(e)),
    [entries],
  );

  return (
    <div className="bg-gray-800 shadow rounded-lg border border-gray-700 p-6">
      <UIForgeActivityStream
        events={activityEvents}
        theme="dark"
        density="comfortable"
        enableGrouping={true}
        groupingThreshold={2}
        showDateSeparators={true}
        showTimeline={true}
        responsive={true}
        renderEvent={renderAuditEvent}
        renderIcon={renderAuditIcon}
        emptyMessage="No matching audit entries"
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Table View (original)
// ---------------------------------------------------------------------------

function AuditTableView({
  entries,
  expandedId,
  onToggleExpand,
}: {
  entries: AuditEntry[];
  expandedId: number | null;
  onToggleExpand: (id: number) => void;
}) {
  return (
    <div className="bg-gray-800 shadow overflow-hidden rounded-lg border border-gray-700">
      <table className="min-w-full divide-y divide-gray-700">
        <thead>
          <tr>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase w-8"></th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase">
              Time
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase">
              Action
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase">
              Direction
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase">
              Details
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase">
              Author
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase">
              SVN Rev
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase">
              Git SHA
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-gray-700">
          {entries.map((entry: AuditEntry) => (
            <AuditRow
              key={entry.id}
              entry={entry}
              expanded={expandedId === entry.id}
              onToggle={() => onToggleExpand(entry.id)}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function AuditRow({
  entry,
  expanded,
  onToggle,
}: {
  entry: AuditEntry;
  expanded: boolean;
  onToggle: () => void;
}) {
  const actionColors: Record<string, string> = {
    sync_cycle: 'bg-cyan-900/50 text-cyan-300',
    conflict_detected: 'bg-red-900/50 text-red-300',
    conflict_resolved: 'bg-green-900/50 text-green-300',
    sync_error: 'bg-red-900/50 text-red-300',
    webhook_received: 'bg-yellow-900/50 text-yellow-300',
    daemon_started: 'bg-emerald-900/50 text-emerald-300',
    auth_login: 'bg-indigo-900/50 text-indigo-300',
    config_updated: 'bg-orange-900/50 text-orange-300',
  };

  return (
    <>
      <tr
        className={`cursor-pointer transition-colors ${
          expanded ? 'bg-gray-700/50' : 'hover:bg-gray-700/30'
        }`}
      >
        <td className="px-4 py-3">
          <span
            className={`inline-block w-2 h-2 rounded-full ${
              entry.success ? 'bg-green-400' : 'bg-red-400'
            }`}
            title={entry.success ? 'Success' : 'Failed'}
          />
        </td>
        <td className="px-4 py-3 text-sm text-gray-400 whitespace-nowrap">
          {new Date(entry.created_at).toLocaleString()}
        </td>
        <td className="px-4 py-3 whitespace-nowrap">
          <span
            className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
              actionColors[entry.action] ?? 'bg-gray-700 text-gray-300'
            }`}
          >
            {entry.action.replace(/_/g, ' ')}
          </span>
        </td>
        <td className="px-4 py-3 whitespace-nowrap">
          {entry.direction ? (
            <DirectionBadge direction={entry.direction} />
          ) : (
            <span className="text-gray-600">-</span>
          )}
        </td>
        <td className="px-4 py-3 text-sm text-gray-300 max-w-sm truncate">
          <button type="button" onClick={onToggle} aria-expanded={expanded} className="text-left text-blue-300 underline">
            {expanded ? 'Hide audit details: ' : 'Show audit details: '}{entry.details ?? entry.action}
          </button>
        </td>
        <td className="px-4 py-3 text-sm text-gray-300">
          {entry.author ?? '-'}
        </td>
        <td className="px-4 py-3 text-sm font-mono text-blue-400">
          {entry.svn_rev ? `r${entry.svn_rev}` : '-'}
        </td>
        <td className="px-4 py-3 text-sm font-mono text-purple-400">
          {entry.git_sha ? entry.git_sha.substring(0, 8) : '-'}
        </td>
      </tr>
      {expanded && (
        <tr>
          <td colSpan={8} className="px-4 py-4 bg-gray-900/50">
            <div className="rounded-lg border border-gray-700 bg-gray-900 p-4">
              <div className="grid grid-cols-2 md:grid-cols-4 gap-4 text-sm">
                <div>
                  <span className="text-gray-500 text-xs uppercase block">Audit ID</span>
                  <span className="text-gray-300">#{entry.id}</span>
                </div>
                <div>
                  <span className="text-gray-500 text-xs uppercase block">Action</span>
                  <span className="text-gray-300">{entry.action}</span>
                </div>
                <div>
                  <span className="text-gray-500 text-xs uppercase block">Status</span>
                  <span className={entry.success ? 'text-green-400' : 'text-red-400'}>
                    {entry.success ? 'Success' : 'Failed'}
                  </span>
                </div>
                <div>
                  <span className="text-gray-500 text-xs uppercase block">Timestamp</span>
                  <span className="text-gray-300">{new Date(entry.created_at).toLocaleString()}</span>
                </div>
              </div>
              {entry.details && (
                <div className="mt-4">
                  <span className="text-gray-500 text-xs uppercase block mb-1">Full Details</span>
                  <div className="bg-gray-800 rounded p-3 border border-gray-700">
                    <pre className="text-sm text-gray-200 whitespace-pre-wrap font-mono">{entry.details}</pre>
                  </div>
                </div>
              )}
              <div className="grid grid-cols-2 md:grid-cols-4 gap-4 mt-4 text-sm">
                <div>
                  <span className="text-gray-500 text-xs uppercase block">Direction</span>
                  <span className="text-gray-300">{entry.direction ?? 'N/A'}</span>
                </div>
                <div>
                  <span className="text-gray-500 text-xs uppercase block">Author</span>
                  <span className="text-gray-300">{entry.author ?? 'System'}</span>
                </div>
                <div>
                  <span className="text-gray-500 text-xs uppercase block">SVN Revision</span>
                  <span className="text-gray-300 font-mono">
                    {entry.svn_rev ? `r${entry.svn_rev}` : 'N/A'}
                  </span>
                </div>
                <div>
                  <span className="text-gray-500 text-xs uppercase block">Git SHA</span>
                  <span className="text-gray-300 font-mono">
                    {entry.git_sha ?? 'N/A'}
                  </span>
                </div>
              </div>
            </div>
          </td>
        </tr>
      )}
    </>
  );
}
