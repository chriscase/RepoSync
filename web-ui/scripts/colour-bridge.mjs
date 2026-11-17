#!/usr/bin/env node
/**
 * #264 colour bridge generator.
 *
 * Pages colour themselves with Tailwind utility classes. Classic, Individual and Team in
 * dark keep those classes exactly. Every other layout and theme sets data-rs-bridge on the
 * shell, and this file's output re-points each colour utility the source uses at a page
 * token (pageTokens.css), following the rules in docs/design/263/v1.6/contracts/colour-bridge.md.
 *
 * Usage: node scripts/colour-bridge.mjs            writes src/layouts/colourBridge.css
 *        node scripts/colour-bridge.mjs --check    exits 1 if the committed file is stale
 *
 * Both exit 1 when a colour class has no rule. The functions are exported for
 * src/layouts/colourBridge.test.ts, which fails when the committed file is stale, when a
 * colour class has no rule, or when a colour class in src is one this scan does not read.
 */
import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
export const OUTPUT = join(ROOT, 'src/layouts/colourBridge.css');
const SHELL = '.rs-layout-shell[data-rs-bridge]';

const GREY = new Set(['gray', 'slate', 'zinc', 'neutral', 'stone']);
/** State families: green and emerald are both OK, yellow and amber both warning, purple the Git side. */
const FAMILY = {
  red: 'danger', rose: 'danger', pink: 'danger',
  green: 'ok', emerald: 'ok', lime: 'ok', teal: 'ok',
  yellow: 'warn', amber: 'warn',
  orange: 'held',
  blue: 'accent', indigo: 'accent', sky: 'accent',
  cyan: 'run',
  purple: 'git', violet: 'git', fuchsia: 'git',
};
const COLOURS = [...GREY, ...Object.keys(FAMILY), 'white', 'black'];
const UTILITIES = ['bg', 'text', 'border', 'border-t', 'border-b', 'border-l', 'border-r', 'border-x', 'border-y',
  'divide', 'ring', 'ring-offset', 'outline', 'placeholder', 'accent'];
const VARIANTS = {
  hover: ':hover', focus: ':focus', 'focus-visible': ':focus-visible', 'focus-within': ':focus-within',
  active: ':active', disabled: ':disabled', checked: ':checked',
};

/**
 * One variant before a colour utility: a plain one (`hover`, `md`), a named group or peer
 * (`group-hover/card`), or one with an arbitrary value (`aria-[invalid=true]`, `[&>a]`).
 * Every variant is read, so a class is never cut short at its prefix; ruleFor decides which
 * ones the bridge knows, and an unknown one leaves the class unmapped, which stops the
 * generator and fails colourBridge.test.ts.
 */
const VARIANT = String.raw`(?:[a-z-]*\[[^\]\s'"\`]+\]|[a-z-]+(?:\/[\w-]+)?)`;

const CLASS_PATTERN = new RegExp(
  String.raw`(?<![\w:/\]-])((?:${VARIANT}:)*)(${UTILITIES.map(u => u.replace('-', '\\-')).sort((a, b) => b.length - a.length).join('|')})-(${COLOURS.join('|')})(?:-(\d{2,3}))?(?:\/(\d{1,3}))?(?![\w/-])`,
  'g',
);

/** The variants of a class prefix (`a:b:`), split on the colons outside brackets. */
function splitVariants(prefix) {
  const variants = [];
  let depth = 0;
  let current = '';
  for (const char of prefix) {
    if (char === '[') depth++;
    else if (char === ']') depth--;
    if (char === ':' && depth === 0) { variants.push(current); current = ''; }
    else current += char;
  }
  return variants;
}

/** Every colour utility class written in the source, with how often it occurs. */
export function scanSource(dir = join(ROOT, 'src')) {
  const counts = new Map();
  const walk = path => {
    for (const name of readdirSync(path)) {
      const full = join(path, name);
      if (statSync(full).isDirectory()) walk(full);
      else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) {
        for (const match of readFileSync(full, 'utf8').matchAll(CLASS_PATTERN)) {
          const cls = match[0];
          counts.set(cls, (counts.get(cls) ?? 0) + 1);
        }
      }
    }
  };
  walk(dir);
  return counts;
}

