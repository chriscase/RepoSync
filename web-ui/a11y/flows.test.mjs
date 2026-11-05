import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { chromium } from 'playwright-core';
import { createServer as createViteServer } from 'vite';
import { startFixture, skipContext } from './fixture.mjs';
const require=createRequire(import.meta.url);
let fixture,vite,browser,origin;
const errors=[];
before(async()=>{
 const executablePath=process.env.REPOSYNC_A11Y_BROWSER;
 assert.ok(executablePath,'Set REPOSYNC_A11Y_BROWSER to an existing admitted Chromium executable. This test never installs a browser.');
 console.log("# setup: starting owned loopback fixture");
 fixture=await startFixture();
 console.log("# setup: starting owned Vite server");
 vite=await createViteServer({server:{host:'127.0.0.1',port:0,proxy:{'/api':fixture.origin,'/ws':{target:fixture.origin,ws:false}}}});await vite.listen();origin=Object.values(vite.resolvedUrls.local)[0];
 console.log("# setup: launching existing browser in a fresh headless context");
 browser=await chromium.launch({executablePath,headless:true,chromiumSandbox:true,timeout:15000});
 console.log("# browser:",browser.version(),"Node:",process.version,"sandbox: enabled");
},{timeout:30000});
after(async()=>{await browser?.close();await vite?.close();await fixture?.close()});
async function open(path='/repos/C'){
 const context=await browser.newContext({reducedMotion:'reduce'});
 await context.route('**/*',route=>new URL(route.request().url()).origin===new URL(origin).origin?route.continue():route.abort());
 const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));page.setDefaultTimeout(8000);
 await page.goto(origin+'login');await page.getByLabel('Username',{exact:true}).fill('fixture-admin');await page.getByLabel('Password',{exact:true}).fill('synthetic-only');await page.getByRole('button',{name:/Sign in/i}).press('Enter');await page.waitForURL(url=>url.pathname!=='/login');await page.goto(origin+path.replace(/^\//,''));
 return {page,close:()=>context.close()};
}
async function reach(page,target){for(let i=0;i<140;i++){if(await target.evaluate(el=>el===document.activeElement))return;await page.keyboard.press('Tab')}assert.fail('Keyboard Tab could not reach '+await target.textContent())}
async function press(page,target){await reach(page,target);await page.keyboard.press('Enter')}
async function audit(page,label){
 await page.addScriptTag({path:require.resolve('axe-core/axe.min.js')});
 const result=await page.evaluate(async()=>window.axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21a','wcag21aa','wcag22aa']}}));
 assert.deepEqual(result.violations.map(v=>({id:v.id,impact:v.impact,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))})),[],label+' axe violations');
}
async function contained(page,count=10){const modal=page.getByRole('dialog');for(let i=0;i<count;i++){await page.keyboard.press(i%2?'Tab':'Shift+Tab');assert.ok(await modal.evaluate(el=>el.contains(document.activeElement)),'focus escaped native modal')} }

test('rs121 keyboard managed removal contains focus, busy Escape refuses, error remains recoverable, focus returns', {timeout:45000},async()=>{
 const {page,close}=await open();try{
  await page.getByRole('heading',{name:'Disposable fixture',exact:true}).waitFor();
  const trigger=page.getByTestId('remove-from-reposync');await press(page,trigger);
  const modal=page.getByRole('dialog',{name:'Remove from RepoSync',exact:true});await modal.waitFor();
  assert.match(await modal.getAttribute('aria-describedby'),/description/);await contained(page);await audit(page,'managed removal');
  await page.keyboard.press('Escape');assert.equal(await modal.count(),0);assert.ok(await trigger.evaluate(el=>el===document.activeElement));
  await press(page,trigger);await page.getByTestId('confirm-remove-from-reposync').waitFor();
  await fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({hold:true})});
  await press(page,page.getByTestId('confirm-remove-from-reposync'));await page.getByRole('dialog').filter({has:page.getByTestId('cancel-managed-remove')}).waitFor();
  await page.waitForFunction(()=>document.querySelector('dialog')?.getAttribute('aria-busy')==='true');
  await page.keyboard.press('Escape');assert.equal(await page.getByRole('dialog').count(),1);assert.equal(await page.getByTestId('cancel-managed-remove').isDisabled(),true);await contained(page);
  await fetch(fixture.origin+'/__fixture/release',{method:'POST'});await page.getByRole('alert').filter({hasText:'Synthetic cleanup refused'}).waitFor();
  await page.keyboard.press('Escape');assert.equal(await page.getByRole('dialog').count(),0);assert.ok(await trigger.evaluate(el=>el===document.activeElement));
  assert.equal(fixture.requests.filter(r=>r.method==='POST'&&r.path==='/api/repos/C/removal/approve').length,1);
 }finally{await close()}
});

