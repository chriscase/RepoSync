import { describe, it, expect } from 'vitest';
import type { Repository, RepositoryPathPolicyRead } from './api';
import { publicConfiguration } from './repositorySave';
import {
  MAX_POLICY_ENTRIES, MAX_POLICY_ENTRY_BYTES, type PathPolicyDraft, type PolicyFieldKey,
  addPolicyEntry, beginPolicyEdit, clearPolicyField, effectivePathPolicy, hasSurroundingWhitespace, parsePreviewPaths, pathPolicyConfiguration,
  pathPolicyDraft, pathPolicyHasRules, pathPolicyIssues, pathPolicyIssueSummary, policyEntryIssue, policyFieldIssues,
  policyFieldSummary, policyFieldWarnings, previewPath, readRepositoryPathPolicy, removePolicyEntry, undoPolicyField,
  updatePolicyEntry,
} from './pathPolicy56';

const LIMITS = { enforced_direction: 'git_to_svn', svn_to_git_filtering: 'not_applied', path_rewriting: 'none_read_only' } as const;
function repo(path_policy?: unknown, extra: Partial<Repository> = {}): Pick<Repository, 'path_policy'> {
  return { ...extra, path_policy: path_policy as RepositoryPathPolicyRead | undefined };
}
const lists = (allowed: string[], blocked: string[]) => repo({ version: 1, ...LIMITS,
  allowed_paths: { state: 'list', entries: allowed }, blocked_patterns: { state: 'list', entries: blocked } });
const malformed = { state: 'malformed', stored_bytes: 4, reason: 'stored value is not a JSON array of strings' };

/** Kept identical to rs56_entry_rule_table_matches_the_ui_table in repo_path_policy_56.rs. */
const ENTRY_RULES: Array<[PolicyFieldKey, string, string | null]> = [
  ['allowed_paths', 'source/', null],
  ['allowed_paths', '/source', null],
  ['allowed_paths', 'team\\docs', null],
  ['allowed_paths', '', 'empty_entry'],
  ['allowed_paths', 'a'.repeat(MAX_POLICY_ENTRY_BYTES + 1), 'entry_too_long'],
  ['allowed_paths', 'a\tb', 'control_character'],
  ['allowed_paths', ' source', 'surrounding_space'],
  ['allowed_paths', 'source ', 'surrounding_space'],
  ['allowed_paths', '/', 'names_no_path'],
  ['allowed_paths', 'a//b', 'empty_component'],
  ['allowed_paths', 'a/../b', 'dot_component'],
  ['allowed_paths', './a', 'dot_component'],
  ['allowed_paths', 'src/*', 'wildcard_in_allowed'],
  ['blocked_patterns', '*.exe', null],
  ['blocked_patterns', 'secret-*', null],
  ['blocked_patterns', '*', null],
  ['blocked_patterns', 'temp/', null],
  ['blocked_patterns', '\\', 'names_no_path'],
  ['blocked_patterns', 'x/./y', 'dot_component'],
];

/**
 * Kept identical to PREVIEW_VECTORS in crates/web/tests/repo_path_policy_56.rs, which
 * evaluates them with reposync_core::path_projection. null = included;
 * '' = excluded (not under an allowed prefix); otherwise the first matching blocked pattern.
 */
const PREVIEW_VECTORS: Array<[string[], string[], string, string | null]> = [
  [[], [], 'anything/at/all.txt', null],
  [['team'], [], 'team', null],
  [['team'], [], 'team/foo.txt', null],
  [['team'], [], 'team-other/x.txt', ''],
  [['team'], [], 'Team/x.txt', ''],
  [['team/'], [], 'team/a.txt', null],
  [['/team'], [], 'team/a.txt', null],
  [['team\\docs'], [], 'team/docs/a.md', null],
  [['team'], [], '/team/x.txt', null],
  [['src'], [], 'src\\x.rs', null],
  [[], ['*.exe'], 'bin/tool.exe', '*.exe'],
  [[], ['*.exe'], 'bin/tool.exe.txt', null],
  [[], ['secret-*'], 'secret-note.txt', 'secret-*'],
  [[], ['secret-*'], 'secret-note/child.txt', 'secret-*'],
  [[], ['secret-*'], 'a/secret-x.txt', null],
  [[], ['secret'], 'secret/leak.txt', 'secret'],
  [[], ['secret'], 'secret', 'secret'],
  [[], ['secret'], 'secrets/x.txt', null],
  [[], ['temp/'], 'temp/x', 'temp/'],
  [[], ['temp/'], 'tempfile', null],
  [[], ['*'], 'a', '*'],
  [[], ['a*b'], 'a*b/c', 'a*b'],
  [[], ['a*b'], 'axb', null],
  [['src'], ['src/gen'], 'src/main.rs', null],
  [['src'], ['src/gen'], 'src/gen/x.rs', 'src/gen'],
  [['src'], ['src/gen'], 'docs/a.md', ''],
  [['src'], ['*.exe'], 'docs/x.exe', ''],
  [[], ['*.log', 'logs'], 'logs/a.log', '*.log'],
  // Whitespace is part of a filename; neither the engine nor the preview trims it.
  [['src'], [], ' src/main.rs', ''],
  [[], ['*.exe'], 'bin/tool.exe ', null],
];

