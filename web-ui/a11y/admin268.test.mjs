// Synthetic direct-route boundary checks only. No external services or accounts.
import test, {before, after} from 'node:test';
import assert from 'node:assert/strict';
import {chromium} from 'playwright-core';
import {createServer} from 'vite';
import {startFixture} from './fixture.mjs';
let fixture, vite, browser, origin;
before(async()=>{
  assert.ok(process.env.REPOSYNC_A11Y_BROWSER,'Existing admitted Chromium required');
  fixture=await startFixture();
  vite=await createServer({server:{host:'127.0.0.1',port:0,proxy:{'/api':fixture.origin}}});
  await vite.listen();origin=vite.resolvedUrls.local[0];
  browser=await chromium.launch({executablePath:process.env.REPOSYNC_A11Y_BROWSER,headless:true,chromiumSandbox:true});
});
after(async()=>{await browser?.close();await vite?.close();await fixture?.close();});
for(const path of ['setup','config']) for(const role of ['viewer','user',null,'admin','legacy','no-session']) {
  test(`#268 ${path} direct route as ${role}`,{timeout:30000},async()=>{
    const context=await browser.newContext();const page=await context.newPage();page.setDefaultTimeout(8000);
    const requests=[],errors=[];let identityRequests=0;
    await context.route('**/*',route=>{
      const url=new URL(route.request().url());
      if(url.origin!==new URL(origin).origin)return route.abort();
      if(url.pathname==='/api/setup/import/status')return route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({phase:'idle',busy:false,can_start:true,current_rev:0,total_revs:0,commits_created:0,log_lines:[]})});
      if(url.pathname==='/api/auth/login'&&role==='legacy')return route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({token:'synthetic-session-only',expires_at:'2099-01-01T00:00:00Z',user:null})});
      if(url.pathname==='/api/auth/me'&&role===null)return route.fulfill({status:403,contentType:'application/json',body:JSON.stringify({error:'identity unavailable'})});
      if(url.pathname==='/api/auth/me'&&role==='legacy')return route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({id:'legacy',username:'admin',role:'admin'})});
      return route.continue();
    });
    await context.addInitScript(role=>{
      if(role!=='no-session'&&role!=='legacy')localStorage.setItem('session_token','synthetic-session-only');
      if(role!==null&&role!=='legacy'&&role!=='no-session')localStorage.setItem('user',JSON.stringify({id:'synthetic-admin',username:'fixture-admin',role}));
    },role);
    page.on('request',request=>{const url=new URL(request.url());if(url.pathname==='/api/auth/me')identityRequests++;if(/^\/api\/(setup|config)(\/|$)/.test(url.pathname))requests.push({method:request.method(),path:url.pathname});});
    page.on('pageerror',error=>errors.push(error.message));
    try {
      if(role==='legacy') {
        await page.goto(origin+'login');
        await page.getByLabel('Username',{exact:true}).fill('fixture-admin');
        await page.getByLabel('Password',{exact:true}).fill('synthetic-only');
        await page.getByRole('button',{name:/Sign in/i}).click();
        await page.waitForURL(url=>url.pathname==='/');
        assert.equal(await page.evaluate(()=>localStorage.getItem('user')),null);
      }
      await page.goto(origin+path);
      if(role==='no-session') {await page.waitForURL(url=>url.pathname==='/login');assert.equal(requests.length,0);}
      else if(role!=='admin'&&role!=='legacy') {
        await page.getByRole('heading',{name:'Access Denied',exact:true}).waitFor();
        assert.equal(new URL(page.url()).pathname,'/'+path);
        assert.equal(await page.locator('main input, main form').count(),0);
        assert.equal(requests.length,0);
      } else {
        if(path==='setup')await page.getByRole('heading',{name:'RepoSync Setup',exact:true}).waitFor();
        else await page.getByRole('heading',{name:/Identity Mappings/}).waitFor();
        await page.waitForLoadState('networkidle');
        assert.ok(requests.some(request=>request.method==='GET'&&request.path===(path==='setup'?'/api/setup/config':'/api/config/identity')));
        assert.equal(await page.getByRole('heading',{name:'Access Denied',exact:true}).count(),0);
        if(role==='legacy') {
          if(path==='setup')await page.getByRole('button',{name:'Get Started',exact:true}).click();
          const field=path==='setup'?page.locator('input#svn_url'):page.getByLabel('Mapping SVN username',{exact:true});
          await field.fill(path==='setup'?'https://synthetic.invalid/unsaved':'unsaved-admin-edit');
          const value=await field.inputValue();
          const identityBefore=identityRequests;
          // Cross the application's 5-second polling period, then exercise browser
          // focus/reconnect signals. Identity verification must not remount the form.
          await page.waitForTimeout(5500);
          await page.evaluate(()=>{
            window.dispatchEvent(new Event('blur'));
            window.dispatchEvent(new Event('focus'));
            window.dispatchEvent(new Event('offline'));
            window.dispatchEvent(new Event('online'));
          });
          await page.waitForTimeout(250);
          assert.equal(await field.inputValue(),value);
          assert.equal(identityRequests,identityBefore);
        }
        const beforeDemotion=requests.length;
        await page.evaluate(()=>{
          const oldValue=localStorage.getItem('user');
          const newValue=JSON.stringify({id:'synthetic-admin',username:'fixture-admin',role:'viewer'});
          localStorage.setItem('user',newValue);
          window.dispatchEvent(new StorageEvent('storage',{key:'user',oldValue,newValue}));
        });
        await page.getByRole('heading',{name:'Access Denied',exact:true}).waitFor();
        assert.equal(await page.locator('main input, main form').count(),0);
        assert.equal(requests.length,beforeDemotion);
        assert.equal(await page.evaluate(()=>localStorage.getItem('session_token')),'synthetic-session-only');

      }
      assert.deepEqual(errors,[]);
    } finally {await context.close();}
  });
}
