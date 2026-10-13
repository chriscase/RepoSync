import {execFileSync} from 'node:child_process';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {test, expect} from '@playwright/test';
import {runCase} from '../../playwright/handlers.ts';
import {isCredentialRefusal, Lab, type Pair} from '../../playwright/runtime.ts';

const pair: Pair={id:'isob',name:'isob',gitRepo:'lab/isob',svnUrl:'https://svn/svn/isob'};

test('worker restart finds its existing pair without registering or importing again', async () => {
  const row = {id: 'persisted', name: 'team', git_repo: 'lab/team', svn_url: 'https://svn:8443/svn/team'};
  const waited: string[] = [];
  const fake = {repos: async () => [row], waitImport: async (id: string) => {waited.push(id);}, sh: async () => {throw new Error('unexpected remote mutation');}};
  expect(await Lab.prototype.createPair.call(fake as unknown as Lab, 'team')).toEqual({id: row.id, name: row.name, gitRepo: row.git_repo, svnUrl: row.svn_url});
  expect(waited).toEqual(['persisted']);
});

for (const fault of ['ambiguous', 'different-remotes']) {
  test(`worker restart rejects ${fault} existing pair`, async () => {
    const row = {id: 'persisted', name: 'team', git_repo: 'lab/team', svn_url: 'https://svn:8443/svn/team'};
    const fake = {repos: async () => fault === 'ambiguous' ? [row, {...row, id: 'other'}] : [{...row, svn_url: 'https://svn:8443/svn/foreign'}], waitImport: async () => {throw new Error('unexpected import wait');}, sh: async () => {throw new Error('unexpected remote mutation');}};
    await expect(Lab.prototype.createPair.call(fake as unknown as Lab, 'team')).rejects.toThrow(/ambiguous|different remotes/);
  });
}

test('denial receipt requires the daemon IP, target repository and refusal status',()=>{
  const ip=['172.18.0.8'];
  const git='router: completed GET /lab/isob.git/info/refs?service=git-upload-pack for 172.18.0.8:5000, 404 Not Found';
  expect(isCredentialRefusal(git,pair,ip,'git')).toBe(true);
  for(const line of [git.replace('404','200'),git.replace('404','401'),git.replace('0.8:','0.9:'),git.replace('isob.git','isoa.git')]) expect(isCredentialRefusal(line,pair,ip,'git')).toBe(false);
  const svn='svn-1 | 172.18.0.8 - alice [date] "OPTIONS /svn/isob/trunk HTTP/1.1" 403 100';
  expect(isCredentialRefusal(svn,pair,ip,'svn')).toBe(true);
  for(const line of [svn.replace('403','401'),svn.replace('403','200'),svn.replace('alice','bob'),svn.replace('0.8 ','0.9 '),svn.replace('isob/','isob-other/')]) expect(isCredentialRefusal(line,pair,ip,'svn')).toBe(false);
});

for(const failure of ['commit','unchanged-source','webhook','denial-observation']) {
  test(`isolation rejects ${failure} failure instead of marking PASS`,async()=>{
    let pass=false,attempt=0,observed=false;
    const states:Record<string,any>={isoa:{svn:'1',git:'a'.repeat(40),records:'0',watermark:'1',maps:'0'},isob:{svn:'1',git:'b'.repeat(40),records:'0',watermark:'1',maps:'0'}};
    const fake={
      alicePassword:'synthetic-alice',bobPassword:'synthetic-bob',gitToken:'synthetic-admin',
      createPair:async(name:string)=>({...pair,id:name,name}),issueScopedGitToken:async(name:string)=>({user:name,token:name}),
      api:async()=>({status:200}),snap:async(p:Pair)=>({...states[p.name]}),
      syncSvnChange:async(p:Pair)=>{states[p.name]={svn:'2',git:(++attempt===1?'c':'d').repeat(40),records:'1',watermark:'2',maps:'0'};return {...states[p.name]};},
      sh:async(_service:string,script:string)=>script.includes('COUNT(*)')?'0':'distinct',
      commitSvn:async(p:Pair)=>{if(failure==='commit')throw new Error('source commit failed');if(failure==='unchanged-source')return '2';states[p.name].svn='3';return '3';},
      triggerSvn:async()=>{if(failure==='webhook')throw new Error('webhook failed');},
      expectCredentialDenial:async(_p:Pair,trigger:()=>Promise<void>)=>{await trigger();observed=true;throw new Error('denial observation unavailable');},
      removeScopedGitTeam:async()=>{},pass:()=>{pass=true;},
    };
    await expect(runCase(fake as unknown as Lab,{id:'LAB_ISOLATION_TWO_REPOS',title:'fixture',suite:'smoke'})).rejects.toThrow();
    expect(pass).toBe(false);
    expect(observed).toBe(failure==='denial-observation');
  });
}


