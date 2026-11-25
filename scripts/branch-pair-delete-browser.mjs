// Browser coverage for safe navigation after deleting the viewed branch pair.
// Happy-path cases hit a real local API when REPOSYNC_REAL_API is set.
// Failure, queued, and stale-URL cases use a deterministic in-process API.
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { readFile, mkdir } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import net from 'node:net';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const uiDir = join(root, 'web-ui');
const viteBin = join(uiDir, 'node_modules/vite/bin/vite.js');
const token = process.env.REPOSYNC_UI_TOKEN || 'branch-pair-delete-browser-token';
const realApi = process.env.REPOSYNC_REAL_API || '';
const parentId = process.env.REPOSYNC_PARENT_ID || '';
const viewedId = process.env.REPOSYNC_VIEWED_PAIR_ID || '';
const listedId = process.env.REPOSYNC_LISTED_PAIR_ID || '';
const viewedBranch = process.env.REPOSYNC_VIEWED_GIT_BRANCH || '';
const listedBranch = process.env.REPOSYNC_LISTED_GIT_BRANCH || '';

const children = [];
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function track(child) {
  children.push(child);
  return child;
}

function stopAll() {
  for (const child of children) {
    try { child.kill('SIGKILL'); } catch { /* already gone */ }
  }
}
process.on('exit', stopAll);
process.on('SIGINT', () => { stopAll(); process.exit(1); });

function chromeBin() {
  const fromEnv = process.env.CHROME_BIN;
  if (fromEnv) return fromEnv;
  const candidates = ['google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser'];
  return candidates[0];
}

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
  return port;
}

function startVite(apiUrl) {
  return freePort().then((port) => {
    const child = track(spawn(process.execPath, [viteBin, '--host', '127.0.0.1', '--strictPort', '--port', String(port)], {
      cwd: uiDir,
      env: { ...process.env, REPOSYNC_TEST_API_URL: apiUrl },
      stdio: 'ignore',
    }));
    return { child, port };
  });
}

async function waitForHttp(url, label) {
  const end = Date.now() + 20000;
  let last = 'no response';
  while (Date.now() < end) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
      last = `status ${response.status}`;
    } catch (error) {
      last = error instanceof Error ? error.message : String(error);
    }
    await pause(100);
  }
  throw new Error(`Timed out waiting for ${label}: ${last}`);
}

function removalDependencyPreview(record) {
  const parent = record.parent_id
    ? { id: record.parent_id, name: record.parent_id }
    : null;
  return {
    repo_id: record.id,
    repo_name: record.name,
    parent,
    children: [],
    parent_removal_blocked: false,
    block_reason: null,
    credentials: [],
    managed_local_path: `repos/${record.id}`,
    sibling_local_paths_preserved: [],
    shared_git_registrations: [],
  };
}

function repo(id, name, parent, gitBranch) {
  const now = '2026-01-01T00:00:00Z';
  return {
    id,
    name,
    parent_id: parent,
    svn_url: 'https://svn.test.invalid/repo',
    svn_branch: parent ? `branches/${gitBranch}` : 'trunk',
    svn_username: 'fixture',
    git_provider: 'github',
    git_api_url: 'https://git.test.invalid',
    git_repo: 'test/repo',
    git_branch: gitBranch,
    sync_mode: 'team',
    poll_interval_secs: 60,
    lfs_threshold_mb: 1,
    auto_merge: false,
    enabled: true,
    created_by: null,
    created_at: now,
    updated_at: now,
    allowed_paths: null,
    blocked_patterns: null,
    consecutive_errors: 0,
    sync_status: 'idle',
    status: 'idle',
    initializing: false,
  };
}

