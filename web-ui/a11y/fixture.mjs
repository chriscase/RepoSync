import { createServer } from 'node:http';

export const syntheticUser = { id:'fixture-admin', enabled:true, username:'fixture-admin', role:'admin', display_name:'Synthetic administrator', email:'fixture@example.invalid', created_at:'2026-11-04T00:00:00Z', updated_at:'2026-11-04T00:00:00Z' };
const stamp=()=>new Date().toISOString();
export function repository(id) { return {id,name:id==='A'?'Synthetic parent':id==='B'?'Synthetic branch':'Disposable fixture',svn_url:'file:///synthetic-only/'+id,svn_branch:id==='B'?'branches/feature':'trunk',svn_username:'fixture',git_provider:'github',git_api_url:'',git_repo:'synthetic/'+id,git_branch:id==='B'?'feature/synthetic':'main',sync_mode:'team',poll_interval_secs:300,lfs_threshold_mb:0,auto_merge:false,enabled:true,allowed_paths:null,blocked_patterns:null,parent_id:id==='B'?'A':null,pair_kind:id==='B'?'branch':'root',created_at:stamp(),updated_at:stamp(),status:'idle',last_svn_revision:null,last_git_hash:null,total_syncs:0,total_errors:0}; }
export function status(repoId) { return {state:'idle',last_sync_at:null,last_svn_revision:null,last_git_hash:null,total_syncs:0,total_conflicts:0,active_conflicts:0,total_errors:0,last_error_at:null,error_observation:{through_id:'0',observed_at:stamp()},uptime_secs:0,observation:{version:1,scope:repoId?'repository':'global',repository_id:repoId||null,configured:'enabled',health:'idle',observed_at:stamp(),data_freshness:'never_recorded',last_successful_sync_at:null,remote_freshness:'not_verified_by_status',registry_busy:false,evidence_complete:true,operations:[],safe_next_action:'Inspect synthetic fixture.',uptime_available:false}}; }
export function progress() { return {phase:'importing',lifecycle:'running',current_rev:21,total_revs:100,commits_created:21,current_file_count:1,lfs_unique_count:0,files_skipped:0,batches_pushed:1,push_started_at:null,verification:null,errors:[],log_lines:['Synthetic log one','Synthetic log two'],started_at:stamp(),completed_at:null,operation_id:'synthetic-operation',busy:true,can_start:false}; }
export const conflict={id:'conflict-1',repo_id:'A',file_path:'example.txt',conflict_type:'content',svn_content:'SVN fixture\n',git_content:'Git fixture\n',base_content:'Base fixture\n',svn_revision:2,git_hash:'a'.repeat(40),status:'pending',resolution:null,resolved_by:null,created_at:'2026-11-04T00:00:00Z',resolved_at:null};

/** Closed, loopback-only API substitute: every write affects only this in-memory fixture. */
export async function startFixture(port=0) {
 const requests=[];let held=false,removeResult='refused',reportedPhase='importing',reportedRevision=21,logGeneration=0,setupActive=true;const pending=new Set(),removed=new Set();
 const activeProgress=()=>({...progress(),phase:reportedPhase,current_rev:reportedRevision,log_lines:[`Synthetic log generation ${logGeneration}`]});
 const server=createServer(async(req,res)=>{
  const url=new URL(req.url,'http://127.0.0.1'),path=url.pathname;let bytes=0,chunks=[];
  for await(const chunk of req){bytes+=chunk.length;if(bytes>65536){res.writeHead(413).end();return;}chunks.push(chunk)}
  let body;try{body=JSON.parse(Buffer.concat(chunks).toString()||'{}')}catch{res.writeHead(400).end();return;}
  const reply=(code,data)=>{if(res.destroyed)return;const json=JSON.stringify(data);res.writeHead(code,{'content-type':'application/json','content-length':Buffer.byteLength(json)});res.end(json)};
  if(path==='/__fixture/state'){if(req.method==='POST'){if('hold' in body)held=!!body.hold;if(['completed','refused'].includes(body.removeResult))removeResult=body.removeResult;if(['connecting','importing','verifying','final_push'].includes(body.phase))reportedPhase=body.phase;if(Number.isSafeInteger(body.current_rev)&&body.current_rev>=0&&body.current_rev<=100)reportedRevision=body.current_rev;if(body.bump_log&&logGeneration<255)logGeneration++;if(typeof body.setup_active==='boolean')setupActive=body.setup_active;}return reply(200,{held,requests})}
  if(path==='/__fixture/release'){held=false;for(const done of pending)done();pending.clear();return reply(200,{ok:true})}
  if(path==='/api/auth/info')return reply(200,{ldap_enabled:false,ldap_domain:null});
  if(path==='/api/auth/login')return body.username==='fixture-admin'&&body.password==='synthetic-only'?reply(200,{token:'synthetic-session-only',user:syntheticUser}):reply(401,{error:'Synthetic login refused'});
  if(req.headers.authorization!=='Bearer synthetic-session-only')return reply(401,{error:'Synthetic session required'});
  requests.push({method:req.method,path,query:Object.fromEntries(url.searchParams),body});
  if(req.method!=='GET'){
   if(held&&path.includes('/repos/'))await new Promise(resolve=>{const timer=setTimeout(done,5000);function done(){clearTimeout(timer);pending.delete(done);resolve()}pending.add(done)});
   if(path.endsWith('/remove')&&removeResult==='completed'){const id=path.split('/')[3];removed.add(id);return reply(200,{ok:true,action:'managed_remove',state:'completed',operation_id:'synthetic-removal-'+id,message:'Synthetic receipt only; no actual repository changed',remote_git:'preserved',remote_svn:'preserved',restore_supported:true,retryable:false,registration_listed:false})}
   if(path.endsWith('/remove')||req.method==='DELETE')return reply(503,{error:'Synthetic cleanup refused; no real repository was changed'});
   return reply(200,{ok:true});
  }
  if(path==='/api/auth/me')return reply(200,syntheticUser);
  if(path==='/api/repos')return reply(200,['A','B','C'].filter(id=>!removed.has(id)).map(repository));
  if(/^\/api\/repos\/[ABC]$/.test(path))return reply(200,repository(path.at(-1)));
  if(path==='/api/status')return reply(200,status(url.searchParams.get('repo_id')));
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
  if(path==='/api/users')return reply(200,[syntheticUser]);
  return reply(404,{error:'No such synthetic fixture resource'});
 });
 await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(port,'127.0.0.1',()=>{server.off('error',reject);resolve()})});
 return {origin:`http://127.0.0.1:${server.address().port}`,requests,close:async()=>{for(const done of pending)done();server.closeAllConnections();await new Promise(resolve=>server.close(resolve))}};
}
