// #262 browser regression for the switchable Classic / Individual / Team shell.
//
// Real Vite-served UI in an existing, admitted headless Chromium (REPOSYNC_A11Y_BROWSER;
// never installed here). Network is limited to the Vite origin, whose /api proxy goes to
// the existing in-memory synthetic fixture (web-ui/a11y/fixture.mjs, imported unchanged).
// The fixture records every authenticated request, so "switching sends no mutation" is
// checked against what actually reached the API.
//
// Run from web-ui/: REPOSYNC_A11Y_BROWSER=/path/to/chromium node --test --test-concurrency=1 tests/layout-foundation.e2e.mjs
import test, { before, after, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { createServer as createViteServer } from 'vite';
import { startFixture, syntheticUser } from '../a11y/fixture.mjs';

const require = createRequire(import.meta.url);
const root = fileURLToPath(new URL('..', import.meta.url));
const PREFIX = 'reposync.layout-preference.v1';
const ADMIN_NAV = [
  ['/', 'Dashboard'], ['/repos', 'Repositories'], ['/conflicts', 'Conflicts'], ['/audit', 'Audit Log'], ['/settings', 'Settings'],
  ['/users', 'Users'], ['/ldap', 'LDAP'], ['/config', 'Config'], ['/setup', 'Setup'],
];
const MEMBER_NAV = ADMIN_NAV.slice(0, 5);
const LAYOUTS = ['classic', 'individual', 'team'];
let fixture, vite, browser, origin, appOrigin;
const pageErrors = [];

before(async () => {
  const executablePath = process.env.REPOSYNC_A11Y_BROWSER;
  assert.ok(executablePath, 'Set REPOSYNC_A11Y_BROWSER to an existing admitted Chromium executable. This test never installs a browser.');
  fixture = await startFixture();
  vite = await createViteServer({ root, server: { host: '127.0.0.1', port: 0, proxy: { '/api': fixture.origin, '/ws': { target: fixture.origin, ws: false } } } });
  await vite.listen();
  origin = Object.values(vite.resolvedUrls.local)[0];
  appOrigin = new URL(origin).origin;
  browser = await chromium.launch({ executablePath, headless: true, chromiumSandbox: true, timeout: 15000 });
  console.log('# browser:', browser.version(), 'Node:', process.version, 'sandbox: enabled');
}, { timeout: 30000 });

after(async () => { await browser?.close(); await vite?.close(); await fixture?.close(); });
afterEach(() => { assert.deepEqual(pageErrors.splice(0), [], 'uncaught page errors'); });

function preferenceKey(identity) {
  return `${PREFIX}:${encodeURIComponent(appOrigin)}:${encodeURIComponent(identity)}`;
}

function preferenceRecord(identity, layout) {
  return JSON.stringify({ version: 1, layout, identity, origin: appOrigin });
}

/** Signs in as the synthetic administrator in a fresh context. `init` runs before every page load. */
async function open(path = '/repos', { viewport = { width: 1280, height: 900 }, init } = {}) {
  const context = await browser.newContext({ reducedMotion: 'reduce', viewport });
  await context.route('**/*', route => new URL(route.request().url()).origin === appOrigin ? route.continue() : route.abort());
  if (init) await context.addInitScript(init.script, init.arg);
  const page = await context.newPage();
  page.on('pageerror', error => pageErrors.push(error.message));
  page.setDefaultTimeout(8000);
  await page.goto(origin + 'login');
  await page.getByLabel('Username', { exact: true }).fill('fixture-admin');
  await page.getByLabel('Password', { exact: true }).fill('synthetic-only');
  await page.getByRole('button', { name: /Sign in/i }).press('Enter');
  await page.waitForURL(url => url.pathname !== '/login');
  await page.goto(origin + path.replace(/^\//, ''));
  await page.locator('[data-rs-layout]').waitFor();
  return { page, close: () => context.close() };
}

const chooser = page => page.getByRole('combobox', { name: 'Layout', exact: true });
const layoutOf = page => page.locator('[data-rs-layout]').getAttribute('data-rs-layout');

async function reach(page, target, key = 'Tab') {
  for (let i = 0; i < 160; i++) {
    if (await target.evaluate(element => element === document.activeElement)) return;
    await page.keyboard.press(key);
  }
  assert.fail('Keyboard Tab could not reach ' + (await target.evaluate(element => element.outerHTML.slice(0, 120))));
}

/** Programmatic selection that does not move focus or scroll, as a pointer-free control. */
async function chooseWithoutFocus(page, layout) {
  await page.evaluate(next => {
    const select = document.querySelector('select[data-rs-layout-chooser]');
    select.value = next;
    select.dispatchEvent(new Event('change', { bubbles: true }));
  }, layout);
  await page.locator(`[data-rs-layout="${layout}"]`).waitFor();
}

async function navLinks(page) {
  return page.locator('[data-rs-layout] > nav').evaluate(nav => [...nav.querySelectorAll('a')].map(a => [a.getAttribute('href'), a.firstChild?.textContent ?? '']));
}

/** Remembers the live shell, routed page and an optional extra node before a switch. */
async function markShell(page, extraSelector = null) {
  await page.evaluate(selector => {
    const main = document.querySelector('main');
    window.__rs262 = {
      main, content: main.firstElementChild, chooser: document.querySelector('select[data-rs-layout-chooser]'),
      extra: selector ? document.querySelector(selector) : null, historyLength: history.length, href: location.href,
    };
  }, extraSelector);
}

/** The same DOM nodes (so no remount), the same URL and history, one main landmark. */
async function assertSameShell(page, label) {
  const result = await page.evaluate(() => {
    const probe = window.__rs262;
    const main = document.querySelector('main');
    return {
      mains: document.querySelectorAll('main').length,
      main: probe.main === main && main.isConnected,
      content: probe.content === main.firstElementChild && probe.content.isConnected,
      chooser: probe.chooser === document.querySelector('select[data-rs-layout-chooser]') && probe.chooser.isConnected,
      extra: probe.extra === null || probe.extra.isConnected,
      history: probe.historyLength === history.length,
      href: probe.href === location.href,
    };
  });
  assert.deepEqual(result, { mains: 1, main: true, content: true, chooser: true, extra: true, history: true, href: true }, label);
}

function mutationsSince(mark) {
  return fixture.requests.slice(mark).filter(request => request.method !== 'GET').map(request => `${request.method} ${request.path}`);
}

async function auditShell(page, label) {
  await page.addScriptTag({ path: require.resolve('axe-core/axe.min.js') });
  const result = await page.evaluate(async () => window.axe.run({ include: [['[data-rs-layout] > nav']] }, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'] } }));
  assert.deepEqual(result.violations.map(v => ({ id: v.id, impact: v.impact, nodes: v.nodes.map(n => n.target) })), [], label + ' axe violations');
}

test('rs262 Classic is the default, keeps the original dark shell colors and navigation, and every layout passes axe', { timeout: 60000 }, async () => {
  const { page, close } = await open('/repos');
  try {
    assert.equal(await layoutOf(page), 'classic');
    assert.equal(await page.locator('[data-rs-layout]').getAttribute('data-rs-theme'), 'dark');
    assert.equal(await page.getByRole('main').count(), 1);
    assert.equal(await page.getByRole('navigation', { name: 'Primary', exact: true }).count(), 1);
    assert.deepEqual(await navLinks(page), ADMIN_NAV);
    // Pre-#262 Tailwind colors: gray-900, gray-950, gray-800, blue-600, white, gray-400.
    const colors = await page.evaluate(() => {
      const css = (element, property) => getComputedStyle(element).getPropertyValue(property);
      const shell = document.querySelector('[data-rs-layout]');
      const nav = shell.querySelector(':scope > nav');
      const active = nav.querySelector('a[aria-current="page"]');
      const idle = nav.querySelector('a:not([aria-current])');
      const logout = [...nav.querySelectorAll('button')].find(button => button.textContent === 'Logout');
      return {
        canvas: css(shell, 'background-color'), chrome: css(nav, 'background-color'), border: css(nav, 'border-bottom-color'),
        borderWidth: css(nav, 'border-bottom-width'), brand: css(nav.querySelector('span.rs-shell-brand'), 'color'),
        active: css(active, 'background-color'), activeText: css(active, 'color'), activeHref: active.getAttribute('href'),
        idle: css(idle, 'background-color'), idleText: css(idle, 'color'), logout: css(logout, 'color'),
        barHeight: nav.firstElementChild.firstElementChild.getBoundingClientRect().height,
      };
    });
    assert.deepEqual(colors, {
      canvas: 'rgb(17, 24, 39)', chrome: 'rgb(3, 7, 18)', border: 'rgb(31, 41, 55)', borderWidth: '1px', brand: 'rgb(255, 255, 255)',
      active: 'rgb(37, 99, 235)', activeText: 'rgb(255, 255, 255)', activeHref: '/repos',
      idle: 'rgba(0, 0, 0, 0)', idleText: 'rgb(156, 163, 175)', logout: 'rgb(156, 163, 175)', barHeight: 64,
    });
    for (const layout of LAYOUTS) {
      await chooseWithoutFocus(page, layout);
      assert.deepEqual(await navLinks(page), ADMIN_NAV, layout);
      await auditShell(page, layout);
    }
  } finally { await close(); }
});

test('rs262 keyboard chooser switches in place: same page, draft, focus and URL, and no mutation', { timeout: 60000 }, async () => {
  const { page, close } = await open('/settings');
  try {
    const name = page.getByLabel('Display Name', { exact: true });
    await name.waitFor();
    await reach(page, name);
    await page.keyboard.press('End');
    await page.keyboard.type(' (unsaved draft)');
    const draft = await name.inputValue();
    assert.match(draft, /\(unsaved draft\)$/);
    const nameId = await name.getAttribute('id');
    await markShell(page, `[id="${nameId}"]`);
    const mark = fixture.requests.length;

    // The chooser precedes the page content, so walk back to it from the dirty field.
    // Arrow keys on a focused, closed <select> change the value on Chromium/Linux.
    const select = chooser(page);
    await reach(page, select, 'Shift+Tab');
    assert.equal(await select.inputValue(), 'classic');
    for (const [key, expected] of [['ArrowDown', 'individual'], ['ArrowDown', 'team'], ['ArrowUp', 'individual'], ['ArrowUp', 'classic']]) {
      await page.keyboard.press(key);
      await page.locator(`[data-rs-layout="${expected}"]`).waitFor();
      assert.equal(await select.inputValue(), expected);
      assert.ok(await select.evaluate(element => element === document.activeElement), `focus stayed on the chooser after ${expected}`);
      await assertSameShell(page, expected);
      assert.equal(await name.inputValue(), draft, `${expected} kept the unsaved draft`);
      assert.ok(await page.evaluate(id => window.__rs262.extra === document.getElementById(id), nameId), `${expected} kept the same input node`);
    }
    assert.deepEqual(mutationsSince(mark), []);
    const stored = await page.evaluate(prefix => Object.keys(localStorage).filter(key => key.startsWith(prefix)).map(key => [key, localStorage.getItem(key)]), PREFIX);
    assert.deepEqual(stored, [[preferenceKey(syntheticUser.id), preferenceRecord(syntheticUser.id, 'classic')]]);
  } finally { await close(); }
});

test('rs262 a dirty repository edit draft keeps its text, selection, scroll and focus across every layout', { timeout: 60000 }, async () => {
  const { page, close } = await open('/repos/A', { viewport: { width: 1280, height: 640 } });
  try {
    await page.getByRole('heading', { name: 'Synthetic parent', exact: true }).waitFor();
    const edit = page.getByRole('button', { name: 'Edit', exact: true });
    await reach(page, edit);
    await page.keyboard.press('Enter');
    const paths = page.getByLabel(/Allowed SVN Paths/);
    await paths.waitFor();
    const text = Array.from({ length: 40 }, (_, i) => `synthetic/path-${i}/`).join('\n');
    await paths.fill(text);
    const pathsId = await paths.getAttribute('id');
    await paths.evaluate(element => { element.setSelectionRange(5, 9); element.scrollTop = 120; });
    await page.evaluate(() => window.scrollTo(0, 240));
    const before = await page.evaluate(() => window.scrollY);
    assert.ok(before > 0, 'the edit form must be tall enough to scroll');
    await markShell(page, `[id="${pathsId}"]`);
    const mark = fixture.requests.length;

    for (const layout of ['team', 'individual', 'classic']) {
      await chooseWithoutFocus(page, layout);
      await assertSameShell(page, layout);
      const state = await paths.evaluate(element => ({
        value: element.value, focused: element === document.activeElement, start: element.selectionStart, end: element.selectionEnd, scrollTop: element.scrollTop,
      }));
      assert.deepEqual(state, { value: text, focused: true, start: 5, end: 9, scrollTop: 120 }, layout);
      assert.ok(await page.evaluate(() => window.scrollY) > 0, `${layout} did not reset page scroll to the top`);
    }
    assert.deepEqual(mutationsSince(mark), [], 'switching sent no save, retry or other mutation');
  } finally { await close(); }
});

test('rs262 Individual and Team keep navigation reachable at phone width; Team is a sticky sidebar when wide', { timeout: 60000 }, async () => {
  const narrow = await open('/repos', { viewport: { width: 390, height: 844 } });
  try {
    const { page } = narrow;
    for (const layout of ['individual', 'team']) {
      await chooseWithoutFocus(page, layout);
      const nav = page.locator('[data-rs-layout] > nav');
      const box = await nav.evaluate(element => ({ right: element.getBoundingClientRect().right, scroll: element.scrollWidth, client: element.clientWidth }));
      assert.ok(box.right <= 390 && box.scroll <= box.client, `${layout} navigation fits the viewport: ${JSON.stringify(box)}`);
      // Start each pass at the first entry, then move only by Tab.
      await page.evaluate(() => window.scrollTo(0, 0));
      await nav.locator('a').first().focus();
      for (const [href] of ADMIN_NAV) {
        const link = nav.locator(`a[href="${href}"]`);
        await reach(page, link);
        const rect = await link.evaluate(element => element.getBoundingClientRect().toJSON());
        assert.ok(rect.left >= 0 && rect.right <= 390, `${layout} ${href} is visible when focused: ${JSON.stringify(rect)}`);
      }
      for (const control of [chooser(page), page.getByRole('button', { name: 'Logout', exact: true })]) {
        await reach(page, control);
        const rect = await control.evaluate(element => element.getBoundingClientRect().toJSON());
        assert.ok(rect.left >= 0 && rect.right <= 390, `${layout} account control is visible: ${JSON.stringify(rect)}`);
      }
    }
  } finally { await narrow.close(); }

  const wide = await open('/repos/A', { viewport: { width: 1280, height: 640 } });
  try {
    const { page } = wide;
    await chooseWithoutFocus(page, 'team');
    await page.evaluate(() => window.scrollTo(0, 200));
    const team = await page.evaluate(() => {
      const nav = document.querySelector('[data-rs-layout] > nav').getBoundingClientRect();
      const main = document.querySelector('main').getBoundingClientRect();
      return { navLeft: nav.left, navTop: nav.top, navWidth: nav.width, navHeight: nav.height, mainLeft: main.left, scrolled: window.scrollY > 0 };
    });
    assert.deepEqual(team, { navLeft: 0, navTop: 0, navWidth: 256, navHeight: 640, mainLeft: 256, scrolled: true });
    assert.equal(await page.getByText('Administration', { exact: true }).isVisible(), true);
  } finally { await wide.close(); }
});

test('rs262 the preference persists per account and falls back to Classic visibly when unusable', { timeout: 60000 }, async () => {
  // Saved, restored after reload, and not inherited by another account identity.
  const saved = await open('/repos');
  try {
    const { page } = saved;
    await chooseWithoutFocus(page, 'individual');
    await page.reload();
    await page.locator('[data-rs-layout="individual"]').waitFor();
    await page.evaluate(user => localStorage.setItem('user', JSON.stringify({ ...user, id: 'fixture-other-account' })), syntheticUser);
    await page.reload();
    await page.locator('[data-rs-layout="classic"]').waitFor();
    assert.equal(await page.locator('.rs-layout-chooser__notice').textContent(), '');
    await page.evaluate(user => localStorage.setItem('user', JSON.stringify(user)), syntheticUser);
    await page.reload();
    await page.locator('[data-rs-layout="individual"]').waitFor();
  } finally { await saved.close(); }

  // Corrupt or foreign values for this account are not used, are reported, and are replaced by a choice.
  for (const [label, value] of [['corrupt', '{not json'], ['foreign', null]]) {
    const seeded = value ?? JSON.stringify({ version: 1, layout: 'team', identity: 'fixture-other-account', origin: 'http://elsewhere.invalid' });
    const run = await open('/repos', { init: { script: ({ key, raw }) => { try { localStorage.setItem(key, raw); } catch { /* storage refused */ } }, arg: { key: preferenceKey(syntheticUser.id), raw: seeded } } });
    try {
      const { page } = run;
      assert.equal(await layoutOf(page), 'classic', label);
      assert.equal(await page.locator('.rs-layout-chooser__notice').evaluate(element => element.firstChild?.textContent), 'Using Classic', label);
      await chooseWithoutFocus(page, 'team');
      assert.equal(await page.evaluate(key => localStorage.getItem(key), preferenceKey(syntheticUser.id)), preferenceRecord(syntheticUser.id, 'team'));
      assert.equal(await page.locator('.rs-layout-chooser__notice').textContent(), '');
    } finally { await run.close(); }
  }

  // A browser that refuses to save still applies the explicit choice, says so, and does not crash.
  const blocked = await open('/repos', { init: { script: prefix => {
    const setItem = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) { if (String(key).startsWith(prefix)) throw new DOMException('Synthetic quota', 'QuotaExceededError'); return setItem.call(this, key, value); };
  }, arg: PREFIX } });
  try {
    const { page } = blocked;
    await chooseWithoutFocus(page, 'team');
    const notice = page.locator('.rs-layout-chooser__notice');
    assert.equal(await notice.evaluate(element => element.firstChild?.textContent), 'Not saved');
    assert.equal(await notice.getAttribute('title'), 'Team applies in this tab until you reload; this browser did not allow the layout preference to be saved.');
    await page.reload();
    await page.locator('[data-rs-layout="classic"]').waitFor();
  } finally { await blocked.close(); }
});

test('rs262 a member keeps member-only navigation in every layout', { timeout: 45000 }, async () => {
  const member = { ...syntheticUser, role: 'user' };
  const { page, close } = await open('/repos', { init: { script: user => { if (localStorage.getItem('session_token')) localStorage.setItem('user', JSON.stringify(user)); }, arg: member } });
  try {
    await page.reload();
    await page.locator('[data-rs-layout]').waitFor();
    for (const layout of LAYOUTS) {
      await chooseWithoutFocus(page, layout);
      assert.deepEqual(await navLinks(page), MEMBER_NAV, layout);
      assert.equal(await page.getByText('Administration', { exact: true }).count(), 0, layout);
    }
  } finally { await close(); }
});
