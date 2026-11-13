import { afterEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import {
  LAYOUT_PREFERENCE_PREFIX, MAX_ACCOUNT_IDENTITY_LENGTH, MAX_PREFERENCE_RECORD_LENGTH,
  browserPreferenceEnvironment, isWellFormedText, parseLayoutPreference, preferenceKey, preferenceScope, readLayoutPreference,
  readNotice, scopeIdentity, serializeLayoutPreference, stableAccountIdentity, useLayoutPreference,
  writeLayoutPreference, writeNotice, type PreferenceEnvironment, type PreferenceScope,
} from './layoutPreference';
import { LAYOUT_IDS } from './layoutCatalog';
import LayoutChooser from './LayoutChooser';

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

function keyFor(identity: string, origin = ORIGIN): string {
  const key = preferenceKey(origin, identity);
  if (key === null) throw new Error(`expected an encodable preference key for ${JSON.stringify(identity)}`);
  return key;
}

function Probe({ user, env }: { user: unknown; env: PreferenceEnvironment }) {
  const preference = useLayoutPreference(user, () => env);
  return createElement('output', null, `${preference.layout}|${preference.notice?.short ?? ''}`);
}
const render = (user: unknown, env: PreferenceEnvironment) => renderToStaticMarkup(createElement(Probe, { user, env }));

/** The hook with its default browser environment, rendered through the real chooser. */
function DefaultChooserProbe({ user }: { user: unknown }) {
  const preference = useLayoutPreference(user);
  return createElement(LayoutChooser, { value: preference.layout, onChange: preference.choose, notice: preference.notice });
}

/** Ids whose `individual` record is exactly `target` code units (quotation marks escape to two). */
function identityForRecordLength(target: number, origin = ORIGIN): string {
  const added = target - record({ version: 1, layout: 'individual', identity: '', origin }).length;
  const quotes = Math.max(0, added - MAX_ACCOUNT_IDENTITY_LENGTH);
  return '"'.repeat(quotes) + 'a'.repeat(added - 2 * quotes);
}

/** Replaces the global localStorage getter with one that throws, as a browser blocking storage does. */
function withRefusedStorage(run: () => void) {
  const saved = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
  Object.defineProperty(globalThis, 'localStorage', { configurable: true, get() { throw new Error('Synthetic SecurityError'); } });
  try { run(); } finally {
    if (saved) Object.defineProperty(globalThis, 'localStorage', saved); else Reflect.deleteProperty(globalThis, 'localStorage');
  }
}

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

  it('records a browser that throws on storage access as refused storage, without throwing', () => {
    vi.stubGlobal('location', { origin: ORIGIN });
    const storage = new MemoryStorage();
    vi.stubGlobal('localStorage', storage);
    expect(browserPreferenceEnvironment()).toEqual({ storage, origin: ORIGIN, storageRefused: false });
    const saved = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    Object.defineProperty(globalThis, 'localStorage', { configurable: true, get() { throw new Error('Synthetic SecurityError'); } });
    try {
      expect(() => browserPreferenceEnvironment()).not.toThrow();
      expect(browserPreferenceEnvironment()).toEqual({ storage: null, origin: ORIGIN, storageRefused: true });
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
  it('starts from the account preference, or Classic with a notice when it cannot be used', () => {
    const storage = new MemoryStorage();
    const scope: PreferenceScope = accountScope('A', storage);
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|</output>');
    writeLayoutPreference(scope, 'team');
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>team|</output>');
    expect(render({ id: 'B' }, environment(storage))).toBe('<output>classic|</output>');
    storage.values.set(keyFor('A'), '{broken');
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|Using Classic</output>');
    storage.failReads = true;
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|Using Classic</output>');
  });

  it('does not touch storage for a sign-in without a stable account identity', () => {
    const storage = new MemoryStorage();
    storage.values.set(keyFor('A'), record({ version: 1, layout: 'team', identity: 'A', origin: ORIGIN }));
    expect(render(null, environment(storage))).toBe('<output>classic|</output>');
    expect(render({ username: 'legacy-login' }, environment(storage))).toBe('<output>classic|</output>');
    expect(storage.reads).toEqual([]);
  });
});

// ---- Retained-review corrections (RS262-1, RS262-2, RS262-3) ----

const LONE_SURROGATE_IDS: readonly string[] = ['\ud800', '\udbff', '\udc00', '\udfff', 'a\ud800b', 'account-\udc00', '\udc00\ud800', '\ud83d', 'end\ud83d'];
const UNICODE_IDS: readonly string[] = ['😀', 'ü-账户-😀', '𝒜𝒸𝒸𝑜𝓊𝓃𝓉'];

describe('#262 RS262-1 lone surrogates', () => {
  it('rejects lone high and low surrogates in an identity or origin without throwing or altering the id', () => {
    for (const id of LONE_SURROGATE_IDS) {
      const label = JSON.stringify(id);
      expect(isWellFormedText(id), label).toBe(false);
      expect(() => stableAccountIdentity({ id }), label).not.toThrow();
      expect(stableAccountIdentity({ id }), label).toBeNull();
      expect(() => preferenceKey(ORIGIN, id), label).not.toThrow();
      expect(preferenceKey(ORIGIN, id), label).toBeNull();
      expect(preferenceKey(`${ORIGIN}${id}`, 'A'), label).toBeNull();
      expect(preferenceScope({ id }, environment()), label).toEqual({ kind: 'unscoped', reason: 'no-account-identity' });
      expect(preferenceScope({ id: 'A' }, environment(new MemoryStorage(), `${ORIGIN}${id}`)), label).toEqual({ kind: 'unscoped', reason: 'no-origin' });
    }
  });

  it('renders Classic through the hook for lone high and low surrogate ids with no preference read or write', () => {
    for (const id of ['\ud800', '\udc00']) {
      const storage = new MemoryStorage();
      storage.values.set(keyFor('A'), serializeLayoutPreference(accountScope('A', storage), 'team'));
      let html = '';
      expect(() => { html = render({ id, role: 'admin' }, environment(storage)); }).not.toThrow();
      expect(html).toBe('<output>classic|</output>');
      expect(writeLayoutPreference(preferenceScope({ id }, environment(storage)), 'team')).toEqual({ status: 'not-saved', reason: 'no-account-identity' });
      expect(storage.reads).toEqual([]);
      expect(storage.writes).toEqual([]);
    }
  });

  it('keeps valid surrogate pairs and other Unicode ids, which save and re-read exactly', () => {
    for (const id of UNICODE_IDS) {
      expect(isWellFormedText(id)).toBe(true);
      expect(stableAccountIdentity({ id })).toBe(id);
      expect(keyFor(id)).toBe(`${LAYOUT_PREFERENCE_PREFIX}:${encodeURIComponent(ORIGIN)}:${encodeURIComponent(id)}`);
      for (const layout of LAYOUT_IDS) {
        const storage = new MemoryStorage();
        const scope = accountScope(id, storage);
        expect(writeLayoutPreference(scope, layout)).toEqual({ status: 'saved' });
        expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout });
        expect(render({ id }, environment(storage))).toBe(`<output>${layout}|</output>`);
      }
    }
  });
});

