import type { AuditListResponse, ConflictPage } from './api';

export interface ConflictFilters {page:number;perPage:number;repoId?:string;status?:string;error?:string}
export interface AuditFilters {page:number;perPage:number;repoId?:string;success?:boolean;action?:string;error?:string}
function scope(params:URLSearchParams) {
  const raw=params.get('page');const page=raw ? Number(raw):1;
  const valid=(!raw || /^[1-9][0-9]*$/.test(raw)) && Number.isSafeInteger(page) && page<=0xffffffff;
  return {page:valid ? page:1,repoId:params.get('repo_id') || undefined,
    error:!valid ? 'Invalid page. Return to the first page.':params.has('repo_id') && !params.get('repo_id') ? 'Repository scope is empty. Choose a repository or all repositories.':undefined};
}
export function conflictFilters(params:URLSearchParams):ConflictFilters {return {...scope(params),perPage:20,status:params.get('status') || undefined};}
export function auditFilters(params:URLSearchParams):AuditFilters {
  const raw=params.get('success');const base=scope(params);
  return {...base,perPage:50,success:raw==='true' ? true:raw==='false' ? false:undefined,action:params.get('action') || undefined,
    error:base.error || (raw!==null && raw!=='' && raw!=='true' && raw!=='false' ? 'Invalid success filter. Choose all, success or failures.':undefined)};
}
/** One URL update resets the page before a changed scope/filter can be requested. */
export function changeListFilter(params:URLSearchParams,key:string,value:string):URLSearchParams {
  const next=new URLSearchParams(params);if(value)next.set(key,value);else next.delete(key);next.set('page','1');return next;
}
export function changeListPage(params:URLSearchParams,page:number):URLSearchParams {const next=new URLSearchParams(params);next.set('page',String(Math.max(1,page)));return next;}
export function conflictPageKey(filters:ConflictFilters) {return ['conflicts','page',filters] as const;}
export function auditPageKey(filters:AuditFilters) {return ['audit','page',filters] as const;}
export function dashboardListLink(path:'/audit'|'/conflicts',repoId?:string,extra:Record<string,string>={}):string {
  const params=new URLSearchParams(extra);if(repoId)params.set('repo_id',repoId);const qs=params.toString();return path+(qs ? '?'+qs:'');
}
const object=(v:unknown):v is Record<string,unknown>=>!!v && typeof v==='object' && !Array.isArray(v);
const count=(v:unknown)=>Number.isSafeInteger(v) && Number(v)>=0;
const text=(v:unknown)=>typeof v==='string';
const nullableText=(v:unknown)=>v===null || text(v);
const nullableCount=(v:unknown)=>v===null || count(v);
function metadata(value:Record<string,unknown>,length:number,page:number,perPage:number):boolean {
  const offset=(page-1)*perPage;
  return count(value.total) && value.page===page && value.per_page===perPage && value.total_pages===Math.ceil(Number(value.total)/perPage)
    && length===Math.min(perPage,Math.max(0,Number(value.total)-offset));
}
export function decodeConflictPage(value:unknown,filters:ConflictFilters):ConflictPage {
  if(!object(value) || !Array.isArray(value.items) || !metadata(value,value.items.length,filters.page,Math.min(100,filters.perPage))
    || !value.items.every(row=>object(row) && ['id','file_path','conflict_type','status','created_at'].every(k=>text(row[k]))
      && nullableText(row.repo_id) && nullableText(row.git_hash) && nullableCount(row.svn_revision) && nullableText(row.resolved_at)
      && (!filters.repoId || row.repo_id===filters.repoId) && (!filters.status || row.status===filters.status))
    || new Set(value.items.map(row=>row.id)).size!==value.items.length)throw new Error('Malformed conflict page; matching results are unknown.');
  return value as unknown as ConflictPage;
}
export function decodeAuditPage(value:unknown,filters:AuditFilters):AuditListResponse {
  const perPage=Math.min(500,filters.perPage),offset=(filters.page-1)*perPage;
  if(!object(value) || !Array.isArray(value.entries) || !metadata(value,value.entries.length,filters.page,perPage) || value.offset!==offset
    || !value.entries.every(row=>object(row) && count(row.id) && text(row.action) && text(row.created_at) && typeof row.success==='boolean'
      && ['repo_id','direction','git_sha','author','details'].every(k=>nullableText(row[k])) && nullableCount(row.svn_rev)
      && (!filters.repoId || row.repo_id===filters.repoId) && (filters.success===undefined || row.success===filters.success) && (!filters.action || row.action===filters.action))
    || new Set(value.entries.map(row=>row.id)).size!==value.entries.length)throw new Error('Malformed audit page; matching results are unknown.');
  return value as unknown as AuditListResponse;
}
