import type { Repository } from './api';

/**
 * #56 repository path policy: typed read, editor draft, PUT serialization,
 * inline validation and a Git→SVN preview.
 *
 * - Reads come only from `path_policy`. The raw `allowed_paths` /
 *   `blocked_patterns` fields are write-only; a missing or unrecognised read is
 *   "unavailable" and is never edited, cleared or submitted.
 * - A field is submitted only after an explicit edit or Clear. Untouched,
 *   malformed and unavailable fields are omitted, so the server keeps them.
 * - Validation mirrors crates/web/src/api/repo_path_policy_56.rs (same codes,
 *   same order). The preview ports reposync_core::path_projection
 *   (normalize_policy_path, path_matches_prefix, path_matches_blocked,
 *   path_is_projected and the exclusion_message order). Rewriting is not
 *   modelled because the engine does not rewrite paths.
 */

export type PolicyFieldKey = 'allowed_paths' | 'blocked_patterns';
export const POLICY_FIELDS: readonly PolicyFieldKey[] = ['allowed_paths', 'blocked_patterns'];
export const MAX_POLICY_ENTRIES = 256;
export const MAX_POLICY_ENTRY_BYTES = 1024;
export const MAX_PREVIEW_PATHS = 50;

export const PATH_POLICY_LIMITATIONS = [
  'These rules filter Git→SVN only. SVN→Git imports every path on the SVN branch in this build; SVN→Git path filtering (#53) is not included.',
  'Paths are not rewritten: an included Git path is written to the same branch-relative SVN path. Rewriting is read-only and cannot be configured here.',
] as const;

const UNAVAILABLE = 'This server did not report the path policy, so it cannot be shown or edited here. Saving other settings leaves it unchanged.';
const UNRECOGNISED = 'The reported path policy was not recognised, so it cannot be edited here. Saving other settings leaves it unchanged.';
const MALFORMED = 'The stored value is not a JSON array of strings. The scheduler refuses to sync this repository until it is replaced or explicitly cleared.';

/** What the editor knows about one stored field. */
export type PolicyOrigin =
  | { kind: 'known'; stored: 'unset' | 'list'; entries: string[] }
  | { kind: 'malformed'; storedBytes: number; reason: string }
  | { kind: 'unavailable'; reason: string };

/** keep: omitted from PUT. edit: entries replace the stored value. clear: explicit null. */
export type PolicyMode = 'keep' | 'edit' | 'clear';
export interface PolicyFieldDraft { origin: PolicyOrigin; mode: PolicyMode; entries: string[] }
export interface PathPolicyDraft {
  allowed_paths: PolicyFieldDraft;
  blocked_patterns: PolicyFieldDraft;
  /** Visible notes, e.g. a recovered draft clear that was deliberately not restored. */
  notices: string[];
}

export type PolicyIssueCode = 'too_many_entries' | 'empty_entry' | 'entry_too_long' | 'control_character'
  | 'surrounding_space' | 'names_no_path' | 'empty_component' | 'dot_component' | 'wildcard_in_allowed';
export interface PolicyIssue { field: PolicyFieldKey; index: number | null; code: PolicyIssueCode; message: string }

export function policyFieldLabel(field: PolicyFieldKey): string {
  return field === 'allowed_paths' ? 'Allowed path prefixes' : 'Blocked patterns';
}
export function policyEntryNoun(field: PolicyFieldKey, count = 1): string {
  const noun = field === 'allowed_paths' ? 'allowed path prefix' : 'blocked pattern';
  return count === 1 ? noun : field === 'allowed_paths' ? 'allowed path prefixes' : 'blocked patterns';
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every(entry => typeof entry === 'string');
}

