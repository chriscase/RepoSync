import { afterEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import {
  LAYOUT_PREFERENCE_PREFIX, MAX_ACCOUNT_IDENTITY_LENGTH, MAX_PREFERENCE_RECORD_LENGTH,
  browserPreferenceEnvironment, parseLayoutPreference, preferenceKey, preferenceScope, readLayoutPreference,
  readNotice, scopeIdentity, serializeLayoutPreference, stableAccountIdentity, useLayoutPreference,
  writeLayoutPreference, writeNotice, type PreferenceEnvironment, type PreferenceScope,
} from './layoutPreference';

const ORIGIN = 'https://ui.example';
const OTHER_ORIGIN = 'http://127.0.0.1:5173';

class MemoryStorage {
  values = new Map<string, string>();
  reads: string[] = [];
  writes: string[] = [];
  failReads = false;
  failWrites = false;
  getItem(key: string) { this.reads.push(key); if (this.failReads) throw new Error('Synthetic storage read refused'); return this.values.get(key) ?? null; }
  setItem(key: string, value: string) { this.writes.push(key); if (this.failWrites) throw new Error('Synthetic quota exceeded'); this.values.set(key, value); }
}

function environment(storage: MemoryStorage | null = new MemoryStorage(), origin: string | null = ORIGIN): PreferenceEnvironment {
  return { storage, origin };
}

function accountScope(identity: string, storage = new MemoryStorage(), origin = ORIGIN) {
  const scope = preferenceScope({ id: identity, role: 'viewer' }, environment(storage, origin));
  if (scope.kind !== 'account') throw new Error(`expected an account scope for ${identity}`);
  return scope;
}

function record(fields: Record<string, unknown>) { return JSON.stringify(fields); }

afterEach(() => { vi.unstubAllGlobals(); });

describe('#262 preference scope', () => {
  it('uses only a genuine stable server user id as the account identity', () => {
    expect(stableAccountIdentity({ id: 'fixture-admin', role: 'admin' })).toBe('fixture-admin');
    expect(stableAccountIdentity({ id: 'x'.repeat(MAX_ACCOUNT_IDENTITY_LENGTH) })).toHaveLength(MAX_ACCOUNT_IDENTITY_LENGTH);
    for (const user of [
      null, undefined, 'fixture-admin', 7, [], {}, { username: 'fixture-admin' }, { id: '' }, { id: '   ' }, { id: 7 }, { id: null },
      { id: ['A'] }, { id: { value: 'A' } }, { id: 'a\u0000b' }, { id: 'line\nbreak' }, { id: 'x'.repeat(MAX_ACCOUNT_IDENTITY_LENGTH + 1) },
    ]) expect(stableAccountIdentity(user), JSON.stringify(user) ?? String(user)).toBeNull();
  });

  it('scopes the key by origin and account and never by role, display name or token', () => {
    const scope = accountScope('user 1');
    expect(scope.key).toBe(`${LAYOUT_PREFERENCE_PREFIX}:${encodeURIComponent(ORIGIN)}:user%201`);
    expect(preferenceKey(ORIGIN, 'user 1')).toBe(scope.key);
    const admin = preferenceScope({ id: 'user 1', role: 'admin', display_name: 'Changed', token: 'synthetic-secret' }, environment());
    expect(admin.kind === 'account' && admin.key).toBe(scope.key);
    expect(accountScope('user 2').key).not.toBe(scope.key);
    expect(accountScope('user 1', new MemoryStorage(), OTHER_ORIGIN).key).not.toBe(scope.key);
    expect(scopeIdentity(scope)).toBe(`account:${scope.key}`);
  });

  it('is unscoped, with a reason, when identity, origin or storage is missing', () => {
    const cases: Array<[unknown, PreferenceEnvironment, string]> = [
      [null, environment(), 'no-account-identity'],
      [{ username: 'legacy' }, environment(), 'no-account-identity'],
      [{ id: 'A' }, environment(new MemoryStorage(), null), 'no-origin'],
      [{ id: 'A' }, environment(new MemoryStorage(), 'null'), 'no-origin'],
      [{ id: 'A' }, environment(new MemoryStorage(), ''), 'no-origin'],
      [{ id: 'A' }, environment(null), 'no-storage'],
    ];
    for (const [user, env, reason] of cases) {
      const scope = preferenceScope(user, env);
      expect(scope).toEqual({ kind: 'unscoped', reason });
      expect(scopeIdentity(scope)).toBe(`unscoped:${reason}`);
    }
  });

  it('treats a browser that throws on storage or location access as having neither', () => {
    vi.stubGlobal('location', { origin: ORIGIN });
    const storage = new MemoryStorage();
    vi.stubGlobal('localStorage', storage);
    expect(browserPreferenceEnvironment()).toEqual({ storage, origin: ORIGIN });
    const saved = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    Object.defineProperty(globalThis, 'localStorage', { configurable: true, get() { throw new Error('Synthetic SecurityError'); } });
    try {
      expect(() => browserPreferenceEnvironment()).not.toThrow();
      expect(browserPreferenceEnvironment()).toEqual({ storage: null, origin: ORIGIN });
    } finally {
      if (saved) Object.defineProperty(globalThis, 'localStorage', saved); else Reflect.deleteProperty(globalThis, 'localStorage');
    }
  });
});

describe('#262 preference read', () => {
  it('reads a stored layout for the same account and origin', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    storage.values.set(scope.key, record({ version: 1, layout: 'team', identity: 'A', origin: ORIGIN }));
    expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout: 'team' });
    storage.values.set(scope.key, serializeLayoutPreference(scope, 'individual'));
    expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout: 'individual' });
  });

  it('falls back to Classic when nothing is stored', () => {
    expect(readLayoutPreference(accountScope('A'))).toEqual({ status: 'absent', layout: 'classic' });
  });

  it('rejects corrupt, unknown, other-version and other-scope records as Classic, without deleting them', () => {
    const scope = accountScope('A');
    const valid = { version: 1, layout: 'team', identity: 'A', origin: ORIGIN };
    const cases: Array<[string, string]> = [
      ['not json', 'corrupt'],
      ['', 'corrupt'],
      ['"team"', 'corrupt'],
      ['team', 'corrupt'],
      ['null', 'corrupt'],
      ['[]', 'corrupt'],
      ['["team"]', 'corrupt'],
      ['{}', 'corrupt'],
      [record({ ...valid, extra: true }), 'corrupt'],
      [record({ layout: 'team', identity: 'A', origin: ORIGIN }), 'corrupt'],
      [record({ ...valid, layout: 'team', origin: ORIGIN + '/'.repeat(MAX_PREFERENCE_RECORD_LENGTH) }), 'corrupt'],
      [record({ ...valid, version: 2 }), 'unsupported-version'],
      [record({ ...valid, version: '1' }), 'unsupported-version'],
      [record({ ...valid, version: 2, theme: 'light' }), 'unsupported-version'],
      [record({ ...valid, identity: 'B' }), 'other-scope'],
      [record({ ...valid, origin: OTHER_ORIGIN }), 'other-scope'],
      [record({ ...valid, layout: 'compact' }), 'unknown-layout'],
      [record({ ...valid, layout: 'Team' }), 'unknown-layout'],
      [record({ ...valid, layout: '__proto__' }), 'unknown-layout'],
      [record({ ...valid, layout: null }), 'unknown-layout'],
      [record({ ...valid, layout: ['team'] }), 'unknown-layout'],
    ];
    for (const [raw, reason] of cases) {
      expect(parseLayoutPreference(raw, scope), raw.slice(0, 80)).toEqual({ status: 'invalid', layout: 'classic', reason });
      scope.storage.setItem(scope.key, raw);
      expect(readLayoutPreference(scope)).toEqual({ status: 'invalid', layout: 'classic', reason });
      expect(scope.storage.getItem(scope.key)).toBe(raw);
    }
  });

  it('returns Classic, not an exception, when the browser refuses to read storage', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    storage.values.set(scope.key, serializeLayoutPreference(scope, 'team'));
    storage.failReads = true;
    expect(() => readLayoutPreference(scope)).not.toThrow();
    expect(readLayoutPreference(scope)).toEqual({ status: 'unavailable', layout: 'classic' });
  });

  it('does not read storage at all without a genuine account scope', () => {
    const storage = new MemoryStorage();
    const scope = preferenceScope({ username: 'legacy-login' }, environment(storage));
    expect(readLayoutPreference(scope)).toEqual({ status: 'unscoped', layout: 'classic' });
    expect(storage.reads).toEqual([]);
  });
});