test('rs121 branch links and typed deletion prompt are keyboard accessible without accidental remote selection', {timeout:45000},async()=>{
 const {page,close}=await open('/repos/A');try{
  const link=page.getByRole('link',{name:'Synthetic branch',exact:true});await link.waitFor();assert.equal(await link.getAttribute('href'),'/repos/B');await press(page,link);await page.waitForURL(url=>url.pathname==='/repos/B');
  await page.goto(origin+'repos/A');await press(page,page.getByTestId('delete-child-pair-B'));
  const modal=page.getByRole('dialog',{name:'Delete branch pair',exact:true});await modal.waitFor();
  const confirm=page.getByTestId('confirm-delete-branch-pair');assert.equal(await confirm.isDisabled(),true);assert.equal(await page.getByTestId('delete-git-opt').isChecked(),false);assert.equal(await page.getByTestId('delete-svn-opt').isChecked(),false);
  const input=page.getByLabel(/Type feature\/synthetic to confirm/);await reach(page,input);await page.keyboard.type('feature/synthetic');assert.equal(await input.inputValue(),'feature/synthetic');await page.waitForFunction(()=>!document.querySelector('[data-testid="confirm-delete-branch-pair"]')?.disabled);assert.equal(await confirm.isDisabled(),false);await contained(page);await audit(page,'branch removal');
  await page.keyboard.press('Escape');assert.equal(await modal.count(),0);assert.ok(await page.getByTestId('delete-child-pair-B').evaluate(el=>el===document.activeElement));
  assert.equal(fixture.requests.filter(r=>r.method==='DELETE').length,0);
 }finally{await close()}
});

test('rs121 conflict navigation/edit/defer works by keyboard with labelled manual content and selected view', {timeout:40000},async()=>{
 const {page,close}=await open('/conflicts/conflict-1');try{
  await page.getByRole('heading',{name:'example.txt',exact:true}).waitFor();const manual=page.getByRole('button',{name:'Manual Edit',exact:true});await press(page,manual);assert.equal(await manual.getAttribute('aria-pressed'),'true');
  const editor=page.getByLabel('Edit merged content:');await reach(page,editor);await page.keyboard.press('ControlOrMeta+A');await page.keyboard.type('Keyboard synthetic resolution\n');assert.equal(await editor.inputValue(),'Keyboard synthetic resolution\n');await audit(page,'conflict manual editor');
  await press(page,page.getByRole('button',{name:'Apply Manual Edit',exact:true}));await page.waitForURL(url=>url.pathname==='/conflicts');
  const manualRequest=fixture.requests.filter(r=>r.method==='POST'&&r.path==='/api/conflicts/conflict-1/resolve');assert.equal(manualRequest.length,1);assert.deepEqual(manualRequest[0].body,{resolution:'custom',merged_content:'Keyboard synthetic resolution\n'});
  await page.goto(origin+'conflicts/conflict-1');await page.getByRole('heading',{name:'example.txt',exact:true}).waitFor();
  await press(page,page.getByRole('button',{name:'Defer',exact:true}));await page.waitForURL(url=>url.pathname==='/conflicts');assert.equal(fixture.requests.filter(r=>r.method==='POST'&&r.path==='/api/conflicts/conflict-1/defer').length,1);
 }finally{await close()}
});

