import assert from 'node:assert/strict';
import {mkdtempSync, mkdirSync, writeFileSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {aggregateReport, enforceReport, bindEvidenceIdentity} from './ci-gate.mjs';

const root=mkdtempSync(join(tmpdir(),'rs-identity-'));
const candidate='a'.repeat(40);
const valid={candidate_sha:candidate,suite:'smoke',seed:1,run_id:'rsidentity1'};
const report={candidate,suite:'smoke',seed:'1',run_id:valid.run_id,suite_result:'PASS'};
const saved={...process.env};
let checks=0;
try {
  process.env.LAB_EVENT_NAME='workflow_dispatch';process.env.LAB_SUITE_INPUT='smoke';process.env.LAB_SEED_INPUT='1';
  const write=(p,data)=>{mkdirSync(join(p,'..'),{recursive:true});writeFileSync(p,JSON.stringify(data));};
  for (const [name,identity] of [
    ['valid',valid],['absent',undefined],['null',null],['array',[]],
    ...Object.keys(valid).map(key=>[ `missing-${key}`, Object.fromEntries(Object.entries(valid).filter(([k])=>k!==key)) ]),
    ['empty-suite',{...valid,suite:''}],['boolean-seed',{...valid,seed:true}],
    ['negative-seed',{...valid,seed:-1}],['unsafe-seed',{...valid,seed:Number.MAX_SAFE_INTEGER+1}],
    ['wrong-seed',{...valid,seed:2}],['wrong-suite',{...valid,suite:'full'}],
    ['wrong-source',{...valid,candidate_sha:'b'.repeat(40)}],
  ]) {
    const evidence=join(root,name,'evidence'), reportDir=join(root,name,'report');
    mkdirSync(evidence,{recursive:true});
    write(join(evidence,'playwright.json'),{suites:[]});write(join(evidence,'self-test.json'),{cases:[]});
    if(identity!==undefined)write(join(evidence,'identity.json'),identity);
    write(join(reportDir,'report.json'),report);
    const bound=bindEvidenceIdentity({evidence,candidate,suite:'smoke',seed:'1'});
    assert.equal(bound.ok,name==='valid',`${name} identity binding`);checks++;
    const enforced=enforceReport({evidence,engine:root,reportDir});
    assert.equal(enforced.ok,name==='valid',`${name} enforcement`);checks++;
    if(name!=='valid') {
      const aggregated=aggregateReport({evidence,engine:root,reportDir,candidate});
      assert.equal(aggregated.ok,false,`${name} aggregation`);checks++;
    }
  }
  const evidence=join(root,'valid','evidence'),reportDir=join(root,'valid','report');
  write(join(evidence,'nested/identity.json'),valid);
  assert.equal(enforceReport({evidence,engine:root,reportDir}).ok,true,'agreeing copies');checks++;
  write(join(evidence,'nested/identity.json'),{...valid,run_id:'rsother1'});
  assert.equal(enforceReport({evidence,engine:root,reportDir}).ok,false,'conflicting copies');checks++;
  writeFileSync(join(evidence,'nested/identity.json'),'');
  assert.equal(enforceReport({evidence,engine:root,reportDir}).ok,false,'empty identity copy');checks++;
  rmSync(join(evidence,'nested'),{recursive:true});
  process.env.LAB_SEED_INPUT='2';
  write(join(evidence,'identity.json'),{candidate_sha:candidate,run_id:valid.run_id});
  assert.equal(aggregateReport({evidence,engine:root,reportDir,candidate}).ok,false,'missing suite/seed cannot relabel seed 1 as seed 2');checks++;
  assert.equal(enforceReport({evidence,engine:root,reportDir}).ok,false,'enforce incomplete identity');checks++;
  write(join(evidence,'identity.json'),valid);
  assert.equal(enforceReport({evidence,engine:root,reportDir}).ok,false,'enforce selection seed mismatch');checks++;
  process.env.LAB_SEED_INPUT='1';
  for(const key of ['candidate','suite','seed','run_id']) {
    write(join(reportDir,'report.json'),Object.fromEntries(Object.entries(report).filter(([k])=>k!==key)));
    assert.equal(enforceReport({evidence,engine:root,reportDir}).ok,false,`missing report ${key}`);checks++;
  }
  console.log(`identity checks PASS ${checks}`);
} finally {
  for(const key of ['LAB_EVENT_NAME','LAB_SUITE_INPUT','LAB_SEED_INPUT']) {
    if(saved[key]===undefined)delete process.env[key];else process.env[key]=saved[key];
  }
  rmSync(root,{recursive:true,force:true});
}
