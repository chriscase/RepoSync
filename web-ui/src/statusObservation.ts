import type { SyncStatus } from './api';

export type EffectiveHealth='idle'|'running'|'initializing'|'busy'|'disabled'|'paused'|'conflict'|'held'|'error'|'unknown';
export interface StatusOperation {
  repository_id:string; kind:string; state:string; operation_id:string|null; updated_at:string|null;
  reason:string|null; target_evidence?:OperationTargetEvidence|null; target_ref:string|null; intended_git_sha:string|null; confirmed_svn_revision:number|null; history_evidence?:Record<string,string|null>|null; held:boolean; details_visible:boolean;
}
export type OperationTargetEvidence = {
  kind:'late_pair';target_git_branch:string;target_svn_branch:string;pinned_git_tip:string;baseline_git_sha:string;
  baseline_svn_revision:number;copy_source_svn_revision:number;copy_source_svn_path:string|null;
  observed_svn_branch_head_revision:number|null;replayed_git_shas:string[];
} | {
  kind:'svn_commit';target_svn_uuid:string;target_svn_branch_path:string|null;source_git_sha:string;
  source_git_parent:string|null;source_git_tree:string;pre_write_svn_revision:number;pre_write_svn_tree:string;intended_svn_tree:string;
};
export interface StatusObservation {
  version:1; scope:string; repository_id:string|null; configured:'enabled'|'disabled'|'mixed'|'unknown'; health:EffectiveHealth;
  observed_at:string; data_freshness:string; last_successful_sync_at:string|null; remote_freshness:string;
  registry_busy:boolean|null; evidence_complete:boolean; operations:StatusOperation[]; safe_next_action:string; uptime_available:boolean;
}
const healthValues:EffectiveHealth[]=['idle','running','initializing','busy','disabled','paused','conflict','held','error','unknown'];
const object=(v:unknown):v is Record<string,unknown>=>!!v && typeof v==='object' && !Array.isArray(v);
const nullableString=(v:unknown)=>v===null || typeof v==='string';
const date=(v:unknown)=>typeof v==='string' && Number.isFinite(Date.parse(v));
const oid=(v:unknown)=>typeof v==='string' && /^[0-9a-f]{40}([0-9a-f]{24})?$/i.test(v);
const treeFingerprint=(v:unknown)=>typeof v==='string' && /^[0-9a-f]{64}$/i.test(v);
const revision=(v:unknown)=>Number.isSafeInteger(v) && Number(v)>=0;
const nonempty=(v:unknown)=>typeof v==='string' && v.length>0;
const states:Record<string,readonly string[]>={
  import:['queued','running','cancel_requested','cancelling','completed','cancelled','failed','reconciliation_required','unknown'],
  svn_commit:['queued','running','completed','failed','reconciliation_required','unknown'],
  git_push:['queued','running','completed','failed','reconciliation_required','unknown'],
  removal:['queued','cancelling','running','completed','failed','reconciliation_required','unknown','unknown_or_held'],
  late_pair:['queued','svn_copy_pending','svn_copied','child_registered','replay_in_progress','completed','failed','reconciliation_required','unknown'],
  pair_refresh:['unknown_or_held'],history:['reconciliation_required','transient_refusal','unknown','unknown_or_held'],
};
function validTargetEvidence(value:unknown,kind:string):boolean {
  if(value===undefined || value===null)return true;
  if(!object(value) || value.kind!==kind)return false;
  if(kind==='late_pair') {
    const fields=['kind','target_git_branch','target_svn_branch','pinned_git_tip','baseline_git_sha','baseline_svn_revision','copy_source_svn_revision','copy_source_svn_path','observed_svn_branch_head_revision','replayed_git_shas'];
    return Object.keys(value).every(k=>fields.includes(k)) && ['target_git_branch','target_svn_branch'].every(k=>nonempty(value[k]))
      && oid(value.pinned_git_tip) && oid(value.baseline_git_sha) && revision(value.baseline_svn_revision) && revision(value.copy_source_svn_revision)
      && nullableString(value.copy_source_svn_path) && (value.observed_svn_branch_head_revision===null || revision(value.observed_svn_branch_head_revision))
      && Array.isArray(value.replayed_git_shas) && value.replayed_git_shas.every(oid);
  }
  if(kind==='svn_commit') {
    const fields=['kind','target_svn_uuid','target_svn_branch_path','source_git_sha','source_git_parent','source_git_tree','pre_write_svn_revision','pre_write_svn_tree','intended_svn_tree'];
    return Object.keys(value).every(k=>fields.includes(k)) && nonempty(value.target_svn_uuid) && nullableString(value.target_svn_branch_path)
      && oid(value.source_git_sha) && (value.source_git_parent===null || oid(value.source_git_parent)) && oid(value.source_git_tree)
      && revision(value.pre_write_svn_revision) && treeFingerprint(value.pre_write_svn_tree) && treeFingerprint(value.intended_svn_tree);
  }
  return false;
}
function validOperation(op:unknown,scope:Record<string,unknown>):op is Record<string,unknown> {
  if(!object(op) || !nonempty(op.repository_id) || typeof op.kind!=='string' || typeof op.state!=='string'
    || !Object.prototype.hasOwnProperty.call(states,op.kind) || !states[op.kind].includes(op.state)
    || (scope.scope==='repository' && op.repository_id!==scope.repository_id)
    || !['operation_id','reason','target_ref'].every(k=>nullableString(op[k]))
    || !(op.updated_at===null || date(op.updated_at)) || !(op.intended_git_sha===null || oid(op.intended_git_sha))
    || !(op.confirmed_svn_revision===null || revision(op.confirmed_svn_revision)) || typeof op.held!=='boolean' || typeof op.details_visible!=='boolean'
    || ((op.state==='reconciliation_required' || op.state==='unknown_or_held') && !op.held)
    || !validTargetEvidence(op.target_evidence,op.kind))return false;
  if(op.history_evidence!==undefined && op.history_evidence!==null && (op.kind!=='history' || !object(op.history_evidence)
    || !Object.entries(op.history_evidence).every(([key,v])=>['handled_checkpoint','prior_observed_tip','fresh_observed_remote_tip','local_bridge_tip'].includes(key) && (v===null || oid(v)))))return false;
  if(!op.details_visible && (op.operation_id!==null || op.target_ref!==null || op.intended_git_sha!==null || op.confirmed_svn_revision!==null
    || op.history_evidence!=null || op.target_evidence!=null))return false;
  return true;
}
/** HTTP errors are rejected by fetchJson first; malformed successful JSON is never cached as status. */
export function decodeSyncStatus(value:unknown,repoId?:string):SyncStatus {
  if(!object(value) || typeof value.state!=='string'
    || !['total_syncs','total_conflicts','active_conflicts','total_errors','uptime_secs'].every(k=>Number.isSafeInteger(value[k]) && Number(value[k])>=0)
    || !(value.last_git_hash===null || (typeof value.last_git_hash==='string' && /^[0-9a-f]{40}([0-9a-f]{24})?$/i.test(value.last_git_hash))) || !(value.last_svn_revision===null || (Number.isSafeInteger(value.last_svn_revision) && Number(value.last_svn_revision)>=0))
    || !(value.last_sync_at===null || date(value.last_sync_at)) || !(value.last_error_at===null || date(value.last_error_at))
    || !object(value.error_observation) || typeof value.error_observation.through_id!=='string' || !date(value.error_observation.observed_at))throw new Error('Malformed status response; health is unknown.');
  const raw=value.observation;
  if(raw!==undefined) {
    if(!object(raw) || raw.version!==1 || !date(raw.observed_at) || !nullableString(raw.repository_id)
      || (repoId ? raw.scope!=='repository' || raw.repository_id!==repoId : raw.scope!=='aggregate' || raw.repository_id!==null)
      || typeof raw.evidence_complete!=='boolean' || typeof raw.safe_next_action!=='string' || typeof raw.uptime_available!=='boolean'
      || !(raw.registry_busy===null || typeof raw.registry_busy==='boolean') || !(raw.last_successful_sync_at===null || date(raw.last_successful_sync_at))
      || typeof raw.data_freshness!=='string' || typeof raw.remote_freshness!=='string' || !Array.isArray(raw.operations)
      || !raw.operations.every(op=>validOperation(op,raw)))throw new Error('Malformed status observation; health is unknown.');
    const knownHealth=healthValues.includes(raw.health as EffectiveHealth);
    return {...value,observation:{...raw,health:knownHealth ? raw.health:'unknown',configured:['enabled','disabled','mixed'].includes(String(raw.configured)) ? raw.configured:'unknown',evidence_complete:raw.evidence_complete && knownHealth && !raw.operations.some(op=>['unknown','unknown_or_held'].includes(String(op.state)))}} as unknown as SyncStatus;
  }
  return value as unknown as SyncStatus;
}
export interface StatusQueryObservation {data?:SyncStatus; error?:unknown; isError?:boolean; dataUpdatedAt?:number}
const labels:Record<EffectiveHealth,string>={idle:'Idle (stored state)',running:'Running',initializing:'Initializing / baseline pending',busy:'Busy / cleanup may still be active',disabled:'Disabled',paused:'Paused after error',conflict:'Conflict requires attention',held:'Held for reconciliation',error:'Error',unknown:'Unknown'};
export function statusObservationView(query:StatusQueryObservation,now=Date.now()) {
  const status=query.data,observation=status?.observation;
  const code=(query.error as {status?:number}|undefined)?.status;
  const failed=!!query.isError || !!query.error;
  const age=query.dataUpdatedAt ? Math.max(0,now-query.dataUpdatedAt):null;
  const stale=!!status && (failed || age===null || age>45_000);
  const availability=code===404 ? 'Repository missing' : code===403 ? 'Status forbidden' : code===401 ? 'Authentication required'
    : failed ? 'Status unavailable / malformed' : !status ? 'Loading status' : stale ? 'Status observation stale' : 'Status observed';
  const health:EffectiveHealth=failed || stale || !observation ? 'unknown' : observation.operations.some(op=>op.held) ? 'held' : !observation.evidence_complete ? 'unknown' : observation.registry_busy ? 'busy' : observation.health;
  return {status,observation,health,healthLabel:labels[health],availability,stale,failed,
    configured:observation?.configured ?? 'unknown',
    notice:status && (failed || stale) ? 'Last-known data is retained below; it is not a current healthy observation.' : !observation ? 'No supported observation is available. Health, freshness and operation evidence remain unknown.' : 'A status snapshot is not remote-freshness, reconciliation or process-collection proof.',
    metric:(field:'total_syncs'|'active_conflicts'|'total_errors'|'total_conflicts')=>status ? `${status[field]}${stale ? ' (last known)':''}`:'Unknown',
    canAct:!!status && !failed && !stale && !!observation?.evidence_complete && health!=='held' && health!=='unknown',
  };
}