test('Git recovery requires both source checkpoints and accepts an unchanged SVN-read cursor', async () => {
  const source = 'c'.repeat(40);
  const before = {svn: '4', git: source, records: '3', maps: '0', watermark: '3', gitWatermark: 'a'.repeat(40), gitColumn: 'a'.repeat(40)};
  const restored = {...before, svn: '5', records: '4', gitWatermark: source, gitColumn: source};
  let current = {...before};
  const fake = {
    snap: async () => ({...current}),
    waitUntil: async (read: () => Promise<boolean>) => {
      for (const state of [before, {...restored, gitWatermark: before.gitWatermark}, {...restored, gitColumn: before.gitColumn}, {...restored, records: before.records}, {...restored, svn: before.svn}]) {
        current = state;
        expect(await read()).toBe(false);
      }
      current = restored;
      expect(await read()).toBe(true);
    },
  };
  expect(await Lab.prototype.waitGitPublication.call(fake as unknown as Lab, pair, before, source)).toEqual(restored);
});


for (const layout of ['legacy', 'scoped', 'missing-scoped', 'invalid-scope', 'missing-database']) {
  test(`snapshot reads the authoritative ${layout} checkpoint without fallback`, async () => {
    const root = mkdtempSync(join(tmpdir(), 'lab-checkpoint-'));
    const database = join(root, 'state.db');
    const source = 'c'.repeat(40), stale = 'd'.repeat(40);
    const scope = '11111111-2222-3333-4444-555555555555';
    try {
      if (layout !== 'missing-database') {
        execFileSync('sqlite3', [database, `
          CREATE TABLE repositories (id TEXT, last_git_sha TEXT${layout === 'legacy' ? '' : ', scope_uuid TEXT'});
          CREATE TABLE kv_state (key TEXT, value TEXT);
          CREATE TABLE commit_map (repo_id TEXT);
          CREATE TABLE sync_records (repo_id TEXT);
          INSERT INTO repositories VALUES ('isob', '${source}'${layout === 'legacy' ? '' : `, '${layout === 'invalid-scope' ? '' : scope}'`});
          INSERT INTO kv_state VALUES ('last_git_sha_isob', '${layout === 'scoped' ? stale : source}');
          ${layout === 'scoped' ? `INSERT INTO kv_state VALUES ('last_git_sha_${scope}', '${source}');` : ''}
        `]);
      }
      let observation = 0;
      const fake = {
        observer: async () => ++observation === 1 ? '5' : source,
        sh: async (_service: string, script: string) => execFileSync('sh', ['-c', script.replaceAll('/var/lib/reposync/reposync.db', database)], {encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe']}),
      };
      const read = Lab.prototype.snap.call(fake as unknown as Lab, pair);
      if (layout === 'invalid-scope' || layout === 'missing-database') await expect(read).rejects.toThrow();
      else {
        const snapshot = await read;
        expect(snapshot.gitWatermark).toBe(layout === 'missing-scoped' ? 'none' : source);
        expect(snapshot.gitColumn).toBe(source);
      }
    } finally { rmSync(root, {recursive: true, force: true}); }
  });
}
