// Designer's own check of the static prototypes. Sandboxed Chromium, run as a non-root user.
// Usage: node render_check.js <copy of the tree> <output folder> [shots.json]
// Run as a non-root user so that Chromium's sandbox is on. Nothing outside the tree is requested.
const { chromium } = require('playwright');
const fs = require('fs'), path = require('path');
const pkg = path.resolve(process.argv[2]), out = path.resolve(process.argv[3]);
const shots = process.argv[4] ? JSON.parse(fs.readFileSync(process.argv[4], 'utf8')) : [];
function walk(d, acc = []) { for (const f of fs.readdirSync(d)) { const p = path.join(d, f); fs.statSync(p).isDirectory() ? walk(p, acc) : acc.push(p); } return acc; }
const pages = walk(pkg).filter(p => p.endsWith('.html') && !p.includes('/tools/')).map(p => path.relative(pkg, p)).sort();
const VIEW = { wide: { width: 1440, height: 900 }, narrow: { width: 390, height: 844 } };
(async () => {
  const browser = await chromium.launch({ headless: true, chromiumSandbox: true });
  // evidence that the sandbox is on: no --no-sandbox flag on any chrome process of this user. Read once, right after launch.
  const mine = pid => { try { return new RegExp('^Uid:\\s+' + process.getuid() + '\\s', 'm').test(fs.readFileSync(`/proc/${pid}/status`, 'utf8')); } catch { return false; } };
  const procs = fs.readdirSync('/proc').filter(x => /^\d+$/.test(x) && mine(x)).map(pid => { try { return fs.readFileSync(`/proc/${pid}/cmdline`, 'utf8').split('\0').join(' '); } catch { return ''; } }).filter(c => c.includes('chrome'));
  const sandbox = { chromeProcesses: procs.length, withNoSandboxFlag: procs.filter(c => c.includes('--no-sandbox')).length, zygotes: procs.filter(c => c.includes('--type=zygote')).length };
  const results = [], blocked = [];
  for (const [vname, vp] of Object.entries(VIEW)) {
    for (const theme of ['light', 'dark']) {
      const ctx = await browser.newContext({ viewport: vp, deviceScaleFactor: 1, reducedMotion: 'reduce', colorScheme: theme });
      await ctx.route('**/*', route => {
        const u = route.request().url();
        if (u.startsWith('file://') && decodeURIComponent(u.slice(7)).startsWith(pkg + '/')) return route.continue();
        blocked.push(u); return route.abort();
      });
      const page = await ctx.newPage();
      for (const rel of pages) {
        const isProto = rel.startsWith('options/') && rel.includes('/screens/') || rel.includes('/flows/') || rel.startsWith('prototypes/');
        const errors = [];
        const onErr = e => errors.push(String(e).slice(0, 200)), onCon = m => { if (m.type() === 'error') errors.push(m.text().slice(0, 200)); };
        page.on('pageerror', onErr); page.on('console', onCon);
        await page.goto('about:blank');
        await page.goto('file://' + path.join(pkg, rel) + (isProto ? `#${theme}-nobar` : ''), { waitUntil: 'load' });
        const r = await page.evaluate(() => {
          const de = document.documentElement, vw = window.innerWidth;
          const res = { pageOverflow: de.scrollWidth > vw + 1 ? de.scrollWidth : 0, inner: [], frames: [], theme: de.getAttribute('data-rs-theme') };
          const vis = el => { const s = getComputedStyle(el); return s.display !== 'none' && s.visibility !== 'hidden' && el.getClientRects().length > 0; };
          const inFrame = el => el.closest('.tw, pre, .diff, .log, [data-proto-bar]');
          // 1. selected elements inside main, measured left and right against the nearest box that draws an edge.
          //    Not measured: height, text that overflows inside a block whose own box fits, the shell, and open menus.
          const BOX = '.sec, .card, .bn, .dlg, .op, .evid, dd, td, th, li, .board > *, .login-card, main';
          for (const el of document.querySelectorAll('main .mono, main .sg, main .tag-needs, main .pill, main .chip, main .stamp, main .btn, main a, main h1, main h2, main h3, main p, main dt, main dd, main label, main .in, main span.err, main span.okl')) {
            if (!vis(el) || inFrame(el)) continue;
            const box = el.parentElement && el.parentElement.closest(BOX); if (!box) continue;
            const a = el.getBoundingClientRect(), b = box.getBoundingClientRect();
            if (a.width === 0) continue;
            if (a.right > b.right + 1.5 || a.left < b.left - 1.5) res.inner.push({ el: el.tagName.toLowerCase() + '.' + el.className.toString().trim().split(/\s+/).join('.'), text: (el.textContent || '').trim().slice(0, 50), over: Math.round(Math.max(a.right - b.right, b.left - a.left)), box: box.tagName.toLowerCase() + '.' + box.className.toString().trim().split(/\s+/)[0] });
            if (a.right > vw + 1.5) res.inner.push({ el: 'PAST-VIEWPORT ' + el.tagName.toLowerCase(), text: (el.textContent || '').trim().slice(0, 50), over: Math.round(a.right - vw), box: 'viewport' });
          }
          // 2. table frames that scroll
          for (const f of document.querySelectorAll('main .tw')) if (vis(f) && f.scrollWidth > f.clientWidth + 1) res.frames.push({ label: f.getAttribute('aria-label') || '', by: f.scrollWidth - f.clientWidth });
          return res;
        });
        page.off('pageerror', onErr); page.off('console', onCon);
        const id = rel.replace(/\.html$/, '') + `__${theme}__${vname}`;
        results.push({ id, rel, theme, view: vname, errors, pageOverflow: r.pageOverflow, inner: r.inner, frames: r.frames, themeMismatch: isProto && !/\/g01-/.test(rel) && r.theme && r.theme !== theme ? r.theme : null });
        if (shots.includes(id)) await page.screenshot({ path: path.join(out, id.replace(/\//g, '_') + '.png'), fullPage: true });
      }
      await ctx.close();
    }
  }
  await browser.close();
  const sum = { pages: pages.length, loads: results.length, sandbox, blockedRequests: blocked.length,
    withErrors: results.filter(r => r.errors.length).length, pageOverflow: results.filter(r => r.pageOverflow).length,
    innerOverflow: results.filter(r => r.inner.length).length, themeMismatch: results.filter(r => r.themeMismatch).length,
    scrollingFramesWide: results.filter(r => r.view === 'wide' && r.frames.length).length, scrollingFramesNarrow: results.filter(r => r.view === 'narrow' && r.frames.length).length };
  fs.writeFileSync(path.join(out, 'check-results.json'), JSON.stringify({ summary: sum, blocked, results }, null, 1));
  console.log(JSON.stringify(sum));
})();
