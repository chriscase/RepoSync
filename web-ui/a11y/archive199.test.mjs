// #199 browser contract only. All API effects are synthetic and in-memory.
import test, { before, after } from 'node:test';
import assert from 'node:assert/strict';
import { chromium } from 'playwright-core';
import { createServer as createViteServer } from 'vite';
import { startFixture, repository } from './fixture.mjs';
let fixture, vite, browser, origin;
before(async () => {
  const executablePath = process.env.REPOSYNC_A11Y_BROWSER;
  assert.ok(executablePath, 'An existing admitted Chromium executable is required.');
  fixture = await startFixture();
  vite = await createViteServer({ server: { host: '127.0.0.1', port: 0, proxy: { '/api': fixture.origin } } });
  await vite.listen(); origin = vite.resolvedUrls.local[0];
  browser = await chromium.launch({ executablePath, headless: true, chromiumSandbox: true, timeout: 15000 });
  console.log('Existing browser', browser.version(), 'sandbox enabled; synthetic API only');
}, { timeout: 30000 });
after(async () => { await browser?.close(); await vite?.close(); await fixture?.close(); });
for (const side of ['git', 'svn']) test(`archive ${side} side keeps detail and readonly history after reload`, { timeout: 45000 }, async () => {
  const context = await browser.newContext({ viewport: { width: side === 'git' ? 1280 : 600, height: 900 } });
  const page = await context.newPage(); page.setDefaultTimeout(8000);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  let archived = false, receipt; const approvals = [];
  await context.route('**/*', async route => {
    const req = route.request(), url = new URL(req.url());
    if (url.origin !== new URL(origin).origin) return route.abort();
    const json = data => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(data) });
    if (url.pathname === '/api/repos/B/removal/review') {
      const response = await route.fetch(); const data = await response.json();
      const choices = req.postDataJSON();
      return json({ ...data, archive_pair: choices.archive_pair ?? false, review_pin: `${choices.delete_git ? 'a' : choices.delete_svn ? 'b' : 'c'}`.repeat(64) });
    }
    if (url.pathname === '/api/repos/B/removal/approve') {
      const body = req.postDataJSON(); approvals.push(body);
      assert.equal(body.archive_pair, true); assert.equal(body.delete_git, side === 'git'); assert.equal(body.delete_svn, side === 'svn');
      archived = true;
      receipt = { ok: true, action: 'managed_remove', protocol: 2, archive_pair: true, state: 'completed', operation_id: 'synthetic-archive-'+side, message: 'Pair archived; survivor and history retained', remote_git: side === 'git' ? 'deleted' : 'untouched', remote_svn: side === 'svn' ? 'deleted' : 'untouched', restore_supported: false, retryable: false, registration_listed: true,
        reviewed: { git: { phase: side === 'git' ? 'verified_deleted' : 'not_requested', errors: [] }, svn: { phase: side === 'svn' ? 'verified_deleted' : 'not_requested', errors: [] }, cleanup_started: true, cleanup_completed: true, cleanup_errors: [] } };
      return json(receipt);
    }
    if (archived && url.pathname === '/api/repos/B') return json({ ...repository('B'), sync_status: 'archived', enabled: false });
    if (archived && url.pathname === '/api/repos/B/removal') return json(receipt);
    return route.continue();
  });
  try {
    await page.goto(origin+'login');
    await page.getByLabel('Username', { exact: true }).fill('fixture-admin');
    await page.getByLabel('Password', { exact: true }).fill('synthetic-only');
    await page.getByRole('button', { name: /Sign in/i }).click();
    await page.waitForURL(url => url.pathname !== '/login');
    await page.goto(origin+'repos/B?section=settings');
    await page.getByTestId('delete-viewed-branch-pair').click();
    await page.getByTestId('delete-'+side+'-opt').check();
    const modal = page.getByRole('dialog');
    await modal.getByText(/after deleting the selected side/).waitFor();
    await page.getByTestId('delete-branch-confirm-input').fill(side === 'git' ? 'feature/synthetic' : 'branches/feature');
    await page.getByTestId('confirm-delete-branch-pair').click();
    await page.getByTestId('archived-pair-notice').waitFor();
    assert.equal(new URL(page.url()).pathname, '/repos/B');
    assert.equal(approvals.length, 1);
    assert.equal(await page.getByTestId('header-sync-switch').isDisabled(), true);
    assert.equal(await page.getByTestId('delete-viewed-branch-pair').count(), 0);
    assert.equal(await page.getByRole('button', { name: 'Edit', exact: true }).isDisabled(), true);
    await page.reload(); await page.getByTestId('archived-pair-notice').waitFor();
    assert.equal(await page.getByTestId('header-sync-switch').isDisabled(), true);
    assert.equal(approvals.length, 1);
    await page.goto(origin+'repos/B?section=activity');
    await page.getByRole('heading', { name: /Recent Audit Log/ }).waitFor();
    assert.deepEqual(errors, []);
  } finally { await context.close(); }
});