/** One field of a `path_policy` read. Anything unrecognised is unavailable, never a clear. */
export function readPolicyField(read: unknown): PolicyOrigin {
  if (!read || typeof read !== 'object') return { kind: 'unavailable', reason: UNRECOGNISED };
  const value = read as Record<string, unknown>;
  const { state, entries, stored_bytes: bytes, reason } = value;
  if (state === 'unset' && Object.keys(value).length === 1) return { kind: 'known', stored: 'unset', entries: [] };
  if (state === 'list' && isStringArray(entries)) return { kind: 'known', stored: 'list', entries: [...entries] };
  if (state === 'malformed') {
    return { kind: 'malformed', storedBytes: typeof bytes === 'number' && Number.isSafeInteger(bytes) && bytes >= 0 ? bytes : 0,
      reason: typeof reason === 'string' && reason ? reason : MALFORMED };
  }
  return { kind: 'unavailable', reason: UNRECOGNISED };
}

export function readRepositoryPathPolicy(repo: Pick<Repository, 'path_policy'> | null | undefined): Record<PolicyFieldKey, PolicyOrigin> {
  const policy: unknown = repo?.path_policy;
  if (!policy || typeof policy !== 'object' || (policy as { version?: unknown }).version !== 1) {
    const origin: PolicyOrigin = { kind: 'unavailable', reason: UNAVAILABLE };
    return { allowed_paths: origin, blocked_patterns: origin };
  }
  const value = policy as Record<string, unknown>;
  return { allowed_paths: readPolicyField(value.allowed_paths), blocked_patterns: readPolicyField(value.blocked_patterns) };
}

/** True only for known, non-empty lists (the server hook treats malformed as no rules). */
export function pathPolicyHasRules(repo: Pick<Repository, 'path_policy'> | null | undefined): boolean {
  const read = readRepositoryPathPolicy(repo);
  return POLICY_FIELDS.some(field => { const origin = read[field]; return origin.kind === 'known' && origin.entries.length > 0; });
}

function keepDraft(origin: PolicyOrigin): PolicyFieldDraft {
  return { origin, mode: 'keep', entries: origin.kind === 'known' ? [...origin.entries] : [] };
}

/**
 * Editor state from a read plus an optional recovered draft configuration.
 * A recovered string restores an explicit replacement. A recovered `null` is
 * NOT restored as a clear, because drafts written before #56 recorded `null`
 * whenever the old editor could not read the policy.
 */
export function pathPolicyDraft(repo: Pick<Repository, 'path_policy'> | null | undefined,
  recovered?: Partial<Record<PolicyFieldKey, string | null | undefined>>): PathPolicyDraft {
  const read = readRepositoryPathPolicy(repo);
  const notices: string[] = [];
  const field = (key: PolicyFieldKey): PolicyFieldDraft => {
    const origin = read[key];
    const saved = recovered && Object.prototype.hasOwnProperty.call(recovered, key) ? recovered[key] : undefined;
    if (saved === undefined || origin.kind === 'unavailable') return keepDraft(origin);
    if (saved === null) {
      notices.push(`A recovered draft would have cleared the ${policyEntryNoun(key, 2)}. It was not restored; choose Clear again if that is intended.`);
      return keepDraft(origin);
    }
    let parsed: unknown;
    try { parsed = JSON.parse(saved); } catch { parsed = undefined; }
    if (!isStringArray(parsed)) throw new Error('The recovered path policy draft could not be read. Review the current settings before saving.');
    return { origin, mode: 'edit', entries: parsed };
  };
  return { allowed_paths: field('allowed_paths'), blocked_patterns: field('blocked_patterns'), notices };
}

function sameEntries(left: string[], right: string[]): boolean {
  return left.length === right.length && left.every((entry, index) => entry === right[index]);
}

/** Whether this field will be sent, and how. `undefined` = omitted (server keeps it). */
export function submittedPolicyValue(draft: PolicyFieldDraft): string | null | undefined {
  if (draft.origin.kind === 'unavailable' || draft.mode === 'keep') return undefined;
  if (draft.mode === 'clear') return null;
  if (draft.origin.kind === 'known' && sameEntries(draft.entries, draft.origin.entries)) return undefined;
  return draft.entries.length ? JSON.stringify(draft.entries) : null;
}

