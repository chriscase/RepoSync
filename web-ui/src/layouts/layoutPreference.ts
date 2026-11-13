import { useState } from 'react';
import { DEFAULT_LAYOUT, LAYOUT_CATALOG, isLayoutId, type LayoutId } from './layoutCatalog';

/**
 * #262 local presentation preference.
 *
 * Only a layout id is stored, in this browser origin's localStorage, under one
 * genuine stable account identity (the server user id). It holds no credential,
 * token, role or server setting, is never sent to the server and never decides
 * what an account may see or do. Anything unexpected falls back to Classic.
 */
export const LAYOUT_PREFERENCE_VERSION = 1;
export const LAYOUT_PREFERENCE_PREFIX = 'reposync.layout-preference.v1';
export const MAX_PREFERENCE_RECORD_LENGTH = 512;
export const MAX_ACCOUNT_IDENTITY_LENGTH = 256;
const MAX_ORIGIN_LENGTH = 512;

export type PreferenceStorage = Pick<Storage, 'getItem' | 'setItem'>;
export interface PreferenceEnvironment {
  readonly storage: PreferenceStorage | null;
  readonly origin: string | null;
}

export type UnscopedReason = 'no-account-identity' | 'no-origin' | 'no-storage';
export type PreferenceScope =
  | { readonly kind: 'account'; readonly key: string; readonly identity: string; readonly origin: string; readonly storage: PreferenceStorage }
  | { readonly kind: 'unscoped'; readonly reason: UnscopedReason };

export type InvalidReason = 'corrupt' | 'unsupported-version' | 'other-scope' | 'unknown-layout';
export type PreferenceRead =
  | { readonly status: 'stored'; readonly layout: LayoutId }
  | { readonly status: 'absent' | 'unscoped' | 'unavailable'; readonly layout: LayoutId }
  | { readonly status: 'invalid'; readonly layout: LayoutId; readonly reason: InvalidReason };

export type PreferenceWrite =
  | { readonly status: 'saved' }
  | { readonly status: 'not-saved'; readonly reason: UnscopedReason | 'storage-error' | 'invalid-layout' };

export interface LayoutPreferenceNotice {
  readonly short: string;
  readonly detail: string;
}

/** The server user id, only when it is a usable non-empty string. */
export function stableAccountIdentity(user: unknown): string | null {
  if (!user || typeof user !== 'object') return null;
  const id = (user as { id?: unknown }).id;
  if (typeof id !== 'string' || id.trim() === '' || id.length > MAX_ACCOUNT_IDENTITY_LENGTH) return null;
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(id)) return null;
  return id;
}

function usableOrigin(origin: string | null): string | null {
  if (typeof origin !== 'string' || origin === '' || origin === 'null' || origin.length > MAX_ORIGIN_LENGTH) return null;
  return origin;
}

/** Reading the browser can itself throw (blocked storage); that is "no storage", never a crash. */
export function browserPreferenceEnvironment(): PreferenceEnvironment {
  let storage: PreferenceStorage | null = null;
  let origin: string | null = null;
  try { storage = typeof localStorage === 'undefined' ? null : localStorage; } catch { storage = null; }
  try { origin = typeof location === 'undefined' ? null : location.origin; } catch { origin = null; }
  return { storage, origin };
}

export function preferenceKey(origin: string, identity: string): string {
  return `${LAYOUT_PREFERENCE_PREFIX}:${encodeURIComponent(origin)}:${encodeURIComponent(identity)}`;
}

export function preferenceScope(user: unknown, environment: PreferenceEnvironment): PreferenceScope {
  const identity = stableAccountIdentity(user);
  if (identity === null) return { kind: 'unscoped', reason: 'no-account-identity' };
  const origin = usableOrigin(environment.origin);
  if (origin === null) return { kind: 'unscoped', reason: 'no-origin' };
  if (!environment.storage) return { kind: 'unscoped', reason: 'no-storage' };
  return { kind: 'account', key: preferenceKey(origin, identity), identity, origin, storage: environment.storage };
}

/** Stable comparison key for a scope, so an account change re-reads its own preference. */
export function scopeIdentity(scope: PreferenceScope): string {
  return scope.kind === 'account' ? `account:${scope.key}` : `unscoped:${scope.reason}`;
}

const RECORD_KEYS = ['identity', 'layout', 'origin', 'version'].join(',');