function startMock() {
  const deleted = new Set();
  const receipts = new Map();
  const records = new Map([
    ['parent-1', repo('parent-1', 'Parent trunk', null, 'main')],
    ['child-stay', repo('child-stay', 'Stay Child', 'parent-1', 'stay-branch')],
    ['child-cancel', repo('child-cancel', 'Cancel Child', 'parent-1', 'cancel-branch')],
    ['child-error', repo('child-error', 'Error Child', 'parent-1', 'error-branch')],
    ['child-denied', repo('child-denied', 'Denied Child', 'parent-1', 'denied-branch')],
    ['child-busy', repo('child-busy', 'Busy Child', 'parent-1', 'busy-branch')],
    ['child-held', repo('child-held', 'Held Child', 'parent-1', 'held-branch')],
    ['child-orphan', repo('child-orphan', 'Orphan Child', 'missing-parent', 'orphan-branch')],
    ['child-queued', repo('child-queued', 'Queued Child', 'parent-1', 'queued-branch')],
    ['child-delayed', repo('child-delayed', 'Delayed Child', 'parent-1', 'delayed-branch')],
    ['child-warn', repo('child-warn', 'Warn Child', 'parent-1', 'warn-branch')],
    ['child-stay-warn', repo('child-stay-warn', 'Stay Warn Child', 'parent-1', 'stay-warn-branch')],
    ['child-queued-gone', repo('child-queued-gone', 'Queued Gone Child', 'parent-1', 'queued-gone-branch')],
  ]);

  const server = createServer(async (req, res) => {
    const url = new URL(req.url || '/', 'http://127.0.0.1');
    const path = url.pathname;
    const send = (status, body) => {
      res.writeHead(status, { 'content-type': 'application/json' });
      res.end(JSON.stringify(body));
    };
    const emptyList = { entries: [], total: 0 };
    if (path === '/api/status' || path === '/api/status/health') {
      send(200, {
        ok: true,
        state: 'idle',
        last_sync_at: null,
        last_svn_revision: null,
        last_git_hash: null,
        total_syncs: 0,
        total_conflicts: 0,
        active_conflicts: 0,
        total_errors: 0,
        last_error_at: null,
        uptime_secs: 1,
      });
      return;
    }
    if (path === '/api/status/system') {
      send(200, {
        disk_free_bytes: 1, disk_total_bytes: 2, disk_usage_percent: 0,
        mem_used_bytes: 1, mem_total_bytes: 2, mem_usage_percent: 0,
        cpu_load_1m: 0, cpu_load_5m: 0, cpu_load_15m: 0,
        git_push_active: false, git_push_pid: null, git_push_elapsed_secs: null,
        data_dir_size_bytes: 0, net_bytes_sent: 0, net_bytes_recv: 0,
        net_up_bytes_per_sec: 0, net_down_bytes_per_sec: 0, svn_active: false,
      });
      return;
    }
    if (path === '/api/sync-records' || path === '/api/commit-map' || path === '/api/audit') {
      send(200, emptyList);
      return;
    }
    if (path === '/api/repos' && req.method === 'GET') {
      send(200, [...records.values()].filter((item) => !deleted.has(item.id)));
      return;
    }
    const repoMatch = path.match(/^\/api\/repos\/([^/]+)(.*)$/);
    if (!repoMatch) {
      send(404, { error: 'repository not found' });
      return;
    }
    const id = decodeURIComponent(repoMatch[1]);
    const rest = repoMatch[2] || '';
    if (rest === '/removal' && req.method === 'GET') {
      const receipt = receipts.get(id);
      send(receipt ? 200 : 404, receipt ?? { error: 'no removal operation' });
      return;
    }
    if (req.method === 'POST' && ['/removal/review', '/removal/approve'].includes(rest)) {
      const chunks = [];
      for await (const chunk of req) chunks.push(chunk);
      const opts = JSON.parse(Buffer.concat(chunks).toString());
      const record = records.get(id);
      if (!record || deleted.has(id)) return send(404, { error: 'repository not found' });
      const { delete_git: deleteGit, delete_svn: deleteSvn, archive_pair: archive } = opts;
      if (typeof deleteGit !== 'boolean' || typeof deleteSvn !== 'boolean' || archive !== (deleteGit !== deleteSvn)) {
        return send(400, { error: 'explicit remote choices required' });
      }
      const pin = createHash('sha256').update(JSON.stringify([id, deleteGit, deleteSvn, archive])).digest('hex');
      if (rest === '/removal/review') {
        return send(200, {
          protocol: 2, repo_id: id, review_pin: pin, delete_git: deleteGit, delete_svn: deleteSvn, archive_pair: archive,
          git: deleteGit ? { remote_url: record.git_api_url, reference: `refs/heads/${record.git_branch}`, expected_sha: 'a'.repeat(40) } : null,
          svn: deleteSvn ? { root_url: record.svn_url, uuid: 'fixture', url: `${record.svn_url}/${record.svn_branch}`, base_revision: 1, expected_present: true } : null,
          dependency_preview: removalDependencyPreview(record), retention: 'Fixture recovery retained.',
        });
      }
      if (opts.review_pin !== pin || opts.request_id !== `ui-remove-v2-${pin}`) {
        return send(409, { request_rejected: true, error: 'approval does not match the reviewed choices' });
      }
      if (id === 'child-error') {
        send(500, { error: 'internal server error' });
        return;
      }
      if (id === 'child-denied') {
        send(401, { error: 'admin access required' });
        return;
      }
      if (id === 'child-busy') {
        send(400, { error: 'branch pair is currently busy (sync or import in progress)' });
        return;
      }
      if (id === 'child-delayed') {
        await pause(1500);
      }
      const queued = id === 'child-queued' || id === 'child-queued-gone';
      const held = id === 'child-held';
      const listed = held || id === 'child-queued';
      if (!listed) deleted.add(id);
      const warnings = ['child-warn', 'child-stay-warn'].includes(id) && deleteGit
        ? [`failed to delete Git branch '${record.git_branch}'`]
        : [];
      const receipt = {
        ok: !queued && !held, action: 'managed_remove', protocol: 2,
        state: held ? 'reconciliation_required' : queued ? 'queued' : 'completed', registration_listed: listed,
        operation_id: queued ? (listed ? 'op-queue-70' : 'op-queue-gone-70') : `op-${id}`,
        review_pin: pin, request_id: opts.request_id, archive_pair: archive,
        remote_git: deleteGit ? (warnings.length ? 'failed' : 'deleted') : 'untouched',
        remote_svn: deleteSvn ? 'deleted' : 'untouched',
        message: held ? 'removal held for reconciliation' : queued ? 'removal queued' : `Branch pair '${id}' removed`, warnings,
        restore_supported: !queued && !held, retryable: queued,
      };
      receipts.set(id, receipt);
      send(held ? 409 : queued ? 202 : 200, receipt);
      return;
    }
    if (deleted.has(id) || id === 'already-removed' || id === 'missing-parent' || !records.has(id)) {
      send(404, { error: 'repository not found' });
      return;
    }
    if (rest === '/branches') {
      const childrenOf = [...records.values()].filter((item) => item.parent_id === id && !deleted.has(item.id));
      send(200, childrenOf);
      return;
    }
    if (rest === '/removal/preview' && req.method === 'GET') {
      if (deleted.has(id) || !records.has(id)) {
        send(404, { error: 'repository not found' });
        return;
      }
      send(200, {
        ok: true,
        action: 'removal_preview',
        dependency_preview: removalDependencyPreview(records.get(id)),
        active_removal: null,
      });
      return;
    }
    if (rest === '/credentials') {
      send(200, { svn_password_set: false, git_token_set: false });
      return;
    }
    if (rest === '/import/status') {
      send(200, {
        phase: 'idle',
        current_rev: 0,
        total_revs: 0,
        commits_created: 0,
        current_file_count: 0,
        lfs_unique_count: 0,
        files_skipped: 0,
        batches_pushed: 0,
        started_at: null,
        can_start: false,
      });
      return;
    }
    if (rest === '' && req.method === 'GET') {
      send(200, records.get(id));
      return;
    }
    send(200, { ok: true });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      resolve({ server, url: `http://127.0.0.1:${port}` });
    });
  });
}

