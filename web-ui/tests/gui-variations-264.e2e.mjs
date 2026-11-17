// #264 browser regression for the Ledger, Console and Guide layouts, the light theme and the
// repository sections.
//
// Real Vite-served UI in an existing, admitted headless Chromium with its sandbox enabled
// (REPOSYNC_A11Y_BROWSER; never installed here). Network is limited to the Vite origin, whose
// /api proxy goes to the in-memory synthetic fixture (web-ui/a11y/fixture.mjs), which records
// every request, so "switching sends no mutation" is checked against what reached the API.
//
// Run from web-ui/: REPOSYNC_A11Y_BROWSER=/path/to/chromium node --test --test-concurrency=1 tests/gui-variations-264.e2e.mjs
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
const LAYOUTS = ['classic', 'individual', 'team', 'ledger', 'console', 'guide'];
const NEW_LAYOUTS = ['ledger', 'console', 'guide'];
const WIDE = { width: 1440, height: 900 };
const NARROW = { width: 390, height: 844 };
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

const preferenceKey = identity => `${PREFIX}:${encodeURIComponent(appOrigin)}:${encodeURIComponent(identity)}`;
const record = (layout, theme, version = 2) => version === 1
  ? JSON.stringify({ version: 1, layout, identity: syntheticUser.id, origin: appOrigin })
  : JSON.stringify({ version: 2, layout, theme, identity: syntheticUser.id, origin: appOrigin });