test('rs121 create form labels, native progress/reduced motion and non-live logs survive actual browser audit', {timeout:45000},async()=>{
 const {page,close}=await open('/repos');try{
  await press(page,page.getByRole('button',{name:'Add Repository',exact:true}).first());const modal=page.getByRole('dialog',{name:'Add Repository',exact:true});await modal.waitFor();
  const name=page.getByLabel('Repository Name',{exact:true});await reach(page,name);await page.keyboard.type('Keyboard fixture');assert.equal(await name.inputValue(),'Keyboard fixture');await contained(page);await audit(page,'repository registration');await page.keyboard.press('Escape');
  await page.goto(origin);const meter=page.getByRole('progressbar',{name:'SVN import progress — Synthetic parent',exact:true});await meter.waitFor();assert.equal(await meter.getAttribute('value'),'21');assert.match(await meter.getAttribute('aria-valuetext'),/21%/);assert.equal(await page.getByLabel('Recent import log').getAttribute('aria-live'),'off');
  const live=meter.locator('..').getByRole('status'),before=await live.textContent();
  await fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({current_rev:29,bump_log:true})});
  await page.waitForFunction(()=>document.querySelector('progress[aria-label="SVN import progress — Synthetic parent"]')?.getAttribute('value')==='29');assert.equal(await live.textContent(),before,'same-bucket revision/log churn is not a live status change');
  await fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({phase:'verifying'})});
  await page.waitForFunction(()=>document.querySelector('progress[aria-label="SVN import progress — Synthetic parent"]')?.nextElementSibling?.textContent?.includes('verifying'));assert.notEqual(await live.textContent(),before);
  const animated=await page.locator('.animate-pulse').evaluateAll(nodes=>nodes.map(el=>getComputedStyle(el).animationName));assert.ok(animated.every(name=>name==='none'));await audit(page,'import progress');
  assert.deepEqual(errors,[],'No application page errors');
 }finally{await close()}
});

test('rs121 completed synthetic removal returns focus to the current main landmark when the opener is removed', {timeout:30000},async()=>{
 const {page,close}=await open('/repos/C');try{
  await page.getByRole('heading',{name:'Disposable fixture',exact:true}).waitFor();
  await fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({removeResult:'completed'})});
  await press(page,page.getByTestId('remove-from-reposync'));
  const confirm=page.getByTestId('confirm-remove-from-reposync');await page.waitForFunction(()=>!document.querySelector('[data-testid="confirm-remove-from-reposync"]')?.disabled);await press(page,confirm);
  await page.waitForURL(url=>url.pathname==='/repos');await page.getByRole('heading',{name:'Repositories',exact:true}).waitFor();
  assert.equal(await page.getByRole('dialog').count(),0);assert.ok(await page.getByRole('main').evaluate(el=>el===document.activeElement));
  const requests=fixture.requests.filter(r=>r.method==='POST'&&r.path==='/api/repos/C/removal/approve');const request=requests.at(-1);assert.equal(request.body.delete_git,false);assert.equal(request.body.delete_svn,false);assert.match(request.body.review_pin,/^[a-f0-9]{64}$/);
 }finally{await close();await fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({removeResult:'refused'})})}
});

test('rs121 settings, administration and setup forms have accessible controls without submitting account or credential changes', {timeout:60000},async()=>{
 for(const [path,heading] of [['/settings','Settings'],['/users','User Management'],['/ldap','LDAP'],['/config','Configuration'],['/setup','RepoSync Setup']]){
  if(path==='/setup')await fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({setup_active:false})});
  const {page,close}=await open(path);try{
   await page.getByRole('heading',{name:new RegExp(heading)}).first().waitFor();
   if(path==='/users'){await press(page,page.getByRole('button',{name:'Create User',exact:true}));await page.getByRole('dialog',{name:'Create User',exact:true}).waitFor();await contained(page)}
   if(path==='/ldap')await page.getByLabel('Server URL',{exact:true}).waitFor();
   if(path==='/config')await page.getByLabel('Mapping SVN username',{exact:true}).waitFor();
   if(path==='/setup'){
    await press(page,page.getByRole('button',{name:'Get Started',exact:true}));
    console.log('# setup visible labels',JSON.stringify(await page.locator('label').allTextContents()));
    assert.equal((await page.locator('button[aria-current="step"]').textContent()).replace(/\s+/g,''),'2SVN');
    await page.getByRole('heading',{name:'SVN Repository',exact:true}).waitFor();
    const input=page.getByRole('textbox',{name:'Repository URL',exact:true});await input.waitFor();await press(page,page.getByRole('button',{name:'Next',exact:true}));
    assert.equal(await input.getAttribute('aria-invalid'),'true');assert.equal(await input.getAttribute('aria-required'),'true');const errorId=await input.getAttribute('aria-describedby');assert.ok(errorId);assert.match(await page.locator(`[id="${errorId}"]`).textContent(),/required/);
   }
   await audit(page,'read-only form '+path);
  }finally{await close()}
 }
 assert.deepEqual(errors,[],'No application page errors in audited form routes');
 assert.equal(fixture.requests.filter(r=>r.method!=='GET'&&(r.path.startsWith('/api/users')||r.path.startsWith('/api/admin/ldap')||r.path.startsWith('/api/config'))).length,0);
});

