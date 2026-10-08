// Drives the shipped ImportProgressCard in a real headless Chrome against the
// disposable Rust API. CDP uses Node built-ins, keeping the UI lock unchanged.
import { spawn } from 'node:child_process';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const { REPOSYNC_UI_URL: url, REPOSYNC_UI_MODE: mode,
  REPOSYNC_UI_BARRIER: barrier, REPOSYNC_UI_ARTIFACTS: artifacts,
  REPOSYNC_UI_TOKEN: token } = process.env;
if (!url || !mode || !barrier || !artifacts || !token) throw new Error('Missing browser fixture input');
const chrome = process.env.CHROME_BIN || (process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'google-chrome');
const profile = join(tmpdir(), `reposync-chrome-${process.pid}`);
await mkdir(profile, { recursive: true });
await mkdir(artifacts, { recursive: true });
const child = spawn(chrome, ['--headless=new', '--no-sandbox', '--disable-gpu',
  '--no-first-run', '--no-default-browser-check', '--remote-debugging-port=0',
  '--remote-allow-origins=*', `--user-data-dir=${profile}`], { stdio: 'ignore' });
const pause = (ms) => new Promise(resolve => setTimeout(resolve, ms));
async function until(fn, label, ms = 30000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const result = await fn();
    if (result) return result;
    await pause(100);
  }
  throw new Error(`Timed out: ${label}`);
}
let ws;
try {
  const portFile = join(profile, 'DevToolsActivePort');
  await until(() => existsSync(portFile), 'Chrome DevTools port');
  const [port] = (await readFile(portFile, 'utf8')).split('\n');
  const target = await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' }).then(r => r.json());
  ws = new WebSocket(target.webSocketDebuggerUrl);
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
    return response.result.value;
  };
  const body = () => evaluate('document.body?.innerText || ""');
  const button = (label) => evaluate(`Array.from(document.querySelectorAll('button')).some(b => b.textContent.trim() === ${JSON.stringify(label)})`);
  const click = async (label) => {
    const clicked = await evaluate(`(() => { const b = Array.from(document.querySelectorAll('button')).find(b => b.textContent.trim() === ${JSON.stringify(label)}); if (!b) return false; b.click(); return true; })()`);
    if (!clicked) throw new Error(`Button missing: ${label}; ${await body()}`);
  };
  const calls = () => evaluate('window.__fixtureCalls || []');
  const screenshot = async (name) => {
    const result = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
    await writeFile(join(artifacts, name), Buffer.from(result.data, 'base64'));
  };
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Page.addScriptToEvaluateOnNewDocument', { source: `
    localStorage.setItem('session_token', ${JSON.stringify(token)});
    localStorage.setItem('user', JSON.stringify({id:'fixture-admin',username:'fixture',role:'admin'}));
    window.__fixtureCalls = [];
    const originalFetch = window.fetch.bind(window);
    window.fetch = async (...args) => {
      const response = await originalFetch(...args);
      const path = String(args[0]);
      if (path.includes('/import')) {
        let payload = null;
        try { payload = await response.clone().json(); } catch {}
        window.__fixtureCalls.push({path, method: args[1]?.method || 'GET', status: response.status, payload});
      }
      return response;
    };
  ` });
  await send('Page.navigate', { url });
  if (mode.startsWith('reconcile-')) {
    await until(() => button('Verify remote'), 'held import verification action');
    const before = await body();
    if (!before.includes('Reconciliation required') || !before.includes('Operation ')) {
      throw new Error(`Missing held operation evidence: ${before}`);
    }
    await screenshot(`${mode}-before.png`);
    await click('Verify remote');
    const receipt = await until(async () => (await calls()).find(c => c.method === 'POST' &&
      c.path.endsWith('/reconcile')), 'exact-operation reconciliation response');
    if (receipt.status !== 200) throw new Error(`Reconciliation API failed: ${JSON.stringify(receipt)}`);
    const operation = receipt.payload?.operation_id;
    if (!operation || !receipt.path.endsWith(`/${operation}/reconcile`)) {
      throw new Error(`Reconciliation did not use the exact operation ID: ${JSON.stringify(receipt)}`);
    }
    if (mode === 'reconcile-complete') {
      if (receipt.payload?.lifecycle !== 'completed' || receipt.payload?.publication_proved !== true ||
          receipt.payload?.checkpoint_completed !== true) throw new Error(`Completion proof missing: ${JSON.stringify(receipt)}`);
      await until(async () => (await body()).includes('Import completed after remote verification'), 'verified completion');
    } else if (mode === 'reconcile-mismatch') {
      if (receipt.payload?.lifecycle !== 'reconciliation_required' || receipt.payload?.publication_proved !== false ||
          !receipt.payload?.remaining_reason?.includes('differs')) throw new Error(`Mismatch was not held: ${JSON.stringify(receipt)}`);
      await until(async () => (await body()).includes('Remote ref differs'), 'mismatch reason');
    } else if (mode === 'reconcile-partial') {
      if (receipt.payload?.lifecycle !== 'reconciliation_required' || receipt.payload?.publication_proved !== true ||
          receipt.payload?.checkpoint_completed !== false) throw new Error(`Partial proof was not held: ${JSON.stringify(receipt)}`);
      await until(async () => (await body()).includes('resume from the confirmed checkpoint'), 'partial hold reason');
    } else throw new Error(`Unknown reconciliation mode ${mode}`);
    await screenshot(`${mode}-after.png`);
    await send('Page.reload', { ignoreCache: true });
    const durable = mode === 'reconcile-complete' ? 'Import completed after remote verification' :
      mode === 'reconcile-mismatch' ? 'Remote ref differs' : 'resume from the confirmed checkpoint';
    await until(async () => (await body()).includes(durable), 'durable reconciliation status after reload');
    if (await button('Start full history import') || await button('Start snapshot import') || await button('Stop import')) {
      throw new Error('Unsafe import action available after verification');
    }
    await screenshot(`${mode}-reload.png`);
    const result = { mode, operation_id: operation, receipt: receipt.payload,
      reconcile_path: receipt.path, reload: durable };
    await writeFile(join(artifacts, `${mode}.json`), JSON.stringify(result, null, 2));
    process.stdout.write(`RELIABILITY_UI_EVIDENCE ${JSON.stringify(result)}\n`);
  } else {
  const setSnapshotRevision = async (rev) => {
    await evaluate(`document.querySelector('[data-testid="import-mode-snapshot"]').click()`);
    await evaluate(`(() => {
      const i = document.querySelector('[data-testid="import-svn-revision"]');
      if (!i) return false;
      const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')?.set;
      if (setter) setter.call(i, ${JSON.stringify(String(rev))});
      else i.value = ${JSON.stringify(String(rev))};
      i.dispatchEvent(new Event('input', { bubbles: true }));
      i.dispatchEvent(new Event('change', { bubbles: true }));
      return true;
    })()`);
  };
  await until(() => button('Start full history import'), 'mounted idle card');
  if (mode === 'snapshot-invalid-rev') {
    await setSnapshotRevision('99');
    await until(() => button('Start snapshot import'), 'snapshot start for invalid rev');
    await click('Start snapshot import');
    await until(async () => {
      const post = (await calls()).find(c => c.method === 'POST' && c.path.endsWith('/import'));
      return post && post.status === 400;
    }, 'refused import POST', 30000);
    const refused = await until(async () => {
      const text = await body();
      return text.includes('revision') || text.includes('refused') || text.includes('invalid');
    }, 'visible refusal feedback', 30000);
    if (!refused) throw new Error(`Missing refusal copy: ${await body()}`);
    const text = await body();
    if (/verified snapshot baseline/i.test(text) || /Bidirectional sync applies/i.test(text)) {
      throw new Error(`Baseline claim on refused import: ${text}`);
    }
    await until(async () => evaluate(`!!document.querySelector('[data-testid="import-baseline-failure"]')`), 'failure notice', 30000)
      .catch(async () => {
        if (!/Snapshot import refused/i.test(await body())) {
          throw new Error(`Missing failure notice: ${await body()}`);
        }
      });
    const result = { mode, refusal_visible: true };
    await writeFile(join(artifacts, `${mode}.json`), JSON.stringify(result, null, 2));
    process.stdout.write(`RELIABILITY_UI_EVIDENCE ${JSON.stringify(result)}\n`);
  } else if (mode === 'full-default') {
    await click('Start full history import');
    const start = await until(async () => (await calls()).find(c => c.method === 'POST' && c.path.endsWith('/import') && c.status === 200), 'start operation');
    if (start.payload?.import_mode !== 'full') throw new Error(`Start response not full: ${JSON.stringify(start.payload)}`);
    const result = { mode, import_mode: start.payload?.import_mode, operation_id: start.payload?.operation_id };
    await writeFile(join(artifacts, `${mode}.json`), JSON.stringify(result, null, 2));
    process.stdout.write(`RELIABILITY_UI_EVIDENCE ${JSON.stringify(result)}\n`);
  } else if (mode === 'snapshot-boundary') {
    await setSnapshotRevision('2');
    await until(() => button('Start snapshot import'), 'snapshot start action');
    await click('Start snapshot import');
    const start = await until(async () => (await calls()).find(c => c.method === 'POST' && c.path.endsWith('/import') && c.status === 200), 'start operation');
    if (start.payload?.import_mode !== 'snapshot' || start.payload?.starting_revision !== 2) {
      throw new Error(`Snapshot start response missing pin: ${JSON.stringify(start.payload)}`);
    }
    await writeFile(join(barrier, 'import_started.ready'), 'ready');
    await until(async () => (await body()).includes('before r2'), 'history boundary during import', 120000);
    await until(async () => (await body()).includes('Import Complete') || (await body()).includes('Completed'), 'snapshot import completion', 120000);
    const result = {
      mode,
      import_mode: start.payload?.import_mode,
      starting_revision: start.payload?.starting_revision,
      history_boundary: start.payload?.history_boundary,
    };
    await screenshot(`${mode}-terminal.png`);
    await writeFile(join(artifacts, `${mode}.json`), JSON.stringify(result, null, 2));
    process.stdout.write(`RELIABILITY_UI_EVIDENCE ${JSON.stringify(result)}\n`);
  } else {
  await click('Start full history import');
  const start = await until(async () => (await calls()).find(c => c.method === 'POST' && c.path.endsWith('/import') && c.status === 200), 'start operation');
  await writeFile(join(barrier, 'import_started.ready'), 'ready');
  const operation = start.payload?.operation_id;
  if (!operation || !(await body()).includes(`Operation ${operation}`)) {
    await until(async () => (await body()).includes(`Operation ${operation}`), 'operation identity in card');
  }
  const result = { mode, operation_id: operation, start_status: start.status };
  if (mode === 'cancel') {
    await until(() => existsSync(join(barrier, 'after_first_local.ready')), 'first local revision barrier')
      .catch(async error => { throw new Error(`${error}; card: ${await body()}; calls: ${JSON.stringify(await calls())}`); });
    await until(() => existsSync(join(barrier, 'failure_ready')), 'durable write fault installed');
    await until(() => button('Stop import'), 'stop action');
    await click('Stop import');
    const failed = await until(async () => (await calls()).find(c => c.method === 'POST' && c.path.endsWith(`/${operation}/cancel`) && c.status === 500), 'failed durable cancel response');
    result.failed_cancel = failed.status;
    const feedback = failed.payload?.error || 'Cancellation failed (500)';
    await until(async () => (await body()).includes(feedback), 'visible cancellation error')
      .catch(async error => { throw new Error(`${error}; expected ${feedback}; card: ${await body()}`); });
    result.failure_feedback = feedback;
    await writeFile(join(barrier, 'failure_seen'), 'seen');
    await until(() => existsSync(join(barrier, 'failure_release')), 'failed-write inspection');
    await click('Stop import');
    const accepted = await until(async () => (await calls()).find(c => c.method === 'POST' && c.path.endsWith(`/${operation}/cancel`) && c.status === 200), 'exact ID accepted cancel');
    if (accepted.payload?.lifecycle !== 'cancel_requested') throw new Error(`Wrong cancel receipt: ${JSON.stringify(accepted)}`);
    result.accepted_cancel = accepted.payload.lifecycle;
    await until(async () => (await body()).includes('Cancellation requested — stopping'), 'requested is not terminal');
    result.requested_label = 'Cancellation requested — stopping';
    await screenshot('requested.png');
    await until(() => existsSync(join(barrier, 'after_first_local.cancel_observed')), 'worker observed stop');
    await writeFile(join(barrier, 'after_first_local.cancel_release'), 'release');
    await until(async () => (await body()).includes('Local through SVN r1; remote confirmed through none.'), 'partial progress', 30000);
    await until(async () => (await body()).includes('Cancelled'), 'terminal cancellation');
    result.partial_label = 'Local through SVN r1; remote confirmed through none.';
  } else if (mode === 'uncertain') {
    await until(async () => (await body()).includes('Reconciliation required'), 'uncertain publication', 30000);
    result.uncertain_label = 'Reconciliation required';
  } else throw new Error(`Unknown mode ${mode}`);
  await screenshot(`${mode}-terminal.png`);
  result.cancel_paths = (await calls()).filter(c => c.method === 'POST' && c.path.endsWith(`/${operation}/cancel`)).map(c => ({ path: c.path, status: c.status }));
  await send('Page.reload', { ignoreCache: true });
  const terminalText = mode === 'cancel' ? 'Local through SVN r1; remote confirmed through none.' : 'Reconciliation required';
  await until(async () => (await body()).includes(terminalText), 'durable status after reload');
  if (await button('Start full history import') || await button('Start snapshot import') || await button('Stop import')) {
    throw new Error('Held import offers unsafe retry after reload');
  }
  result.reload = 'durable held status';
  await screenshot(`${mode}-reload.png`);
  await writeFile(join(artifacts, `${mode}.json`), JSON.stringify(result, null, 2));
  process.stdout.write(`RELIABILITY_UI_EVIDENCE ${JSON.stringify(result)}\n`);
  }
  }
} finally {
  ws?.close();
  child.kill();
}