/** The policy part of a repository PUT body. Only explicitly changed fields appear. */
export function pathPolicyConfiguration(draft: PathPolicyDraft): Partial<Record<PolicyFieldKey, string | null>> {
  const output: Partial<Record<PolicyFieldKey, string | null>> = {};
  for (const field of POLICY_FIELDS) {
    const value = submittedPolicyValue(draft[field]);
    if (value !== undefined) output[field] = value;
  }
  return output;
}

export function normalizePolicyPath(path: string): string {
  return path.replace(/\\/g, '/').replace(/^\/+/, '');
}

function hasControlCharacter(value: string): boolean {
  for (let index = 0; index < value.length; index++) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || (code >= 0x7f && code <= 0x9f)) return true;
  }
  return false;
}

/** One entry, in the server's rule order. */
export function policyEntryIssue(field: PolicyFieldKey, entry: string): { code: PolicyIssueCode; message: string } | null {
  if (entry === '') return { code: 'empty_entry', message: 'Enter a path, or remove this entry.' };
  if (new TextEncoder().encode(entry).length > MAX_POLICY_ENTRY_BYTES) return { code: 'entry_too_long', message: 'Use at most 1024 bytes.' };
  if (hasControlCharacter(entry)) return { code: 'control_character', message: 'Remove tabs, line breaks or other control characters.' };
  if (entry.startsWith(' ') || entry.endsWith(' ')) return { code: 'surrounding_space', message: 'Remove the leading or trailing space.' };
  const body = normalizePolicyPath(entry).replace(/\/+$/, '');
  if (!body) return { code: 'names_no_path', message: "Name a path; '/' alone matches nothing." };
  for (const component of body.split('/')) {
    if (!component) return { code: 'empty_component', message: "Remove the empty path component ('//')." };
    if (component === '.' || component === '..') return { code: 'dot_component', message: "Remove '.' or '..' path components." };
  }
  if (field === 'allowed_paths' && body.includes('*')) {
    return { code: 'wildcard_in_allowed', message: "Allowed paths are literal prefixes; '*' is not a wildcard here. Use a blocked pattern instead." };
  }
  return null;
}

/** Inline issues for one field being edited (shown whether or not it changed). */
export function policyFieldIssues(field: PolicyFieldKey, draft: PolicyFieldDraft): PolicyIssue[] {
  if (draft.mode !== 'edit' || draft.origin.kind === 'unavailable') return [];
  const issues: PolicyIssue[] = [];
  if (draft.entries.length > MAX_POLICY_ENTRIES) {
    issues.push({ field, index: null, code: 'too_many_entries', message: `Use at most ${MAX_POLICY_ENTRIES} entries.` });
  }
  draft.entries.forEach((entry, index) => {
    const issue = policyEntryIssue(field, entry);
    if (issue) issues.push({ field, index, ...issue });
  });
  return issues;
}

/** Issues that block saving: only fields that would actually be submitted. */
export function pathPolicyIssues(draft: PathPolicyDraft): PolicyIssue[] {
  return POLICY_FIELDS.flatMap(field => typeof submittedPolicyValue(draft[field]) === 'string' ? policyFieldIssues(field, draft[field]) : []);
}

export function pathPolicyIssueSummary(issues: PolicyIssue[]): string {
  const count = issues.length;
  return `Nothing was saved. Fix ${count} path policy ${count === 1 ? 'entry' : 'entries'} first: `
    + issues.map(issue => `${policyFieldLabel(issue.field)}${issue.index === null ? '' : ` ${issue.index + 1}`}: ${issue.message}`).join(' ');
}