async function replaceByKeyboard(page,control,value){await reach(page,control);await page.keyboard.press('ControlOrMeta+A');await page.keyboard.type(value)}
async function describedFeedback(page,control,text){const id=await control.getAttribute('aria-describedby');assert.ok(id,'feedback must be associated');const feedback=page.locator(`[id="${id}"]`);assert.equal(await feedback.textContent(),text);assert.equal(await feedback.getAttribute('role'),'alert')}

test('rs121 pairing validation associates each error and clears it after correction without sending a refused preview', {timeout:40000},async()=>{
 const {page,close}=await open('/repos/A');try{
  const writesBefore=fixture.requests.filter(r=>r.method!=='GET').length;
  await press(page,page.getByRole('button',{name:'Add Branch Pair',exact:true}));
  const modal=page.getByRole('dialog',{name:'Review Branch Pair',exact:true});await modal.waitFor();
  const git=page.getByRole('textbox',{name:/^Branch Name/}),svn=page.getByRole('textbox',{name:/^SVN Branch Path/}),preview=page.getByRole('button',{name:'Preview Pair',exact:true});
  await replaceByKeyboard(page,git,'bad..name');await replaceByKeyboard(page,svn,'branches/valid');
  assert.equal(await git.getAttribute('aria-invalid'),'true');await describedFeedback(page,git,'Must not contain ".."');assert.equal(await svn.getAttribute('aria-invalid'),null);assert.equal(await preview.isDisabled(),true);
  await replaceByKeyboard(page,git,'valid');assert.equal(await git.getAttribute('aria-invalid'),null);assert.equal(await git.getAttribute('aria-describedby'),null);
  await replaceByKeyboard(page,svn,'branches/bad path');assert.equal(await svn.getAttribute('aria-invalid'),'true');await describedFeedback(page,svn,'Invalid characters');assert.equal(await git.getAttribute('aria-invalid'),null);assert.equal(await preview.isDisabled(),true);
  await audit(page,'pairing validation feedback');
  await replaceByKeyboard(page,svn,'branches/valid');assert.equal(await svn.getAttribute('aria-invalid'),null);assert.equal(await svn.getAttribute('aria-describedby'),null);assert.equal(await preview.isDisabled(),false);
  await page.keyboard.press('Escape');assert.equal(await modal.count(),0);assert.equal(fixture.requests.filter(r=>r.method!=='GET').length,writesBefore);
 }finally{await close()}
});