/** Signs in as the synthetic administrator in a fresh context, optionally with a stored preference. */
async function open(path, { viewport = WIDE, stored = null } = {}) {
  const context = await browser.newContext({ reducedMotion: 'reduce', viewport });
  await context.route('**/*', route => new URL(route.request().url()).origin === appOrigin ? route.continue() : route.abort());
  const page = await context.newPage();
  page.on('pageerror', error => pageErrors.push(error.message));
  page.setDefaultTimeout(8000);
  await page.goto(origin + 'login');
  await page.getByLabel('Username', { exact: true }).fill('fixture-admin');
  await page.getByLabel('Password', { exact: true }).fill('synthetic-only');
  await page.getByRole('button', { name: /Sign in/i }).press('Enter');
  await page.waitForURL(url => url.pathname !== '/login');
  if (stored) await page.evaluate(([key, value]) => localStorage.setItem(key, value), [preferenceKey(syntheticUser.id), stored]);
  await page.goto(origin + path.replace(/^\//, ''));
  await page.locator('[data-rs-layout]').waitFor();
  return { page, close: () => context.close() };
}

const shell = page => page.locator('.rs-layout-shell');
const attrs = page => shell(page).evaluate(el => ({ layout: el.dataset.rsLayout, theme: el.dataset.rsTheme, bridge: el.hasAttribute('data-rs-bridge') }));

/** Programmatic selection that does not move focus, for the layout or the theme select. */
async function choose(page, kind, value) {
  await page.evaluate(([selector, next]) => {
    const select = document.querySelector(selector);
    select.value = next;
    select.dispatchEvent(new Event('change', { bubbles: true }));
  }, [kind === 'layout' ? 'select[data-rs-layout-chooser]' : 'select[data-rs-theme-chooser]', value]);
  await page.locator(kind === 'layout' ? `[data-rs-layout="${value}"]` : `[data-rs-theme="${value}"]`).waitFor();
}

const mutationsSince = mark => fixture.requests.slice(mark).filter(request => request.method !== 'GET').map(request => `${request.method} ${request.path}`);

async function reach(page, target, key = 'Tab') {
  for (let i = 0; i < 160; i++) {
    if (await target.evaluate(element => element === document.activeElement)) return;
    await page.keyboard.press(key);
  }
  assert.fail('Keyboard Tab could not reach ' + (await target.evaluate(element => element.outerHTML.slice(0, 120))));
}

test('rs264 every layout and theme switches in place: same nodes, draft and URL, no mutation, bridge only where tokens apply', { timeout: 90000 }, async () => {
  const { page, close } = await open('/repos/A?section=settings');
  try {
    await page.getByRole('button', { name: /^Edit$/ }).click();
    const field = page.locator('input[type="number"]').first();
    await field.waitFor();
    await field.fill('4321');
    await page.evaluate(() => { window.__rs264 = { main: document.querySelector('main'), content: document.querySelector('main').firstElementChild, field: document.querySelector('input[type="number"]'), href: location.href, history: history.length }; });
    const mark = fixture.requests.length;
    for (const theme of ['dark', 'light']) {
      await choose(page, 'theme', theme);
      for (const layout of LAYOUTS) {
        await choose(page, 'layout', layout);
        const state = await attrs(page);
        assert.deepEqual(state, { layout, theme, bridge: !(['classic', 'individual', 'team'].includes(layout) && theme === 'dark') }, `${layout} ${theme}`);
        const same = await page.evaluate(() => {
          const probe = window.__rs264; const main = document.querySelector('main');
          return { main: probe.main === main, content: probe.content === main.firstElementChild, field: probe.field.isConnected && probe.field.value, href: probe.href === location.href, history: probe.history === history.length };
        });
        assert.deepEqual(same, { main: true, content: true, field: '4321', href: true, history: true }, `${layout} ${theme} keeps the page`);
      }
    }
    assert.deepEqual(mutationsSince(mark), [], 'switching layout or theme sends no mutation');
  } finally { await close(); }
});

test('rs264 a stored theme survives reload; a #262 record selects its layout in dark; an unknown theme falls back visibly', { timeout: 60000 }, async () => {
  let session = await open('/', { stored: record('guide', 'light') });
  try {
    assert.deepEqual(await attrs(session.page), { layout: 'guide', theme: 'light', bridge: true });
    await session.page.reload(); await session.page.locator('[data-rs-layout="guide"]').waitFor();
    assert.deepEqual(await attrs(session.page), { layout: 'guide', theme: 'light', bridge: true });
  } finally { await session.close(); }
  session = await open('/', { stored: record('console', null, 1) });
  try {
    assert.deepEqual(await attrs(session.page), { layout: 'console', theme: 'dark', bridge: true });
  } finally { await session.close(); }
  session = await open('/', { stored: JSON.stringify({ version: 2, layout: 'ledger', theme: 'sepia', identity: syntheticUser.id, origin: appOrigin }) });
  try {
    assert.deepEqual(await attrs(session.page), { layout: 'classic', theme: 'dark', bridge: false });
    assert.match(await session.page.locator('.rs-layout-chooser__notice').textContent(), /Using Classic/);
  } finally { await session.close(); }
});

test('rs264 the existing layouts hide More, the icons, the derived regions and the section navigation', { timeout: 60000 }, async () => {
  for (const layout of ['classic', 'individual', 'team']) {
    const { page, close } = await open('/repos/A', { stored: record(layout, 'dark') });
    try {
      const hidden = await page.evaluate(() => {
        const visible = el => !!el && getComputedStyle(el).display !== 'none' && el.getClientRects().length > 0;
        return {
          more: visible(document.querySelector('.rs-shell-more')),
          icons: [...document.querySelectorAll('.rs-nav-icon')].some(visible),
          derived: [...document.querySelectorAll('[data-rs-derived]')].some(visible),
          sections: visible(document.querySelector('.rs-page-sections')),
          help: [...document.querySelectorAll('nav a')].some(a => a.getAttribute('href') === '/docs'),
        };
      });
      assert.deepEqual(hidden, { more: false, icons: false, derived: false, sections: false, help: false }, layout);
    } finally { await close(); }
  }
});

test('rs264 More at phone width: reading-order tab stops, focus moves in, Escape returns, a route change closes', { timeout: 90000 }, async () => {
  for (const layout of NEW_LAYOUTS) {
    const { page, close } = await open('/', { viewport: NARROW, stored: record(layout, 'light') });
    try {
      const more = page.locator('.rs-shell-more');
      assert.equal(await more.isVisible(), true, `${layout}: More is shown at phone width`);
      assert.equal(await more.getAttribute('aria-expanded'), 'false');
      // Tab order: the four workspace links, then More.
      // Start sequential focus navigation at the top of the shell, before the navigation.
      await page.evaluate(() => { const root = document.querySelector('.rs-layout-shell'); root.setAttribute('tabindex', '-1'); root.focus(); root.removeAttribute('tabindex'); });
      const order = [];
      for (let i = 0; i < 12 && order.length < 5; i++) {
        await page.keyboard.press('Tab');
        const tag = await page.evaluate(() => { const el = document.activeElement; return el?.closest('nav') ? (el.classList.contains('rs-shell-more') ? 'More' : el.getAttribute('href')) : null; });
        if (tag && !order.includes(tag)) order.push(tag);
      }
      assert.deepEqual(order, ['/', '/repos', '/conflicts', '/audit', 'More'], `${layout}: workspace links then More`);
      await more.focus(); await page.keyboard.press('Enter');
      await page.waitForFunction(() => document.querySelector('.rs-layout-shell').hasAttribute('data-nav-open'));
      assert.equal(await more.getAttribute('aria-expanded'), 'true');
      assert.equal(await page.evaluate(() => document.activeElement.getAttribute('href')), '/settings', `${layout}: focus moves to the first revealed link`);
      await page.keyboard.press('Escape');
      assert.equal(await more.getAttribute('aria-expanded'), 'false');
      assert.equal(await more.evaluate(el => el === document.activeElement), true, `${layout}: Escape returns focus to More`);
      await more.click();
      await page.getByRole('link', { name: 'My account', exact: true }).click();
      await page.waitForURL(url => url.pathname === '/settings');
      await page.waitForFunction(() => document.querySelector('.rs-shell-more').getAttribute('aria-expanded') === 'false', null, { timeout: 3000 })
        .catch(() => assert.fail(`${layout}: a route change closes More`));
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
      assert.equal(overflow, false, `${layout}: no page wider than the window`);
    } finally { await close(); }
  }
});

test('rs264 a keyboard layout change keeps the chooser shown and focused; where the new layout keeps it behind More, More opens', { timeout: 120000 }, async () => {
  for (const [label, viewport] of [['wide', WIDE], ['phone width', NARROW]]) {
    const { page, close } = await open('/', { viewport, stored: record('classic', 'dark') });
    try {
      const select = page.locator('select[data-rs-layout-chooser]');
      assert.equal(await select.isVisible(), true, `${label}: Classic shows the chooser`);
      await select.focus();
      // Arrow keys on a closed select change it at once, so this walks every layout from the keyboard.
      for (const layout of LAYOUTS.slice(1)) {
        await page.keyboard.press('ArrowDown');
        await page.locator(`[data-rs-layout="${layout}"]`).waitFor();
        // Give Chromium's deferred check for a focused element that is no longer shown time to run.
        await page.waitForTimeout(250);
        assert.equal(await select.evaluate(el => el === document.activeElement), true, `${label} ${layout}: the chooser keeps focus`);
        assert.equal(await select.isVisible(), true, `${label} ${layout}: the chooser stays shown`);
      }
      const moreOpen = await shell(page).evaluate(el => el.hasAttribute('data-nav-open'));
      assert.equal(moreOpen, true, `${label}: Guide keeps the chooser behind More, so More opened`);
      const more = page.locator('.rs-shell-more');
      assert.equal(await more.getAttribute('aria-expanded'), 'true');
      await page.keyboard.press('Escape');
      assert.equal(await more.getAttribute('aria-expanded'), 'false');
      assert.equal(await more.evaluate(el => el === document.activeElement), true, `${label}: Escape returns focus to More`);
    } finally { await close(); }
  }
  // At a wide window Ledger and Console show the chooser in the bar, so a change to them leaves More closed.
  for (const layout of ['ledger', 'console']) {
    const { page, close } = await open('/', { viewport: WIDE, stored: record('classic', 'dark') });
    try {
      const select = page.locator('select[data-rs-layout-chooser]');
      await select.focus();
      await select.selectOption(layout);
      await page.locator(`[data-rs-layout="${layout}"]`).waitFor();
      await page.waitForTimeout(250);
      assert.equal(await select.evaluate(el => el === document.activeElement), true, `wide ${layout}: the chooser keeps focus`);
      assert.equal(await shell(page).evaluate(el => el.hasAttribute('data-nav-open')), false, `wide ${layout}: More stays closed`);
    } finally { await close(); }
  }
});

test('rs264 repository sections: tabs navigate, Back works, unknown falls back to Overview, a Settings draft survives', { timeout: 90000 }, async () => {
  const { page, close } = await open('/repos/A?section=settings', { stored: record('console', 'dark') });
  try {
    const pageRoot = page.locator('[data-rs-page="repo"]');
    assert.equal(await pageRoot.getAttribute('data-rs-section'), 'settings');
    await page.getByRole('button', { name: /^Edit$/ }).click();
    const field = page.locator('input[type="number"]').first();
    await field.waitFor(); await field.fill('987');
    const nav = page.getByRole('navigation', { name: 'Repository sections' });
    await nav.getByRole('link', { name: 'Activity', exact: true }).click();
    await page.waitForURL(url => url.searchParams.get('section') === 'activity');
    assert.equal(await pageRoot.getAttribute('data-rs-section'), 'activity');
    assert.equal(await nav.getByRole('link', { name: 'Activity', exact: true }).getAttribute('aria-current'), 'page');
    assert.equal(await field.isVisible(), false, 'Settings is hidden on Activity');
    await choose(page, 'layout', 'ledger');
    await page.goBack(); await page.waitForURL(url => url.searchParams.get('section') === 'settings');
    assert.equal(await field.inputValue(), '987', 'the draft survives a section change, a layout change and Back');
    await page.goto(origin + 'repos/A?section=nonsense'); await pageRoot.waitFor();
    assert.equal(await pageRoot.getAttribute('data-rs-section'), 'overview');
  } finally { await close(); }
});

test('rs264 Edit shows Settings only where the section navigation is shown; in the existing layouts it keeps URL, history and scroll', { timeout: 90000 }, async () => {
  const view = page => page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() =>
    resolve({ url: location.href, history: history.length, scroll: Math.round(window.scrollY) })))));
  // A DOM click, so the browser does not scroll the button into view first.
  const edit = page => page.getByRole('button', { name: /^Edit$/ }).evaluate(button => button.click());
  for (const layout of ['classic', 'individual', 'team']) {
    const { page, close } = await open('/repos/A', { stored: record(layout, 'dark') });
    try {
      await page.getByRole('button', { name: /^Edit$/ }).waitFor();
      await page.evaluate(() => window.scrollTo(0, 120));
      const before = await view(page);
      assert.equal(before.scroll, 120, `${layout}: the page is scrolled before Edit`);
      await edit(page);
      await page.getByRole('button', { name: /^Edit$/ }).waitFor({ state: 'detached' });
      assert.deepEqual(await view(page), before, `${layout}: Edit keeps the URL, the history and the scroll position`);
    } finally { await close(); }
  }
  const { page, close } = await open('/repos/A', { stored: record('console', 'dark') });
  try {
    const before = await view(page);
    await edit(page);
    await page.waitForURL(url => url.searchParams.get('section') === 'settings');
    assert.equal(await page.locator('[data-rs-page="repo"]').getAttribute('data-rs-section'), 'settings');
    assert.equal((await view(page)).history, before.history + 1, 'Console: Edit shows Settings as a navigation Back can undo');
  } finally { await close(); }
});