async function connectChrome() {
  const profile = join(tmpdir(), `reposync-pair-delete-${process.pid}`);
  await mkdir(profile, { recursive: true });
  const chrome = track(spawn(chromeBin(), [
    '--headless=new', '--no-sandbox', '--disable-gpu', '--no-first-run',
    '--no-default-browser-check', '--remote-debugging-port=0', '--remote-allow-origins=*',
    `--user-data-dir=${profile}`,
  ], { stdio: 'ignore' }));
  const portFile = join(profile, 'DevToolsActivePort');
  const end = Date.now() + 15000;
  while (!existsSync(portFile)) {
    if (Date.now() > end) throw new Error('Chrome DevTools port did not appear');
    if (chrome.exitCode != null) throw new Error(`Chrome exited ${chrome.exitCode}`);
    await pause(50);
  }
  const [port] = (await readFile(portFile, 'utf8')).split('\n');
  const target = await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' }).then((r) => r.json());
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  const pending = new Map();
  let serial = 0;
  ws.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    if (!message.id) return;
    const pair = pending.get(message.id);
    if (!pair) return;
    pending.delete(message.id);
    if (message.error) pair.reject(new Error(JSON.stringify(message.error)));
    else pair.resolve(message.result);
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++serial;
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async (expression) => {
    const response = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
    return response.result?.value;
  };
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Page.addScriptToEvaluateOnNewDocument', { source: `
    localStorage.setItem('session_token', ${JSON.stringify(token)});
    localStorage.setItem('user', JSON.stringify({id:'fixture-admin',username:'fixture',role:'admin'}));
    window.__pairFetches = [];
    const originalFetch = window.fetch.bind(window);
    window.fetch = async (...args) => {
      const path = String(args[0]);
      const method = String(args[1]?.method || 'GET').toUpperCase();
      const entry = { path, method, started: Date.now() };
      window.__pairFetches.push(entry);
      if (method === 'POST' && path.endsWith('/removal/approve')) entry.approval = JSON.parse(args[1].body);
      const response = await originalFetch(...args);
      if (method === 'POST' && path.endsWith('/removal/review') && response.ok) entry.review = await response.clone().json();
      return response;
    };
  ` });
  return { send, evaluate, ws, chrome };
}

async function runScenarios(uiOrigin, session, mode) {
  const { send, evaluate } = session;
  const results = [];
  const body = () => evaluate('document.body?.innerText || ""');
  const pathOf = () => evaluate('location.pathname');
  const until = async (fn, label, ms = 15000) => {
    const end = Date.now() + ms;
    let last = '';
    while (Date.now() < end) {
      try {
        const value = await fn();
        if (value) return value;
      } catch (error) {
        last = error instanceof Error ? error.message : String(error);
      }
      await pause(100);
    }
    throw new Error(`Timed out: ${label}; path=${await pathOf().catch(() => '?')}; body=${(await body().catch(() => last)).slice(0, 500)}`);
  };
  const click = async (testId, descendant = '') => {
    const clicked = await evaluate(`(() => {
      const el = document.querySelector(${JSON.stringify(`[data-testid="${testId}"]${descendant}`)});
      if (!el) return false;
      el.click();
      return true;
    })()`);
    if (!clicked) throw new Error(`Missing [data-testid=${testId}]${descendant}\n${await body()}`);
  };
  const goto = async (pathname, ready) => {
    await send('Page.navigate', { url: `${uiOrigin}${pathname}` });
    await until(async () => (await pathOf()) === pathname && (!ready || await ready()), `open ${pathname}`);
  };
  const fill = async (gitBranch) => {
    await evaluate(`(() => {
      const input = document.querySelector('[data-testid="delete-branch-confirm-input"]');
      const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
      setter.call(input, ${JSON.stringify(gitBranch)});
      input.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    })()`);
    await until(async () => (await evaluate(`document.querySelector('[data-testid="confirm-delete-branch-pair"]')?.disabled === false`)), 'confirm enabled');
  };
  const clickCheckboxIfNeeded = async (testId, wantChecked) => {
    const toggled = await evaluate(`(() => {
      const el = document.querySelector('[data-testid="${testId}"]');
      if (!el) return 'missing';
      if (el.checked === ${wantChecked ? 'true' : 'false'}) return 'ok';
      el.click();
      return el.checked === ${wantChecked ? 'true' : 'false'} ? 'ok' : 'still-wrong';
    })()`);
    if (toggled === 'missing') throw new Error(`Missing checkbox ${testId}`);
    if (toggled !== 'ok') throw new Error(`Checkbox ${testId} did not reach checked=${wantChecked}`);
  };
  const setRemoteDeletion = async (enabled) => {
    await clickCheckboxIfNeeded('delete-git-opt', enabled);
    await clickCheckboxIfNeeded('delete-svn-opt', enabled);
    if (enabled) {
      const preview = await evaluate(`document.querySelector('[data-testid="branch-remote-deletion-preview"]')?.innerText || ''`);
      if (!preview.includes('will be deleted')) {
        throw new Error(`Remote opt-in preview did not show deletion: ${preview.slice(0, 300)}`);
      }
    }
  };
  const removalWrites = () => evaluate(`window.__pairFetches.filter(entry =>
    entry.method === 'DELETE' || (entry.method === 'POST' && (entry.path.endsWith('/removal/approve') || entry.path.endsWith('/removal/continue'))))`);
  const confirmDelete = async (id, gitBranch, { remote = false } = {}) => {
    await until(() => evaluate(`!!document.querySelector('[data-testid="delete-branch-confirm-input"]')`), 'confirmation dialog');
    await setRemoteDeletion(remote);
    await fill(gitBranch);
    const before = (await removalWrites()).length;
    await click('confirm-delete-branch-pair');
    await until(async () => (await removalWrites()).length > before, 'reviewed removal approval recorded');
    const writes = (await removalWrites()).slice(before);
    const approval = writes[0]?.approval;
    if (writes.length !== 1 || writes[0].path !== `/api/repos/${id}/removal/approve`
      || approval?.delete_git !== remote || approval.delete_svn !== remote || approval.archive_pair !== false
      || !/^[a-f0-9]{64}$/.test(approval.review_pin) || approval.request_id !== `ui-remove-v2-${approval.review_pin}`) {
      throw new Error(`Unexpected removal request for ${id}: ${JSON.stringify(writes)}`);
    }
    const review = await evaluate(`window.__pairFetches.filter(entry => entry.path === ${JSON.stringify(`/api/repos/${id}/removal/review`)} && entry.review).at(-1)?.review`);
    if (review?.repo_id !== id || review.review_pin !== approval.review_pin || review.delete_git !== remote || review.delete_svn !== remote) {
      throw new Error(`Approval does not match the displayed review for ${id}`);
    }
  };
  const pollsFor = async (id, since) => evaluate(`window.__pairFetches.filter(entry => entry.method === 'GET' && entry.started > ${since} && String(entry.path).split(/[/?=&]/).includes(${JSON.stringify(id)})).map(entry => entry.path)`);
  const notice = () => evaluate(`document.querySelector('[data-testid="branch-pair-removal-notice"]')?.innerText || ''`);
  const heading = () => evaluate(`document.querySelector('[data-testid="repo-detail-heading"]')?.textContent || ''`);

  async function expectStay(id, gitBranch, text) {
    await goto(`/repos/${id}`, async () => (await heading()) !== '');
    await click('delete-viewed-branch-pair');
    await confirmDelete(id, gitBranch);
    await until(async () => ((await body()).includes(text)), text);
    if ((await pathOf()) !== `/repos/${id}`) throw new Error(`Left ${id} for ${await pathOf()}`);
    if ((await body()).includes('Branch pair removed.')) throw new Error(`Failure was presented as removal: ${await body()}`);
  }

  if (mode === 'real') {
    if (!parentId || !viewedId || !listedId || !viewedBranch || !listedBranch) {
      throw new Error('Real API scenarios require parent, viewed, and listed pair ids');
    }
    await goto(`/repos/${parentId}`, async () => (await heading()).length > 0 && (await body()).includes('Listed pair'));
    const pathBeforeChildDelete = await pathOf();
    await click(`delete-child-pair-${listedId}`);
    await confirmDelete(listedId, listedBranch, { remote: false });
    await until(async () => !(await evaluate(`!!document.querySelector('[data-testid="branch-pair-row-${listedId}"]')`)), 'listed pair row removed');
    if ((await pathOf()) !== pathBeforeChildDelete) throw new Error(`Child delete left the parent: ${await pathOf()}`);
    if ((await heading()) === '') throw new Error('Parent heading disappeared');
    results.push('real-delete-child-stays-on-parent');

    await click(`branch-pair-row-${viewedId}`, ` a[href="/repos/${viewedId}"]`);
    await until(async () => (await pathOf()) === `/repos/${viewedId}` && (await heading()).includes('Viewed pair'), 'open viewed pair');
    await click('delete-viewed-branch-pair');
    const during = Date.now();
    await confirmDelete(viewedId, viewedBranch, { remote: false });
    await until(async () => (await pathOf()) === `/repos/${parentId}` && (await heading()).includes('Parent trunk'), 'parent after viewed delete', 20000);
    const history = await send('Page.getNavigationHistory');
    const dead = (history.entries || []).filter((entry) => String(entry.url).includes(`/repos/${viewedId}`));
    if (dead.length) throw new Error(`History still contains the removed pair: ${JSON.stringify(history.entries)}`);
    if (!(await notice()).includes('Branch pair removed.')) throw new Error(`Missing removal notice: ${await notice()}`);
    const settled = await evaluate('Date.now()');
    await pause(5500);
    const polls = await pollsFor(viewedId, settled);
    if (polls.length) throw new Error(`Viewed pair detail kept being fetched: ${JSON.stringify(polls)}`);
    await evaluate('history.back()');
    await pause(500);
    await until(async () => (await pathOf()) !== `/repos/${viewedId}`, 'back leaves dead pair');
    if ((await pathOf()) === `/repos/${viewedId}`) throw new Error('Back returned to the removed pair');
    if (await evaluate(`!!document.querySelector('[data-testid="repo-not-found"]')`)) {
      throw new Error('Back opened the not-found screen for the removed pair');
    }
    const afterBack = await pollsFor(viewedId, during);
    if (afterBack.filter((path) => path).length && (await pathOf()) === `/repos/${viewedId}`) {
      throw new Error('Back resumed the removed detail');
    }
    results.push('real-delete-viewed-replaces-history');
    return results;
  }

  await goto('/repos/child-cancel', async () => (await heading()).includes('Cancel Child'));
  const deletesBeforeCancel = (await removalWrites()).length;
  await click('delete-viewed-branch-pair');
  await until(async () => Boolean(await evaluate(`!!document.querySelector('[data-testid="delete-branch-modal"]')`)), 'delete modal');
  await click('cancel-delete-branch-pair');
  await until(async () => !(await evaluate(`!!document.querySelector('[data-testid="delete-branch-modal"]')`)), 'modal closed');
  if ((await removalWrites()).length !== deletesBeforeCancel) throw new Error('Cancel issued a removal request');
  if ((await pathOf()) !== '/repos/child-cancel') throw new Error('Cancel left the page');
  results.push('cancel-modal');

  await expectStay('child-error', 'error-branch', 'internal server error');
  results.push('server-error');
  await expectStay('child-denied', 'denied-branch', 'admin access required');
  const stillAuthed = await evaluate(`localStorage.getItem('session_token') === ${JSON.stringify(token)} && location.pathname !== '/login'`);
  if (!stillAuthed) throw new Error('Permission failure cleared the session or sent the user to login');
  results.push('permission-rejection');
  await expectStay('child-busy', 'busy-branch', 'currently busy');
  results.push('busy-rejection');
  await expectStay('child-held', 'held-branch', 'removal held for reconciliation');
  if ((await notice()).includes('Removal is not complete.')) throw new Error('Held operation was presented as queued');
  results.push('reconciliation-hold');

  await goto('/repos/child-orphan', async () => (await heading()).includes('Orphan Child'));
  await click('delete-viewed-branch-pair');
  await confirmDelete('child-orphan', 'orphan-branch');
  await until(async () => (await pathOf()) === '/repos' && (await body()).includes('Repositories'), 'repository list fallback');
  if ((await pathOf()).includes('missing-parent')) throw new Error('Navigated to the missing parent');
  if (!(await notice()).includes('Branch pair removed.')) throw new Error(`Parent-absent removal notice missing: ${await notice()}`);
  results.push('missing-parent-list-fallback');

  await goto('/repos/child-warn', async () => (await heading()).includes('Warn Child'));
  await click('delete-viewed-branch-pair');
  await confirmDelete('child-warn', 'warn-branch', { remote: true });
  await until(async () => (await pathOf()) === '/repos/parent-1', 'parent after warnings');
  await until(async () => (await body()).includes('Parent trunk'), 'parent detail loaded', 20000);
  await until(
    async () => (await notice()).includes("failed to delete Git branch 'warn-branch'"),
    'warning visible after navigation',
    20000,
  );
  const warning = await notice();
  if (!warning.includes('cleanup warnings')) throw new Error(`Warnings were presented as a clean removal: ${warning}`);
  results.push('warnings-remain-visible');

  await goto('/repos/child-queued', async () => (await heading()).includes('Queued Child'));
  await click('delete-viewed-branch-pair');
  await confirmDelete('child-queued', 'queued-branch');
  await until(async () => (await notice()).includes('Removal is not complete.') && (await notice()).includes('op-queue-70'), 'queued progress');
  if ((await pathOf()) !== '/repos/child-queued') throw new Error(`Queued removal left the pair: ${await pathOf()}`);
  if ((await notice()).includes('Branch pair removed.')) throw new Error('Queued removal was presented as complete');
  results.push('queued-not-complete');

  await goto('/repos/child-delayed', async () => (await heading()).includes('Delayed Child'));
  await click('delete-viewed-branch-pair');
  const delayStart = Date.now();
  await confirmDelete('child-delayed', 'delayed-branch');
  await pause(400);
  if ((await pathOf()) !== '/repos/child-delayed') throw new Error('Delayed deletion navigated before completion');
  if (!(await body()).includes('Removing…')) throw new Error('Delayed deletion did not show in-progress UI');
  await until(async () => (await pathOf()) === '/repos/parent-1' && (await notice()).includes('Branch pair removed.'), 'navigate after delayed completion');
  if (Date.now() - delayStart < 1000) throw new Error('Navigation happened before the delayed API completed');
  results.push('delayed-completion');

  await goto('/repos/child-queued-gone', async () => (await heading()).includes('Queued Gone Child'));
  await click('delete-viewed-branch-pair');
  await confirmDelete('child-queued-gone', 'queued-gone-branch');
  await until(async () => (await pathOf()) === '/repos/parent-1' && (await notice()).includes('Removal is not complete.') && (await notice()).includes('op-queue-gone-70'), 'queued departure');
  if ((await notice()).includes('Branch pair removed.')) throw new Error('Unfinished removal was presented as complete');
  const goneSettled = await evaluate('Date.now()');
  await pause(5500);
  const gonePolls = await pollsFor('child-queued-gone', goneSettled);
  if (gonePolls.length) throw new Error(`Queued-gone detail kept being fetched: ${JSON.stringify(gonePolls)}`);
  results.push('queued-gone-leaves-without-claiming-completion');

  await goto('/repos/parent-1', async () => (await heading()).includes('Parent trunk'));
  await click('delete-child-pair-child-stay');
  await confirmDelete('child-stay', 'stay-branch');
  await until(async () => !(await evaluate(`!!document.querySelector('[data-testid="branch-pair-row-child-stay"]')`)), 'mock child row removed');
  if ((await pathOf()) !== '/repos/parent-1') throw new Error(`Mock child delete left the parent: ${await pathOf()}`);
  results.push('mock-delete-child-stays-on-parent');

  // Combine the two requirements: list refresh must keep the parent mounted
  // while the remote-cleanup warning remains visible on that same page.
  await until(async () => Boolean(await evaluate(`!!document.querySelector('[data-testid="branch-pair-row-child-stay-warn"]')`)), 'warning child listed');
  const parentHistoryBefore = await send('Page.getNavigationHistory');
  const parentListReads = () => evaluate(`window.__pairFetches.filter(entry => entry.method === 'GET' && entry.path === '/api/repos/parent-1/branches').length`);
  const readsBeforeWarning = await parentListReads();
  await click('delete-child-pair-child-stay-warn');
  await confirmDelete('child-stay-warn', 'stay-warn-branch', { remote: true });
  const childWarningText = "failed to delete Git branch 'stay-warn-branch'";
  await until(async () => (await notice()).includes(childWarningText), 'child cleanup warning visible on parent');
  await until(async () => !(await evaluate(`!!document.querySelector('[data-testid="branch-pair-row-child-stay-warn"]')`)), 'warning child row removed');
  if ((await parentListReads()) <= readsBeforeWarning) throw new Error('Warning child removal did not refresh the parent list');
  if ((await pathOf()) !== '/repos/parent-1' || !(await heading()).includes('Parent trunk')) {
    throw new Error(`Warning child removal left the parent: ${await pathOf()}`);
  }
  if (!(await evaluate(`!!document.querySelector('[data-testid="branch-pair-row-child-cancel"]')`))) {
    throw new Error('Warning child removal lost the surviving sibling');
  }
  const parentHistoryAfter = await send('Page.getNavigationHistory');
  if (JSON.stringify(parentHistoryAfter) !== JSON.stringify(parentHistoryBefore)) {
    throw new Error('Warning child removal changed parent navigation history');
  }
  await pause(5500);
  const retainedWarning = await notice();
  if (!retainedWarning.includes('cleanup warnings') || !retainedWarning.includes(childWarningText)) {
    throw new Error(`Child warning did not survive parent refresh: ${retainedWarning}`);
  }
  if ((await pathOf()) !== '/repos/parent-1') throw new Error('Parent polling navigated after warning child removal');
  results.push('delete-child-with-warnings-stays-on-parent');

  await goto('/repos/already-removed');
  await until(async () => Boolean(await evaluate(`!!document.querySelector('[data-testid="repo-not-found"]')`)), 'friendly not-found');
  if ((await body()).includes('Error loading repository')) throw new Error('Stale URL used the raw error screen');
  const staleSettled = await evaluate('Date.now()');
  await pause(5500);
  const stalePolls = await pollsFor('already-removed', staleSettled);
  if (stalePolls.length) throw new Error(`Stale detail kept being fetched: ${JSON.stringify(stalePolls)}`);
  await click('repo-not-found-repositories');
  await until(async () => (await pathOf()) === '/repos', 'not-found navigation');
  results.push('stale-url-not-found');

  return results;
}

async function withUi(apiUrl, fn) {
  const vite = await startVite(apiUrl);
  const origin = `http://127.0.0.1:${vite.port}`;
  try {
    await waitForHttp(origin, 'vite');
    return await fn(origin);
  } finally {
    vite.child.kill('SIGKILL');
  }
}

const session = await connectChrome();
try {
  const results = [];
  if (realApi) {
    results.push(...await withUi(realApi, (origin) => runScenarios(origin, session, 'real')));
  }
  const mock = await startMock();
  try {
    const mockResults = await withUi(mock.url, (origin) => runScenarios(origin, session, 'mock'));
    results.push(...mockResults);
  } finally {
    await new Promise((resolve) => mock.server.close(resolve));
  }
  console.log(JSON.stringify({ ok: true, results }, null, 2));
} catch (error) {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
} finally {
  try { session.ws.close(); } catch { /* ignore */ }
  stopAll();
}