describe('#262 RS262-2 record size', () => {
  it('does not persist an accepted id of 256 quotation marks whose record the reader would reject', () => {
    const quotes = '"'.repeat(MAX_ACCOUNT_IDENTITY_LENGTH);
    expect(stableAccountIdentity({ id: quotes })).toBe(quotes);
    expect(record({ version: 1, layout: 'team', identity: quotes, origin: ORIGIN }).length).toBeGreaterThan(MAX_PREFERENCE_RECORD_LENGTH);
    const storage = new MemoryStorage();
    const scope = preferenceScope({ id: quotes }, environment(storage));
    expect(scope).toEqual({ kind: 'unscoped', reason: 'record-too-large' });
    for (const layout of LAYOUT_IDS) expect(writeLayoutPreference(scope, layout)).toEqual({ status: 'not-saved', reason: 'record-too-large' });
    expect(readLayoutPreference(scope)).toEqual({ status: 'unscoped', layout: 'classic' });
    expect(render({ id: quotes }, environment(storage))).toBe('<output>classic|</output>');
    expect(storage.reads).toEqual([]);
    expect(storage.writes).toEqual([]);
    const notice = writeNotice('team', { status: 'not-saved', reason: 'record-too-large' });
    expect(notice?.detail).toBe('Team applies in this tab until you reload; this account identity and site address are too long to store a layout preference for.');
    expect(notice?.detail).not.toContain('browser');
  });

  it('saves every layout at exactly the record limit and refuses the whole scope one code unit beyond it', () => {
    const exact = identityForRecordLength(MAX_PREFERENCE_RECORD_LENGTH);
    const beyond = identityForRecordLength(MAX_PREFERENCE_RECORD_LENGTH + 1);
    for (const id of [exact, beyond]) {
      expect(id.length).toBeLessThanOrEqual(MAX_ACCOUNT_IDENTITY_LENGTH);
      expect(stableAccountIdentity({ id })).toBe(id);
    }
    const storage = new MemoryStorage();
    const scope = accountScope(exact, storage);
    const lengths: Record<string, number> = {};
    for (const layout of LAYOUT_IDS) {
      expect(writeLayoutPreference(scope, layout)).toEqual({ status: 'saved' });
      lengths[layout] = (storage.values.get(scope.key) ?? '').length;
      expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout });
    }
    expect(lengths).toEqual({ classic: MAX_PREFERENCE_RECORD_LENGTH - 3, individual: MAX_PREFERENCE_RECORD_LENGTH, team: MAX_PREFERENCE_RECORD_LENGTH - 6 });
    // Team alone would still fit one unit beyond, but whether a scope persists does not depend on the layout chosen.
    expect(record({ version: 1, layout: 'team', identity: beyond, origin: ORIGIN }).length).toBeLessThan(MAX_PREFERENCE_RECORD_LENGTH);
    const refusedStorage = new MemoryStorage();
    const refused = preferenceScope({ id: beyond }, environment(refusedStorage));
    expect(refused).toEqual({ kind: 'unscoped', reason: 'record-too-large' });
    for (const layout of LAYOUT_IDS) expect(writeLayoutPreference(refused, layout)).toEqual({ status: 'not-saved', reason: 'record-too-large' });
    expect(refusedStorage.reads).toEqual([]);
    expect(refusedStorage.writes).toEqual([]);
  });

  it('refuses an oversized record in the writer itself before setItem, keeping the earlier preference', () => {
    const identity = identityForRecordLength(MAX_PREFERENCE_RECORD_LENGTH + 1);
    const storage = new MemoryStorage();
    const key = keyFor(identity);
    const earlier = 'earlier synthetic preference';
    storage.values.set(key, earlier);
    // A hand-built scope bypasses preferenceScope's bound, so only the writer's own guard applies here.
    const constructed: PreferenceScope = { kind: 'account', key, identity, origin: ORIGIN, storage };
    expect(writeLayoutPreference(constructed, 'individual')).toEqual({ status: 'not-saved', reason: 'record-too-large' });
    expect(storage.writes).toEqual([]);
    expect(storage.values.get(key)).toBe(earlier);
    // Whatever the writer does save re-reads as the same stored layout.
    expect(writeLayoutPreference(constructed, 'team')).toEqual({ status: 'saved' });
    expect(readLayoutPreference(constructed)).toEqual({ status: 'stored', layout: 'team' });
  });

  it('round-trips every layout for accepted ordinary, escaped and Unicode ids', () => {
    for (const id of ['fixture-admin', 'user 1', 'quote"and\\backslash', '"'.repeat(100), ...UNICODE_IDS]) {
      for (const layout of LAYOUT_IDS) {
        const storage = new MemoryStorage();
        const scope = accountScope(id, storage);
        expect(writeLayoutPreference(scope, layout), id).toEqual({ status: 'saved' });
        expect((storage.values.get(scope.key) ?? '').length).toBeLessThanOrEqual(MAX_PREFERENCE_RECORD_LENGTH);
        expect(readLayoutPreference(scope), id).toEqual({ status: 'stored', layout });
      }
    }
  });
});

