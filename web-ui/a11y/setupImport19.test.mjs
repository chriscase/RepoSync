// Run from web-ui with REPOSYNC_A11Y_BROWSER set to a sandbox-capable Chromium:
// node --test --test-concurrency=1 a11y/setupImport19.test.mjs
import assert from 'node:assert/strict';
import test, { before, after } from 'node:test';
import { chromium } from 'playwright-core';
import { createServer } from 'vite';
import { progress, startFixture, syntheticUser } from './fixture.mjs';

let fixture, vite, browser, origin;
before(async () => {
  fixture = await startFixture();
  vite = await createServer({ server: { host: '127.0.0.1', port: 0, hmr: false, watch: null, proxy: { '/api': fixture.origin } } });
  await vite.listen();
  origin = new URL(vite.resolvedUrls.local[0]).origin;
  const executablePath = process.env.REPOSYNC_A11Y_BROWSER;
  assert.ok(executablePath, 'Set REPOSYNC_A11Y_BROWSER to an existing Chromium executable');
  browser = await chromium.launch({ executablePath, headless: true, chromiumSandbox: true });
});
after(async () => { await browser?.close(); await vite?.close(); await fixture?.close(); });

const first = 'fixture-operation-A';
const next = 'fixture-operation-B';
const originalRepository = 'fixture-original-repo';
function observation(operationId, overrides = {}) {
  return {
    ...progress(), operation_id: operationId, operation_repository_id: operationId ? originalRepository : undefined,
    can_cancel: true, can_start: false,
    last_local_svn_rev: 37, last_confirmed_svn_rev: 19,
    log_lines: [`Worker log for ${operationId}`], ...overrides,
  };
}
const json = (route, body) => route.fulfill({ json: body });
const stopButton = page => page.getByRole('button', { name: 'Cancel Import', exact: true });
const startButton = page => page.getByRole('button', { name: /Start Full Import|Restart Import|Retry Import|Start import for /i });
const cancelled = page => page.getByRole('heading', { name: 'Import Cancelled', exact: true });
async function noEnabledAction(button) {
  assert.ok(await button.count() === 0 || await button.isDisabled());
}

async function wizard(t, initial, { disconnected = false } = {}) {
  const context = await browser.newContext({ reducedMotion: 'reduce' });
  t.after(() => context.close());
  await context.addInitScript(user => {
    localStorage.setItem('user', JSON.stringify(user));
    localStorage.setItem('session_token', 'synthetic-session-only');
  }, syntheticUser);
  const model = { status: initial, reads: 0, writes: [], disconnected };
  const page = await context.newPage();
  page.setDefaultTimeout(10000);
  await context.route('**/*', async route => {
    const req = route.request(), url = new URL(req.url());
    if (url.origin !== origin) return route.abort();
    if (url.pathname === '/api/setup/import/status') {
      assert.equal(req.method(), 'GET');
      model.reads++;
      return model.disconnected ? route.abort('connectionreset') : json(route, model.status);
    }
    if (url.pathname.startsWith('/api/setup/') && req.method() !== 'GET') {
      const id = url.searchParams.get('operation_id');
      const target = url.pathname === '/api/setup/import'
        ? { repositoryId: url.searchParams.get('expected_repository_id') } : { id };
      model.writes.push({ method: req.method(), path: url.pathname, ...target });
      return model.write ? model.write(route, id) : json(route, { ok: false, message: 'Unexpected fixture write' });
    }
    return route.continue();
  });
  await page.goto(origin + '/setup');
  return { page, model };
}

