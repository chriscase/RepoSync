import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { QueryClient } from '@tanstack/react-query';
import { type SyncStatus } from './api';
import { errorStatusQuery, errorStatusView } from './errorStatus';
import { StatusObservationPanel } from './components/StatusObservation';
import { decodeSyncStatus, statusObservationView, type StatusObservation, type StatusOperation } from './statusObservation';
import { syncStateVisual } from './utils/syncStateColor';
const observed='2026-11-03T20:00:00Z',now=Date.parse(observed);
const operation:StatusOperation={repository_id:'a',kind:'import',state:'reconciliation_required',operation_id:'operation-a',updated_at:observed,reason:'Publication response lost',target_ref:'refs/heads/main',intended_git_sha:'a'.repeat(40),confirmed_svn_revision:7,held:true,details_visible:true};
function sample(override:Partial<StatusObservation>={}):SyncStatus {
  return {state:'idle',last_sync_at:null,last_svn_revision:null,last_git_hash:null,total_syncs:12,total_conflicts:0,active_conflicts:0,total_errors:2,last_error_at:null,error_observation:{through_id:'2',observed_at:observed},uptime_secs:0,
    observation:{version:1,scope:'repository',repository_id:'a',configured:'enabled',health:'idle',observed_at:observed,data_freshness:'never_recorded',last_successful_sync_at:null,remote_freshness:'not_verified_by_status',registry_busy:false,evidence_complete:true,operations:[],safe_next_action:'Inspect evidence. Skip and Retry do not prove reconciliation or release holds.',uptime_available:false,...override}};
}
function markup(data:SyncStatus|undefined,error?:unknown,updatedAt=now){return renderToStaticMarkup(createElement(StatusObservationPanel,{view:statusObservationView({data,error,isError:!!error,dataUpdatedAt:updatedAt},now)}));}
describe('#116 shared truthful observation and accessible text',()=>{
  it.each([
    ['paused','Paused after error'],['busy','Busy / cleanup may still be active'],['running','Running'],['initializing','Initializing / baseline pending'],['disabled','Disabled'],['held','Held for reconciliation'],['unknown','Unknown'],['conflict','Conflict requires attention'],['error','Error'],
  ] as const)('renders %s separately from configured enabled with accessible announcements', (health,label)=>{
    const data=sample({health});const html=markup(data);expect(html).toContain(label);expect(html).toContain('Sync configuration: enabled');expect(html).toContain('role="status"');expect(html).toContain('aria-live="polite"');expect(html).toContain('aria-atomic="true"');expect(html).toContain('Remote contents: not verified');expect(html).not.toContain('>Active<');
  });
  it('never treats cancelled lifecycle or idle registry as release of a durable publication hold',()=>{
    const data=sample({health:'idle',operations:[{...operation,state:'cancelled'}]});const view=statusObservationView({data,dataUpdatedAt:now},now),html=markup(data);
    expect(view.health).toBe('held');expect(view.canAct).toBe(false);expect(html).toContain('cancelled (hold retained)');expect(html).toContain('operation-a');expect(html).toContain('refs/heads/main');expect(html).toContain('do not prove reconciliation');
  });
  it('retains explicit rewritten-history tips without implying a new graph verification',()=>{
    const html=markup(sample({health:'held',operations:[{...operation,kind:'history',operation_id:null,reason:'observed_remote_rewrite',history_evidence:{handled_checkpoint:'a'.repeat(40),fresh_observed_remote_tip:'b'.repeat(40)}}]}));
    expect(html).toContain('observed_remote_rewrite');expect(html).toContain('handled_checkpoint: '+ 'a'.repeat(40));expect(html).toContain('fresh_observed_remote_tip: '+ 'b'.repeat(40));expect(html).toContain('Unavailable / restricted');
  });
  it('shows missing initial metrics as unknown and never substitutes configuration updated_at',()=>{
    const html=markup(undefined);expect(html).toContain('Loading status');expect(html).toContain('Syncs: Unknown');expect(html).toContain('Last successful sync: Unknown');expect(html).not.toContain('0 syncs');expect(html).not.toContain('Active');expect(syncStateVisual('unrecognized')).toBe('unknown');expect(syncStateVisual(undefined)).toBe('unknown');
  });
  it('keeps aged snapshots as last-known data and makes effective health unknown',()=>{
    const data=sample();const view=statusObservationView({data,dataUpdatedAt:now-46_000},now);
    expect(view.health).toBe('unknown');expect(view.metric('total_syncs')).toBe('12 (last known)');expect(view.availability).toContain('stale');expect(view.canAct).toBe(false);
  });
  it('falls back unknown for legacy or future enum evidence instead of inventing a healthy schema',()=>{
    const {observation:_,...legacy}=sample();void _;
    expect(statusObservationView({data:decodeSyncStatus(legacy,'a'),dataUpdatedAt:now},now).health).toBe('unknown');
    const future=sample();(future.observation as unknown as {health:string}).health='new_health';const decoded=decodeSyncStatus(future,'a');expect(decoded.observation?.health).toBe('unknown');expect(decoded.observation?.evidence_complete).toBe(false);
  });
  it.each([{}, {error:'forbidden'}, {...sample(),total_syncs:null}, {...sample(),state:42}, {...sample(),last_sync_at:'not a date'}, {...sample(),observation:{...sample().observation,repository_id:'foreign'}}, {...sample(),observation:{...sample().observation,operations:[{...operation,history_evidence:{fresh_observed_remote_tip:'not-an-object-id'}}]}}])('rejects malformed or foreign 200 responses without zero/healthy defaults: %j',value=>{
    expect(()=>decodeSyncStatus(value,'a')).toThrow('Malformed');
  });
});