test('rs121 LDAP and notification failures are announced and associated without invalidating or losing inputs', {timeout:45000},async()=>{
 const {page,close}=await open('/ldap');try{
  const ldapRequests=()=>fixture.requests.filter(r=>r.method!=='GET'&&r.path.startsWith('/api/admin/ldap'));
  const beforeLdap=ldapRequests().length;
  const url=page.getByLabel('Server URL',{exact:true});await url.waitFor();await replaceByKeyboard(page,url,'ldaps://synthetic-review.example.invalid');
  const group=page.getByRole('group',{name:'LDAP configuration settings',exact:true});
  await press(page,page.getByRole('button',{name:'Test Connection',exact:true}));await page.getByRole('alert').filter({hasText:'Synthetic directory test refused'}).waitFor();await describedFeedback(page,group,'Synthetic directory test refused');
  assert.equal(await url.inputValue(),'ldaps://synthetic-review.example.invalid');assert.equal(await url.getAttribute('aria-invalid'),null);
  await press(page,page.getByRole('button',{name:'Save Configuration',exact:true}));await page.getByRole('alert').filter({hasText:'Synthetic directory save refused'}).waitFor();await describedFeedback(page,group,'API error 503: {"error":"Synthetic directory save refused"}');assert.equal(await url.inputValue(),'ldaps://synthetic-review.example.invalid');assert.equal(await url.getAttribute('aria-invalid'),null);assert.equal(ldapRequests().length,beforeLdap+2);assert.deepEqual(ldapRequests().slice(-2).map(r=>[r.method,r.path]),[['POST','/api/admin/ldap/test'],['PUT','/api/admin/ldap']]);await audit(page,'LDAP failure feedback');
  await page.goto(origin+'config');const teams=page.getByLabel('Teams Webhook URL',{exact:true});await teams.waitFor();await replaceByKeyboard(page,teams,'https://synthetic-notifications.example.invalid/fixture');
  const notify=page.getByRole('group',{name:'Microsoft Teams notification settings',exact:true});const notifications=()=>fixture.requests.filter(r=>r.method!=='GET'&&r.path.startsWith('/api/config/notifications'));const beforeNotify=notifications().length;
  await press(page,notify.getByRole('button',{name:'Save',exact:true}));await notify.getByRole('alert').filter({hasText:'Save failed'}).waitFor();await describedFeedback(page,teams,'Save failed');assert.equal(await teams.getAttribute('aria-invalid'),null);
  await press(page,notify.getByRole('button',{name:'Test',exact:true}));await notify.getByRole('alert').filter({hasText:'Synthetic notification test refused'}).waitFor();await describedFeedback(page,teams,'Synthetic notification test refused');await describedFeedback(page,notify,'Synthetic notification test refused');assert.equal(await teams.inputValue(),'https://synthetic-notifications.example.invalid/fixture');assert.equal(await teams.getAttribute('aria-invalid'),null);assert.equal(notifications().length,beforeNotify+2);assert.deepEqual(notifications().slice(-2).map(r=>[r.method,r.path]),[['POST','/api/config/notifications'],['POST','/api/config/notifications/test']]);await audit(page,'notification failure feedback');
 }finally{await close()}
});

test('rs121 Edit User retains immutable selected username, exact target, pending containment and focus return', {timeout:45000},async()=>{
 const {page,close}=await open('/users');try{
  const row=name=>page.getByRole('row').filter({has:page.getByRole('cell',{name,exact:true})});
  const alpha=row('fixture-alpha'),beta=row('fixture-beta');await alpha.waitFor();
  for(const r of [alpha,beta]){assert.equal(await r.getByRole('cell',{name:'Shared synthetic name',exact:true}).count(),1);assert.equal(await r.getByRole('cell',{name:'shared@example.invalid',exact:true}).count(),1)}
  await press(page,alpha.getByRole('button',{name:'Edit',exact:true}));const first=page.getByRole('dialog',{name:'Edit User: fixture-alpha',exact:true});await first.waitFor();await contained(page);await page.keyboard.press('Escape');assert.equal(await first.count(),0);assert.ok(await alpha.getByRole('button',{name:'Edit',exact:true}).evaluate(el=>el===document.activeElement));
  const edit=beta.getByRole('button',{name:'Edit',exact:true});await press(page,edit);const modal=page.getByRole('dialog',{name:'Edit User: fixture-beta',exact:true});await modal.waitFor();
  await replaceByKeyboard(page,modal.getByLabel('Display Name',{exact:true}),'Edited synthetic name');await replaceByKeyboard(page,modal.getByLabel('Email',{exact:true}),'edited@example.invalid');assert.equal(await modal.getByRole('heading',{name:'Edit User: fixture-beta',exact:true}).count(),1);await audit(page,'edit selected account');
  const writes=()=>fixture.requests.filter(r=>r.method==='PUT'&&r.path.startsWith('/api/users/'));const before=writes().length;
  await fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({hold:true})});await press(page,modal.getByRole('button',{name:'Save Changes',exact:true}));await page.waitForFunction(()=>document.querySelector('dialog')?.getAttribute('aria-busy')==='true');await page.keyboard.press('Escape');assert.equal(await modal.count(),1);assert.equal(await modal.getByRole('button',{name:'Cancel',exact:true}).isDisabled(),true);assert.equal(await modal.getByRole('heading',{name:'Edit User: fixture-beta',exact:true}).count(),1);await contained(page);
  await fetch(fixture.origin+'/__fixture/release',{method:'POST'});await modal.waitFor({state:'detached'});assert.equal(writes().length,before+1);assert.equal(writes().at(-1).path,'/api/users/fixture-beta');assert.deepEqual(writes().at(-1).body,{display_name:'Edited synthetic name',email:'edited@example.invalid',role:'user',enabled:true});assert.ok(await edit.evaluate(el=>el===document.activeElement));
 }finally{await fetch(fixture.origin+'/__fixture/release',{method:'POST'});await close()}
});