describe('#262 preference write and isolation', () => {
  it('stores only the version, layout id, identity and origin', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    expect(writeLayoutPreference(scope, 'team')).toEqual({ status: 'saved' });
    expect([...storage.values.keys()]).toEqual([scope.key]);
    expect(storage.values.get(scope.key)).toBe(`{"version":1,"layout":"team","identity":"A","origin":"${ORIGIN}"}`);
    const admin = preferenceScope({ id: 'A', role: 'admin', token: 'synthetic-secret', password: 'synthetic-password' }, environment(storage));
    expect(writeLayoutPreference(admin, 'individual')).toEqual({ status: 'saved' });
    const stored = storage.values.get(scope.key) ?? '';
    for (const secret of ['admin', 'synthetic-secret', 'synthetic-password', 'role', 'token']) expect(stored).not.toContain(secret);
    expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout: 'individual' });
  });

  it('keeps one account and one origin from inheriting another preference', () => {
    const storage = new MemoryStorage();
    const alpha = accountScope('alpha', storage);
    const beta = accountScope('beta', storage);
    const elsewhere = accountScope('alpha', storage, OTHER_ORIGIN);
    writeLayoutPreference(alpha, 'team');
    writeLayoutPreference(beta, 'individual');
    expect(readLayoutPreference(alpha)).toEqual({ status: 'stored', layout: 'team' });
    expect(readLayoutPreference(beta)).toEqual({ status: 'stored', layout: 'individual' });
    expect(readLayoutPreference(elsewhere)).toEqual({ status: 'absent', layout: 'classic' });
    // A record copied under another account's key is still not trusted.
    storage.values.set(elsewhere.key, storage.values.get(alpha.key) ?? '');
    expect(readLayoutPreference(elsewhere)).toEqual({ status: 'invalid', layout: 'classic', reason: 'other-scope' });
  });

  it('never throws and never writes when it cannot or must not save', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    storage.failWrites = true;
    expect(() => writeLayoutPreference(scope, 'team')).not.toThrow();
    expect(writeLayoutPreference(scope, 'team')).toEqual({ status: 'not-saved', reason: 'storage-error' });
    storage.failWrites = false;
    storage.writes.length = 0;
    for (const value of ['compact', 'Team', '', null, '__proto__']) {
      expect(writeLayoutPreference(scope, value)).toEqual({ status: 'not-saved', reason: 'invalid-layout' });
    }
    const unscoped = preferenceScope({ username: 'legacy-login' }, environment(storage));
    expect(writeLayoutPreference(unscoped, 'team')).toEqual({ status: 'not-saved', reason: 'no-account-identity' });
    expect(storage.writes).toEqual([]);
    expect(storage.values.size).toBe(0);
  });
});