describe('#262 RS262-3 refused storage access', () => {
  it('carries a refused storage getter through scope, read, write and notices for a genuine account', () => {
    vi.stubGlobal('location', { origin: ORIGIN });
    vi.stubGlobal('localStorage', new MemoryStorage());
    withRefusedStorage(() => {
      const env = browserPreferenceEnvironment();
      expect(env).toEqual({ storage: null, origin: ORIGIN, storageRefused: true });
      const scope = preferenceScope({ id: 'A' }, env);
      expect(scope).toEqual({ kind: 'unscoped', reason: 'storage-refused' });
      const read = readLayoutPreference(scope);
      expect(read).toEqual({ status: 'unavailable', layout: 'classic' });
      expect(readNotice(read)?.short).toBe('Using Classic');
      expect(writeLayoutPreference(scope, 'team')).toEqual({ status: 'not-saved', reason: 'storage-refused' });
      expect(writeNotice('team', { status: 'not-saved', reason: 'storage-refused' })?.detail).toBe('Team applies in this tab until you reload; this browser did not allow the layout preference to be saved.');
    });
  });

  it('renders Classic with the visible Using Classic notice through the default-environment hook and the chooser', () => {
    vi.stubGlobal('location', { origin: ORIGIN });
    vi.stubGlobal('localStorage', new MemoryStorage());
    withRefusedStorage(() => {
      let html = '';
      expect(() => { html = renderToStaticMarkup(createElement(DefaultChooserProbe, { user: { id: 'A', role: 'viewer' } })); }).not.toThrow();
      expect(html).toContain('<option value="classic" selected="">Classic</option>');
      expect(html).toMatch(/role="status"[^>]*title="This browser did not allow the saved layout preference to be read, so Classic is shown\.">Using Classic<span class="sr-only">/);
    });
  });

  it('keeps missing identity, origin or storage quiet and distinct from refused storage, and restores the global', () => {
    const storage = new MemoryStorage();
    vi.stubGlobal('location', { origin: ORIGIN });
    vi.stubGlobal('localStorage', storage);
    withRefusedStorage(() => {
      expect(renderToStaticMarkup(createElement(DefaultChooserProbe, { user: { username: 'legacy-login' } }))).not.toContain('Using Classic');
    });
    expect(preferenceScope({ username: 'legacy-login' }, { storage: null, origin: ORIGIN, storageRefused: true })).toEqual({ kind: 'unscoped', reason: 'no-account-identity' });
    expect(preferenceScope({ id: 'A' }, { storage: null, origin: null, storageRefused: true })).toEqual({ kind: 'unscoped', reason: 'no-origin' });
    const absent = preferenceScope({ id: 'A' }, { storage: null, origin: ORIGIN });
    expect(absent).toEqual({ kind: 'unscoped', reason: 'no-storage' });
    expect(readLayoutPreference(absent)).toEqual({ status: 'unscoped', layout: 'classic' });
    expect(readNotice(readLayoutPreference(absent))).toBeNull();
    expect(browserPreferenceEnvironment()).toEqual({ storage, origin: ORIGIN, storageRefused: false });
    expect(storage.reads).toEqual([]);
    expect(storage.writes).toEqual([]);
  });
});