describe('#56 path-policy read: missing or malformed data is never a clear', () => {
  it('reads unset, stored lists, stored [] and malformed fields distinctly', () => {
    const read = readRepositoryPathPolicy(repo({ version: 1, ...LIMITS, allowed_paths: { state: 'unset' }, blocked_patterns: malformed }));
    expect(read.allowed_paths).toEqual({ kind: 'known', stored: 'unset', entries: [] });
    expect(read.blocked_patterns).toMatchObject({ kind: 'malformed', storedBytes: 4 });
    const empty = readRepositoryPathPolicy(repo({ version: 1, ...LIMITS, allowed_paths: { state: 'list', entries: [] }, blocked_patterns: { state: 'list', entries: ['*.exe'] } }));
    expect(empty.allowed_paths).toEqual({ kind: 'known', stored: 'list', entries: [] });
    expect(empty.blocked_patterns).toEqual({ kind: 'known', stored: 'list', entries: ['*.exe'] });
  });
  it.each([
    ['absent path_policy (server without #56)', undefined],
    ['unsupported version', { version: 2, ...LIMITS, allowed_paths: { state: 'unset' }, blocked_patterns: { state: 'unset' } }],
    ['null', null],
  ])('treats %s as unavailable for both fields', (_name, policy) => {
    const read = readRepositoryPathPolicy(repo(policy));
    expect(read.allowed_paths.kind).toBe('unavailable');
    expect(read.blocked_patterns.kind).toBe('unavailable');
  });
  it('treats an unrecognised field shape as unavailable', () => {
    const read = readRepositoryPathPolicy(repo({ version: 1, ...LIMITS, allowed_paths: { state: 'list', entries: [1] }, blocked_patterns: { state: 'unset', entries: ['x'] } }));
    expect(read.allowed_paths.kind).toBe('unavailable');
    expect(read.blocked_patterns.kind).toBe('unavailable');
  });
  it('never reads the write-only legacy columns, even when a server sends null', () => {
    const legacy = { allowed_paths: null, blocked_patterns: null } as Partial<Repository>;
    const draft = pathPolicyDraft(repo(undefined, legacy));
    expect(draft.allowed_paths.origin.kind).toBe('unavailable');
    expect(pathPolicyConfiguration(draft)).toEqual({});
    expect(pathPolicyConfiguration(clearPolicyField(beginPolicyEdit(draft, 'allowed_paths'), 'allowed_paths'))).toEqual({});
  });
});

