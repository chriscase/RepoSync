import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import Repositories from './pages/Repositories';
import { StatusObservationPanel } from './components/StatusObservation';
import { api, ApiError, type Repository, type SyncStatus } from './api';
import { errorStatusQuery } from './errorStatus';
import { decodeSyncStatus, statusObservationView, type StatusOperation } from './statusObservation';
import { canConfirmObservedSkip, runObservedSkip, latestScopedSkipObservation, type ScopedSkipObservation } from './statusActions';
const stamp='2026-11-03T20:00:00Z',now=Date.parse(stamp);
const held:StatusOperation={repository_id:'a',kind:'import',state:'reconciliation_required',operation_id:'op-a',updated_at:stamp,reason:'held publication',target_ref:'refs/heads/main',intended_git_sha:'a'.repeat(40),confirmed_svn_revision:null,held:true,details_visible:true};
function sample(id='a'):SyncStatus {return {state:'error_paused',last_sync_at:null,last_svn_revision:null,last_git_hash:null,total_syncs:12,total_conflicts:0,active_conflicts:0,total_errors:1,last_error_at:null,error_observation:{through_id:'1',observed_at:stamp},uptime_secs:0,observation:{version:1,scope:'repository',repository_id:id,configured:'enabled',health:'paused',observed_at:stamp,data_freshness:'never_recorded',last_successful_sync_at:null,remote_freshness:'not_verified_by_status',registry_busy:false,evidence_complete:true,operations:[],safe_next_action:'Inspect scoped evidence',uptime_available:false}};}
function repo(id:string,parent:string|null=null):Repository {return {id,name:id,svn_url:'svn://fixture.invalid/project',svn_branch:'trunk',git_repo:'fixture/repo',git_branch:'main',enabled:true,parent_id:parent,updated_at:stamp,created_at:stamp} as Repository;}
const fetchMock=vi.fn();let client:QueryClient;
beforeEach(()=>{
  client=new QueryClient({defaultOptions:{queries:{retry:false,staleTime:Infinity}}});
  const entries=new Map([['session_token','fixture-token'],['user','{"id":"actor","role":"admin"}']]);
  vi.stubGlobal('localStorage',{getItem:(key:string)=>entries.get(key) ?? null,setItem:(key:string,v:string)=>entries.set(key,v),removeItem:(key:string)=>entries.delete(key),get length(){return entries.size;},key:(index:number)=>Array.from(entries.keys())[index] ?? null});
  vi.stubGlobal('window',{location:{pathname:'/repos',href:'/repos'}});vi.stubGlobal('fetch',fetchMock);fetchMock.mockReset();
});
afterEach(()=>{client.clear();vi.unstubAllGlobals();});
function renderOverview(){return renderToStaticMarkup(createElement(QueryClientProvider,{client},createElement(MemoryRouter,{initialEntries:['/repos']},createElement(Repositories))));}
describe('RS116-1 actual overview parent-card composition',()=>{
  it.each([false,true])('renders the parent own paused/held status when children=%s',children=>{
    const parent=repo('parent'),child=repo('child','parent');client.setQueryData(['repos'],children ? [parent,child]:[parent]);
    const parentStatus=sample('parent');if(children){parentStatus.observation!.health='held';parentStatus.observation!.operations=[{...held,repository_id:'parent',operation_id:'parent-only-operation'}];client.setQueryData(errorStatusQuery('child').queryKey,{...sample('child'),observation:{...sample('child').observation!,health:'running'}});}
    client.setQueryData(errorStatusQuery('parent').queryKey,parentStatus);
    const html=renderOverview();expect(html).toContain(children ? 'Held for reconciliation':'Paused after error');expect(html).toContain('aria-live="polite"');expect(html).toContain('Syncs: 12');expect(html).not.toContain('>Active<');if(children)expect(html).toContain('parent-only-operation');
  });
  it('actually creates the childless parent scoped query and announces unknown loading rather than timestamp/zero fallback',()=>{
    client.setQueryData(['repos'],[repo('parent-loading')]);const html=renderOverview();
    expect(client.getQueryCache().find({queryKey:errorStatusQuery('parent-loading').queryKey,exact:true})).toBeDefined();expect(html).toContain('Loading status');expect(html).toContain('Syncs: Unknown');expect(html).not.toContain('0 syncs');
  });
  it('shows a parent failed-refetch status and last-known counters in the actual overview',async()=>{
    client.setQueryData(['repos'],[repo('parent-error'),repo('child','parent-error')]);client.setQueryData(errorStatusQuery('parent-error').queryKey,sample('parent-error'));
    fetchMock.mockResolvedValueOnce(new Response('{"error":"forbidden"}',{status:403}));await expect(client.fetchQuery({...errorStatusQuery('parent-error'),staleTime:0})).rejects.toThrow();
    const html=renderOverview();expect(html).toContain('Status forbidden');expect(html).toContain('12 (last known)');expect(html).toContain('Unknown');
  });
});
describe('RS116-2 the actual Skip entry/confirmation policy',()=>{
  const payload={pinned_cursor:'a'.repeat(40),selected_commits:['b'.repeat(40)],expected_remote_tip:'c'.repeat(40),reason:'operator_exact_skip'};
  it.each(['failed','aged','held'] as const)('an already-open selected interaction sends zero requests after status becomes %s',transition=>{
    let state:ScopedSkipObservation={repoId:'a',detailLive:true,administrator:true,query:{data:sample(),dataUpdatedAt:now}};
    expect(canConfirmObservedSkip(state,'a',now)).toBe(true);const selected=[...payload.selected_commits];expect(selected).toHaveLength(1);
    if(transition==='failed')state={...state,query:{...state.query,error:new ApiError(500,'unavailable'),isError:true}};
    if(transition==='aged')state={...state,query:{...state.query,dataUpdatedAt:now-46_000}};
    if(transition==='held'){const data=sample();data.observation!.health='held';data.observation!.operations=[held];state={...state,query:{data,dataUpdatedAt:now}};}
    expect(canConfirmObservedSkip(state,'a',now)).toBe(false);
    expect(()=>runObservedSkip(()=>state,'a',()=>api.skipCommit('a',payload),now)).toThrow('no request was sent');expect(fetchMock).not.toHaveBeenCalled();
  });
  it('preserves the eligible exact-tip API request and refuses cross-scope/admin loss',async()=>{
    const state:ScopedSkipObservation={repoId:'a',detailLive:true,administrator:true,query:{data:sample(),dataUpdatedAt:now}};
    fetchMock.mockResolvedValueOnce(new Response('{"ok":true}'));await runObservedSkip(()=>state,'a',()=>api.skipCommit('a',payload),now);
    expect(fetchMock).toHaveBeenCalledTimes(1);expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual(payload);
    for(const changed of [{...state,repoId:'b'},{...state,administrator:false},{...state,detailLive:false}])expect(()=>runObservedSkip(()=>changed,'a',()=>api.skipCommit('a',payload),now)).toThrow();expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
describe('RS116-5 checked-client nested lifecycle/evidence refusal',()=>{
  const invalid=[
    {...held,repository_id:'foreign'}, {...held,kind:'unrecognized'}, {...held,state:'future_state'},
    {...held,confirmed_svn_revision:-1}, {...held,intended_git_sha:'not-an-oid'}, {...held,state:'reconciliation_required',held:false},
    {...held,kind:'late_pair',target_evidence:{kind:'late_pair',pinned_git_tip:'bad'}},
    {...held,kind:'svn_commit',target_evidence:{kind:'svn_commit',target_svn_uuid:'fixture',target_svn_branch_path:'trunk',source_git_sha:'a'.repeat(40),source_git_parent:null,source_git_tree:'b'.repeat(40),pre_write_svn_revision:4,pre_write_svn_tree:'not-a-tree-fingerprint',intended_svn_tree:'f'.repeat(64)}},
  ];
  it.each(invalid)('does not cache a foreign/unsupported 200 lifecycle as healthy: %j',async operation=>{
    const original=sample();client.setQueryData(errorStatusQuery('a').queryKey,original);
    const bad=sample();bad.observation!.health='idle';bad.observation!.operations=[operation as StatusOperation];fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(bad)));
    await expect(client.fetchQuery({...errorStatusQuery('a'),staleTime:0})).rejects.toThrow('Malformed');const q=client.getQueryState<SyncStatus>(errorStatusQuery('a').queryKey)!;
    expect(q.data).toEqual(original);const view=statusObservationView({data:q.data,error:q.error,isError:true,dataUpdatedAt:q.dataUpdatedAt});expect(view.health).toBe('unknown');expect(view.canAct).toBe(false);expect(view.metric('total_syncs')).toBe('12 (last known)');
  });
  it('retains valid aggregate multiple scopes and restricted null-private evidence',()=>{
    const global=sample();global.observation={...global.observation!,scope:'aggregate',repository_id:null,health:'held',operations:[held,{...held,repository_id:'b',operation_id:null,target_ref:null,intended_git_sha:null,reason:'Restricted evidence',details_visible:false}]};
    expect(decodeSyncStatus(global).observation!.operations).toHaveLength(2);expect(statusObservationView({data:decodeSyncStatus(global),dataUpdatedAt:now},now).canAct).toBe(false);
  });
  it('downgrades supported unknown evidence even if a malformed producer claims complete idle',()=>{
    const value=sample();value.observation={...value.observation!,health:'idle',operations:[{...held,state:'unknown',held:false,operation_id:null,target_ref:null,intended_git_sha:null,details_visible:false}]};
    const decoded=decodeSyncStatus(value,'a');expect(decoded.observation!.evidence_complete).toBe(false);expect(statusObservationView({data:decoded,dataUpdatedAt:now},now).health).toBe('unknown');
  });
});
describe('RS116-6 actual target evidence display',()=>{
  it('labels late-pair pinned/baseline/copy values without claiming publication confirmation',()=>{
    const data=sample();data.observation!.health='held';data.observation!.operations=[{...held,kind:'late_pair',target_ref:'recorded-feature',target_evidence:{kind:'late_pair',target_git_branch:'recorded-feature',target_svn_branch:'branches/recorded',pinned_git_tip:'d'.repeat(40),baseline_git_sha:'b'.repeat(40),baseline_svn_revision:5,copy_source_svn_revision:4,copy_source_svn_path:'trunk',observed_svn_branch_head_revision:null,replayed_git_shas:[]}}];
    const decoded=decodeSyncStatus(data,'a'),html=renderToStaticMarkup(createElement(StatusObservationPanel,{view:statusObservationView({data:decoded,dataUpdatedAt:now},now)}));
    expect(html).toContain('Stored target Git branch: recorded-feature');expect(html).toContain('Pinned Git tip: '+'d'.repeat(40));expect(html).toContain('Copy source SVN revision: 4');expect(html).toContain('not final publication confirmation');
  });
  it('labels SVN source commit and pre-write/intent separately from confirmed publication',()=>{
    const data=sample();data.observation!.health='held';data.observation!.operations=[{...held,kind:'svn_commit',target_ref:'branches/recorded',intended_git_sha:null,target_evidence:{kind:'svn_commit',target_svn_uuid:'fixture-uuid',target_svn_branch_path:'branches/recorded',source_git_sha:'c'.repeat(40),source_git_parent:null,source_git_tree:'d'.repeat(40),pre_write_svn_revision:4,pre_write_svn_tree:'e'.repeat(64),intended_svn_tree:'f'.repeat(64)}}];
    const html=renderToStaticMarkup(createElement(StatusObservationPanel,{view:statusObservationView({data:decodeSyncStatus(data,'a'),dataUpdatedAt:now},now)}));expect(html).toContain('branch path: branches/recorded');expect(html).toContain('Source Git commit: '+'c'.repeat(40));expect(html).toContain('Source/intent is not confirmed publication');
  });
});

describe('RS116-2 mutation entry re-reads actual cache before a React refetch render',()=>{
  it.each(['failed','aged','held','evicted'] as const)('refuses %s evidence even when the editor still holds the older eligible render',transition=>{
    const data=sample();client.setQueryData(errorStatusQuery('a').queryKey,data,{updatedAt:now});
    const rendered:ScopedSkipObservation={repoId:'a',detailLive:true,administrator:true,query:{data,dataUpdatedAt:now}};
    expect(canConfirmObservedSkip(rendered,'a',now)).toBe(true);
    if(transition==='failed')client.getQueryCache().find({queryKey:errorStatusQuery('a').queryKey,exact:true})!.setState({status:'error',error:new ApiError(500,'refetch failed')});
    if(transition==='aged')client.setQueryData(errorStatusQuery('a').queryKey,data,{updatedAt:now-46_000});
    if(transition==='held'){const updated=sample();updated.observation!.health='held';updated.observation!.operations=[held];client.setQueryData(errorStatusQuery('a').queryKey,updated,{updatedAt:now});}
    if(transition==='evicted')client.removeQueries({queryKey:errorStatusQuery('a').queryKey,exact:true});
    expect(canConfirmObservedSkip(rendered,'a',now)).toBe(true); // Deliberately stale rendered state remains eligible.
    const read=()=>latestScopedSkipObservation(client,rendered);
    expect(canConfirmObservedSkip(read(),'a',now)).toBe(false);expect(()=>runObservedSkip(read,'a',()=>api.skipCommit('a',{pinned_cursor:'a'.repeat(40),selected_commits:['b'.repeat(40)],expected_remote_tip:'c'.repeat(40),reason:'operator_exact_skip'}),now)).toThrow('no request was sent');expect(fetchMock).not.toHaveBeenCalled();
  });
});
