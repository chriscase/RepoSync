import { describe, expect, it } from 'vitest';

// The colour bridge (scripts/colour-bridge.mjs, src/layouts/colourBridge.css), checked from the
// source: the committed file is what the generator writes, every colour class has a rule, and
// the generator reads every colour class the source writes, whatever its variant prefix.
//
// This project installs no Node type definitions (see LayoutFoundation.test.ts), so node:fs and
// the generator are loaded through specifiers the type checker does not resolve, and typed here.
// Vitest runs these tests in Node.
const NODE_FS = 'node:fs';
const GENERATOR = '../../scripts/colour-bridge.mjs';

interface NodeFs {
  readFileSync(path: string | URL, encoding: 'utf8'): string;
  readdirSync(path: string | URL): string[];
  statSync(path: string | URL): { isDirectory(): boolean };
}
interface ParsedClass {
  cls: string;
  variants: string[];
  utility: string;
  colour: string;
  step: number | null;
  opacity: number | null;
}
interface Generator {
  OUTPUT: string;
  scanSource(dir?: string): Map<string, number>;
  generate(counts?: Map<string, number>): { css: string; unmapped: string[]; classes: number };
  parseClass(cls: string): ParsedClass | null;
  tokenFor(parsed: ParsedClass): string | null;
  ruleFor(parsed: ParsedClass, token: string): string | null;
}

const nodeFs = async () => (await import(/* @vite-ignore */ NODE_FS)) as NodeFs;
const generator = async () => (await import(/* @vite-ignore */ GENERATOR)) as Generator;

/** The rule the generator writes for one class, or null when it writes none. */
async function rule(cls: string): Promise<string | null> {
  const { parseClass, tokenFor, ruleFor } = await generator();
  const parsed = parseClass(cls);
  const token = parsed && tokenFor(parsed);
  return parsed && token ? ruleFor(parsed, token) : null;
}

/*
 * A deliberately broad scan, independent of the generator's pattern. The source is cut into
 * class-like tokens at whitespace, quotes, backticks, braces, parentheses, commas and
 * semicolons. A token is a colour class when it ends in a colour utility (more utilities and
 * every Tailwind colour name, not only those the bridge maps), after any prefix ending in a
 * colon: a plain variant, a named group (`group-hover/card:`), an arbitrary variant
 * (`aria-[invalid=true]:`, `[&>a]:`) or several of these, and an optional `!` or `-`.
 */