test('rs264 the Console and Ledger repository grids apply only to the loaded page and never move a dialog', { timeout: 90000 }, async () => {
  for (const [layout, width] of [['console', 1280], ['ledger', 1000]]) {
    const { page, close } = await open('/repos/A', { viewport: { width, height: 900 }, stored: record(layout, 'dark') });
    try {
      const root = page.locator('[data-rs-page="repo"]');
      await page.locator('.rs-page-sections').waitFor();
      assert.equal(await root.evaluate(el => getComputedStyle(el).display), 'grid', `${layout}: the loaded page is arranged on the grid`);
      await page.getByRole('button', { name: /^Pause sync/ }).click();
      const dialog = page.getByRole('dialog').first(); await dialog.waitFor();
      const box = await dialog.evaluate(el => { const r = el.getBoundingClientRect(); return { top: r.top, left: r.left, right: innerWidth - r.right }; });
      assert.ok(box.top > 8, `${layout}: the dialog is not pinned to the top edge (${box.top})`);
      assert.ok(Math.abs(box.left - box.right) <= 2, `${layout}: the dialog stays centred (${box.left} vs ${box.right})`);
      await page.keyboard.press('Escape');
      await page.goto(origin + 'repos/no-such-repository'); await page.locator('[data-rs-page="repo"]').waitFor();
      await page.waitForTimeout(500);
      assert.notEqual(await page.locator('[data-rs-page="repo"]').evaluate(el => getComputedStyle(el).display), 'grid', `${layout}: the not-found view keeps one column`);
    } finally { await close(); }
  }
});

