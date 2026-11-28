import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { api, type Repository } from './api';
import { auditFilters, auditPageKey, changeListFilter, changeListPage, conflictFilters, conflictPageKey, dashboardListLink, decodeAuditPage, decodeConflictPage } from './listPagination';
import Conflicts from './pages/Conflicts';
import AuditLog from './pages/AuditLog';
import Dashboard from './pages/Dashboard';

const stamp='2026-11-04T01:00:00Z';
const fetchMock=vi.fn();let client:QueryClient;
function conflictPage(total=25,page=1,repo='A',status='detected') {
  return {items:Array.from({length:Math.min(20,Math.max(0,total-(page-1)*20))},(_,n)=>({id:`conflict-${(page-1)*20+n}`,file_path:`file-${n}.txt`,conflict_type:'content',status,svn_revision:null,git_hash:null,created_at:stamp,resolved_at:null,repo_id:repo})),total,page,per_page:20,total_pages:Math.ceil(total/20)};
}
function auditPage(total=65,page=1,repo='A',success=false,action='sync_error') {
  return {entries:Array.from({length:Math.min(50,Math.max(0,total-(page-1)*50))},(_,n)=>({id:(page-1)*50+n+1,repo_id:repo,action,direction:null,svn_rev:null,git_sha:null,author:null,details:'retained evidence',created_at:stamp,success})),total,page,per_page:50,total_pages:Math.ceil(total/50),offset:(page-1)*50};
}
function repo(id:string):Repository {return {id,name:`Repository ${id}`,git_branch:'main',svn_branch:'trunk',enabled:true,updated_at:stamp,created_at:stamp} as Repository;}
beforeEach(()=>{
  // Observe the completed error in SSR rather than starting a fresh mount retry; actual cache failures are awaited below.
  client=new QueryClient({defaultOptions:{queries:{retry:false,retryOnMount:false,staleTime:Infinity}}});
  const entries=new Map([['session_token','fixture-token'],['user','{"id":"actor","role":"admin"}']]);
  vi.stubGlobal('localStorage',{getItem:(key:string)=>entries.get(key) ?? null,setItem:(key:string,v:string)=>entries.set(key,v),removeItem:(key:string)=>entries.delete(key),get length(){return entries.size;},key:(index:number)=>Array.from(entries.keys())[index] ?? null});
  vi.stubGlobal('window',{location:{pathname:'/audit',href:'/audit'}});vi.stubGlobal('fetch',fetchMock);fetchMock.mockReset();
  client.setQueryData(['repos'],[repo('A'),repo('B')]);
});
afterEach(()=>{client.clear();vi.unstubAllGlobals();});
function render(component:typeof Conflicts,path:string) {return renderToStaticMarkup(createElement(QueryClientProvider,{client},createElement(MemoryRouter,{initialEntries:[path]},createElement(component))));}


