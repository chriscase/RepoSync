import { changeListFilter, changeListPage, conflictFilters, conflictPageKey } from '../listPagination';
import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link, useSearchParams } from 'react-router-dom';
import { api, type ConflictListItem } from '../api';
import { RepoBadge } from '../components/Badges';

export default function Conflicts() {
  const [searchParams,setSearchParams]=useSearchParams();
  const filters=conflictFilters(searchParams);
  const filter=filters.status ?? '';
  const repoFilter=filters.repoId ?? 'all';
  const setFilter=(value:string)=>setSearchParams(changeListFilter(searchParams,'status',value));
  const setRepoFilter=(value:string)=>setSearchParams(changeListFilter(searchParams,'repo_id',value==='all' ? '':value));

  const { data: repos } = useQuery({
    queryKey: ['repos'],
    queryFn: api.getRepos,
  });

  const repoNameMap = useMemo(() => {
    const m = new Map<string, string>();
    if (repos) repos.forEach((r) => m.set(r.id, r.name));
    return m;
  }, [repos]);

  const {data:response,isPending,isError,error,refetch}=useQuery({
    queryKey:conflictPageKey(filters),queryFn:()=>api.getConflictsPage(filters),enabled:!filters.error,
  });
  const conflicts=response?.items;

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold text-gray-100">Conflicts</h1>
        <div className="flex space-x-2 items-center">
          {(repos || filters.repoId) && (
            <select
              value={repoFilter}
              onChange={(e) => setRepoFilter(e.target.value)}
              aria-label="Repository filter"
              className="bg-gray-800 border border-gray-600 text-gray-200 rounded-md px-3 py-1 text-sm"
            >
              <option value="all">All Repositories</option>
              {filters.repoId && !repos?.some(r=>r.id===filters.repoId) && <option value={filters.repoId}>Repository {filters.repoId}</option>}
              {repos?.map((r) => (
                <option key={r.id} value={r.id}>
                  {r.name}
                </option>
              ))}
            </select>
          )}
          <span className="text-sm text-gray-400">Status: {filter || 'All'}</span>
          {['', 'detected', 'queued', 'deferred', 'resolved'].map((f) => (
            <button
              key={f}
              aria-pressed={filter===f}
              onClick={() => setFilter(f)}
              className={`px-3 py-1 rounded-md text-sm ${
                filter === f
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
              }`}
            >
              {f === '' ? 'All' : f.charAt(0).toUpperCase() + f.slice(1)}
            </button>
          ))}
        </div>
      </div>

      {filters.error ? <div role="alert">{filters.error} <button onClick={()=>setSearchParams(new URLSearchParams())}>Clear filters</button></div>
      : isError ? <div role="alert" className="text-red-300">Error loading conflicts: {error.message}. Results are unknown. <button onClick={()=>void refetch()}>Retry</button></div>
      : isPending || !response ? <div role="status">Loading conflicts…</div>
      : conflicts && conflicts.length > 0 ? (
        <div className="bg-gray-800 shadow overflow-hidden rounded-lg border border-gray-700">
          <table className="min-w-full divide-y divide-gray-700">
            <thead className="bg-gray-700/50">
              <tr>
                <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
                  Repository
                </th>
                <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
                  File
                </th>
                <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
                  Type
                </th>
                <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
                  Status
                </th>
                <th className="px-6 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
                  Created
                </th>
                <th className="px-6 py-3 text-right text-xs font-medium text-gray-400 uppercase tracking-wider">
                  Action
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-700">
              {conflicts.map((conflict: ConflictListItem) => (
                <ConflictRow
                  key={conflict.id}
                  conflict={conflict}
                  repoName={
                    conflict.repo_id ? repoNameMap.get(conflict.repo_id) ?? 'Unknown' : 'Unknown'
                  }
                />
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="bg-gray-800 shadow rounded-lg p-12 text-center border border-gray-700">
          <p className="text-gray-300 text-lg">{response?.total===0 ? 'No matching conflicts':'No conflicts on this page'}</p>
          <p className="text-gray-500 text-sm mt-1">{response?.total===0 ? 'Try another status or repository filter.':'Matching records exist on other pages.'}</p>
        </div>
      )}
      {response && !filters.error && !isError && <div aria-live="polite" aria-atomic="true" className="flex items-center gap-4">
        <span>{response.total} matching conflicts · {response.total_pages ? `Page ${response.page} / ${response.total_pages}`:'No matching pages'}</span>
        <button disabled={response.page<=1} onClick={()=>setSearchParams(changeListPage(searchParams,response.page-1))}>Previous</button>
        <button disabled={response.page>=response.total_pages} onClick={()=>setSearchParams(changeListPage(searchParams,response.page+1))}>Next</button>
        {response.page>1 && !response.items.length && <button onClick={()=>setSearchParams(changeListPage(searchParams,1))}>Return to first page</button>}
      </div>}
    </div>
  );
}

function ConflictRow({ conflict, repoName }: { conflict: ConflictListItem; repoName: string }) {
  const statusColors: Record<string, string> = {
    detected: 'bg-red-900/50 text-red-300',
    queued: 'bg-yellow-900/50 text-yellow-300',
    deferred: 'bg-gray-700 text-gray-300',
    resolved: 'bg-green-900/50 text-green-300',
  };

  const typeLabels: Record<string, string> = {
    content: 'Content',
    edit_delete: 'Edit/Delete',
    rename: 'Rename',
    property: 'Property',
    branch: 'Branch',
    binary: 'Binary',
  };

  return (
    <tr className="hover:bg-gray-700/50">
      <td className="px-6 py-4 whitespace-nowrap">
        <RepoBadge name={repoName} />
      </td>
      <td className="px-6 py-4 whitespace-nowrap">
        <code className="text-sm font-mono text-gray-200">{conflict.file_path}</code>
      </td>
      <td className="px-6 py-4 whitespace-nowrap">
        <span className="text-sm text-gray-300">
          {typeLabels[conflict.conflict_type] ?? conflict.conflict_type}
        </span>
      </td>
      <td className="px-6 py-4 whitespace-nowrap">
        <span
          className={`inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium ${
            statusColors[conflict.status] ?? 'bg-gray-700 text-gray-300'
          }`}
        >
          {conflict.status}
        </span>
      </td>
      <td className="px-6 py-4 whitespace-nowrap text-sm text-gray-400">
        {new Date(conflict.created_at).toLocaleString()}
      </td>
      <td className="px-6 py-4 whitespace-nowrap text-right">
        <Link
          to={`/conflicts/${conflict.id}`}
          className="text-blue-400 hover:text-blue-300 text-sm font-medium"
        >
          {conflict.status === 'resolved' ? 'View' : 'Resolve'}
        </Link>
      </td>
    </tr>
  );
}