/** Exactly {version, layout, identity, origin} for this scope; anything else is not used. */
export function parseLayoutPreference(raw: string, scope: Extract<PreferenceScope, { kind: 'account' }>): PreferenceRead {
  const invalid = (reason: InvalidReason): PreferenceRead => ({ status: 'invalid', layout: DEFAULT_LAYOUT, reason });
  if (raw.length > MAX_PREFERENCE_RECORD_LENGTH) return invalid('corrupt');
  let value: unknown;
  try { value = JSON.parse(raw); } catch { return invalid('corrupt'); }
  if (!value || typeof value !== 'object' || Array.isArray(value)) return invalid('corrupt');
  const record = value as Record<string, unknown>;
  if (Object.keys(record).sort().join(',') !== RECORD_KEYS) {
    return 'version' in record && record.version !== LAYOUT_PREFERENCE_VERSION ? invalid('unsupported-version') : invalid('corrupt');
  }
  if (record.version !== LAYOUT_PREFERENCE_VERSION) return invalid('unsupported-version');
  if (record.identity !== scope.identity || record.origin !== scope.origin) return invalid('other-scope');
  if (!isLayoutId(record.layout)) return invalid('unknown-layout');
  return { status: 'stored', layout: record.layout };
}

export function readLayoutPreference(scope: PreferenceScope): PreferenceRead {
  if (scope.kind !== 'account') return { status: 'unscoped', layout: DEFAULT_LAYOUT };
  let raw: string | null;
  try { raw = scope.storage.getItem(scope.key); } catch { return { status: 'unavailable', layout: DEFAULT_LAYOUT }; }
  if (raw === null) return { status: 'absent', layout: DEFAULT_LAYOUT };
  if (typeof raw !== 'string') return { status: 'invalid', layout: DEFAULT_LAYOUT, reason: 'corrupt' };
  return parseLayoutPreference(raw, scope);
}

export function serializeLayoutPreference(scope: Extract<PreferenceScope, { kind: 'account' }>, layout: LayoutId): string {
  return JSON.stringify({ version: LAYOUT_PREFERENCE_VERSION, layout, identity: scope.identity, origin: scope.origin });
}

/** Never throws. Only an enum id is written, and only for a genuine account scope. */
export function writeLayoutPreference(scope: PreferenceScope, layout: unknown): PreferenceWrite {
  if (!isLayoutId(layout)) return { status: 'not-saved', reason: 'invalid-layout' };
  if (scope.kind !== 'account') return { status: 'not-saved', reason: scope.reason };
  try {
    scope.storage.setItem(scope.key, serializeLayoutPreference(scope, layout));
    return { status: 'saved' };
  } catch {
    return { status: 'not-saved', reason: 'storage-error' };
  }
}

/** A preference that exists but cannot be used is reported, not silently replaced. */
export function readNotice(read: PreferenceRead): LayoutPreferenceNotice | null {
  if (read.status === 'invalid') {
    return { short: 'Using Classic', detail: 'The saved layout preference could not be used, so Classic is shown. Choosing a layout replaces it.' };
  }
  if (read.status === 'unavailable') {
    return { short: 'Using Classic', detail: 'This browser did not allow the saved layout preference to be read, so Classic is shown.' };
  }
  return null;
}

/** A chosen layout still applies when it cannot be saved; the person is told why. */
export function writeNotice(layout: LayoutId, write: PreferenceWrite): LayoutPreferenceNotice | null {
  if (write.status === 'saved') return null;
  const applies = `${LAYOUT_CATALOG[layout].label} applies in this tab until you reload`;
  switch (write.reason) {
    case 'no-account-identity':
      return { short: 'Not saved', detail: `${applies}; this sign-in has no stable account identity to save it under.` };
    case 'no-origin':
      return { short: 'Not saved', detail: `${applies}; this page has no stable origin to save it under.` };
    case 'invalid-layout':
      return { short: 'Not saved', detail: 'That layout is not available in this build.' };
    default:
      return { short: 'Not saved', detail: `${applies}; this browser did not allow the layout preference to be saved.` };
  }
}

interface PreferenceState {
  readonly scopeKey: string;
  readonly layout: LayoutId;
  readonly notice: LayoutPreferenceNotice | null;
}

function initialState(scope: PreferenceScope): PreferenceState {
  const read = readLayoutPreference(scope);
  return { scopeKey: scopeIdentity(scope), layout: read.layout, notice: readNotice(read) };
}

export interface LayoutPreferenceControl {
  readonly layout: LayoutId;
  readonly notice: LayoutPreferenceNotice | null;
  readonly choose: (layout: LayoutId) => void;
}

/**
 * The shell's presentation state. An account change normally remounts the shell
 * (SessionBoundary keys on the session epoch); a scope change without a remount
 * still re-reads that account's own preference rather than carrying one over.
 */
export function useLayoutPreference(user: unknown, environment: () => PreferenceEnvironment = browserPreferenceEnvironment): LayoutPreferenceControl {
  const scope = preferenceScope(user, environment());
  const scopeKey = scopeIdentity(scope);
  const [state, setState] = useState<PreferenceState>(() => initialState(scope));
  let current = state;
  if (state.scopeKey !== scopeKey) {
    current = initialState(scope);
    setState(current);
  }
  const choose = (layout: LayoutId) => {
    if (!isLayoutId(layout)) return;
    const write = writeLayoutPreference(scope, layout);
    setState({ scopeKey, layout, notice: writeNotice(layout, write) });
  };
  return { layout: current.layout, notice: current.notice, choose };
}