export function parseClass(cls) {
  const match = new RegExp(`^${CLASS_PATTERN.source}$`).exec(cls);
  if (!match) return null;
  const [, prefix, utility, colour, step, opacity] = match;
  return {
    cls, variants: splitVariants(prefix), utility, colour,
    step: step === undefined ? null : Number(step), opacity: opacity === undefined ? null : Number(opacity),
  };
}

/** The page token a parsed class resolves to, or null when no rule decides it. */
export function tokenFor({ utility, colour, step, opacity }) {
  const translucent = opacity !== null && opacity <= 50;
  if (colour === 'black') return utility === 'bg' && opacity !== null ? 'scrim' : null;
  if (colour === 'white') return null;
  if (GREY.has(colour)) {
    if (utility === 'bg') {
      if (step <= 100) return 'surface-2';
      if (step >= 900) return 'bg';
      if (step === 800) return 'surface';
      if (step === 700 || (step >= 200 && step <= 400)) return 'surface-2';
      return 'surface-3';
    }
    if (utility === 'text' || utility === 'placeholder') {
      if (step <= 200) return 'text';
      if (step <= 400) return 'text-2';
      // Steps 700 to 900 occur only on the documentation page's light panel, where they are
      // body and heading text; mapping them to the muted token would mute the whole page.
      if (step >= 800) return 'text';
      if (step === 700) return 'text-2';
      return 'text-3';
    }
    if (utility === 'ring' || utility === 'outline') return step >= 700 ? 'line' : 'line-strong';
    // border, divide, ring-offset
    if (utility === 'ring-offset') return 'bg';
    return step >= 700 ? 'line' : 'line-strong';
  }
  const family = FAMILY[colour];
  if (!family) return null;
  if (utility === 'bg') {
    if (step >= 800 || step <= 200 || translucent) return family === 'git' ? 'accent-soft' : `${family}-soft`;
    if (family === 'danger') return 'danger-solid';
    return family;
  }
  if (utility === 'text' || utility === 'placeholder') {
    if (step <= 200) return 'text';
    return family === 'accent' ? 'link' : family;
  }
  if (utility === 'ring' || utility === 'outline') return family === 'accent' ? 'focus' : family;
  if (utility === 'ring-offset') return 'bg';
  if (utility === 'accent') return family === 'accent' ? 'link' : family;
  // border, divide
  return family === 'accent' ? 'link' : family;
}

function escapeClass(cls) {
  return cls.replace(/[^\w-]/g, '\\$&');
}

/** `aria-[name=value]` as the attribute selector `[aria-name="value"]`; null for any other form. */
function ariaAttribute(variant) {
  const match = /^aria-\[([a-z-]+)(?:=([\w-]+))?\]$/.exec(variant);
  if (!match) return null;
  const [, name, value] = match;
  return value === undefined ? `[aria-${name}]` : `[aria-${name}="${value}"]`;
}

const MEDIA = { sm: 640, md: 768, lg: 1024, xl: 1280, '2xl': 1536 };

/**
 * One CSS rule for a class, or null when a variant is not one this bridge knows. A group
 * variant applies when an ancestor `.group` (or `.group/<name>` for a named group) is in that
 * state; `aria-[…]` applies when the element has that ARIA attribute.
 */
