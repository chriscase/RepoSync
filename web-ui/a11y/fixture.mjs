import { createServer } from 'node:http';

export const syntheticUser = { id:'fixture-admin', enabled:true, username:'fixture-admin', role:'admin', display_name:'Synthetic administrator', email:'fixture@example.invalid', created_at:'2026-11-04T00:00:00Z', updated_at:'2026-11-04T00:00:00Z' };
export const syntheticEditors = ['alpha','beta'].map(name=>({...syntheticUser,id:`fixture-${name}`,username:`fixture-${name}`,display_name:'Shared synthetic name',email:'shared@example.invalid',role:'user'}));
const stamp=()=>new Date().toISOString();
export function repository(id) { return {id,name:id==='A'?'Synthetic parent':id==='B'?'Synthetic branch':'Disposable fixture',svn_url:'file:///synthetic-only/'+id,svn_branch:id==='B'?'branches/feature':'trunk',svn_username:'fixture',git_provider:'github',git_api_url:'',git_repo:'synthetic/'+id,git_branch:id==='B'?'feature/synthetic':'main',sync_mode:'team',poll_interval_secs:300,lfs_threshold_mb:0,auto_merge:false,enabled:true,allowed_paths:null,blocked_patterns:null,parent_id:id==='B'?'A':null,pair_kind:id==='B'?'branch':'root',created_at:stamp(),updated_at:stamp(),status:'idle',last_svn_revision:null,last_git_hash:null,total_syncs:0,total_errors:0}; }
export function status(repoId) { return {state:'idle',last_sync_at:null,last_svn_revision:null,last_git_hash:null,total_syncs:0,total_conflicts:0,active_conflicts:0,total_errors:0,last_error_at:null,error_observation:{through_id:'0',observed_at:stamp()},uptime_secs:0,observation:{version:1,scope:repoId?'repository':'global',repository_id:repoId||null,configured:'enabled',health:'idle',observed_at:stamp(),data_freshness:'never_recorded',last_successful_sync_at:null,remote_freshness:'not_verified_by_status',registry_busy:false,evidence_complete:true,operations:[],safe_next_action:'Inspect synthetic fixture.',uptime_available:false}}; }
export function progress() { return {phase:'importing',lifecycle:'running',current_rev:21,total_revs:100,commits_created:21,current_file_count:1,lfs_unique_count:0,files_skipped:0,batches_pushed:1,push_started_at:null,verification:null,errors:[],log_lines:['Synthetic log one','Synthetic log two'],started_at:stamp(),completed_at:null,operation_id:'synthetic-operation',busy:true,can_start:false}; }
export const conflict={id:'conflict-1',repo_id:'A',file_path:'example.txt',conflict_type:'content',svn_content:'SVN fixture\n',git_content:'Git fixture\n',base_content:'Base fixture\n',svn_revision:2,git_hash:'a'.repeat(40),status:'pending',resolution:null,resolved_by:null,created_at:'2026-11-04T00:00:00Z',resolved_at:null};
export const skipContext={pinned_cursor:'a'.repeat(40),observed_remote_tip:'c'.repeat(40),observed_bridge_tip:'c'.repeat(40),pending_commits:[{sha:'b'.repeat(40),subject:'First synthetic pending commit',excluded:false},{sha:'c'.repeat(40),subject:'Second synthetic pending commit',excluded:false}]};