describe('#56 GET → edit → PUT body: only explicit changes are submitted', () => {
  const base = () => pathPolicyDraft(lists(['src/', 'config/'], ['*.exe']));
  it('omits both fields for an unrelated settings save', () => {
    expect(pathPolicyConfiguration(base())).toEqual({});
    expect(pathPolicyConfiguration(beginPolicyEdit(base(), 'allowed_paths'))).toEqual({});
  });
  it('replaces one field and keeps the other', () => {
    let draft = beginPolicyEdit(base(), 'allowed_paths');
    draft = updatePolicyEntry(draft, 'allowed_paths', 1, 'build');
    draft = addPolicyEntry(draft, 'allowed_paths');
    draft = updatePolicyEntry(draft, 'allowed_paths', 2, 'team/docs');
    expect(pathPolicyConfiguration(draft)).toEqual({ allowed_paths: '["src/","build","team/docs"]' });
  });
  it('replaces both fields', () => {
    let draft = addPolicyEntry(base(), 'blocked_patterns');
    draft = updatePolicyEntry(draft, 'blocked_patterns', 1, 'secret-*');
    draft = removePolicyEntry(beginPolicyEdit(draft, 'allowed_paths'), 'allowed_paths', 0);
    expect(pathPolicyConfiguration(draft)).toEqual({ allowed_paths: '["config/"]', blocked_patterns: '["*.exe","secret-*"]' });
  });
  it('clears both fields explicitly with null, and Undo restores keep', () => {
    const cleared = clearPolicyField(clearPolicyField(base(), 'allowed_paths'), 'blocked_patterns');
    expect(pathPolicyConfiguration(cleared)).toEqual({ allowed_paths: null, blocked_patterns: null });
    expect(policyFieldSummary('allowed_paths', cleared.allowed_paths)).toBe('Will be cleared on save');
    expect(pathPolicyConfiguration(undoPolicyField(cleared, 'allowed_paths'))).toEqual({ blocked_patterns: null });
  });
  it('removing every entry is a visible clear', () => {
    let draft = beginPolicyEdit(base(), 'blocked_patterns');
    draft = removePolicyEntry(draft, 'blocked_patterns', 0);
    expect(pathPolicyConfiguration(draft)).toEqual({ blocked_patterns: null });
    expect(policyFieldSummary('blocked_patterns', draft.blocked_patterns)).toBe('No entries — will be cleared on save');
  });
  it('keeps a malformed field until Replace or Clear', () => {
    const draft = pathPolicyDraft(repo({ version: 1, ...LIMITS, allowed_paths: malformed, blocked_patterns: { state: 'unset' } }));
    expect(pathPolicyConfiguration(draft)).toEqual({});
    expect(policyFieldSummary('allowed_paths', draft.allowed_paths)).toBe('Stored value is malformed — unchanged on save');
    const replaced = updatePolicyEntry(addPolicyEntry(draft, 'allowed_paths'), 'allowed_paths', 0, 'src');
    expect(pathPolicyConfiguration(replaced)).toEqual({ allowed_paths: '["src"]' });
    expect(pathPolicyConfiguration(clearPolicyField(draft, 'allowed_paths'))).toEqual({ allowed_paths: null });
  });
  it('survives the shared save allowlist unchanged', () => {
    const draft = clearPolicyField(updatePolicyEntry(beginPolicyEdit(base(), 'allowed_paths'), 'allowed_paths', 0, 'lib'), 'blocked_patterns');
    const body = { name: 'Synthetic', ...pathPolicyConfiguration(draft) };
    expect(publicConfiguration(body)).toEqual({ name: 'Synthetic', allowed_paths: '["lib","config/"]', blocked_patterns: null });
    expect(publicConfiguration({ name: 'Synthetic', ...pathPolicyConfiguration(base()) })).toEqual({ name: 'Synthetic' });
  });
});

describe('#56 recovered drafts', () => {
  it('restores an explicit replacement but never a recovered null as a clear', () => {
    const draft = pathPolicyDraft(lists(['src'], ['*.exe']), { allowed_paths: '["lib"]', blocked_patterns: null });
    expect(pathPolicyConfiguration(draft)).toEqual({ allowed_paths: '["lib"]' });
    expect(draft.blocked_patterns.mode).toBe('keep');
    expect(draft.notices).toHaveLength(1);
    expect(draft.notices[0]).toContain('not restored');
  });
  it('ignores recovered values when the current read is unavailable', () => {
    const draft = pathPolicyDraft(repo(undefined), { allowed_paths: '["lib"]', blocked_patterns: null });
    expect(pathPolicyConfiguration(draft)).toEqual({});
  });
  it('refuses an unreadable recovered value instead of guessing', () => {
    expect(() => pathPolicyDraft(lists([], []), { allowed_paths: 'src/' })).toThrow(/could not be read/);
    expect(() => pathPolicyDraft(lists([], []), { allowed_paths: '[1]' })).toThrow(/could not be read/);
  });
});

describe('#56 inline validation mirrors the server rules', () => {
  it.each(ENTRY_RULES)('%s %j → %s', (field, entry, code) => {
    expect(policyEntryIssue(field, entry)?.code ?? null).toBe(code);
  });
  it('counts UTF-8 bytes, not UTF-16 units, like the server', () => {
    expect(policyEntryIssue('allowed_paths', 'é'.repeat(512))).toBeNull();
    expect(policyEntryIssue('allowed_paths', 'é'.repeat(513))?.code).toBe('entry_too_long');
  });
  it('flags too many entries for the list', () => {
    const many = pathPolicyDraft(lists(Array.from({ length: MAX_POLICY_ENTRIES + 1 }, (_, i) => `p${i}`), []));
    const edited = beginPolicyEdit(many, 'allowed_paths');
    expect(policyFieldIssues('allowed_paths', edited.allowed_paths).map(issue => issue.code)).toEqual(['too_many_entries']);
  });
  it('blocks saving only fields that would be submitted', () => {
    const legacy = pathPolicyDraft(lists(['src/*'], []));
    const unchanged = beginPolicyEdit(legacy, 'allowed_paths');
    expect(policyFieldIssues('allowed_paths', unchanged.allowed_paths).map(issue => issue.code)).toEqual(['wildcard_in_allowed']);
    expect(pathPolicyIssues(unchanged)).toEqual([]);
    const changed: PathPolicyDraft = addPolicyEntry(unchanged, 'allowed_paths');
    const issues = pathPolicyIssues(changed);
    expect(issues.map(issue => [issue.index, issue.code])).toEqual([[0, 'wildcard_in_allowed'], [1, 'empty_entry']]);
    expect(pathPolicyIssueSummary(issues)).toMatch(/^Nothing was saved\. Fix 2 path policy entries first: Allowed path prefixes 1: /);
  });
  it('warns about duplicates and star placement without blocking', () => {
    expect(policyFieldWarnings('allowed_paths', ['src', '/src/'])).toEqual(['Entry 2 repeats an earlier entry.']);
    expect(policyFieldWarnings('blocked_patterns', ['*', 'a*b', '*.exe', 'tmp*'])).toEqual([
      "Entry 1 ('*') blocks every path.", "Entry 2: '*' is a wildcard only at the start or end of a pattern."]);
  });
});

