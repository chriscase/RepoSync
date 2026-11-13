// #56 browser regression for the repository path-policy editor.
//
// Real Vite-served UI in an existing, admitted headless Chromium (REPOSYNC_A11Y_BROWSER;
// never installed here). Network is limited to two owned loopback servers. All /api
// traffic enters a policy API that implements the documented #56 contract for
// repository "P" (GET returns `path_policy` and no raw columns; PUT: absent keeps,
// null clears, string replaces; expected_updated_at CAS; version-1 save receipts) and
// forwards every other route unchanged to the existing synthetic fixture
// (web-ui/a11y/fixture.mjs). A single /api proxy key avoids Vite's config-merge order.
// Server-side validation and SQLite effects are covered by crates/web/tests/repo_path_policy_56.rs;
// this file proves what the browser sends and shows.
//
// Run from web-ui/: REPOSYNC_A11Y_BROWSER=/path/to/chromium node --test --test-concurrency=1 tests/path-policy-56.e2e.mjs
import test, { before, after, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { createServer, request as httpRequest } from 'node:http';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { createServer as createViteServer } from 'vite';
import { startFixture, repository } from '../a11y/fixture.mjs';

const require = createRequire(import.meta.url);
const root = fileURLToPath(new URL('..', import.meta.url));
const TOKEN = 'Bearer synthetic-session-only';
let fixture, policy, vite, browser, origin;
const pageErrors = [];

function readField(raw) {
  if (raw === null) return { state: 'unset' };
  try {
    const value = JSON.parse(raw);
    if (Array.isArray(value) && value.every(entry => typeof entry === 'string')) return { state: 'list', entries: value };
  } catch { /* malformed below */ }
  return { state: 'malformed', stored_bytes: Buffer.byteLength(raw), reason: 'stored value is not a JSON array of strings; the scheduler refuses to sync this repository until the value is replaced or explicitly cleared' };
}

/** Loopback-only stand-in for GET/PUT /api/repos/P and its receipt lookups; forwards the rest. */
async function startPolicyApi(fixtureOrigin) {
  const api = { requests: [], receipts: new Map(), hidePolicy: false, raw: { allowed: null, blocked: null }, settings: {} };
  api.reset = (allowed, blocked, { hidePolicy = false } = {}) => {
    const base = repository('C');
    delete base.allowed_paths; delete base.blocked_patterns;
    api.settings = { ...base, id: 'P', name: 'Policy fixture', updated_at: '2026-11-13T00:00:00.000Z' };
    api.raw = { allowed, blocked }; api.hidePolicy = hidePolicy; api.requests.length = 0; api.receipts.clear();
  };
  api.detail = () => ({ ...api.settings, ...(api.hidePolicy ? {} : { path_policy: { version: 1,
    allowed_paths: readField(api.raw.allowed), blocked_patterns: readField(api.raw.blocked),
    enforced_direction: 'git_to_svn', svn_to_git_filtering: 'not_applied', path_rewriting: 'none_read_only' } }) });
  api.puts = () => api.requests.filter(request => request.method === 'PUT' && request.path === '/api/repos/P');
  const server = createServer(async (req, res) => {
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    const text = Buffer.concat(chunks).toString();
    const reply = (code, data) => { const json = JSON.stringify(data); res.writeHead(code, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(json) }); res.end(json); };
    const path = new URL(req.url, 'http://127.0.0.1').pathname;
    const lookup = /^\/api\/(?:repos\/P\/save-requests|repository-save-requests)\/([^/]+)$/.exec(path);
    if (path !== '/api/repos/P' && !lookup) {
      const target = new URL(req.url, fixtureOrigin);
      const upstream = httpRequest(target, { method: req.method, headers: { ...req.headers, host: target.host } }, response => {
        res.writeHead(response.statusCode ?? 502, response.headers); response.pipe(res);
      });
      upstream.on('error', () => { if (!res.headersSent) res.writeHead(502); res.end(); });
      upstream.end(text);
      return;
    }
    if (req.headers.authorization !== TOKEN) return reply(401, { error: 'Synthetic session required' });
    let body = {}; try { body = text ? JSON.parse(text) : {}; } catch { return reply(400, { error: 'invalid JSON' }); }
    api.requests.push({ method: req.method, path, body, raw: text });
    if (lookup && req.method === 'GET') { const saved = api.receipts.get(lookup[1]); return saved ? reply(200, saved.receipt) : reply(404, { error: 'save request not found' }); }
    if (lookup) return reply(405, { error: 'Synthetic receipt lookups are GET only' });
    if (req.method === 'GET') return reply(200, api.detail());
    if (req.method !== 'PUT') return reply(405, { error: 'Synthetic policy API supports GET and PUT only' });
    const previous = api.receipts.get(body.save_request_id);
    if (previous) return previous.raw === text ? reply(200, { ...api.detail(), save_receipt: previous.receipt }) : reply(409, { error: 'save request reused with different content' });
    if (body.expected_updated_at && body.expected_updated_at !== api.settings.updated_at) return reply(409, { error: 'repository changed since it was read' });
    const columns = [['allowed_paths', 'allowed'], ['blocked_patterns', 'blocked']];
    for (const [column] of columns) {
      if (column in body && body[column] !== null && (typeof body[column] !== 'string' || readField(body[column]).state !== 'list')) {
        return reply(400, { error: `${column} not_array: must be a JSON array of strings; nothing was saved` });
      }
    }
    for (const [column, key] of columns) if (column in body) api.raw[key] = body[column];
    for (const setting of ['name', 'svn_url', 'svn_branch', 'svn_username', 'git_provider', 'git_api_url', 'git_repo', 'git_branch', 'sync_mode', 'poll_interval_secs', 'lfs_threshold_mb', 'auto_merge', 'enabled']) {
      if (setting in body) api.settings[setting] = body[setting];
    }
    api.settings.updated_at = new Date(Date.parse(api.settings.updated_at) + 1000).toISOString();
    const receipt = { version: 1, request_id: body.save_request_id, kind: 'configuration', repo_id: 'P', configuration_updated_at: api.settings.updated_at, credential_fields: [], propagated_repositories: 0 };
    if (body.save_request_id) api.receipts.set(body.save_request_id, { raw: text, receipt });
    return reply(200, { ...api.detail(), save_receipt: receipt });
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', () => { server.off('error', reject); resolve(); }); });
  api.origin = `http://127.0.0.1:${server.address().port}`;
  api.close = async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); };
  return api;
}