/** Closed, loopback-only API substitute: every write affects only this in-memory fixture. */
export async function startFixture(port=0) {
 const requests=[];let held=false,removeResult='refused',reportedPhase='importing',reportedRevision=21,logGeneration=0,setupActive=true,skipMode='off';const pending=new Set(),removed=new Set();
 const activeProgress=()=>({...progress(),phase:reportedPhase,current_rev:reportedRevision,log_lines:[`Synthetic log generation ${logGeneration}`]});
 const server=createServer(async(req,res)=>{
  const url=new URL(req.url,'http://127.0.0.1'),path=url.pathname;let bytes=0,chunks=[];
  for await(const chunk of req){bytes+=chunk.length;if(bytes>65536){res.writeHead(413).end();return;}chunks.push(chunk)}
  let body;try{body=JSON.parse(Buffer.concat(chunks).toString()||'{}')}catch{res.writeHead(400).end();return;}
  const reply=(code,data)=>{if(res.destroyed)return;const json=JSON.stringify(data);res.writeHead(code,{'content-type':'application/json','content-length':Buffer.byteLength(json)});res.end(json)};
  if(path==='/__fixture/state'){if(req.method==='POST'){if('hold' in body)held=!!body.hold;if(['completed','refused'].includes(body.removeResult))removeResult=body.removeResult;if(['connecting','importing','verifying','final_push'].includes(body.phase))reportedPhase=body.phase;if(Number.isSafeInteger(body.current_rev)&&body.current_rev>=0&&body.current_rev<=100)reportedRevision=body.current_rev;if(body.bump_log&&logGeneration<255)logGeneration++;if(typeof body.setup_active==='boolean')setupActive=body.setup_active;if(['off','eligible','held','unknown'].includes(body.skip_mode))skipMode=body.skip_mode;}return reply(200,{held,requests})}
  if(path==='/__fixture/release'){held=false;for(const done of pending)done();pending.clear();return reply(200,{ok:true})}
  if(path==='/api/auth/info')return reply(200,{ldap_enabled:false,ldap_domain:null});
  if(path==='/api/auth/login')return body.username==='fixture-admin'&&body.password==='synthetic-only'?reply(200,{token:'synthetic-session-only',user:syntheticUser}):reply(401,{error:'Synthetic login refused'});
  if(req.headers.authorization!=='Bearer synthetic-session-only')return reply(401,{error:'Synthetic session required'});
  requests.push({method:req.method,path,query:Object.fromEntries(url.searchParams),body});
  // #65 transport fixture only: no engine, native effect or real account.
  const removalRequestRecord=requests.at(-1);
  const removalRoute=/^\/api\/repos\/([ABC])\/removal\/(review|approve|continue|request)$/.exec(path);
  const removalStatus=/^\/api\/repos\/([ABC])\/removal$/.exec(path);
  const restoreRoute=/^\/api\/repos\/([ABC])\/restore$/.exec(path);
  const restored=id=>requests.some(r=>r.syntheticRemovalRestored===id);
  const generation=id=>requests.filter(r=>r.syntheticRemovalRestored===id).length;
  const pinFor=(id,git,svn)=>(id.charCodeAt(0).toString(16)+Number(git)+Number(svn)+generation(id).toString(16).padStart(8,'0')).padEnd(64,'a');
  const retained=id=>requests.filter(r=>r.syntheticRemovalReceipt?.repo_id===id).map(r=>r.syntheticRemovalReceipt);
  const currentReceipt=receipt=>({...receipt,registration_listed:!removed.has(receipt.repo_id)});
  if(removalRoute){
   const [,id,action]=removalRoute;
   if(action==='review'&&req.method==='POST'){
    if(removed.has(id))return reply(404,{error:'Synthetic registration was removed'});
    const git=body.delete_git??false,svn=body.delete_svn??false;
    if(typeof git!=='boolean'||typeof svn!=='boolean'||((git||svn)&&id!=='B'))return reply(409,{error:'Synthetic exact choices refused'});
    const repo=repository(id);
    return reply(200,{ok:true,protocol:2,repo_id:id,review_pin:pinFor(id,git,svn),delete_git:git,delete_svn:svn,
     git:git?{remote_url:`https://git.example.invalid/synthetic/${id}.git`,reference:`refs/heads/${repo.git_branch}`,expected_sha:'a'.repeat(40)}:null,
     svn:svn?{root_url:repo.svn_url,uuid:'synthetic-uuid-'+id,url:repo.svn_url+'/'+repo.svn_branch,base_revision:7,expected_present:true}:null,
     dependency_preview:{repo_id:id,repo_name:repo.name,parent:repo.parent_id?{id:repo.parent_id,name:repository(repo.parent_id).name}:null,children:[],credentials:[],managed_local_path:'/synthetic-only/'+id,sibling_local_paths_preserved:[],shared_git_registrations:[],parent_removal_blocked:false,block_reason:null},
     retention:'Synthetic fixture only. Retained recovery; restore stays disabled and does not recreate remotes.'});
   }
   if(action==='approve'&&req.method==='POST'){
    const previous=retained(id).find(r=>r.request_id===body.request_id&&r.review_pin===body.review_pin);
    if(previous)return reply(200,currentReceipt(previous));
    if(typeof body.delete_git!=='boolean'||typeof body.delete_svn!=='boolean'||body.review_pin!==pinFor(id,body.delete_git,body.delete_svn)||body.request_id!==`ui-remove-v2-${body.review_pin}`||((body.delete_git||body.delete_svn)&&id!=='B')||removed.has(id))return reply(409,{request_rejected:true,error:'Synthetic exact review refused'});
    if(held)await new Promise(resolve=>{const timer=setTimeout(done,5000);function done(){clearTimeout(timer);pending.delete(done);resolve()}pending.add(done)});
    if(removeResult!=='completed')return reply(503,{error:'Synthetic cleanup refused; no real repository was changed'});
    const side=(requested,name)=>({phase:requested?'verified_deleted':'not_requested',operation_marker:`synthetic-removal-${id}:${name}`,errors:[]});
    const receipt={ok:true,action:'managed_remove',protocol:2,repo_id:id,state:'completed',operation_id:`synthetic-removal-${id}-${generation(id)}`,request_id:body.request_id,review_pin:body.review_pin,updated_at:stamp(),message:'Synthetic receipt only; no actual repository changed',remote_git:body.delete_git?'deleted':'untouched',remote_svn:body.delete_svn?'deleted':'untouched',restore_supported:true,retryable:false,registration_listed:false,
     reviewed:{git:side(body.delete_git,'git'),svn:side(body.delete_svn,'svn'),cleanup_started:true,cleanup_completed:true,cleanup_errors:[]},
     recovery:{operation_id:`synthetic-removal-${id}-${generation(id)}`,name:repository(id).name,last_svn_rev:0,last_git_sha:'',commit_map_count:0,remote_git:body.delete_git?'deleted':'untouched',remote_svn:body.delete_svn?'deleted':'untouched',restore_supported:true,retention:'Synthetic retained recovery'}};
    removalRequestRecord.syntheticRemovalReceipt=receipt;removed.add(id);return reply(200,receipt);
   }
   if(action==='request'&&req.method==='GET'){
    const receipt=retained(id).find(r=>r.request_id===url.searchParams.get('request_id')&&r.review_pin===url.searchParams.get('review_pin'));
    return receipt?reply(200,currentReceipt(receipt)):reply(404,{error:'No matching synthetic operation receipt'});
   }
   if(action==='continue'&&req.method==='POST'){
    const receipt=retained(id).find(r=>r.operation_id===body.operation_id);
    return receipt?reply(200,currentReceipt(receipt)):reply(404,{error:'No synthetic operation to continue'});
   }
  }
  if(removalStatus&&req.method==='GET'){
   const receipts=retained(removalStatus[1]);const receipt=url.searchParams.has('operation_id')?receipts.find(r=>r.operation_id===url.searchParams.get('operation_id')):receipts.at(-1);
   return receipt?reply(200,currentReceipt(receipt)):reply(404,{error:'No synthetic removal receipt'});
  }
  if(restoreRoute&&req.method==='POST'){
   const id=restoreRoute[1];if(!removed.has(id))return reply(200,{ok:true,action:'managed_restore',state:'already_listed',repo_id:id,enabled:!restored(id),message:'Synthetic registration already exists'});
   removalRequestRecord.syntheticRemovalRestored=id;removed.delete(id);return reply(200,{ok:true,action:'managed_restore',state:'restored',repo_id:id,enabled:false,message:'Synthetic registration restored disabled; no actual data changed'});
  }
  if(req.method==='GET'&&/^\/api\/repos\/[ABC]$/.test(path)){
   const id=path.at(-1);if(removed.has(id))return reply(404,{error:'Synthetic repository not found'});if(restored(id))return reply(200,{...repository(id),enabled:false});
  }
  if(req.method==='GET'&&path==='/api/repos'&&['A','B','C'].some(restored))return reply(200,['A','B','C'].filter(id=>!removed.has(id)).map(id=>({...repository(id),enabled:!restored(id)})));
  if(req.method==='GET'&&path==='/api/status'&&restored(url.searchParams.get('repo_id'))){const value=status(url.searchParams.get('repo_id'));value.observation.configured='disabled';return reply(200,value);}
  if(req.method!=='GET'){
   if(held&&(path.includes('/repos/')||path.startsWith('/api/users/')))await new Promise(resolve=>{const timer=setTimeout(done,5000);function done(){clearTimeout(timer);pending.delete(done);resolve()}pending.add(done)});
   if(path==='/api/admin/ldap/test')return reply(200,{ok:false,message:'Synthetic directory test refused'});
   if(path==='/api/admin/ldap')return reply(503,{error:'Synthetic directory save refused'});
   if(path==='/api/config/notifications/test')return reply(200,{ok:false,error:'Synthetic notification test refused'});
   if(path==='/api/config/notifications')return reply(503,{error:'Synthetic notification save refused'});
   if(req.method==='PUT'&&syntheticEditors.some(u=>path===`/api/users/${u.id}`))return reply(200,{...syntheticEditors.find(u=>path===`/api/users/${u.id}`),...body});
   if(path==='/api/repos/A/skip-commit'){
    if(skipMode!=='eligible'||body.pinned_cursor!==skipContext.pinned_cursor||body.expected_remote_tip!==skipContext.observed_remote_tip||body.expected_bridge_tip!==skipContext.observed_bridge_tip||JSON.stringify(body.selected_commits)!==JSON.stringify([skipContext.pending_commits[0].sha]))return reply(409,{error:'Synthetic exact-tip selection refused'});
    return reply(200,{ok:true,message:'Synthetic exact selection only',old_sha:skipContext.pinned_cursor,new_sha:skipContext.pending_commits[0].sha,excluded_commits:body.selected_commits,remaining_pending:[skipContext.pending_commits[1].sha]});
   }
   if(path.endsWith('/remove')&&removeResult==='completed'){const id=path.split('/')[3];removed.add(id);return reply(200,{ok:true,action:'managed_remove',state:'completed',operation_id:'synthetic-removal-'+id,message:'Synthetic receipt only; no actual repository changed',remote_git:'preserved',remote_svn:'preserved',restore_supported:true,retryable:false,registration_listed:false})}
   if(path.endsWith('/remove')||req.method==='DELETE')return reply(503,{error:'Synthetic cleanup refused; no real repository was changed'});
   return reply(200,{ok:true});
  }
  if(path==='/api/auth/me')return reply(200,syntheticUser);
  if(path==='/api/repos')return reply(200,['A','B','C'].filter(id=>!removed.has(id)).map(repository));
  if(/^\/api\/repos\/[ABC]$/.test(path))return reply(200,repository(path.at(-1)));
  if(path==='/api/status'){
   const repoId=url.searchParams.get('repo_id'),value=status(repoId);
   if(repoId==='A'&&skipMode!=='off'){
    value.state='error_paused';value.observation.health=skipMode==='eligible'?'paused':skipMode;value.observation.evidence_complete=skipMode!=='unknown';
    if(skipMode==='held')value.observation.operations=[{repository_id:'A',kind:'import',state:'reconciliation_required',operation_id:'synthetic-held-operation',updated_at:stamp(),reason:'Synthetic held evidence',target_ref:'refs/heads/main',intended_git_sha:'a'.repeat(40),confirmed_svn_revision:null,held:true,details_visible:true}];
   }
   return reply(200,value);
  }
  if(path==='/api/repos/A/skip-commit/context')return reply(200,{ok:true,context:skipContext});
  if(path==='/api/status/health')return reply(200,{ok:true});
  if(path==='/api/status/system')return reply(200,{disk_free_bytes:100,disk_total_bytes:200,disk_usage_percent:50,mem_used_bytes:100,mem_total_bytes:200,mem_usage_percent:50,cpu_load_1m:0,cpu_load_5m:0,cpu_load_15m:0,git_push_active:false,git_push_pid:null,git_push_elapsed_secs:null,data_dir_size_bytes:0,net_bytes_sent:0,net_bytes_recv:0,net_up_bytes_per_sec:0,net_down_bytes_per_sec:0,svn_active:false});
  if(path.startsWith('/api/users/')&&path.endsWith('/credentials'))return reply(200,[]);
  if(path==='/api/admin/ldap')return reply(200,{enabled:false,url:'ldaps://directory.example.invalid',base_dn:'dc=example,dc=invalid',search_filter:'(uid={username})',display_name_attr:'displayName',email_attr:'mail',group_attr:'memberOf',bind_dn:'',bind_password_set:false});
  if(path==='/api/config/notifications')return reply(200,{teams_webhook_url:''});
  if(path.endsWith('/credentials'))return reply(200,{svn_password_set:false,git_token_set:false});
  if(path.endsWith('/branches'))return reply(200,path.includes('/A/')?[repository('B')]:[]);
  if(path.endsWith('/import/status'))return reply(200,(path.startsWith('/api/setup/')&&setupActive)||path.includes('/A/')?activeProgress():{...activeProgress(),phase:'idle',lifecycle:undefined,current_rev:0,total_revs:0,commits_created:0,log_lines:[],started_at:null,busy:false,can_start:true,operation_id:undefined});
  if(path.endsWith('/removal/preview')){const id=path.split('/')[3];return reply(200,{ok:true,dependency_preview:{repo_id:id,children:[],credentials:[],managed_local_path:'/synthetic-only/'+id,sibling_local_paths_preserved:[],shared_git_registrations:[],parent_removal_blocked:false,block_reason:null}})}
  if(path==='/api/conflicts/conflict-1')return reply(200,conflict);
  if(path==='/api/conflicts')return reply(200,{items:[conflict],total:1,page:1,per_page:Number(url.searchParams.get('per_page')||50),total_pages:1});
  if(path==='/api/audit')return reply(200,{entries:[],total:0,page:1,per_page:Number(url.searchParams.get('limit')||50),total_pages:0,offset:0});
  if(path==='/api/commit-map'||path==='/api/sync-records')return reply(200,{entries:[],total:0});
  if(path==='/api/config/identity')return reply(200,[]);
  if(path==='/api/users')return reply(200,[syntheticUser,...syntheticEditors]);
  return reply(404,{error:'No such synthetic fixture resource'});
 });
 await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(port,'127.0.0.1',()=>{server.off('error',reject);resolve()})});
 return {origin:`http://127.0.0.1:${server.address().port}`,requests,close:async()=>{for(const done of pending)done();server.closeAllConnections();await new Promise(resolve=>server.close(resolve))}};
}