for (const [phase, lifecycle] of [['connecting', 'queued'], ['importing', 'running'], ['verifying', 'running'], ['final_push', 'running']]) {
  test(`RS-19 can request a stop during ${phase} and waits for the worker to stop`, async t => {
    const { page, model } = await wizard(t, observation(first, { phase, lifecycle }));
    await stopButton(page).waitFor();
    model.write = (route, id) => {
      model.status = observation(first, { phase, lifecycle: 'cancel_requested', can_cancel: false });
      return json(route, { ok: true, operation_id: id, lifecycle: 'cancel_requested', message: 'Stop requested' });
    };
    await stopButton(page).click();
    await page.getByText(/Stop requested/i).first().waitFor();
    assert.deepEqual(model.writes, [{ method: 'POST', path: '/api/setup/import/cancel', id: first }]);
    await noEnabledAction(stopButton(page));
    await noEnabledAction(startButton(page));
    assert.equal(await cancelled(page).count(), 0);
    model.status = observation(first, { phase, lifecycle: 'cancelling', can_cancel: false, log_lines: ['Worker is cleaning up'] });
    await page.getByText('Worker is cleaning up', { exact: true }).waitFor();
    assert.equal(await cancelled(page).count(), 0);
    model.status = observation(first, { phase: 'cancelled', lifecycle: 'cancelled', busy: false, can_cancel: false });
    await cancelled(page).waitFor();
    if (phase === 'final_push') {
      await page.reload();
      await cancelled(page).waitFor();
      assert.deepEqual(model.writes, [{ method: 'POST', path: '/api/setup/import/cancel', id: first }]);
    }
    const checkpoints = page.locator('dl');
    assert.match(await checkpoints.locator('dt').filter({ hasText: /^Local progress$/ }).locator('+ dd').innerText(), /\br37\b/);
    assert.match(await checkpoints.locator('dt').filter({ hasText: /^Confirmed publication$/ }).locator('+ dd').innerText(), /\br19\b/);
    await page.getByText(/Published commits remain in place/i).first().waitFor();
    await noEnabledAction(startButton(page));
  });
}

for (const failure of ['ok=false', 'disconnect', 'wrong operation']) {
  test(`RS-19 ${failure} during a stop leaves the outcome unconfirmed`, async t => {
    const { page, model } = await wizard(t, observation(first));
    model.write = route => {
      if (failure === 'disconnect') return route.abort('connectionreset');
      if (failure === 'wrong operation') return json(route, { ok: true, operation_id: next, lifecycle: 'cancelled' });
      return json(route, { ok: false, message: 'The fixture refused this stop' });
    };
    await stopButton(page).click();
    const error = page.getByRole('alert').filter({ hasText: failure === 'ok=false' ? /fixture refused/ : /confirm/i });
    await error.first().waitFor();
    assert.equal(await cancelled(page).count(), 0);
    assert.equal(await page.getByText(/^Stop requested/i).count(), 0);
    assert.deepEqual(model.writes, [{ method: 'POST', path: '/api/setup/import/cancel', id: first }]);
  });
}

test('RS-19 fresh Welcome reports an unavailable status and finds the running import on reconnect', async t => {
  const { page, model } = await wizard(t, observation(first), { disconnected: true });
  const welcome = page.getByRole('heading', { name: 'Welcome to RepoSync', exact: true });
  await welcome.waitFor();
  await page.getByRole('alert').filter({ hasText: /import status/i }).waitFor();
  assert.ok(await welcome.isVisible());
  assert.equal(await stopButton(page).count(), 0);
  assert.deepEqual(model.writes, []);
  const failedReads = model.reads;
  assert.ok(failedReads > 0);

  model.disconnected = false;
  await page.getByText(`Operation ${first}`, { exact: true }).waitFor();
  await stopButton(page).waitFor();
  assert.ok(await stopButton(page).isEnabled());
  assert.equal(await welcome.count(), 0);
  assert.ok(model.reads > failedReads);
  assert.deepEqual(model.writes, []);
});

test('RS-19 unreadable polls hold actions, then the wizard recovers the same operation', async t => {
  const { page, model } = await wizard(t, observation(first));
  await stopButton(page).waitFor();
  model.disconnected = true;
  const error = page.getByRole('alert').filter({ hasText: /status/i }).first();
  await error.waitFor();
  await noEnabledAction(stopButton(page));
  await noEnabledAction(startButton(page));
  assert.equal(await cancelled(page).count(), 0);
  model.disconnected = false;
  await error.waitFor({ state: 'hidden' });
  assert.ok(await stopButton(page).isEnabled());
  assert.deepEqual(model.writes, []);
});