test('rs121 rendered Skip dialog confirms only the selected observed prefix and closes when scoped status becomes held or unknown', {timeout:45000},async()=>{
 const state=mode=>fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({skip_mode:mode})});
 await state('eligible');const {page,close}=await open('/repos/A');try{
  const writes=()=>fixture.requests.filter(r=>r.method==='POST'&&r.path==='/api/repos/A/skip-commit');const before=writes().length;
  const trigger=page.getByTestId('skip-commit-button');await trigger.waitFor();await press(page,trigger);
  const modal=page.getByRole('dialog',{name:'Skip selected pending commits',exact:true});await modal.waitFor();assert.equal(await modal.getAttribute('data-testid'),'skip-commit-modal');
  const description=await modal.getAttribute('aria-describedby');assert.ok(description);assert.match(await page.locator(`[id="${description}"]`).textContent(),/Unselected work stays pending/);
  const confirm=page.getByTestId('skip-commit-confirm');assert.equal(await confirm.isDisabled(),true);await contained(page);await page.keyboard.press('Escape');await modal.waitFor({state:'detached'});assert.ok(await trigger.evaluate(el=>el===document.activeElement));assert.equal(writes().length,before);
  await press(page,trigger);await modal.waitFor();const first=page.getByTestId('skip-commit-select-0'),second=page.getByTestId('skip-commit-select-1');await reach(page,first);await page.keyboard.press('Space');assert.equal(await first.isChecked(),true);assert.equal(await second.isChecked(),false);assert.equal(await confirm.isDisabled(),false);await audit(page,'selected exact-tip Skip confirmation');await press(page,confirm);await modal.waitFor({state:'detached'});
  assert.equal(writes().length,before+1);assert.deepEqual(writes().at(-1).body,{pinned_cursor:skipContext.pinned_cursor,selected_commits:[skipContext.pending_commits[0].sha],expected_remote_tip:skipContext.observed_remote_tip,expected_bridge_tip:skipContext.observed_bridge_tip,reason:'operator_exact_skip'});assert.ok(!writes().at(-1).body.selected_commits.includes(skipContext.pending_commits[1].sha));
  for(const mode of ['held','unknown']){
   await state('eligible');await page.goto(origin+'repos/A');await trigger.waitFor();await press(page,trigger);await modal.waitFor();await reach(page,page.getByTestId('skip-commit-select-0'));await page.keyboard.press('Space');assert.equal(await confirm.isDisabled(),false);
   await state(mode);await modal.waitFor({state:'detached',timeout:8000});assert.equal(await trigger.count(),0);assert.equal(writes().length,before+1,'retired selected confirmation must send no additional request');
  }
 }finally{await state('off');await close()}
});

async function rs65RestoreSyntheticC(){
 const response=await fetch(fixture.origin+'/api/repos/C/restore',{method:'POST',headers:{authorization:'Bearer synthetic-session-only','content-type':'application/json'},body:'{}'});
 assert.equal(response.status,200,'controlled synthetic setup only');
}

test('rs65 keyboard confirmation refuses unavailable review and keeps fresh exact targets inside the modal', {timeout:45000},async()=>{
 await rs65RestoreSyntheticC();const start=fixture.requests.length;const {page,close}=await open('/repos/C');try{
  await page.route('**/api/repos/C/removal/review',route=>route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:'Synthetic review unavailable'})}));
  const trigger=page.getByTestId('remove-from-reposync');await press(page,trigger);
  const modal=page.getByRole('dialog',{name:'Remove from RepoSync',exact:true});await modal.waitFor();
  await modal.getByRole('alert').filter({hasText:'Synthetic review unavailable'}).waitFor();
  assert.equal(await page.getByTestId('confirm-remove-from-reposync').isDisabled(),true);
  assert.equal(fixture.requests.slice(start).filter(r=>r.path.endsWith('/removal/approve')).length,0);
  await page.unroute('**/api/repos/C/removal/review');await press(page,modal.getByRole('button',{name:'Refresh exact-target review',exact:true}));
  await page.waitForFunction(()=>!document.querySelector('[data-testid="confirm-remove-from-reposync"]')?.disabled);
  const review=modal.getByTestId('reviewed-removal-targets');assert.equal(await review.isVisible(),true);
  assert.match(await review.textContent(),/Git: preserved/);assert.match(await review.textContent(),/SVN: preserved/);
  await contained(page);await audit(page,'rs65 exact-target review within native modal');
  await page.keyboard.press('Escape');assert.equal(await modal.count(),0);assert.ok(await trigger.evaluate(el=>el===document.activeElement));
  assert.equal(fixture.requests.slice(start).filter(r=>r.path.endsWith('/removal/approve')).length,0,'read-only review and cancellation never approve removal');
  console.log('# RS65_BROWSER_REVIEW',JSON.stringify({read_only:true,unavailable_blocks:true,exact_targets_inside_modal:true,keyboard_focus_contained:true,axe_violations:0,approval_requests:0}));
 }finally{await close()}
});