/** Non-blocking notes about entries the engine accepts but that rarely do what was meant. */
export function policyFieldWarnings(field: PolicyFieldKey, entries: string[]): string[] {
  const warnings: string[] = [];
  const seen = new Set<string>();
  entries.forEach((entry, index) => {
    const normalized = normalizePolicyPath(entry).replace(/\/+$/, '');
    if (normalized && seen.has(normalized)) warnings.push(`Entry ${index + 1} repeats an earlier entry.`);
    seen.add(normalized);
    if (field === 'blocked_patterns') {
      const pattern = normalizePolicyPath(entry);
      if (pattern === '*') warnings.push(`Entry ${index + 1} ('*') blocks every path.`);
      else if (pattern.slice(1, -1).includes('*')) warnings.push(`Entry ${index + 1}: '*' is a wildcard only at the start or end of a pattern.`);
    }
  });
  return warnings;
}

// ---------------------------------------------------------------- edit actions

function withField(draft: PathPolicyDraft, field: PolicyFieldKey, next: PolicyFieldDraft): PathPolicyDraft {
  return field === 'allowed_paths' ? { ...draft, allowed_paths: next } : { ...draft, blocked_patterns: next };
}
export function beginPolicyEdit(draft: PathPolicyDraft, field: PolicyFieldKey): PathPolicyDraft {
  const current = draft[field];
  if (current.origin.kind === 'unavailable' || current.mode === 'edit') return draft;
  return withField(draft, field, { ...current, mode: 'edit', entries: current.origin.kind === 'known' ? [...current.origin.entries] : [] });
}
export function clearPolicyField(draft: PathPolicyDraft, field: PolicyFieldKey): PathPolicyDraft {
  const current = draft[field];
  if (current.origin.kind === 'unavailable') return draft;
  return withField(draft, field, { ...current, mode: 'clear', entries: [] });
}
export function undoPolicyField(draft: PathPolicyDraft, field: PolicyFieldKey): PathPolicyDraft {
  return withField(draft, field, keepDraft(draft[field].origin));
}
export function addPolicyEntry(draft: PathPolicyDraft, field: PolicyFieldKey): PathPolicyDraft {
  const editing = draft[field].mode === 'edit' ? draft : beginPolicyEdit(draft, field);
  const current = editing[field];
  if (current.mode !== 'edit') return draft;
  return withField(editing, field, { ...current, entries: [...current.entries, ''] });
}
export function updatePolicyEntry(draft: PathPolicyDraft, field: PolicyFieldKey, index: number, value: string): PathPolicyDraft {
  const current = draft[field];
  if (current.mode !== 'edit' || index < 0 || index >= current.entries.length) return draft;
  return withField(draft, field, { ...current, entries: current.entries.map((entry, i) => i === index ? value : entry) });
}
export function removePolicyEntry(draft: PathPolicyDraft, field: PolicyFieldKey, index: number): PathPolicyDraft {
  const current = draft[field];
  if (current.mode !== 'edit' || index < 0 || index >= current.entries.length) return draft;
  return withField(draft, field, { ...current, entries: current.entries.filter((_, i) => i !== index) });
}

/** Short visible state of one field. */
export function policyFieldSummary(field: PolicyFieldKey, draft: PolicyFieldDraft): string {
  const { origin } = draft;
  const count = (n: number) => `${n} ${policyEntryNoun(field, n)}`;
  const none = field === 'allowed_paths' ? 'No allowed-path restriction' : 'No blocked patterns';
  if (origin.kind === 'unavailable') return 'Not available from this server — unchanged on save';
  if (draft.mode === 'clear') return 'Will be cleared on save';
  if (draft.mode === 'edit') {
    const value = submittedPolicyValue(draft);
    if (value === undefined) return `${draft.entries.length ? count(draft.entries.length) : none} — unchanged`;
    return value === null ? 'No entries — will be cleared on save' : `${count(draft.entries.length)} — will replace the stored value on save`;
  }
  if (origin.kind === 'malformed') return 'Stored value is malformed — unchanged on save';
  return origin.entries.length ? count(origin.entries.length) : `${none}${origin.stored === 'list' ? ' (stored as an empty list)' : ''}`;
}

