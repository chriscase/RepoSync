import { useQuery } from '@tanstack/react-query';
import { errorStatusQuery } from '../errorStatus';
import { statusObservationView } from '../statusObservation';
import { useStatusObservation } from '../useStatusObservation';

export function StatusObservationPanel({view}:{view:ReturnType<typeof statusObservationView>}) {
  const o=view.observation;
  const color=view.health==='idle' ? 'text-green-300' : ['held','paused','conflict','error'].includes(view.health) ? 'text-yellow-300':'text-gray-300';
  return <section aria-label="Repository status and evidence" className="space-y-1 text-xs mt-3">
    <p role="status" aria-live="polite" aria-atomic="true" className={color}>{view.healthLabel}. {view.availability}.</p>
    <p>Sync configuration: {view.configured}. {view.notice}</p>
    <p>Observation: {o?.observed_at ?? 'Unknown'}. Last successful sync: {o?.last_successful_sync_at ?? (o?.data_freshness==='never_recorded' ? 'Not recorded':'Unknown')}.</p>
    <p>Supported durable-read coverage: {o ? o.evidence_complete ? 'complete snapshot':'incomplete / unknown':'unknown'}. This is not a lease or collection proof.</p>
    <p>Source freshness: {o?.data_freshness ?? 'unknown'}. Remote contents: not verified by this status read.</p>
    <p>Syncs: {view.metric('total_syncs')}; active conflicts: {view.metric('active_conflicts')}; errors: {view.metric('total_errors')}.</p>
    {o?.operations.map((operation,index)=><div key={`${operation.kind}:${operation.operation_id ?? index}`} className="border-l border-gray-600 pl-2 break-words">
      <p>{operation.repository_id}: {operation.kind} — {operation.state}{operation.held ? ' (hold retained)':''}.</p>
      <p>Operation: {operation.operation_id ?? 'Unavailable / restricted'}. Ref: {operation.target_ref ?? 'Unknown'}. Intended Git: {operation.intended_git_sha ?? 'Unknown'}. Confirmed SVN: {operation.confirmed_svn_revision ?? 'Unknown'}.</p>
      {operation.history_evidence && <p>{Object.entries(operation.history_evidence).map(([name,value])=>`${name}: ${value ?? 'Unknown'}`).join('; ')}</p>}
      {operation.reason && <p>{operation.reason}</p>}
      {!operation.details_visible && <p>Some evidence is unavailable or restricted; no completion is inferred.</p>}
    </div>)}
    <p>{o?.safe_next_action ?? 'Refresh status and inspect scoped evidence. Unknown does not authorize Retry, Skip or hold release.'}</p>
  </section>;
}
export default function RepositoryStatusSummary({repoId}:{repoId:string}) {
  const query=useQuery({...errorStatusQuery(repoId),refetchInterval:15000,retry:false});
  return <StatusObservationPanel view={useStatusObservation(query)}/>;
}