test('rs65 lost completed reply reload uses only request lookup, then explicit disabled restore', {timeout:60000},async()=>{
 await rs65RestoreSyntheticC();await fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({removeResult:'completed'})});
 const start=fixture.requests.length;const {page,close}=await open('/repos/C');try{
  let lostResolve,lostReject;const lost=new Promise((resolve,reject)=>{lostResolve=resolve;lostReject=reject});
  await page.route('**/api/repos/C/removal/approve',async route=>{try{
   const response=await route.fetch();assert.equal(response.status(),200);await response.body();await route.abort('failed');lostResolve();
  }catch(error){lostReject(error);await route.abort('failed').catch(()=>{})}});
  await press(page,page.getByTestId('remove-from-reposync'));
  await page.waitForFunction(()=>!document.querySelector('[data-testid="confirm-remove-from-reposync"]')?.disabled);
  await press(page,page.getByTestId('confirm-remove-from-reposync'));await lost;
  const approvals=()=>fixture.requests.slice(start).filter(r=>r.method==='POST'&&r.path==='/api/repos/C/removal/approve');
  assert.equal(approvals().length,1);assert.equal(approvals()[0].body.delete_git,false);assert.equal(approvals()[0].body.delete_svn,false);
  await page.reload();await page.getByRole('heading',{name:'Repository not found',exact:true}).waitFor();
  const recovery=page.getByTestId('removed-registration-recovery');await recovery.getByText('Removal completed; verified cleanup recorded',{exact:true}).waitFor();
  const restore=recovery.getByRole('button',{name:'Restore registration (stays disabled)',exact:true});await restore.waitFor();assert.equal(await restore.isEnabled(),true);
  const reads=fixture.requests.slice(start).filter(r=>r.method==='GET'&&r.path==='/api/repos/C/removal/request');assert.ok(reads.length>=1);
  assert.equal(approvals().length,1,'reload must not replay approval');
  assert.equal(fixture.requests.slice(start).filter(r=>r.method==='POST'&&r.path==='/api/repos/C/removal/continue').length,0,'lookup must not continue effects');
  assert.equal(fixture.requests.slice(start).filter(r=>r.method==='POST'&&r.path==='/api/repos/C/restore').length,0,'rendering must not restore automatically');
  await audit(page,'rs65 retained recovery after lost reply');await press(page,restore);
  await page.getByRole('heading',{name:'Disposable fixture',exact:true}).waitFor();await page.getByText('Sync configured disabled',{exact:true}).waitFor();
  assert.equal(fixture.requests.slice(start).filter(r=>r.method==='POST'&&r.path==='/api/repos/C/restore').length,1);
  assert.equal(fixture.requests.slice(start).filter(r=>r.method==='PUT'&&r.path==='/api/repos/C').length,0,'restore must not enable sync or overwrite configuration');
  assert.equal(approvals().length,1);assert.deepEqual(errors,[],'no application page errors');
  console.log('# RS65_BROWSER_LOST_REPLY',JSON.stringify({approval_requests:1,remote_git:false,remote_svn:false,read_only_request_lookup:reads.length,no_automatic_effect_retry:true,explicit_restore_requests:1,restored_enabled:false,axe_violations:0}));
 }finally{await close();await fetch(fixture.origin+'/__fixture/state',{method:'POST',body:JSON.stringify({removeResult:'refused'})})}
});
