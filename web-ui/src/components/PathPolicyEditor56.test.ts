import { describe, it, expect } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { RepositoryPathPolicyRead } from '../api';
import PathPolicyEditor56 from './PathPolicyEditor56';
import { addPolicyEntry, beginPolicyEdit, pathPolicyDraft, type PathPolicyDraft } from '../pathPolicy56';

// Static-render component regressions (no DOM, no browser). Keyboard focus and axe
// checks live in web-ui/tests/path-policy-56.e2e.mjs.
const LIMITS = { enforced_direction: 'git_to_svn', svn_to_git_filtering: 'not_applied', path_rewriting: 'none_read_only' } as const;
function draftFor(policy: unknown): PathPolicyDraft {
  return pathPolicyDraft({ path_policy: policy as RepositoryPathPolicyRead | undefined });
}
const stored = draftFor({ version: 1, ...LIMITS, allowed_paths: { state: 'list', entries: ['src/', 'config/'] }, blocked_patterns: { state: 'list', entries: ['*.exe'] } });
const noop = () => {};

function attribute(tag: string, name: string): string | undefined {
  return tag.match(new RegExp(` ${name}="([^"]*)"`))?.[1];
}

describe('#56 PathPolicyEditor56 static rendering', () => {
  it('starts collapsed with controlled disclosure buttons and a visible limitation note', () => {
    const html = renderToStaticMarkup(createElement(PathPolicyEditor56, { draft: stored, onChange: noop }));
    const toggles = [...html.matchAll(/<button[^>]*aria-expanded="(true|false)"[^>]*>/g)];
    expect(toggles.map(match => match[1])).toEqual(['false', 'false']);
    for (const [tag] of toggles) {
      const panel = attribute(tag, 'aria-controls');
      expect(panel).toBeTruthy();
      expect(html).toContain(`id="${panel}" hidden=""`);
    }
    expect(html).toContain('role="note"');
    expect(html).toContain('SVN→Git imports every path on the SVN branch in this build; SVN→Git path filtering (#53) is not included.');
    expect(html).toContain('Rewriting is read-only and cannot be configured here.');
    expect(html).toContain('2 allowed path prefixes');
    expect(html).toContain('1 blocked pattern');
  });

  it('is read-only without onChange: stored entries, no inputs and no edit actions', () => {
    const html = renderToStaticMarkup(createElement(PathPolicyEditor56, { draft: stored, initiallyExpanded: true }));
    expect(html).toContain('<code class="rounded bg-gray-800 px-1.5 py-0.5 text-xs text-gray-100">src/</code>');
    expect(html).toContain('<code class="rounded bg-gray-800 px-1.5 py-0.5 text-xs text-gray-100">*.exe</code>');
    expect(html).not.toMatch(/<input/);
    expect(html).not.toContain('>Edit<');
    expect(html).not.toContain('>Clear<');
    expect((html.match(/aria-expanded="true"/g) || [])).toHaveLength(2);
  });

  it('binds an inline error to its labelled input and keeps the invalid list open', () => {
    const editing = addPolicyEntry(beginPolicyEdit(stored, 'allowed_paths'), 'allowed_paths');
    const html = renderToStaticMarkup(createElement(PathPolicyEditor56, { draft: editing, onChange: noop }));
    const invalid = html.match(/<input[^>]*aria-invalid="true"[^>]*>/g) || [];
    expect(invalid).toHaveLength(1);
    const invalidInput = invalid[0];
    if (invalidInput === undefined) throw new Error('expected one rendered input with aria-invalid="true"');
    const inputId = attribute(invalidInput, 'id');
    const errorId = attribute(invalidInput, 'aria-describedby');
    expect(errorId).toBe(`${inputId}-error`);
    expect(html).toContain(`id="${errorId}" role="alert"`);
    expect(html).toContain('Enter a path, or remove this entry.');
    expect(html).toContain(`for="${inputId}"`);
    expect(html).toContain('>Allowed path prefix 3</label>');
    expect(html).toMatch(/Remove<span class="sr-only"> allowed path prefix 3<\/span>/);
    const allowedToggle = html.match(/<button[^>]*aria-expanded="true"[^>]*>/)?.[0] ?? '';
    expect(allowedToggle).toContain('disabled=""');
    expect(html).toContain('3 allowed path prefixes — will replace the stored value on save — 1 to fix before saving');
    expect((html.match(/<input[^>]*type="text"/g) || [])).toHaveLength(3);
  });

  it('offers only explicit Replace or Clear for a malformed stored value', () => {
    const malformed = draftFor({ version: 1, ...LIMITS, allowed_paths: { state: 'malformed', stored_bytes: 4, reason: 'Synthetic malformed reason' }, blocked_patterns: { state: 'unset' } });
    const html = renderToStaticMarkup(createElement(PathPolicyEditor56, { draft: malformed, onChange: noop }));
    expect(html).toContain('Stored value is malformed — unchanged on save');
    expect(html).toContain('Synthetic malformed reason');
    expect(html).toMatch(/Replace with a new list<span class="sr-only"> allowed path prefixes<\/span>/);
    expect(html).toMatch(/Clear<span class="sr-only"> stored allowed path prefixes<\/span>/);
    expect(html).not.toMatch(/<input/);
    expect(html).toContain('The stored allowed path prefixes value is malformed.');
  });

  it('disables every policy control when the server did not report the policy', () => {
    const html = renderToStaticMarkup(createElement(PathPolicyEditor56, { draft: draftFor(undefined), onChange: noop }));
    expect(html).not.toMatch(/aria-expanded=/);
    expect(html).not.toMatch(/>(Edit|Clear|Add|Undo)</);
    expect((html.match(/Not available from this server — unchanged on save/g) || [])).toHaveLength(2);
    expect(html).toContain('The path policy was not reported by this server, so no preview is possible.');
  });

  it('renders preview outcomes with the engine reasons and a status summary', () => {
    const preview = 'src/main.rs\nsrc/tool.exe\ndocs/readme.md';
    const html = renderToStaticMarkup(createElement(PathPolicyEditor56, { draft: stored, initialPreview: preview }));
    expect(html).toContain('<caption class="sr-only">Git→SVN preview results</caption>');
    expect(html).toContain('<th scope="col"');
    expect(html).toContain('Included in Git→SVN');
    expect(html).toContain('Excluded: matches blocked pattern *.exe');
    expect(html).toContain('Excluded: not under an allowed path prefix');
    expect(html).toContain('1 of 3 sample paths included in Git→SVN. SVN→Git is not filtered in this build.');
    expect(html).toMatch(/<label for="([^"]+)"[^>]*>Sample Git paths \(one per line\)<\/label><textarea id="\1"/);
  });

  it('shows recovered-draft notices and only those', () => {
    expect(stored.notices).toEqual([]);
    const withNotice: PathPolicyDraft = { ...stored, notices: ['Synthetic recovered notice'] };
    expect(renderToStaticMarkup(createElement(PathPolicyEditor56, { draft: withNotice }))).toContain('<p class="text-xs text-amber-200">Synthetic recovered notice</p>');
  });

  it('previews typed paths verbatim, keeping and visibly marking surrounding whitespace', () => {
    const blockedOnly = draftFor({ version: 1, ...LIMITS, allowed_paths: { state: 'unset' }, blocked_patterns: { state: 'list', entries: ['*.exe'] } });
    const html = renderToStaticMarkup(createElement(PathPolicyEditor56, { draft: blockedOnly, initialPreview: ' src/main.rs\nbin/tool.exe \nbin/tool.exe' }));
    expect(html).toContain('<code class="text-gray-100 whitespace-pre"> src/main.rs</code>');
    expect(html).toContain('<code class="text-gray-100 whitespace-pre">bin/tool.exe </code>');
    expect(html).toContain('<code class="text-gray-100 whitespace-pre">bin/tool.exe</code>');
    expect((html.match(/Includes leading or trailing whitespace/g) || [])).toHaveLength(2);
    // A trimming pipeline would block the second line too and report 1 of 3.
    expect(html).toContain('2 of 3 sample paths included in Git→SVN.');
    expect((html.match(/Excluded: matches blocked pattern \*\.exe/g) || [])).toHaveLength(1);
  });
});