const fetchMock=vi.fn();let client:QueryClient;
beforeEach(()=>{client=new QueryClient({defaultOptions:{queries:{retry:false}}});vi.stubGlobal('fetch',fetchMock);vi.stubGlobal('localStorage',{getItem:()=> 'fixture-token',removeItem:()=>{}});vi.stubGlobal('window',{location:{pathname:'/repos/a',href:'/repos/a'}});fetchMock.mockReset();});
afterEach(()=>{client.clear();vi.unstubAllGlobals();});
describe('#116 central checked client and actual shared cache',()=>{
  it.each([403,404,500])('retains last known data after HTTP %s but disables acknowledgement and healthy/action states',async code=>{
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(sample())));await client.fetchQuery(errorStatusQuery('a'));
    fetchMock.mockResolvedValueOnce(new Response('{"error":"not status"}',{status:code}));await expect(client.fetchQuery({...errorStatusQuery('a'),staleTime:0})).rejects.toThrow();
    const state=client.getQueryState<SyncStatus>(errorStatusQuery('a').queryKey)!;
    expect(state.data?.total_syncs).toBe(12);const view=statusObservationView({data:state.data,error:state.error,isError:true,dataUpdatedAt:state.dataUpdatedAt});
    expect(view.health).toBe('unknown');expect(view.metric('total_syncs')).toBe('12 (last known)');expect(view.canAct).toBe(false);expect(errorStatusView(state.data,true).observation).toBeUndefined();
    const html=markup(state.data,state.error);expect(html).toContain(code===403 ? 'Status forbidden':code===404 ? 'Repository missing':'Status unavailable');expect(html).toContain('last known');
  });
  it('does not replace a good snapshot with malformed JSON or a transport failure',async()=>{
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(sample())));await client.fetchQuery(errorStatusQuery('a'));
    for(const response of [()=>fetchMock.mockResolvedValueOnce(new Response('{"error":"bad 200"}')),()=>fetchMock.mockRejectedValueOnce(new TypeError('offline'))]){
      response();await expect(client.fetchQuery({...errorStatusQuery('a'),staleTime:0})).rejects.toThrow();expect(client.getQueryData<SyncStatus>(errorStatusQuery('a').queryKey)?.total_syncs).toBe(12);
    }
  });
});