describe('#262 preference notices', () => {
  it('reports a stored value that could not be used, and stays quiet when nothing was stored', () => {
    expect(readNotice({ status: 'invalid', layout: 'classic', reason: 'corrupt' })?.short).toBe('Using Classic');
    expect(readNotice({ status: 'unavailable', layout: 'classic' })?.detail).toContain('did not allow the saved layout preference to be read');
    for (const status of ['absent', 'unscoped'] as const) expect(readNotice({ status, layout: 'classic' })).toBeNull();
    expect(readNotice({ status: 'stored', layout: 'team' })).toBeNull();
  });

  it('tells the person a chosen layout applies but was not saved, and why', () => {
    expect(writeNotice('team', { status: 'saved' })).toBeNull();
    const failed = writeNotice('team', { status: 'not-saved', reason: 'storage-error' });
    expect(failed?.short).toBe('Not saved');
    expect(failed?.detail).toBe('Team applies in this tab until you reload; this browser did not allow the layout preference to be saved.');
    expect(writeNotice('individual', { status: 'not-saved', reason: 'no-account-identity' })?.detail).toContain('no stable account identity');
    expect(writeNotice('team', { status: 'not-saved', reason: 'no-origin' })?.detail).toContain('no stable origin');
    expect(writeNotice('team', { status: 'not-saved', reason: 'no-storage' })?.detail).toContain('did not allow the layout preference to be saved');
  });
});

describe('#262 useLayoutPreference initial state', () => {
  function Probe({ user, env }: { user: unknown; env: PreferenceEnvironment }) {
    const preference = useLayoutPreference(user, () => env);
    return createElement('output', null, `${preference.layout}|${preference.notice?.short ?? ''}`);
  }
  const render = (user: unknown, env: PreferenceEnvironment) => renderToStaticMarkup(createElement(Probe, { user, env }));

  it('starts from the account preference, or Classic with a notice when it cannot be used', () => {
    const storage = new MemoryStorage();
    const scope: PreferenceScope = accountScope('A', storage);
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|</output>');
    writeLayoutPreference(scope, 'team');
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>team|</output>');
    expect(render({ id: 'B' }, environment(storage))).toBe('<output>classic|</output>');
    storage.values.set(preferenceKey(ORIGIN, 'A'), '{broken');
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|Using Classic</output>');
    storage.failReads = true;
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|Using Classic</output>');
  });

  it('does not touch storage for a sign-in without a stable account identity', () => {
    const storage = new MemoryStorage();
    storage.values.set(preferenceKey(ORIGIN, 'A'), record({ version: 1, layout: 'team', identity: 'A', origin: ORIGIN }));
    expect(render(null, environment(storage))).toBe('<output>classic|</output>');
    expect(render({ username: 'legacy-login' }, environment(storage))).toBe('<output>classic|</output>');
    expect(storage.reads).toEqual([]);
  });
});