// Delay a completed read after capturing its bytes. Real abort signals are observable,
// but delivering the buffered response after abort also exercises stale-result rejection.
async function bufferNextRead(page) {
  await page.evaluate(() => {
    const original = window.fetch;
    let release;
    const gate = new Promise(resolve => { release = resolve; });
    const held = window.rs19Held = { captured: false, aborted: false, reads: 0, release };
    window.fetch = async function (input, init) {
      const url = new URL(input instanceof Request ? input.url : input, location.href);
      if (url.pathname !== '/api/setup/import/status') return original.call(this, input, init);
      held.reads++;
      const response = await original.call(this, input, init);
      if (held.captured) return response;
      const copy = new Response(await response.text(), { status: response.status, headers: response.headers });
      const signal = init?.signal ?? (input instanceof Request && input.signal);
      signal?.addEventListener('abort', () => { held.aborted = true; }, { once: true });
      held.captured = true;
      await gate;
      return copy;
    };
  });
  await page.waitForFunction(() => window.rs19Held.captured);
}

test('RS-19 serial polls and a late read cannot replace the observed successor', async t => {
  const { page, model } = await wizard(t, observation(first));
  await stopButton(page).waitFor();
  await bufferNextRead(page);
  // Keep the first read held across two regular 1.5 s polling periods.
  await page.waitForTimeout(3200);
  assert.equal(await page.evaluate(() => window.rs19Held.reads), 1);
  model.write = (route, id) => {
    model.status = observation(next);
    return json(route, { ok: true, operation_id: id, lifecycle: 'cancel_requested' });
  };
  await stopButton(page).click();
  const successor = page.getByText(`Worker log for ${next}`, { exact: true });
  await successor.waitFor();
  assert.ok(await page.evaluate(() => window.rs19Held.aborted));
  await page.evaluate(async () => { window.rs19Held.release(); await new Promise(requestAnimationFrame); });
  assert.ok(await successor.isVisible());
  assert.equal(await page.getByText(`Worker log for ${first}`, { exact: true }).count(), 0);
  assert.deepEqual(model.writes, [{ method: 'POST', path: '/api/setup/import/cancel', id: first }]);
});

test('RS-19 remount reads the successor after aborting a pending read on exit', async t => {
  const { page, model } = await wizard(t, observation(first));
  await stopButton(page).waitFor();
  await bufferNextRead(page);
  await page.getByRole('button', { name: 'Exit to Dashboard', exact: true }).click();
  await page.waitForURL(url => url.pathname === '/');
  assert.ok(await page.evaluate(() => window.rs19Held.aborted));
  model.status = observation(next);
  await page.goBack();
  const successor = page.getByText(`Worker log for ${next}`, { exact: true });
  await successor.waitFor();
  await page.evaluate(async () => { window.rs19Held.release(); await new Promise(requestAnimationFrame); });
  assert.ok(await successor.isVisible());
  await page.reload();
  await successor.waitFor();
  assert.deepEqual(model.writes, []);
});

test('RS-19 memory-only completion and a legacy worker without a durable ID cannot authorize writes', async t => {
  const { page, model } = await wizard(t, observation(first));
  await stopButton(page).waitFor();
  const reason = 'An older setup import could still hold this checkout.';
  model.status = observation(undefined, {
    can_cancel: false, lifecycle: undefined, start_blocked_reason: reason, log_lines: ['Unnamed legacy worker'],
  });
  await page.getByText('Unnamed legacy worker', { exact: true }).waitFor();
  await page.getByText(reason, { exact: true }).waitFor();
  await noEnabledAction(stopButton(page));
  model.status = observation(undefined, { phase: 'completed', lifecycle: undefined, busy: false, can_cancel: false, log_lines: ['Memory-only completion'] });
  await page.getByText('Memory-only completion', { exact: true }).waitFor();
  assert.equal(await page.getByRole('heading', { name: 'Import Complete', exact: true }).count(), 0);
  await noEnabledAction(startButton(page));
  assert.deepEqual(model.writes, []);
});