before(async () => {
  const executablePath = process.env.REPOSYNC_A11Y_BROWSER;
  assert.ok(executablePath, 'Set REPOSYNC_A11Y_BROWSER to an existing admitted Chromium executable. This test never installs a browser.');
  fixture = await startFixture();
  policy = await startPolicyApi(fixture.origin);
  vite = await createViteServer({ root, server: { host: '127.0.0.1', port: 0, proxy: {
    '/api': policy.origin,
    '/ws': { target: fixture.origin, ws: false },
  } } });
  await vite.listen();
  origin = Object.values(vite.resolvedUrls.local)[0];
  browser = await chromium.launch({ executablePath, headless: true, chromiumSandbox: true, timeout: 15000 });
}, { timeout: 30000 });
after(async () => { await browser?.close(); await vite?.close(); await policy?.close(); await fixture?.close(); });
beforeEach(() => { policy.reset('["src/","config/"]', '["*.exe"]'); });

async function open(path = '/repos/P') {
  const context = await browser.newContext({ reducedMotion: 'reduce' });
  await context.route('**/*', route => new URL(route.request().url()).origin === new URL(origin).origin ? route.continue() : route.abort());
  const page = await context.newPage(); page.on('pageerror', error => pageErrors.push(error.message)); page.setDefaultTimeout(8000);
  await page.goto(origin + 'login');
  await page.getByLabel('Username', { exact: true }).fill('fixture-admin');
  await page.getByLabel('Password', { exact: true }).fill('synthetic-only');
  await page.getByRole('button', { name: /Sign in/i }).press('Enter');
  await page.waitForURL(url => url.pathname !== '/login');
  await page.goto(origin + path.replace(/^\//, ''));
  await page.getByRole('heading', { name: 'Policy fixture', exact: true }).first().waitFor();
  return { page, close: () => context.close() };
}
async function reach(page, target) {
  for (let i = 0; i < 200; i++) { if (await target.evaluate(el => el === document.activeElement)) return; await page.keyboard.press('Tab'); }
  assert.fail('Keyboard Tab could not reach ' + await target.evaluate(el => el.outerHTML.slice(0, 120)));
}
async function press(page, target) { await reach(page, target); await page.keyboard.press('Enter'); }
async function audit(page, label) {
  await page.addScriptTag({ path: require.resolve('axe-core/axe.min.js') });
  const result = await page.evaluate(async () => window.axe.run(document.querySelector('[data-testid="path-policy-56"]'), { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'] } }));
  assert.deepEqual(result.violations.map(v => ({ id: v.id, nodes: v.nodes.map(n => n.target) })), [], label + ' axe violations');
}
const section = page => page.getByTestId('path-policy-56');
const field = (page, key) => page.getByTestId(`path-policy-${key}`);
async function startEditing(page) { await press(page, page.getByRole('button', { name: 'Edit', exact: true })); await page.getByRole('button', { name: 'Save', exact: true }).waitFor(); }
async function save(page, expectedPuts) {
  await press(page, page.getByRole('button', { name: 'Save', exact: true }));
  for (let i = 0; i < 80 && policy.puts().length < expectedPuts; i++) await new Promise(resolve => setTimeout(resolve, 50));
  assert.equal(policy.puts().length, expectedPuts, 'unexpected number of repository PUTs');
  await page.getByRole('button', { name: 'Edit', exact: true }).waitFor();
}

test('rs56 unrelated settings save omits both policy fields and the reread policy is preserved', { timeout: 45000 }, async () => {
  const { page, close } = await open(); try {
    await field(page, 'allowed_paths').getByText('2 allowed path prefixes', { exact: true }).waitFor();
    await startEditing(page);
    const name = page.getByLabel('Name', { exact: true }); await reach(page, name);
    await name.fill('Policy fixture renamed');
    await save(page, 1);
    const body = policy.puts()[0].body;
    assert.equal(body.name, 'Policy fixture renamed');
    assert.equal('allowed_paths' in body, false); assert.equal('blocked_patterns' in body, false);
    assert.deepEqual(policy.raw, { allowed: '["src/","config/"]', blocked: '["*.exe"]' });
    await field(page, 'allowed_paths').getByText('2 allowed path prefixes', { exact: true }).waitFor();
    await field(page, 'blocked_patterns').getByText('1 blocked pattern', { exact: true }).waitFor();
  } finally { await close(); }
});

test('rs56 keyboard edit: invalid entry blocks save inline, valid replacement then explicit clear', { timeout: 60000 }, async () => {
  const { page, close } = await open(); try {
    await startEditing(page);
    const allowed = field(page, 'allowed_paths');
    await press(page, allowed.getByRole('button', { name: 'Edit allowed path prefixes', exact: true }));
    await press(page, allowed.getByRole('button', { name: 'Add allowed path prefix', exact: true }));
    const added = allowed.getByLabel('Allowed path prefix 3', { exact: true });
    assert.ok(await added.evaluate(el => el === document.activeElement), 'Add must move focus to the new entry');
    await page.keyboard.type('../outside');
    assert.equal(await added.getAttribute('aria-invalid'), 'true');
    const errorId = await added.getAttribute('aria-describedby');
    assert.match(await page.locator(`[id="${errorId}"]`).textContent(), /'\.' or '\.\.'/);
    assert.equal(await allowed.getByRole('button', { name: 'Hide allowed path prefixes', exact: true }).isDisabled(), true);
    await audit(page, 'inline policy validation');
    await press(page, page.getByRole('button', { name: 'Save', exact: true }));
    await page.getByRole('alert').filter({ hasText: 'Nothing was saved. Fix 1 path policy entry first' }).waitFor();
    assert.equal(policy.puts().length, 0, 'an invalid list must never be sent');

    await added.fill('team/docs');
    assert.equal(await added.getAttribute('aria-invalid'), null);
    await save(page, 1);
    assert.equal(policy.puts()[0].body.allowed_paths, '["src/","config/","team/docs"]');
    assert.equal('blocked_patterns' in policy.puts()[0].body, false);
    await allowed.getByText('3 allowed path prefixes', { exact: true }).waitFor();

    await startEditing(page);
    const blocked = field(page, 'blocked_patterns');
    await press(page, blocked.getByRole('button', { name: 'Clear stored blocked patterns', exact: true }));
    await blocked.getByText('Will be cleared on save', { exact: true }).waitFor();
    await save(page, 2);
    assert.deepEqual(Object.keys(policy.puts()[1].body).filter(key => key.endsWith('_paths') || key.endsWith('_patterns')), ['blocked_patterns']);
    assert.equal(policy.puts()[1].body.blocked_patterns, null);
    assert.deepEqual(policy.raw, { allowed: '["src/","config/","team/docs"]', blocked: null });
    await blocked.getByText('No blocked patterns', { exact: true }).waitFor();
  } finally { await close(); }
});

test('rs56 malformed stored value is kept by unrelated saves and changes only by explicit Clear', { timeout: 45000 }, async () => {
  policy.reset('src/', '["*.exe"]');
  const { page, close } = await open(); try {
    const allowed = field(page, 'allowed_paths');
    await allowed.getByText('Stored value is malformed — unchanged on save', { exact: true }).waitFor();
    await section(page).getByText(/The stored allowed path prefixes value is malformed/).waitFor();
    await startEditing(page);
    await page.getByLabel('Name', { exact: true }).fill('Malformed kept');
    await save(page, 1);
    assert.equal('allowed_paths' in policy.puts()[0].body, false);
    assert.equal(policy.raw.allowed, 'src/');
    await page.getByText('Malformed kept', { exact: true }).first().waitFor(); // refetched before the next edit
    await startEditing(page);
    await press(page, allowed.getByRole('button', { name: 'Clear stored allowed path prefixes', exact: true }));
    await save(page, 2);
    assert.equal(policy.puts()[1].body.allowed_paths, null);
    assert.equal('blocked_patterns' in policy.puts()[1].body, false);
    await allowed.getByText('No allowed-path restriction', { exact: true }).waitFor();
  } finally { await close(); }
});

test('rs56 server without path_policy: policy controls are unavailable and saves omit both fields', { timeout: 45000 }, async () => {
  policy.reset('["src/"]', '["*.exe"]', { hidePolicy: true });
  const { page, close } = await open(); try {
    await startEditing(page);
    assert.equal(await section(page).getByText('Not available from this server — unchanged on save', { exact: true }).count(), 2);
    assert.equal(await section(page).getByRole('button').count(), 0);
    assert.equal(await page.getByRole('link', { name: 'Git Hook' }).count(), 0);
    await page.getByLabel('Name', { exact: true }).fill('Old server rename');
    await save(page, 1);
    const body = policy.puts()[0].body;
    assert.equal('allowed_paths' in body, false); assert.equal('blocked_patterns' in body, false);
    assert.deepEqual(policy.raw, { allowed: '["src/"]', blocked: '["*.exe"]' });
  } finally { await close(); }
});

test('rs56 keyboard Remove moves focus, saves only that change, and the actual reread preserves unrelated fields', { timeout: 60000 }, async () => {
  const before = policy.detail();
  const formFields = ['name', 'svn_url', 'svn_branch', 'svn_username', 'git_provider', 'git_api_url', 'git_repo', 'git_branch', 'sync_mode', 'poll_interval_secs', 'lfs_threshold_mb', 'auto_merge'];
  const { page, close } = await open(); try {
    await startEditing(page);
    const allowed = field(page, 'allowed_paths');
    await press(page, allowed.getByRole('button', { name: 'Edit allowed path prefixes', exact: true }));
    await press(page, allowed.getByRole('button', { name: 'Remove allowed path prefix 2', exact: true }));
    const first = allowed.getByLabel('Allowed path prefix 1', { exact: true });
    assert.ok(await first.evaluate(el => el === document.activeElement), 'Remove must move focus to the previous entry');
    assert.equal(await first.inputValue(), 'src/');
    assert.equal(await allowed.getByLabel('Allowed path prefix 2', { exact: true }).count(), 0);
    await allowed.getByText('1 allowed path prefix — will replace the stored value on save', { exact: true }).waitFor();

    // Removing the only blocked pattern moves focus to Add; Undo returns the field to keep.
    const blocked = field(page, 'blocked_patterns');
    await press(page, blocked.getByRole('button', { name: 'Edit blocked patterns', exact: true }));
    await press(page, blocked.getByRole('button', { name: 'Remove blocked pattern 1', exact: true }));
    assert.ok(await blocked.getByRole('button', { name: 'Add blocked pattern', exact: true }).evaluate(el => el === document.activeElement),
      'removing the last entry must move focus to Add');
    await blocked.getByText('No entries — will be cleared on save', { exact: true }).waitFor();
    await press(page, blocked.getByRole('button', { name: 'Undo blocked patterns changes', exact: true }));
    await blocked.getByText('1 blocked pattern', { exact: true }).waitFor();

    await save(page, 1);
    const body = policy.puts()[0].body;
    assert.equal(body.allowed_paths, '["src/"]');
    assert.equal('blocked_patterns' in body, false, 'an undone field must not be submitted');
    for (const key of formFields) assert.deepEqual(body[key], before[key], `submitted ${key} unchanged`);

    // Actual reread of the stored repository, then the page's own refetch.
    const response = await fetch(policy.origin + '/api/repos/P', { headers: { authorization: TOKEN } });
    assert.equal(response.status, 200);
    const reread = await response.json();
    assert.deepEqual(reread.path_policy.allowed_paths, { state: 'list', entries: ['src/'] });
    assert.deepEqual(reread.path_policy.blocked_patterns, { state: 'list', entries: ['*.exe'] });
    for (const key of [...formFields, 'enabled', 'parent_id', 'created_at']) assert.deepEqual(reread[key], before[key], `reread ${key} preserved`);
    assert.notEqual(reread.updated_at, before.updated_at);
    await allowed.getByText('1 allowed path prefix', { exact: true }).waitFor();
    await blocked.getByText('1 blocked pattern', { exact: true }).waitFor();
  } finally { await close(); }
});

test('rs56 collapsed lists and Git→SVN preview are keyboard operable and match engine semantics', { timeout: 45000 }, async () => {
  const { page, close } = await open(); try {
    const toggle = field(page, 'allowed_paths').getByRole('button', { name: 'Show allowed path prefixes', exact: true });
    assert.equal(await toggle.getAttribute('aria-expanded'), 'false');
    await press(page, toggle);
    const shown = field(page, 'allowed_paths').getByRole('button', { name: 'Hide allowed path prefixes', exact: true });
    assert.equal(await shown.getAttribute('aria-expanded'), 'true');
    assert.equal(await page.locator(`[id="${await shown.getAttribute('aria-controls')}"]`).isVisible(), true);
    const samples = section(page).getByLabel('Sample Git paths (one per line)', { exact: true });
    await reach(page, samples);
    // The last two lines differ from earlier ones only by whitespace, which is part of a filename.
    const typed = 'src/main.rs\nsrc/tool.exe\nsrc-other/a.txt\nconfig\\app.toml\n src/lead.rs\nsrc/tool.exe ';
    await page.keyboard.type(typed);
    assert.equal(await samples.inputValue(), typed);
    const rows = section(page).getByTestId('path-policy-preview').locator('tbody tr');
    const marked = 'Includes leading or trailing whitespace';
    assert.deepEqual(await rows.evaluateAll(trs => trs.map(tr => [
      tr.cells[0].querySelector('code').textContent, tr.cells[1].textContent, tr.cells[0].querySelector('span')?.textContent ?? null])), [
      ['src/main.rs', 'Included in Git→SVN', null],
      ['src/tool.exe', 'Excluded: matches blocked pattern *.exe', null],
      ['src-other/a.txt', 'Excluded: not under an allowed path prefix', null],
      ['config\\app.toml', 'Included in Git→SVN', null],
      [' src/lead.rs', 'Excluded: not under an allowed path prefix', marked],
      ['src/tool.exe ', 'Included in Git→SVN', marked],
    ]);
    await section(page).getByRole('status').filter({ hasText: '3 of 6 sample paths included in Git→SVN. SVN→Git is not filtered in this build.' }).waitFor();
    await section(page).getByText(/SVN→Git imports every path on the SVN branch in this build/).waitFor();
    assert.equal(policy.puts().length, 0, 'preview must not write');
    await audit(page, 'path policy read-only preview');
    assert.deepEqual(pageErrors, []);
  } finally { await close(); }
});