describe('#119 URL filters reset pages atomically and bind each request/cache',()=>{
  it.each([['repo_id','B'],['status','resolved'],['success','true'],['action','config_updated']])('changing %s from a late page requests page1 in the same update', (key,value)=>{
    const original=new URLSearchParams('page=9&repo_id=A&status=detected&success=false&action=sync_error');
    const next=changeListFilter(original,key,value);expect(next.get('page')).toBe('1');expect(next.get(key)).toBe(value);expect(original.get('page')).toBe('9');
    for(const unchanged of ['repo_id','status','success','action'].filter(k=>k!==key))expect(next.get(unchanged)).toBe(original.get(unchanged));
  });
  it('preserves scope and filters while moving pages and removes only the cleared filter',()=>{
    const params=new URLSearchParams('repo_id=A%26B&action=sync_error&success=false&page=2');const next=changeListPage(params,3);expect(auditFilters(next)).toMatchObject({repoId:'A&B',page:3,action:'sync_error',success:false});
    const cleared=changeListFilter(next,'success','');expect(cleared.has('success')).toBe(false);expect(cleared.get('repo_id')).toBe('A&B');expect(cleared.get('action')).toBe('sync_error');expect(cleared.get('page')).toBe('1');
  });
  it('uses all filters in cache keys and refuses invalid URL input instead of fetching a different scope',()=>{
    const original=auditFilters(new URLSearchParams('repo_id=A&page=2&action=sync_error&success=false'));
    const keys=[original,{...original,page:1},{...original,repoId:'B'},{...original,success:true},{...original,action:'other'}].map(a=>JSON.stringify(auditPageKey(a)));expect(new Set(keys).size).toBe(5);
    expect(auditFilters(new URLSearchParams('success=maybe')).error).toContain('Invalid');expect(conflictFilters(new URLSearchParams('page=0')).error).toContain('Invalid');expect(conflictFilters(new URLSearchParams('repo_id=')).error).toContain('scope');
  });
  it('sends repository/status/page and outcome/action filters to their actual checked APIs',async()=>{
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(conflictPage(25,2,'A&B','deferred'))));await api.getConflictsPage({repoId:'A&B',status:'deferred',page:2,perPage:20});
    const conflictUrl=new URL(fetchMock.mock.calls[0][0],'http://fixture');expect(Object.fromEntries(conflictUrl.searchParams)).toEqual({page:'2',per_page:'20',status:'deferred',repo_id:'A&B'});
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(auditPage(65,2,'A&B',false,'sync_error'))));await api.getAuditLog(50,2,false,'A&B','sync_error');
    const auditUrl=new URL(fetchMock.mock.calls[1][0],'http://fixture');expect(Object.fromEntries(auditUrl.searchParams)).toEqual({limit:'50',page:'2',success:'false',repo_id:'A&B',action:'sync_error'});
  });
});
describe('#119 checked pages cannot turn malformed or failed reads into confirmed empty',()=>{
  it.each(['wrong total','foreign scope','wrong status','missing rows','duplicate rows'] as const)('refuses a malformed conflict success: %s',kind=>{
    const value=conflictPage();if(kind==='wrong total')value.total=0;if(kind==='foreign scope')value.items[0].repo_id='B';if(kind==='wrong status')value.items[0].status='resolved';if(kind==='missing rows')value.items.pop();if(kind==='duplicate rows')value.items[1].id=value.items[0].id;
    expect(()=>decodeConflictPage(value,{page:1,perPage:20,repoId:'A',status:'detected'})).toThrow('unknown');
  });
  it.each(['wrong total','foreign scope','wrong outcome','wrong action','missing rows','duplicate rows','bad metadata'] as const)('refuses a malformed audit success: %s',kind=>{
    const value=auditPage();if(kind==='wrong total')value.total=0;if(kind==='foreign scope')value.entries[0].repo_id='B';if(kind==='wrong outcome')value.entries[0].success=true;if(kind==='wrong action')value.entries[0].action='other';if(kind==='missing rows')value.entries.pop();if(kind==='duplicate rows')value.entries[1].id=value.entries[0].id;if(kind==='bad metadata')value.offset=50;
    expect(()=>decodeAuditPage(value,{page:1,perPage:50,repoId:'A',success:false,action:'sync_error'})).toThrow('unknown');
  });
  it('accepts a genuine zero-total page and a nonempty matching set with an out-of-range page distinctly',()=>{
    expect(decodeConflictPage(conflictPage(0),{page:1,perPage:20}).total).toBe(0);
    const late=decodeAuditPage(auditPage(65,99),{page:99,perPage:50});expect(late.entries).toEqual([]);expect(late.total).toBe(65);expect(late.total_pages).toBe(2);
  });
  it('retains a previous checked page on failed refetch and the actual UI shows error/retry, not empty',async()=>{
    const filters=conflictFilters(new URLSearchParams('repo_id=A'));const key=conflictPageKey(filters);const original=conflictPage();client.setQueryData(key,original);
    fetchMock.mockResolvedValueOnce(new Response('{"error":"forbidden"}',{status:403}));await expect(client.fetchQuery({queryKey:key,queryFn:()=>api.getConflictsPage(filters),staleTime:0,retry:false})).rejects.toThrow();expect(client.getQueryData(key)).toEqual(original);
    const html=render(Conflicts,'/conflicts?repo_id=A');expect(html).toContain('Error loading conflicts');expect(html).toContain('Results are unknown');expect(html).toContain('Retry');expect(html).not.toContain('No matching conflicts');
  });
});
describe('#119 actual list-page composition and scoped dashboard links',()=>{
  it('renders a scoped later conflict page, matching totals and navigation using the canonical query',()=>{
    const path='/conflicts?repo_id=A&status=detected&page=2';const filters=conflictFilters(new URLSearchParams(path.split('?')[1]));client.setQueryData(conflictPageKey(filters),conflictPage(25,2));const html=render(Conflicts,path);
    expect(html).toContain('25');expect(html).toContain('matching conflicts');expect(html).toContain('Page 2 / 2');expect(html).toContain('file-0.txt');expect(html).toContain('Previous');expect(html).toContain('Next');expect(html).toContain('aria-pressed="true"');expect(client.getQueryCache().find({queryKey:conflictPageKey(filters),exact:true})).toBeDefined();
  });
  it.each([0,25])('a zero-row page with total%s does not claim everything is synchronized',total=>{
    const page=total ? 99:1,path=`/conflicts?repo_id=A&page=${page}`;const filters=conflictFilters(new URLSearchParams(path.split('?')[1]));client.setQueryData(conflictPageKey(filters),conflictPage(total,page));const html=render(Conflicts,path);
    expect(html).toContain(total ? 'No conflicts on this page':'No matching conflicts');expect(html).not.toContain('Everything is in sync');if(total)expect(html).toContain('Return to first page');
  });
  it('keeps an unknown exact status filter visible on a confirmed empty deep link',()=>{
    const path='/conflicts?repo_id=A&status=unrecognized';const filters=conflictFilters(new URLSearchParams(path.split('?')[1]));client.setQueryData(conflictPageKey(filters),conflictPage(0,1,'A','unrecognized'));const html=render(Conflicts,path);expect(html).toContain('Status: unrecognized');expect(html).toContain('No matching conflicts');expect(html).not.toContain('Everything is in sync');
  });
  it('renders pending conflict reads as loading, never an empty/healthy claim',()=>{
    const html=render(Conflicts,'/conflicts?repo_id=A');expect(html).toContain('Loading conflicts');expect(html).not.toContain('No matching conflicts');expect(html).not.toContain('Everything is in sync');
  });
  it('renders scoped audit filters, server total and the second page without post-filtering',()=>{
    const path='/audit?repo_id=A&page=2&success=false&action=sync_error';const filters=auditFilters(new URLSearchParams(path.split('?')[1]));client.setQueryData(auditPageKey(filters),auditPage(65,2));const html=render(AuditLog,path);
    expect(html).toContain('Showing 15 of 65 matching entries');expect(html).toContain('Page 2 of 2');expect(html).toContain('15 failures on this page');expect(html).toContain('aria-pressed="true"');expect(client.getQueryCache().find({queryKey:auditPageKey(filters),exact:true})).toBeDefined();
  });
  it.each(['pending','failed','malformed'] as const)('audit %s is not confirmed empty and offers a recovery control on failure',async kind=>{
    const filters=auditFilters(new URLSearchParams('repo_id=A'));
    if(kind!=='pending') {
      fetchMock.mockResolvedValueOnce(new Response(kind==='failed' ? '{"error":"read failed"}':JSON.stringify({...auditPage(),entries:[]}),{status:kind==='failed' ? 500:200}));
      await expect(client.fetchQuery({queryKey:auditPageKey(filters),queryFn:()=>api.getAuditLog(50,1,undefined,'A'),retry:false})).rejects.toThrow();
    }
    const html=render(AuditLog,'/audit?repo_id=A');expect(html).not.toContain('No matching audit entries');expect(html).not.toContain('No audit entries on this page');expect(html).toContain('Matching total unknown');if(kind!=='pending'){expect(html).toContain('Results are unknown');expect(html).toContain('Retry');}
  });
  it('confirms truly empty audit filters, and keeps a registered-id scope even if the repo list is unavailable',()=>{
    const filters=auditFilters(new URLSearchParams('repo_id=historical'));client.removeQueries({queryKey:['repos']});client.setQueryData(auditPageKey(filters),auditPage(0,1,'historical'));const html=render(AuditLog,'/audit?repo_id=historical');expect(html).toContain('No matching audit entries');expect(html).toContain('Repository historical');expect(html).toContain('Showing 0 of 0 matching entries');
  });
  it('the actual Dashboard emits encoded scoped conflict/audit/error links and unknown recent activity on read failure',async()=>{
    const id='A&B';client.setQueryData(['repos'],[repo(id),repo('B')]);
    fetchMock.mockResolvedValueOnce(new Response('{"error":"read failed"}',{status:500}));
    await expect(client.fetchQuery({queryKey:['audit','recent',id],queryFn:()=>api.getAuditLog(20,undefined,undefined,id),retry:false})).rejects.toThrow();
    const html=render(Dashboard,'/dashboard?repo_id=A%26B');expect(html).toContain('href="/conflicts?repo_id=A%26B"');expect(html).toContain('href="/audit?repo_id=A%26B"');expect(html).toContain('href="/audit?success=false&amp;repo_id=A%26B"');expect(html).toContain('Error loading recent activity');expect(html).not.toContain('No matching activity');
    expect(dashboardListLink('/audit',id,{success:'false'})).toBe('/audit?success=false&repo_id=A%26B');
  });
});