describe('#56 preview matches the engine projection', () => {
  it.each(PREVIEW_VECTORS)('allowed %j blocked %j: %s → %j', (allowed, blocked, path, expected) => {
    const outcome = previewPath(path, allowed, blocked);
    expect(outcome.included).toBe(expected === null);
    if (expected === '') expect(outcome.reason).toEqual({ kind: 'not_allowed' });
    else if (expected !== null) expect(outcome.reason).toEqual({ kind: 'blocked', pattern: expected });
  });
  it('previews the lists as they will be after saving', () => {
    let draft = pathPolicyDraft(lists(['src'], []));
    draft = clearPolicyField(draft, 'allowed_paths');
    draft = updatePolicyEntry(addPolicyEntry(draft, 'blocked_patterns'), 'blocked_patterns', 0, '*.exe');
    expect(effectivePathPolicy(draft)).toEqual({ ok: true, allowed: [], blocked: ['*.exe'] });
  });
  it('refuses to preview a kept malformed or unavailable policy', () => {
    const bad = pathPolicyDraft(repo({ version: 1, ...LIMITS, allowed_paths: { state: 'unset' }, blocked_patterns: malformed }));
    expect(effectivePathPolicy(bad)).toMatchObject({ ok: false });
    expect(effectivePathPolicy(clearPolicyField(bad, 'blocked_patterns'))).toEqual({ ok: true, allowed: [], blocked: [] });
    expect(effectivePathPolicy(pathPolicyDraft(repo(undefined)))).toMatchObject({ ok: false });
  });
  it('keeps sample lines verbatim, drops only empty lines and bounds the list', () => {
    expect(parsePreviewPaths(' src/a \n\n docs/b\n')).toEqual([' src/a ', ' docs/b']);
    expect(parsePreviewPaths('a\n \n\nb')).toEqual(['a', ' ', 'b']);
    expect(parsePreviewPaths(Array.from({ length: 80 }, (_, i) => `p${i}`).join('\n'))).toHaveLength(50);
  });
  it('preserves leading and trailing whitespace through the textarea-to-preview pipeline', () => {
    // A trimming parser would turn both lines into different files and flip both results.
    const typed = ' src/main.rs\nbin/tool.exe \n';
    const policy = effectivePathPolicy(pathPolicyDraft(lists(['src'], ['*.exe'])));
    if (!policy.ok) throw new Error('fixture policy must be previewable');
    const outcomes = parsePreviewPaths(typed).map(path => previewPath(path, policy.allowed, policy.blocked));
    expect(outcomes).toEqual([
      { path: ' src/main.rs', included: false, reason: { kind: 'not_allowed' } },
      { path: 'bin/tool.exe ', included: false, reason: { kind: 'not_allowed' } },
    ]);
    const blockedOnly = parsePreviewPaths(typed).map(path => previewPath(path, [], ['*.exe']));
    expect(blockedOnly.map(outcome => outcome.included)).toEqual([true, true]);
    expect(previewPath('bin/tool.exe', [], ['*.exe']).included).toBe(false);
    expect(hasSurroundingWhitespace(' src/main.rs')).toBe(true);
    expect(hasSurroundingWhitespace('bin/tool.exe ')).toBe(true);
    expect(hasSurroundingWhitespace('src/main.rs')).toBe(false);
  });
  it('offers the hook link only for known non-empty rules', () => {
    expect(pathPolicyHasRules(lists(['src'], []))).toBe(true);
    expect(pathPolicyHasRules(lists([], []))).toBe(false);
    expect(pathPolicyHasRules(repo({ version: 1, ...LIMITS, allowed_paths: malformed, blocked_patterns: { state: 'unset' } }))).toBe(false);
    expect(pathPolicyHasRules(repo(undefined))).toBe(false);
  });
});
