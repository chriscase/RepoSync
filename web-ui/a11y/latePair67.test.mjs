import test from 'node:test';
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {chromium} from 'playwright-core';
import {createServer} from 'vite';
import {startFixture} from './fixture.mjs';

// Actual React/browser behavior with synthetic HTTP responses. Native SVN/Git
// publication is independently exercised by the Rust HTTP/engine regressions.
test('rs67 actual dialog invalidates changed reviews and distinguishes held publication from completion', {timeout:60000}, async()=>{
  const executablePath=process.env.REPOSYNC_A11Y_BROWSER;
  assert.ok(executablePath,'Select the existing admitted browser; this test installs nothing.');
  let fixture,vite,browser;
  try {
    fixture=await startFixture();
    vite=await createServer({server:{host:'127.0.0.1',port:0,proxy:{'/api':fixture.origin,'/ws':{target:fixture.origin,ws:false}}}});
    await vite.listen();const origin=vite.resolvedUrls.local[0];
    browser=await chromium.launch({executablePath,headless:true,chromiumSandbox:true,timeout:15000});
    const context=await browser.newContext({reducedMotion:'reduce'});
    const page=await context.newPage();page.setDefaultTimeout(8000);
    const requests=[];const errors=[];page.on('pageerror',error=>errors.push(error.message));
    let held=false,completed=false;
    await page.route('**/*',async route=>{
      const request=route.request(),url=new URL(request.url());
      if(url.origin!==new URL(origin).origin)return route.abort();
      if(url.pathname!=='/api/repos/C/branches'||request.method()!=='POST')return route.continue();
      const body=request.postDataJSON();requests.push(body);
      assert.equal(body.reconciliation,true);
      const pin=`pin:${body.git_branch}:${body.svn_branch}:${body.include_parent}`;
      if(body.action==='publish'){
        assert.equal(body.review_pin,pin);assert.equal(body.dry_run,false);assert.equal(body.preview,false);held=true;
      }
      if(body.action==='resume'){
        assert.equal(body.review_pin,pin);assert.equal(body.operation_id,'fixture-operation-C');completed=true;
      }
      const response={mode:completed?'published':held?'durable_operation':'review',published:completed,admitted:true,
        pair_state:completed?'completed':held?'reconciliation_held':'preview',scheduler_active:completed,
        policy_version:'late_pair_reconciliation_v2',parent_id:'C',git_branch:body.git_branch,svn_branch:body.svn_branch,
        git_tip:'a'.repeat(40),svn_source_revision:3,svn_target_revision:held?6:4,
        verified_baseline:{git_sha:'b'.repeat(40),svn_revision:2,evidence:'synthetic reviewed fixture'},baseline_missing_reason:null,
        inherited_work:[],pending_git:{count:2,summary:['A1','A2']},
        pending_svn:{knowable:true,parent_head_revision:3,baseline_revision:2,note:'synthetic'},conflicts:[],
        unknowns:held&&!completed?['Synthetic publication outcome held; nothing is acknowledged']:[],
        proposed_svn_copy_source_revision:null,existing_svn_target:{exists:true,equivalent:false,uuid:'synthetic',revision:4,url:'file:///synthetic-only/target',note:'synthetic'},
        skip_import_requested:false,skip_import_applied:false,skip_import_note:'Every pending commit remains accounted for',
        review_pin:pin,include_parent:body.include_parent,result_git_sha:'d'.repeat(40),result_tree_digest:'e'.repeat(64),
        operation_id:held?'fixture-operation-C':null,observation:'Synthetic durable fixture observation',handled_steps:completed?2:0,total_steps:2};
      await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(response)});
    });
    await page.goto(origin+'login');await page.getByLabel('Username',{exact:true}).fill('fixture-admin');
    await page.getByLabel('Password',{exact:true}).fill('synthetic-only');await page.getByRole('button',{name:/Sign in/i}).press('Enter');
    await page.waitForURL(url=>url.pathname!=='/login');await page.goto(origin+'repos/C');
    await page.getByRole('button',{name:'Add Branch Pair',exact:true}).click();
    const dialog=page.getByRole('dialog',{name:'Review Branch Pair',exact:true});await dialog.waitFor();
    const publish=page.getByRole('button',{name:'Publish reviewed result',exact:true});
    const preview=page.getByRole('button',{name:'Preview Pair',exact:true});
    const name=page.getByLabel(/^Branch Name/),svn=page.getByLabel(/^SVN Branch Path/);
    await name.fill('feature');assert.equal(await publish.isDisabled(),true);
    await preview.click();await publish.waitFor();await page.waitForFunction(()=>![...document.querySelectorAll('button')].find(el=>el.textContent==='Publish reviewed result')?.disabled);
    await svn.fill('branches/changed');assert.equal(await publish.isDisabled(),true,'a changed target retires the old pin');
    await preview.click();await page.getByText('pin:feature:branches/changed:false',{exact:true}).count();
    const include=page.getByLabel('Include parent SVN advancement in this reviewed result');await include.check();
    assert.equal(await publish.isDisabled(),true,'parent selection retires the old result');
    await preview.click();await page.waitForFunction(()=>![...document.querySelectorAll('button')].find(el=>el.textContent==='Publish reviewed result')?.disabled);
    await publish.click();await page.getByRole('alert').filter({hasText:'Synthetic publication outcome held'}).waitFor();
    assert.equal(await name.isDisabled(),true);assert.equal(await svn.isDisabled(),true);
    assert.match(await dialog.textContent(),/reconciliation_held/);assert.match(await dialog.textContent(),/scheduler: false/);
    assert.equal(await page.getByRole('button',{name:'Publish reviewed result',exact:true}).count(),0);
    const require=createRequire(import.meta.url);await page.addScriptTag({path:require.resolve('axe-core/axe.min.js')});
    const audit=await dialog.evaluate(async element=>window.axe.run(element,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}}));
    assert.deepEqual(audit.violations.map(value=>value.id),[]);
    for(let index=0;index<8;index++){
      await page.keyboard.press(index%2?'Tab':'Shift+Tab');
      assert.ok(await dialog.evaluate(element=>element.contains(document.activeElement)),'focus stays in the real modal');
    }
    await page.getByRole('button',{name:'Inspect operation',exact:true}).click();
    await page.getByRole('button',{name:'Reconcile / resume',exact:true}).click();
    await page.waitForFunction(()=>document.body.textContent.includes('Pair publication completed.'));
    assert.match(await dialog.textContent(),/scheduler: true/);assert.match(await dialog.textContent(),/handled steps: 2\/2/);
    assert.equal(requests.filter(request=>request.action==='publish').length,1);
    assert.equal(requests.filter(request=>request.action==='resume').length,1);
    assert.deepEqual(errors,[]);
    console.log('RS67_UI_FIXTURE: real dialog, pinned requests, form drift, held status, focus, axe, explicit recovery and completed rendering passed; synthetic HTTP only');
    await context.close();
  } finally {
    await browser?.close();await vite?.close();await fixture?.close();
  }
});