test('rs264 no route is wider than the window at phone width in the new layouts', { timeout: 180000 }, async () => {
  const routes = ['/', '/repos', '/repos/A', '/conflicts', '/conflicts/conflict-1', '/audit', '/settings', '/users', '/ldap', '/config', '/setup', '/docs'];
  for (const layout of NEW_LAYOUTS) {
    const { page, close } = await open('/', { viewport: NARROW, stored: record(layout, 'dark') });
    try {
      for (const route of routes) {
        await page.goto(origin + route.replace(/^\//, '')); await page.locator('main').waitFor();
        await page.waitForTimeout(300);
        const sizes = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, window: innerWidth }));
        assert.ok(sizes.scroll <= sizes.window, `${layout} ${route}: ${sizes.scroll} > ${sizes.window}`);
      }
    } finally { await close(); }
  }
});

test('rs264 axe finds no violation in the new layouts that Classic dark does not already have, on four routes', { timeout: 180000 }, async () => {
  const routes = ['/', '/repos/A', '/settings', '/config'];
  const run = async (page) => {
    await page.addScriptTag({ path: require.resolve('axe-core/axe.min.js') });
    const result = await page.evaluate(async () => window.axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'] } }));
    return new Set(result.violations.map(v => v.id));
  };
  const baseline = new Map();
  {
    const { page, close } = await open('/', { stored: record('classic', 'dark') });
    try {
      for (const route of routes) { await page.goto(origin + route.replace(/^\//, '')); await page.locator('main').waitFor(); await page.waitForTimeout(500); baseline.set(route, await run(page)); }
    } finally { await close(); }
  }
  for (const layout of NEW_LAYOUTS) {
    for (const theme of ['dark', 'light']) {
      const { page, close } = await open('/', { stored: record(layout, theme) });
      try {
        for (const route of routes) {
          await page.goto(origin + route.replace(/^\//, '')); await page.locator('main').waitFor(); await page.waitForTimeout(500);
          const found = [...await run(page)].filter(id => !baseline.get(route).has(id));
          assert.deepEqual(found, [], `${layout} ${theme} ${route}: new axe violations`);
        }
      } finally { await close(); }
    }
  }
});