export function ruleFor(parsed, token) {
  let selector = `.${escapeClass(parsed.cls)}`;
  let media = null;
  let group = null;
  let element = '';
  for (const variant of parsed.variants) {
    const named = /^group-([a-z-]+)(?:\/([\w-]+))?$/.exec(variant);
    const aria = ariaAttribute(variant);
    if (VARIANTS[variant]) selector += VARIANTS[variant];
    else if (named && VARIANTS[named[1]]) group = `.${escapeClass(named[2] === undefined ? 'group' : `group/${named[2]}`)}${VARIANTS[named[1]]}`;
    else if (aria) selector += aria;
    else if (MEDIA[variant]) media = MEDIA[variant];
    else if (variant === 'placeholder') element = '::placeholder';
    else return null;
  }
  selector += element;
  const value = `var(--rs-${token})`;
  let declarations;
  const side = { 'border-t': 'top', 'border-b': 'bottom', 'border-l': 'left', 'border-r': 'right' }[parsed.utility];
  switch (parsed.utility) {
    case 'bg': declarations = `background-color: ${value};`; break;
    case 'text': declarations = `color: ${value};`; break;
    case 'placeholder': selector += '::placeholder'; declarations = `color: ${value};`; break;
    case 'ring': declarations = `--tw-ring-color: ${value};`; break;
    case 'ring-offset': declarations = `--tw-ring-offset-color: ${value};`; break;
    case 'outline': declarations = `outline-color: ${value};`; break;
    case 'accent': declarations = `accent-color: ${value};`; break;
    case 'divide': selector += ' > :not([hidden]) ~ :not([hidden])'; declarations = `border-color: ${value};`; break;
    case 'border-x': declarations = `border-left-color: ${value}; border-right-color: ${value};`; break;
    case 'border-y': declarations = `border-top-color: ${value}; border-bottom-color: ${value};`; break;
    default: declarations = side ? `border-${side}-color: ${value};` : `border-color: ${value};`;
  }
  const scoped = group ? `${SHELL} ${group} ${selector}` : `${SHELL} ${selector}`;
  const rule = `${scoped} { ${declarations} }`;
  return media ? `@media (min-width: ${media}px) { ${rule} }` : rule;
}

const PREAMBLE = `/*
 * GENERATED by scripts/colour-bridge.mjs from the colour classes in src. Do not edit by hand:
 * change a page, then run \`node scripts/colour-bridge.mjs\`. The generator stops when a colour
 * class has no rule. src/layouts/colourBridge.test.ts fails when this file is stale, when a
 * colour class has no rule, or when a colour class in src is one the generator does not read.
 *
 * Active only under data-rs-bridge (every layout in light; Ledger, Console and Guide in dark).
 * Classic, Individual and Team in dark do not set it and keep the page classes exactly.
 */

/* Role classes for colours a rule cannot decide. Each defaults to the white it replaced. */
.rs-text-on-accent, .rs-text-on-danger, .rs-text-on-state { color: #fff; }
.rs-hover-text-strong:hover { color: #fff; }
.rs-knob, .rs-surface-white { background-color: #fff; }
${SHELL} .rs-text-on-accent { color: var(--rs-accent-text); }
${SHELL} .rs-text-on-danger { color: var(--rs-danger-solid-text); }
${SHELL} .rs-text-on-state { color: var(--rs-surface); }
${SHELL} .rs-hover-text-strong:hover { color: var(--rs-text); }
${SHELL} .rs-knob { background-color: var(--rs-surface); box-shadow: 0 0 0 1px var(--rs-line-strong); }
${SHELL} .rs-surface-white { background-color: var(--rs-surface); }
`;

export function generate(counts = scanSource()) {
  const rules = [];
  const unmapped = [];
  for (const cls of [...counts.keys()].sort()) {
    const parsed = parseClass(cls);
    const token = parsed && tokenFor(parsed);
    const rule = token && ruleFor(parsed, token);
    if (!rule) unmapped.push(cls);
    else rules.push([parsed.variants.length, rule]);
  }
  // Base rules first, then variants, so a hover or focus colour wins over the resting one.
  const ordered = rules.sort((a, b) => a[0] - b[0]).map(([, rule]) => rule);
  return { css: `${PREAMBLE}\n${ordered.join('\n')}\n`, unmapped, classes: counts.size };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const { css, unmapped, classes } = generate();
  if (unmapped.length) {
    console.error(`No bridge rule for: ${unmapped.join(', ')}`);
    process.exit(1);
  }
  if (process.argv.includes('--check')) {
    const current = readFileSync(OUTPUT, 'utf8');
    if (current !== css) { console.error(`${relative(ROOT, OUTPUT)} is stale; run node scripts/colour-bridge.mjs`); process.exit(1); }
    console.log(`colour bridge up to date: ${classes} classes`);
  } else {
    writeFileSync(OUTPUT, css);
    console.log(`wrote ${relative(ROOT, OUTPUT)}: ${classes} classes`);
  }
}