const BROAD_UTILITIES = 'bg|text|border(?:-[trblxyse])?|divide|ring|ring-offset|outline|placeholder|accent|from|via|to|fill|stroke|decoration|shadow|caret';
const BROAD_COLOURS = 'slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose|white|black';
const TOKEN = /[^\s'"`{}(),;]+/g;
const BROAD_CLASS = new RegExp(String.raw`^(?:.*:)?!?-?(?:${BROAD_UTILITIES})-(?:${BROAD_COLOURS})(?:-\d{2,3})?(?:\/\d{1,3})?$`);

async function broadScan(): Promise<Map<string, string>> {
  const fs = await nodeFs();
  const found = new Map<string, string>();
  const walk = (dir: URL) => {
    for (const name of fs.readdirSync(dir)) {
      const entry = new URL(name, dir);
      if (fs.statSync(entry).isDirectory()) walk(new URL(`${name}/`, dir));
      else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) {
        for (const [token] of fs.readFileSync(entry, 'utf8').matchAll(TOKEN)) {
          if (BROAD_CLASS.test(token)) found.set(token, entry.pathname.replace(/^.*\/src\//, 'src/'));
        }
      }
    }
  };
  walk(new URL('../', import.meta.url));
  return found;
}

describe('#264 colour bridge: the committed file and the source agree', () => {
  it('(a) regenerating from the source gives exactly the committed colourBridge.css', async () => {
    const { generate, OUTPUT } = await generator();
    const committed = (await nodeFs()).readFileSync(OUTPUT, 'utf8');
    expect(generate().css === committed, 'colourBridge.css is stale: run node scripts/colour-bridge.mjs').toBe(true);
  });

  it('(b) leaves no colour class without a rule', async () => {
    const { generate } = await generator();
    const { unmapped, classes } = generate();
    expect(unmapped).toEqual([]);
    expect(classes).toBeGreaterThan(200);
  });

  it('(c) reads every colour class a broad scan of the source finds, with any variant prefix', async () => {
    const { scanSource } = await generator();
    const scanned = scanSource();
    const broad = await broadScan();
    const missed = [...broad].filter(([cls]) => !scanned.has(cls)).map(([cls, file]) => `${cls} (${file})`);
    expect(missed).toEqual([]);
    // The broad scan does find the classes that once slipped past the generator's prefix.
    for (const cls of ['group-hover/card:text-blue-400', 'group-hover/branch:text-purple-300', 'aria-[invalid=true]:border-red-400']) {
      expect(broad.has(cls), cls).toBe(true);
      expect(scanned.has(cls), cls).toBe(true);
    }
  });
});

describe('#264 colour bridge: variants', () => {
  it('reads a named group variant whole and scopes it to that named group', async () => {
    const { parseClass } = await generator();
    expect(parseClass('group-hover/card:text-blue-400')).toMatchObject({ variants: ['group-hover/card'], utility: 'text', colour: 'blue', step: 400 });
    expect(await rule('group-hover/card:text-blue-400')).toBe(
      '.rs-layout-shell[data-rs-bridge] .group\\/card:hover .group-hover\\/card\\:text-blue-400 { color: var(--rs-link); }',
    );
    expect(await rule('group-hover/branch:text-purple-300')).toBe(
      '.rs-layout-shell[data-rs-bridge] .group\\/branch:hover .group-hover\\/branch\\:text-purple-300 { color: var(--rs-git); }',
    );
  });

  it('keeps the unnamed group variant as before, and reads the other group states the same way', async () => {
    expect(await rule('group-hover:text-blue-400')).toBe(
      '.rs-layout-shell[data-rs-bridge] .group:hover .group-hover\\:text-blue-400 { color: var(--rs-link); }',
    );
    expect(await rule('group-focus-within/card:text-blue-400')).toBe(
      '.rs-layout-shell[data-rs-bridge] .group\\/card:focus-within .group-focus-within\\/card\\:text-blue-400 { color: var(--rs-link); }',
    );
  });

  it('turns an aria-[…] variant into an attribute selector on the element', async () => {
    const { parseClass } = await generator();
    expect(parseClass('aria-[invalid=true]:border-red-400')).toMatchObject({ variants: ['aria-[invalid=true]'], utility: 'border', colour: 'red' });
    expect(await rule('aria-[invalid=true]:border-red-400')).toBe(
      '.rs-layout-shell[data-rs-bridge] .aria-\\[invalid\\=true\\]\\:border-red-400[aria-invalid="true"] { border-color: var(--rs-danger); }',
    );
    expect(await rule('hover:aria-[invalid=true]:border-red-400')).toBe(
      '.rs-layout-shell[data-rs-bridge] .hover\\:aria-\\[invalid\\=true\\]\\:border-red-400:hover[aria-invalid="true"] { border-color: var(--rs-danger); }',
    );
  });

  it('keeps existing variants and pseudo-elements as before', async () => {
    expect(await rule('hover:bg-gray-700')).toBe('.rs-layout-shell[data-rs-bridge] .hover\\:bg-gray-700:hover { background-color: var(--rs-surface-2); }');
    expect(await rule('focus:placeholder-gray-400')).toBe(
      '.rs-layout-shell[data-rs-bridge] .focus\\:placeholder-gray-400:focus::placeholder { color: var(--rs-text-2); }',
    );
    expect(await rule('md:text-gray-300')).toBe('@media (min-width: 768px) { .rs-layout-shell[data-rs-bridge] .md\\:text-gray-300 { color: var(--rs-text-2); } }');
  });

  it('reads other variant forms whole and writes no rule for them, so they are reported as unmapped', async () => {
    const { parseClass, generate } = await generator();
    for (const cls of ['peer-checked:bg-blue-600', 'group-hover/card:[&>a]:text-blue-400', 'data-[state=open]:bg-gray-700', 'group-[.is-open]:text-blue-400']) {
      expect(parseClass(cls)?.cls, cls).toBe(cls);
      expect(await rule(cls), cls).toBeNull();
    }
    expect(parseClass('[&>a]:text-blue-400')?.variants).toEqual(['[&>a]']);
    expect(parseClass('[&:not(:first-child)]:border-gray-700')?.variants).toEqual(['[&:not(:first-child)]']);
    expect(generate(new Map([['peer-checked:bg-blue-600', 1], ['bg-gray-800', 1]])).unmapped).toEqual(['peer-checked:bg-blue-600']);
  });
});
