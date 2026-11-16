// #263 capture harness: renders static synthetic prototype pages with the owner-approved
// sandboxed Chromium (run as uid 1000 via setpriv; never --no-sandbox) and records, per page,
// theme and viewport: a PNG, console errors, failed or blocked requests, horizontal overflow,
// the theme the page actually applied, and visible elements that extend past the viewport
// without a clipping or scrolling ancestor.
//
// Driver: the machine's preinstalled playwright-core 1.63.0 (/opt/node-tools, read-only), whose
// pinned browser is exactly the installed Chromium 1243 (153.0.8010.12). Nothing is installed.
// Usage (as uid 1000): node capture.mjs <package-root> <out-dir> <pages.json>
// pages.json: [{ "id": "ledger/g02-dashboard", "path": "options/ledger/screens/g02-dashboard.html",
//               "themes": { "light": {...}, "dark": {...} } }]
// A theme entry says how to select it: { "hash": "light-nobar" } sets the address fragment,
// { "query": "theme=dark" } a query string, { "attribute": ["html", "data-theme", "dark"] } sets an
// attribute after load, and { "colorScheme": "dark" } emulates the device setting. They combine.
import { chromium } from '/opt/node-tools/node_modules/playwright-core/index.mjs';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const [root, out, pagesFile] = process.argv.slice(2);
const pages = JSON.parse(readFileSync(pagesFile, 'utf8'));
const VIEWPORTS = { wide: { width: 1440, height: 900 }, narrow: { width: 390, height: 844 } };
const rootUrl = pathToFileURL(resolve(root) + '/').href;
const results = [];
mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ executablePath: process.env.REPOSYNC_A11Y_BROWSER, headless: true, chromiumSandbox: true, timeout: 15000 });
try {
  for (const page of pages) {
    for (const [theme, how] of Object.entries(page.themes)) {
      for (const [vpName, viewport] of Object.entries(VIEWPORTS)) {
        const context = await browser.newContext({ viewport, deviceScaleFactor: 1, reducedMotion: 'reduce', colorScheme: how.colorScheme ?? 'light' });
        const blocked = [];
        const failed = [];
        // Only files inside the package may load; everything else is aborted and recorded.
        await context.route('**/*', route => {
          const url = route.request().url();
          if (url.startsWith(rootUrl)) return route.continue();
          blocked.push(url);
          return route.abort();
        });
        const tab = await context.newPage();
        const errors = [];
        tab.on('pageerror', error => errors.push('pageerror: ' + error.message));
        tab.on('console', message => { if (message.type() === 'error') errors.push('console: ' + message.text()); });
        tab.on('requestfailed', request => { if (request.url().startsWith(rootUrl)) failed.push(request.url().slice(rootUrl.length)); });
        const url = new URL(page.path, rootUrl);
        if (how.query) url.search = how.query;
        if (how.hash) url.hash = how.hash;
        const file = join(out, `${page.id.replace(/[^a-zA-Z0-9._-]+/g, '_')}__${theme}__${vpName}.png`);
        try {
          await tab.goto(url.href, { waitUntil: 'load', timeout: 15000 });
          if (how.attribute) await tab.evaluate(([selector, name, value]) => document.querySelector(selector)?.setAttribute(name, value), how.attribute);
          await tab.waitForTimeout(100);
          await tab.screenshot({ path: file, fullPage: true });
          const metrics = await tab.evaluate(() => {
            const vw = window.innerWidth;
            const describe = el => el.tagName.toLowerCase() + (el.id ? '#' + el.id : '') + (el.classList.length ? '.' + [...el.classList].join('.') : '');
            const offscreen = [];
            for (const el of document.body.querySelectorAll('*')) {
              const r = el.getBoundingClientRect();
              if (r.width <= 1 || r.height <= 1) continue;
              if (r.right <= vw + 1 && r.left >= -1) continue;
              const style = getComputedStyle(el);
              if (style.visibility === 'hidden' || style.display === 'none') continue;
              let a = el.parentElement, contained = false;
              while (a && a !== document.body && a !== document.documentElement) {
                if (getComputedStyle(a).overflowX !== 'visible') { contained = true; break; }
                a = a.parentElement;
              }
              if (!contained) offscreen.push({ el: describe(el), left: Math.round(r.left), right: Math.round(r.right) });
            }
            const html = document.documentElement;
            return {
              scrollWidth: html.scrollWidth, innerWidth: vw, height: html.scrollHeight, title: document.title,
              appliedTheme: html.getAttribute('data-rs-theme'),
              shellThemes: [...document.querySelectorAll('.rs-layout-shell')].map(s => s.getAttribute('data-rs-theme')),
              offscreenCount: offscreen.length, offscreen: offscreen.slice(0, 8),
            };
          });
          results.push({ id: page.id, path: page.path, theme, viewport: vpName, file, horizontalOverflow: metrics.scrollWidth > metrics.innerWidth, ...metrics, errors, blocked, failed, status: 'ok' });
        } catch (error) {
          results.push({ id: page.id, path: page.path, theme, viewport: vpName, errors, blocked, failed, status: 'failed: ' + error.message.split('\n')[0] });
        }
        await context.close();
      }
    }
  }
} finally {
  await browser.close();
}
writeFileSync(join(out, 'capture-results.json'), JSON.stringify(results, null, 2));
const themeMismatch = r => r.status === 'ok' && r.appliedTheme !== null && pages.find(p => p.id === r.id)?.expectTheme !== false && r.appliedTheme !== r.theme;
const bad = results.filter(r => r.status !== 'ok' || r.errors.length || r.blocked.length || r.failed.length || r.horizontalOverflow || r.offscreenCount || themeMismatch(r));
console.log(JSON.stringify({
  captures: results.length, withIssues: bad.length,
  failedStatus: results.filter(r => r.status !== 'ok').length,
  withErrors: results.filter(r => r.errors.length).length,
  withBlocked: results.filter(r => r.blocked.length).length,
  withFailedRequests: results.filter(r => r.failed.length).length,
  horizontalOverflow: results.filter(r => r.horizontalOverflow).length,
  offscreen: results.filter(r => r.offscreenCount).length,
  themeMismatch: results.filter(themeMismatch).length,
}, null, 2));