// ---------------------------------------------------------------- preview

export function pathMatchesPrefix(path: string, prefix: string): boolean {
  const normalizedPath = normalizePolicyPath(path);
  const normalizedPrefix = normalizePolicyPath(prefix).replace(/\/+$/, '');
  if (!normalizedPrefix) return false;
  return normalizedPath === normalizedPrefix || normalizedPath.startsWith(`${normalizedPrefix}/`);
}

export function pathMatchesBlocked(path: string, pattern: string): boolean {
  const normalizedPath = normalizePolicyPath(path);
  const normalizedPattern = normalizePolicyPath(pattern);
  if (!normalizedPattern) return false;
  if (normalizedPattern.startsWith('*')) return normalizedPath.endsWith(normalizedPattern.slice(1));
  if (normalizedPattern.endsWith('*')) {
    const stem = normalizedPattern.slice(0, -1);
    if (stem) return normalizedPath.startsWith(stem);
  }
  return pathMatchesPrefix(normalizedPath, normalizedPattern);
}

export type PreviewReason = { kind: 'not_allowed' } | { kind: 'blocked'; pattern: string };
export interface PreviewOutcome { path: string; included: boolean; reason: PreviewReason | null }

/** Same decision and reason order as path_is_projected / exclusion_message. */
export function previewPath(path: string, allowed: string[], blocked: string[]): PreviewOutcome {
  if (!allowed.length && !blocked.length) return { path, included: true, reason: null };
  if (allowed.length && !allowed.some(prefix => pathMatchesPrefix(path, prefix))) return { path, included: false, reason: { kind: 'not_allowed' } };
  const pattern = blocked.find(candidate => pathMatchesBlocked(path, candidate));
  return pattern === undefined ? { path, included: true, reason: null } : { path, included: false, reason: { kind: 'blocked', pattern } };
}

export type EffectivePathPolicy = { ok: true; allowed: string[]; blocked: string[] } | { ok: false; reason: string };

/** The lists the engine would use after saving this draft. */
export function effectivePathPolicy(draft: PathPolicyDraft): EffectivePathPolicy {
  const lists: Record<PolicyFieldKey, string[]> = { allowed_paths: [], blocked_patterns: [] };
  for (const field of POLICY_FIELDS) {
    const current = draft[field];
    if (current.origin.kind === 'unavailable') return { ok: false, reason: 'The path policy was not reported by this server, so no preview is possible.' };
    if (current.mode === 'clear') lists[field] = [];
    else if (current.mode === 'edit') lists[field] = [...current.entries];
    else if (current.origin.kind === 'malformed') {
      return { ok: false, reason: `The stored ${policyEntryNoun(field, 2)} value is malformed. The scheduler refuses to sync this repository, so nothing is projected until it is replaced or cleared.` };
    } else lists[field] = [...current.origin.entries];
  }
  return { ok: true, allowed: lists.allowed_paths, blocked: lists.blocked_patterns };
}

/**
 * Sample paths exactly as typed, one per line. Leading/trailing whitespace is part
 * of a filename (the engine never trims), so only empty lines are dropped. A
 * textarea's value already normalizes line breaks to "\n".
 */
export function parsePreviewPaths(text: string): string[] {
  return text.split('\n').filter(line => line !== '').slice(0, MAX_PREVIEW_PATHS);
}

/** Whether a path starts or ends with whitespace, so the preview can say so visibly. */
export function hasSurroundingWhitespace(path: string): boolean {
  return /^\s|\s$/.test(path);
}

export function previewReasonText(reason: PreviewReason | null): string {
  if (!reason) return 'Included in Git→SVN';
  return reason.kind === 'not_allowed' ? 'Excluded: not under an allowed path prefix' : `Excluded: matches blocked pattern ${reason.pattern}`;
}
