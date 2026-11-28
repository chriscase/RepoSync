import { describe, expect, it } from 'vitest';

// #264 page tokens (pageTokens.css), checked against the CSS that uses them and against the
// contrast targets of the accepted design (docs/design/263/v1.6/contracts/design-system.md §9,
// contrast-computed.csv): 4.5:1 for text and state colours, 3:1 for control borders and focus.
//
// Seven token sets have page tokens: Ledger, Console and Guide in light and in dark, and
// Classic, Individual and Team in light. Each new layout's set is its layout block (type,
// spacing, radius) plus its theme block (colour). Classic, Individual and Team in dark declare
// no page tokens and are not checked here.
//
// node:fs is loaded through a specifier the type checker does not resolve (see LayoutFoundation.test.ts).
const NODE_FS = 'node:fs';
const readCss = async (name: string) =>
  ((await import(/* @vite-ignore */ NODE_FS)) as { readFileSync(path: URL, encoding: 'utf8'): string })
    .readFileSync(new URL(name, import.meta.url), 'utf8')
    .replace(/\/\*[\s\S]*?\*\//g, '');

const NEW_LAYOUTS = ['ledger', 'console', 'guide'] as const;
const EXISTING_LAYOUTS = ['classic', 'individual', 'team'] as const;
type Layout = (typeof NEW_LAYOUTS)[number] | (typeof EXISTING_LAYOUTS)[number];
type Theme = 'light' | 'dark';
interface TokenSet { name: string; layout: Layout; theme: Theme }
const TOKEN_SETS: TokenSet[] = [
  ...NEW_LAYOUTS.flatMap(layout => (['light', 'dark'] as const).map(theme => ({ name: `${layout} ${theme}`, layout, theme }))),
  { name: 'classic, individual and team light', layout: 'classic', theme: 'light' },
];

/** Every innermost rule of a stylesheet (rules inside @media included), as selector and body. */
function rules(css: string): { selector: string; body: string }[] {
  return [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map(([, selector, body]) => ({ selector: selector.trim(), body }));
}
const declared = (body: string) => new Map([...body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)].map(([, name, value]) => [name, value.trim()]));
const used = (body: string) => new Set([...body.matchAll(/var\((--[\w-]+)/g)].map(([, name]) => name));

/**
 * Whether a rule applies to a token set, from what its selector names: the layouts in
 * `data-rs-layout`, and the theme in `data-rs-theme`. A rule that names no layout applies to
 * every set here (each one carries data-rs-bridge).
 */
function applies(selector: string, set: TokenSet): boolean {
  const layouts = [...selector.matchAll(/data-rs-layout='(\w+)'/g)].map(([, layout]) => layout);
  const themes = [...selector.matchAll(/data-rs-theme='(\w+)'/g)].map(([, theme]) => theme);
  const layoutOk = layouts.length === 0 || layouts.includes(set.layout);
  const themeOk = themes.length === 0 || themes.includes(set.theme);
  return layoutOk && themeOk;
}

/** The tokens pageTokens.css gives one set: its layout block and its theme block. */
async function tokenValues(set: TokenSet): Promise<Map<string, string>> {
  const values = new Map<string, string>();
  for (const { selector, body } of rules(await readCss('./pageTokens.css'))) {
    if (applies(selector, set)) for (const [name, value] of declared(body)) values.set(name, value);
  }
  return values;
}

function luminance(hex: string): number {
  const match = /^#([0-9a-f]{6}|[0-9a-f]{3})$/i.exec(hex);
  if (!match) throw new Error(`not a hex colour: ${hex}`);
  const digits = match[1].length === 3 ? [...match[1]].map(d => d + d).join('') : match[1];
  const [r, g, b] = [0, 2, 4].map(i => parseInt(digits.slice(i, i + 2), 16) / 255)
    .map(c => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}
/** WCAG 2 contrast ratio of two hex colours. */
function contrast(a: string, b: string): number {
  const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (light + 0.05) / (dark + 0.05);
}

/** The design's pairs (contrast-computed.csv): foreground, background, target. */
const PAIRS: [string, string, number][] = [
  ['text', 'bg', 4.5], ['text', 'surface', 4.5], ['text-2', 'surface', 4.5],
  ['text-3', 'bg', 4.5], ['text-3', 'surface', 4.5], ['text-3', 'surface-2', 4.5],
  ['link', 'surface', 4.5], ['accent-text', 'accent', 4.5], ['danger-solid-text', 'danger-solid', 4.5],
  ['ok', 'surface', 4.5], ['run', 'surface', 4.5], ['warn', 'surface', 4.5], ['held', 'surface', 4.5],
  ['danger', 'surface', 4.5], ['off', 'surface', 4.5],
  ['text', 'ok-soft', 4.5], ['warn', 'warn-soft', 4.5], ['held', 'held-soft', 4.5], ['danger', 'danger-soft', 4.5],
  ['svn', 'surface', 4.5], ['git', 'surface', 4.5], ['code-text', 'code-bg', 4.5],
  ['line-strong', 'surface', 3], ['focus', 'surface', 3],
  ['shell-text-muted', 'shell-chrome', 4.5], ['shell-text-strong', 'shell-chrome', 4.5],
  ['shell-accent-text', 'shell-accent', 4.5], ['shell-alert-text', 'shell-alert', 4.5],
  ['shell-notice-text', 'shell-chrome', 4.5], ['shell-focus', 'shell-chrome', 3],
];

describe('#264 page tokens: every token the styles use has a value', () => {
  it.each(TOKEN_SETS)('$name', async set => {
    const values = await tokenValues(set);
    const missing: string[] = [];
    for (const file of ['./pageClasses.css', './layoutOptions.css', './colourBridge.css']) {
      const applying = rules(await readCss(file)).filter(({ selector }) => applies(selector, set));
      // A token these styles set for themselves (the diff viewer's) needs no value from pageTokens.css.
      const ownTokens = new Set(applying.flatMap(({ body }) => [...declared(body).keys()]));
      for (const { body } of applying) {
        for (const token of used(body)) if (!values.has(token) && !ownTokens.has(token)) missing.push(`${file} ${token}`);
      }
    }
    expect([...new Set(missing)]).toEqual([]);
    // The check reads real rules: each set uses page and shell tokens from these files.
    expect(values.has('--rs-text') && values.has('--rs-surface') && values.has('--rs-shell-chrome')).toBe(true);
  });

  it('gives the new layouts their type and shape tokens, which the styles use only there', async () => {
    for (const set of TOKEN_SETS.filter(({ layout }) => layout !== 'classic')) {
      const values = await tokenValues(set);
      for (const token of ['--rs-font-ui', '--rs-radius', '--rs-control-h']) expect(values.has(token), `${set.name} ${token}`).toBe(true);
    }
  });
});

describe('#264 page tokens: contrast meets the design targets', () => {
  it('computes WCAG contrast as the design does', () => {
    expect(contrast('#000000', '#ffffff')).toBeCloseTo(21, 5);
    expect(contrast('#fff', '#fff')).toBeCloseTo(1, 5);
    // Ledger light, body text on surface: 17.76:1 in contrast-computed.csv.
    expect(contrast('#16181d', '#ffffff')).toBeCloseTo(17.76, 2);
  });

  it.each(TOKEN_SETS)('$name: text on page and surface, secondary and muted text on surface reach 4.5:1', async set => {
    const values = await tokenValues(set);
    for (const [foreground, background] of [['text', 'bg'], ['text', 'surface'], ['text-2', 'surface'], ['text-3', 'surface']]) {
      const ratio = contrast(values.get(`--rs-${foreground}`) ?? '', values.get(`--rs-${background}`) ?? '');
      expect(ratio, `${set.name}: ${foreground} on ${background} is ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
    }
  });

  it.each(TOKEN_SETS)('$name: every pair in the design meets its target', async set => {
    const values = await tokenValues(set);
    const below = PAIRS.flatMap(([foreground, background, target]) => {
      const ratio = contrast(values.get(`--rs-${foreground}`) ?? '', values.get(`--rs-${background}`) ?? '');
      return ratio >= target ? [] : [`${foreground} on ${background}: ${ratio.toFixed(2)}:1, target ${target}:1`];
    });
    expect(below).toEqual([]);
  });
});
