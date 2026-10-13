import {test, expect} from '@playwright/test';
import {runCase} from '../../playwright/handlers.ts';
import {isCredentialRefusal, type Lab, type Pair} from '../../playwright/runtime.ts';

const pair: Pair={id:'isob',name:'isob',gitRepo:'lab/isob',svnUrl:'https://svn/svn/isob'};
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