test('RS-19 names and pins the next eligible repository when it differs from the failed import', async t => {
  const { page, model } = await wizard(t, observation(first));
  await stopButton(page).waitFor();
  const eligible = { id: 'fixture-next-repo', name: 'Next synthetic repository' };
  model.status = observation(first, {
    phase: 'failed', lifecycle: 'failed', busy: false, can_cancel: false, can_start: true,
    operation_repository_id: originalRepository, start_repository_id: eligible.id, start_repository_name: eligible.name,
    errors: ['Fixture import failed'], log_lines: ['First fixture import failed'],
  });
  const startEligible = page.getByRole('button', { name: `Start import for ${eligible.name}`, exact: true });
  await startEligible.waitFor();
  assert.equal(await page.getByRole('button', { name: /Retry Import|Restart Import/i }).count(), 0);
  model.write = route => json(route, { ok: true, operation_id: next, lifecycle: 'queued', message: 'Replacement accepted' });
  await startEligible.click();
  // The first read after the receipt still reports the previous operation.
  await noEnabledAction(startButton(page));
  assert.equal(await page.getByRole('heading', { name: 'Import Complete', exact: true }).count(), 0);
  model.status = observation(next, { operation_repository_id: eligible.id });
  await page.getByText(`Worker log for ${next}`, { exact: true }).waitFor();
  await stopButton(page).waitFor();
  assert.deepEqual(model.writes, [{ method: 'POST', path: '/api/setup/import', repositoryId: eligible.id }]);
});

test('RS-19 a reconciliation hold survives reload without reporting failure or replaying a write', async t => {
  const { page, model } = await wizard(t, observation(first));
  await stopButton(page).waitFor();
  model.status = observation(first, {
    phase: 'failed', lifecycle: 'reconciliation_required', busy: false, can_cancel: false,
    outcome_detail: 'Fixture publication could not be confirmed',
    last_local_git_sha: 'a'.repeat(40), last_confirmed_git_sha: 'b'.repeat(40),
  });
  const held = page.getByRole('heading', { name: 'Reconciliation required', exact: true });
  await held.waitFor();
  await page.reload();
  await held.waitFor();
  await page.getByText('Fixture publication could not be confirmed', { exact: true }).waitFor();
  assert.equal(await page.getByRole('heading', { name: /Import (failed|complete|cancelled)/i }).count(), 0);
  const local = page.locator('dt').filter({ hasText: /^Local progress$/ }).locator('+ dd');
  const published = page.locator('dt').filter({ hasText: /^Confirmed publication$/ }).locator('+ dd');
  assert.ok((await local.innerText()).includes('a'.repeat(40)));
  assert.ok((await published.innerText()).includes('b'.repeat(40)));
  await noEnabledAction(stopButton(page));
  await noEnabledAction(startButton(page));
  assert.deepEqual(model.writes, []);
});

test('RS-19 interrupted legacy progress stays on fresh Welcome and is historical on the reopened Import step', async t => {
  const interrupted = observation(undefined, {
    phase: 'idle', historical_phase: 'importing', lifecycle: undefined, busy: false,
    can_cancel: false, can_start: true, start_repository_id: originalRepository,
    start_repository_name: 'Original synthetic repository', has_worker_progress: false,
    last_local_svn_rev: null, last_confirmed_svn_rev: null, log_lines: [],
  });
  async function noWorker(page) {
    assert.equal(await stopButton(page).count(), 0);
    assert.equal(await page.getByRole('progressbar', { name: /SVN import progress.*Setup import/i }).count(), 0);
    assert.equal(await page.getByRole('button', { name: /^Server Monitor\b/i }).count(), 0);
    assert.equal(await page.getByText(/^(Importing|Import in progress)$/i).count(), 0);
  }

  const fresh = await wizard(t, interrupted);
  await fresh.page.waitForLoadState('networkidle');
  assert.ok(fresh.model.reads > 0);
  await fresh.page.getByRole('heading', { name: 'Welcome to RepoSync', exact: true }).waitFor();
  assert.equal(await fresh.page.getByRole('heading', { name: 'Import SVN History', exact: true }).count(), 0);
  await noWorker(fresh.page);
  assert.deepEqual(fresh.model.writes, []);

  const { page, model } = await wizard(t, observation(first));
  await stopButton(page).waitFor();
  model.status = interrupted;
  await page.reload();
  await page.getByText(/Interrupted setup history.*last reported “Importing”.*outcome and publication are unknown/i).waitFor();
  await startButton(page).waitFor();
  assert.ok(await startButton(page).isEnabled());
  assert.equal(await page.getByText('Ready to begin the import process.', { exact: true }).count(), 0);
  await noWorker(page);
  assert.deepEqual(model.writes, []);
});
