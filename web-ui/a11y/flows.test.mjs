import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { chromium } from 'playwright-core';
import { createServer as createViteServer } from 'vite';
import { startFixture } from './fixture.mjs';
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
  assert.equal(fixture.requests.filter(r=>r.method==='POST'&&r.path==='/api/repos/C/remove').length,1);
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
  const requests=fixture.requests.filter(r=>r.method==='POST'&&r.path==='/api/repos/C/remove');const request=requests.at(-1);assert.equal(request.query.delete_git,'false');assert.equal(request.query.delete_svn,'false');
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
    assert.match(await page.locator('button[aria-current="step"]').textContent(),/SVN Repository/);
    const input=page.getByRole('textbox',{name:'Repository URL',exact:true});await input.waitFor();await press(page,page.getByRole('button',{name:'Next',exact:true}));
    assert.equal(await input.getAttribute('aria-invalid'),'true');assert.equal(await input.getAttribute('aria-required'),'true');const errorId=await input.getAttribute('aria-describedby');assert.ok(errorId);assert.match(await page.locator(`[id="${errorId}"]`).textContent(),/required/);
   }
   await audit(page,'read-only form '+path);
  }finally{await close()}
 }
 assert.deepEqual(errors,[],'No application page errors in audited form routes');
 assert.equal(fixture.requests.filter(r=>r.method!=='GET'&&(r.path.startsWith('/api/users')||r.path.startsWith('/api/admin/ldap')||r.path.startsWith('/api/config'))).length,0);
});
