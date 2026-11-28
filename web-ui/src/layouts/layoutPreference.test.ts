import { afterEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import {
  LAYOUT_PREFERENCE_PREFIX, LAYOUT_PREFERENCE_VERSION, LEGACY_LAYOUT_PREFERENCE_VERSION, MAX_ACCOUNT_IDENTITY_LENGTH, MAX_PREFERENCE_RECORD_LENGTH,
  browserPreferenceEnvironment, isWellFormedText, parseLayoutPreference, preferenceKey, preferenceScope, readLayoutPreference,
  readNotice, scopeIdentity, serializeLayoutPreference, stableAccountIdentity, useLayoutPreference,
  writeLayoutPreference, writeNotice, type LayoutPreferenceControl, type PreferenceEnvironment, type PreferenceScope,
} from './layoutPreference';
import { EXISTING_LAYOUT_IDS, LAYOUT_IDS, THEME_IDS, type LayoutId, type ThemeId } from './layoutCatalog';
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
  return createElement('output', null, `${preference.layout}|${preference.theme}|${preference.notice?.short ?? ''}`);
}
const render = (user: unknown, env: PreferenceEnvironment) => renderToStaticMarkup(createElement(Probe, { user, env }));

/**
 * Renders the hook and applies each action in turn, as a person choosing in the shell would,
 * and returns the hook's state after the last one. Each action runs during a render (a
 * render-phase update), which the server renderer applies before it finishes, so the
 * hook's own choose and chooseTheme are exercised without a DOM.
 */
function choices(user: unknown, env: PreferenceEnvironment, actions: ReadonlyArray<(preference: LayoutPreferenceControl) => void>): LayoutPreferenceControl {
  const states: LayoutPreferenceControl[] = [];
  let step = 0;
  function ChoiceProbe() {
    const preference = useLayoutPreference(user, () => env);
    states.push(preference);
    if (step < actions.length) actions[step++](preference);
    return null;
  }
  renderToStaticMarkup(createElement(ChoiceProbe));
  expect(step).toBe(actions.length);
  return states[states.length - 1];
}

/** The hook with its default browser environment, rendered through the real chooser. */
function DefaultChooserProbe({ user }: { user: unknown }) {
  const preference = useLayoutPreference(user);
  return createElement(LayoutChooser, {
    value: preference.layout, onChange: preference.choose, theme: preference.theme, onThemeChange: preference.chooseTheme, notice: preference.notice,
  });
}

/**
 * Ids whose longest record is exactly `target` code units (quotation marks escape to two).
 * The longest record is Individual (the longest layout id) in Light (the longest theme id).
 */
function identityForRecordLength(target: number, origin = ORIGIN): string {
  const added = target - record({ version: 2, layout: 'individual', theme: 'light', identity: '', origin }).length;
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

/** Every layout in every theme. */
const COMBINATIONS: ReadonlyArray<readonly [LayoutId, ThemeId]> = LAYOUT_IDS.flatMap(layout => THEME_IDS.map(theme => [layout, theme] as const));

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
    // #264 keeps the #262 key, so a saved layout is found again after the record version changes.
    expect(LAYOUT_PREFERENCE_PREFIX).toBe('reposync.layout-preference.v1');
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

describe('#262 preference read, as extended by #264', () => {
  it('reads a stored layout and theme for the same account and origin', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    storage.values.set(scope.key, record({ version: 2, layout: 'team', theme: 'light', identity: 'A', origin: ORIGIN }));
    expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout: 'team', theme: 'light' });
    storage.values.set(scope.key, serializeLayoutPreference(scope, 'individual'));
    expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout: 'individual', theme: 'dark' });
    storage.values.set(scope.key, serializeLayoutPreference(scope, 'guide', 'light'));
    expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout: 'guide', theme: 'light' });
  });

  it('falls back to Classic and dark when nothing is stored', () => {
    expect(readLayoutPreference(accountScope('A'))).toEqual({ status: 'absent', layout: 'classic', theme: 'dark' });
  });

  it('rejects corrupt, unknown, other-version and other-scope records as Classic and dark, without deleting them', () => {
    const scope = accountScope('A');
    const valid = { version: 2, layout: 'team', theme: 'light', identity: 'A', origin: ORIGIN };
    const legacy = { version: 1, layout: 'team', identity: 'A', origin: ORIGIN };
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
      [record({ ...legacy, extra: true }), 'corrupt'],
      [record({ layout: 'team', theme: 'light', identity: 'A', origin: ORIGIN }), 'corrupt'],
      [record({ layout: 'team', identity: 'A', origin: ORIGIN }), 'corrupt'],
      // Each version has exactly its own fields: version 2 needs a theme and version 1 has none.
      [record({ version: 2, layout: 'team', identity: 'A', origin: ORIGIN }), 'corrupt'],
      [record({ ...legacy, theme: 'dark' }), 'corrupt'],
      [record({ ...valid, layout: 'team', origin: ORIGIN + '/'.repeat(MAX_PREFERENCE_RECORD_LENGTH) }), 'corrupt'],
      [record({ ...legacy, layout: 'team', origin: ORIGIN + '/'.repeat(MAX_PREFERENCE_RECORD_LENGTH) }), 'corrupt'],
      [record({ ...valid, version: 3 }), 'unsupported-version'],
      [record({ ...valid, version: 0 }), 'unsupported-version'],
      [record({ ...valid, version: '2' }), 'unsupported-version'],
      [record({ ...legacy, version: '1' }), 'unsupported-version'],
      [record({ ...valid, version: null }), 'unsupported-version'],
      [record({ ...valid, identity: 'B' }), 'other-scope'],
      [record({ ...valid, origin: OTHER_ORIGIN }), 'other-scope'],
      [record({ ...legacy, identity: 'B' }), 'other-scope'],
      [record({ ...legacy, origin: OTHER_ORIGIN }), 'other-scope'],
      [record({ ...valid, layout: 'compact' }), 'unknown-layout'],
      [record({ ...valid, layout: 'Team' }), 'unknown-layout'],
      [record({ ...valid, layout: '__proto__' }), 'unknown-layout'],
      [record({ ...valid, layout: null }), 'unknown-layout'],
      [record({ ...valid, layout: ['team'] }), 'unknown-layout'],
      [record({ ...legacy, layout: 'compact' }), 'unknown-layout'],
      [record({ ...legacy, layout: 'Team' }), 'unknown-layout'],
      [record({ ...valid, theme: 'sepia' }), 'unknown-theme'],
      [record({ ...valid, theme: 'Light' }), 'unknown-theme'],
      [record({ ...valid, theme: ' dark' }), 'unknown-theme'],
      [record({ ...valid, theme: '' }), 'unknown-theme'],
      [record({ ...valid, theme: '__proto__' }), 'unknown-theme'],
      [record({ ...valid, theme: 'constructor' }), 'unknown-theme'],
      [record({ ...valid, theme: null }), 'unknown-theme'],
      [record({ ...valid, theme: ['light'] }), 'unknown-theme'],
      [record({ ...valid, theme: 1 }), 'unknown-theme'],
    ];
    for (const [raw, reason] of cases) {
      expect(parseLayoutPreference(raw, scope), raw.slice(0, 80)).toEqual({ status: 'invalid', layout: 'classic', theme: 'dark', reason });
      scope.storage.setItem(scope.key, raw);
      expect(readLayoutPreference(scope)).toEqual({ status: 'invalid', layout: 'classic', theme: 'dark', reason });
      expect(scope.storage.getItem(scope.key)).toBe(raw);
    }
  });

  it('returns Classic and dark, not an exception, when the browser refuses to read storage', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    storage.values.set(scope.key, serializeLayoutPreference(scope, 'team', 'light'));
    storage.failReads = true;
    expect(() => readLayoutPreference(scope)).not.toThrow();
    expect(readLayoutPreference(scope)).toEqual({ status: 'unavailable', layout: 'classic', theme: 'dark' });
  });

  it('does not read storage at all without a genuine account scope', () => {
    const storage = new MemoryStorage();
    const scope = preferenceScope({ username: 'legacy-login' }, environment(storage));
    expect(readLayoutPreference(scope)).toEqual({ status: 'unscoped', layout: 'classic', theme: 'dark' });
    expect(storage.reads).toEqual([]);
  });
});

describe('#264 version-1 preference migration (E2)', () => {
  it('reads a #262 version-1 record as its layout in dark, without rewriting it', () => {
    expect(LAYOUT_PREFERENCE_VERSION).toBe(2);
    expect(LEGACY_LAYOUT_PREFERENCE_VERSION).toBe(1);
    // A #262 build could only have saved one of the existing three layouts.
    for (const layout of EXISTING_LAYOUT_IDS) {
      const storage = new MemoryStorage();
      const scope = accountScope('A', storage);
      const legacy = record({ version: 1, layout, identity: 'A', origin: ORIGIN });
      storage.values.set(scope.key, legacy);
      expect(parseLayoutPreference(legacy, scope)).toEqual({ status: 'stored', layout, theme: 'dark' });
      expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout, theme: 'dark' });
      expect(readNotice(readLayoutPreference(scope))).toBeNull();
      expect(render({ id: 'A' }, environment(storage))).toBe(`<output>${layout}|dark|</output>`);
      expect(storage.writes).toEqual([]);
      expect(storage.values.get(scope.key)).toBe(legacy);
    }
  });

  it('rewrites the record as version 2 under the same key on the next layout or theme choice', () => {
    for (const layout of EXISTING_LAYOUT_IDS) {
      const storage = new MemoryStorage();
      const scope = accountScope('A', storage);
      storage.values.set(scope.key, record({ version: 1, layout, identity: 'A', origin: ORIGIN }));
      // A theme choice keeps the migrated layout.
      expect(choices({ id: 'A' }, environment(storage), [preference => preference.chooseTheme('light')])).toMatchObject({ layout, theme: 'light', notice: null });
      expect(storage.writes).toEqual([scope.key]);
      expect([...storage.values.keys()]).toEqual([scope.key]);
      expect(storage.values.get(scope.key)).toBe(`{"version":2,"layout":"${layout}","theme":"light","identity":"A","origin":"${ORIGIN}"}`);
      expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout, theme: 'light' });
    }
    // A layout choice keeps the migrated dark theme.
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    storage.values.set(scope.key, record({ version: 1, layout: 'team', identity: 'A', origin: ORIGIN }));
    expect(choices({ id: 'A' }, environment(storage), [preference => preference.choose('console')])).toMatchObject({ layout: 'console', theme: 'dark', notice: null });
    expect(storage.values.get(scope.key)).toBe(`{"version":2,"layout":"console","theme":"dark","identity":"A","origin":"${ORIGIN}"}`);
  });
});

describe('#262 preference write and isolation, as extended by #264', () => {
  it('stores only the version, layout id, theme id, identity and origin', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    expect(writeLayoutPreference(scope, 'team')).toEqual({ status: 'saved' });
    expect([...storage.values.keys()]).toEqual([scope.key]);
    expect(storage.values.get(scope.key)).toBe(`{"version":2,"layout":"team","theme":"dark","identity":"A","origin":"${ORIGIN}"}`);
    expect(writeLayoutPreference(scope, 'ledger', 'light')).toEqual({ status: 'saved' });
    expect(storage.values.get(scope.key)).toBe(`{"version":2,"layout":"ledger","theme":"light","identity":"A","origin":"${ORIGIN}"}`);
    expect(Object.keys(JSON.parse(storage.values.get(scope.key) ?? '{}'))).toEqual(['version', 'layout', 'theme', 'identity', 'origin']);
    const admin = preferenceScope({ id: 'A', role: 'admin', token: 'synthetic-secret', password: 'synthetic-password' }, environment(storage));
    expect(writeLayoutPreference(admin, 'individual')).toEqual({ status: 'saved' });
    const stored = storage.values.get(scope.key) ?? '';
    for (const secret of ['admin', 'synthetic-secret', 'synthetic-password', 'role', 'token']) expect(stored).not.toContain(secret);
    expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout: 'individual', theme: 'dark' });
  });

  it('keeps one account and one origin from inheriting another preference', () => {
    const storage = new MemoryStorage();
    const alpha = accountScope('alpha', storage);
    const beta = accountScope('beta', storage);
    const elsewhere = accountScope('alpha', storage, OTHER_ORIGIN);
    writeLayoutPreference(alpha, 'team', 'light');
    writeLayoutPreference(beta, 'individual');
    expect(readLayoutPreference(alpha)).toEqual({ status: 'stored', layout: 'team', theme: 'light' });
    expect(readLayoutPreference(beta)).toEqual({ status: 'stored', layout: 'individual', theme: 'dark' });
    expect(readLayoutPreference(elsewhere)).toEqual({ status: 'absent', layout: 'classic', theme: 'dark' });
    // A record copied under another account's key is still not trusted.
    storage.values.set(elsewhere.key, storage.values.get(alpha.key) ?? '');
    expect(readLayoutPreference(elsewhere)).toEqual({ status: 'invalid', layout: 'classic', theme: 'dark', reason: 'other-scope' });
  });

  it('never throws and never writes when it cannot or must not save', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    storage.failWrites = true;
    expect(() => writeLayoutPreference(scope, 'team')).not.toThrow();
    expect(writeLayoutPreference(scope, 'team')).toEqual({ status: 'not-saved', reason: 'storage-error' });
    expect(writeLayoutPreference(scope, 'team', 'light')).toEqual({ status: 'not-saved', reason: 'storage-error' });
    storage.failWrites = false;
    storage.writes.length = 0;
    for (const value of ['compact', 'Team', '', null, '__proto__']) {
      expect(writeLayoutPreference(scope, value)).toEqual({ status: 'not-saved', reason: 'invalid-layout' });
      expect(writeLayoutPreference(scope, value, 'light')).toEqual({ status: 'not-saved', reason: 'invalid-layout' });
    }
    for (const value of ['sepia', 'Light', 'Dark', ' dark', '', null, '__proto__', 'constructor', 1, ['light']]) {
      expect(writeLayoutPreference(scope, 'team', value), String(value)).toEqual({ status: 'not-saved', reason: 'invalid-theme' });
    }
    const unscoped = preferenceScope({ username: 'legacy-login' }, environment(storage));
    expect(writeLayoutPreference(unscoped, 'team')).toEqual({ status: 'not-saved', reason: 'no-account-identity' });
    expect(writeLayoutPreference(unscoped, 'team', 'light')).toEqual({ status: 'not-saved', reason: 'no-account-identity' });
    expect(storage.writes).toEqual([]);
    expect(storage.values.size).toBe(0);
  });
});

describe('#262 preference notices, as extended by #264', () => {
  it('reports a stored value that could not be used, and stays quiet when nothing was stored', () => {
    expect(readNotice({ status: 'invalid', layout: 'classic', theme: 'dark', reason: 'corrupt' })?.short).toBe('Using Classic');
    expect(readNotice({ status: 'invalid', layout: 'classic', theme: 'dark', reason: 'unknown-theme' })).toEqual({
      short: 'Using Classic', detail: 'The saved layout preference could not be used, so Classic is shown. Choosing a layout replaces it.',
    });
    expect(readNotice({ status: 'unavailable', layout: 'classic', theme: 'dark' })?.detail).toContain('did not allow the saved layout preference to be read');
    for (const status of ['absent', 'unscoped'] as const) expect(readNotice({ status, layout: 'classic', theme: 'dark' })).toBeNull();
    expect(readNotice({ status: 'stored', layout: 'team', theme: 'dark' })).toBeNull();
    expect(readNotice({ status: 'stored', layout: 'guide', theme: 'light' })).toBeNull();
  });

  it('tells the person a chosen layout applies but was not saved, and why', () => {
    expect(writeNotice('team', { status: 'saved' })).toBeNull();
    const failed = writeNotice('team', { status: 'not-saved', reason: 'storage-error' });
    expect(failed?.short).toBe('Not saved');
    expect(failed?.detail).toBe('Team applies in this tab until you reload; this browser did not allow the layout preference to be saved.');
    expect(writeNotice('individual', { status: 'not-saved', reason: 'no-account-identity' })?.detail).toContain('no stable account identity');
    expect(writeNotice('team', { status: 'not-saved', reason: 'no-origin' })?.detail).toContain('no stable origin');
    expect(writeNotice('team', { status: 'not-saved', reason: 'no-storage' })?.detail).toContain('did not allow the layout preference to be saved');
    expect(writeNotice('team', { status: 'not-saved', reason: 'invalid-layout' })).toEqual({ short: 'Not saved', detail: 'That layout is not available in this build.' });
    expect(writeNotice('guide', { status: 'not-saved', reason: 'storage-error' })?.detail).toBe('Guide applies in this tab until you reload; this browser did not allow the layout preference to be saved.');
  });

  it('names the theme when a chosen theme applies but was not saved, and reports an unknown theme', () => {
    expect(writeNotice('team', { status: 'saved' }, { theme: 'light' })).toBeNull();
    expect(writeNotice('team', { status: 'not-saved', reason: 'storage-error' }, { theme: 'light' })).toEqual({
      short: 'Not saved', detail: 'The Light theme applies in this tab until you reload; this browser did not allow the layout preference to be saved.',
    });
    expect(writeNotice('team', { status: 'not-saved', reason: 'no-account-identity' }, { theme: 'dark' })?.detail)
      .toBe('The Dark theme applies in this tab until you reload; this sign-in has no stable account identity to save it under.');
    expect(writeNotice('team', { status: 'not-saved', reason: 'invalid-theme' })).toEqual({ short: 'Not saved', detail: 'That theme is not available in this build.' });
  });
});

describe('#262 useLayoutPreference, as extended by #264', () => {
  it('starts from the account preference, or Classic and dark with a notice when it cannot be used', () => {
    const storage = new MemoryStorage();
    const scope: PreferenceScope = accountScope('A', storage);
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|dark|</output>');
    writeLayoutPreference(scope, 'team');
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>team|dark|</output>');
    writeLayoutPreference(scope, 'console', 'light');
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>console|light|</output>');
    expect(render({ id: 'B' }, environment(storage))).toBe('<output>classic|dark|</output>');
    storage.values.set(keyFor('A'), record({ version: 2, layout: 'team', theme: 'sepia', identity: 'A', origin: ORIGIN }));
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|dark|Using Classic</output>');
    storage.values.set(keyFor('A'), '{broken');
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|dark|Using Classic</output>');
    storage.failReads = true;
    expect(render({ id: 'A' }, environment(storage))).toBe('<output>classic|dark|Using Classic</output>');
  });

  it('does not touch storage for a sign-in without a stable account identity', () => {
    const storage = new MemoryStorage();
    storage.values.set(keyFor('A'), record({ version: 2, layout: 'team', theme: 'light', identity: 'A', origin: ORIGIN }));
    expect(render(null, environment(storage))).toBe('<output>classic|dark|</output>');
    expect(render({ username: 'legacy-login' }, environment(storage))).toBe('<output>classic|dark|</output>');
    expect(storage.reads).toEqual([]);
  });

  it('keeps the current theme when a layout is chosen and the current layout when a theme is chosen', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    writeLayoutPreference(scope, 'team', 'light');
    expect(choices({ id: 'A' }, environment(storage), [preference => preference.choose('guide')])).toMatchObject({ layout: 'guide', theme: 'light', notice: null });
    expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout: 'guide', theme: 'light' });
    const final = choices({ id: 'A' }, environment(storage), [
      preference => preference.chooseTheme('dark'),
      preference => preference.choose('classic'),
      preference => preference.chooseTheme('light'),
    ]);
    expect(final).toMatchObject({ layout: 'classic', theme: 'light', notice: null });
    expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout: 'classic', theme: 'light' });
    expect(storage.writes).toHaveLength(5);
  });

  it('ignores an unknown layout or theme id passed to choose or chooseTheme', () => {
    const storage = new MemoryStorage();
    const scope = accountScope('A', storage);
    writeLayoutPreference(scope, 'team', 'light');
    storage.writes.length = 0;
    const result = choices({ id: 'A' }, environment(storage), [
      preference => { preference.chooseTheme('sepia' as ThemeId); preference.choose('compact' as LayoutId); },
    ]);
    expect(result).toMatchObject({ layout: 'team', theme: 'light', notice: null });
    expect(storage.writes).toEqual([]);
  });

  it('applies a chosen theme or layout that cannot be saved, and says so', () => {
    const storage = new MemoryStorage();
    const theme = choices({ username: 'legacy-login' }, environment(storage), [preference => preference.chooseTheme('light')]);
    expect(theme).toMatchObject({ layout: 'classic', theme: 'light' });
    expect(theme.notice).toEqual({ short: 'Not saved', detail: 'The Light theme applies in this tab until you reload; this sign-in has no stable account identity to save it under.' });
    const layout = choices({ username: 'legacy-login' }, environment(storage), [preference => preference.choose('ledger')]);
    expect(layout).toMatchObject({ layout: 'ledger', theme: 'dark' });
    expect(layout.notice?.detail).toBe('Ledger applies in this tab until you reload; this sign-in has no stable account identity to save it under.');
    storage.failWrites = true;
    const refused = choices({ id: 'A' }, environment(storage), [preference => preference.chooseTheme('light')]);
    expect(refused).toMatchObject({ layout: 'classic', theme: 'light' });
    expect(refused.notice?.detail).toBe('The Light theme applies in this tab until you reload; this browser did not allow the layout preference to be saved.');
    expect(storage.values.size).toBe(0);
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
      storage.values.set(keyFor('A'), serializeLayoutPreference(accountScope('A', storage), 'team', 'light'));
      let html = '';
      expect(() => { html = render({ id, role: 'admin' }, environment(storage)); }).not.toThrow();
      expect(html).toBe('<output>classic|dark|</output>');
      expect(writeLayoutPreference(preferenceScope({ id }, environment(storage)), 'team')).toEqual({ status: 'not-saved', reason: 'no-account-identity' });
      expect(storage.reads).toEqual([]);
      expect(storage.writes).toEqual([]);
    }
  });

  // Covers all six layouts in both themes: a Unicode id must round-trip whatever is chosen.
  it('keeps valid surrogate pairs and other Unicode ids, which save and re-read exactly', () => {
    for (const id of UNICODE_IDS) {
      expect(isWellFormedText(id)).toBe(true);
      expect(stableAccountIdentity({ id })).toBe(id);
      expect(keyFor(id)).toBe(`${LAYOUT_PREFERENCE_PREFIX}:${encodeURIComponent(ORIGIN)}:${encodeURIComponent(id)}`);
      for (const [layout, theme] of COMBINATIONS) {
        const storage = new MemoryStorage();
        const scope = accountScope(id, storage);
        expect(writeLayoutPreference(scope, layout, theme)).toEqual({ status: 'saved' });
        expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout, theme });
        expect(render({ id }, environment(storage))).toBe(`<output>${layout}|${theme}|</output>`);
      }
    }
  });
});

describe('#262 RS262-2 record size, as extended by #264', () => {
  // Covers all six layouts in both themes: no combination may persist for this scope.
  it('does not persist an accepted id of 256 quotation marks whose record the reader would reject', () => {
    const quotes = '"'.repeat(MAX_ACCOUNT_IDENTITY_LENGTH);
    expect(stableAccountIdentity({ id: quotes })).toBe(quotes);
    expect(record({ version: 2, layout: 'team', theme: 'dark', identity: quotes, origin: ORIGIN }).length).toBeGreaterThan(MAX_PREFERENCE_RECORD_LENGTH);
    const storage = new MemoryStorage();
    const scope = preferenceScope({ id: quotes }, environment(storage));
    expect(scope).toEqual({ kind: 'unscoped', reason: 'record-too-large' });
    for (const [layout, theme] of COMBINATIONS) expect(writeLayoutPreference(scope, layout, theme)).toEqual({ status: 'not-saved', reason: 'record-too-large' });
    expect(readLayoutPreference(scope)).toEqual({ status: 'unscoped', layout: 'classic', theme: 'dark' });
    expect(render({ id: quotes }, environment(storage))).toBe('<output>classic|dark|</output>');
    expect(storage.reads).toEqual([]);
    expect(storage.writes).toEqual([]);
    const notice = writeNotice('team', { status: 'not-saved', reason: 'record-too-large' });
    expect(notice?.detail).toBe('Team applies in this tab until you reload; this account identity and site address are too long to store a layout preference for.');
    expect(notice?.detail).not.toContain('browser');
  });

  // Covers all six layouts in both themes: the scope bound is set by the longest combination.
  it('saves every layout and theme at exactly the record limit and refuses the whole scope one code unit beyond it', () => {
    const exact = identityForRecordLength(MAX_PREFERENCE_RECORD_LENGTH);
    const beyond = identityForRecordLength(MAX_PREFERENCE_RECORD_LENGTH + 1);
    for (const id of [exact, beyond]) {
      expect(id.length).toBeLessThanOrEqual(MAX_ACCOUNT_IDENTITY_LENGTH);
      expect(stableAccountIdentity({ id })).toBe(id);
    }
    const storage = new MemoryStorage();
    const scope = accountScope(exact, storage);
    const lengths: Record<string, number> = {};
    for (const [layout, theme] of COMBINATIONS) {
      expect(writeLayoutPreference(scope, layout, theme)).toEqual({ status: 'saved' });
      lengths[`${layout}/${theme}`] = (storage.values.get(scope.key) ?? '').length;
      expect(readLayoutPreference(scope)).toEqual({ status: 'stored', layout, theme });
    }
    expect(lengths).toEqual({
      'classic/dark': MAX_PREFERENCE_RECORD_LENGTH - 4, 'classic/light': MAX_PREFERENCE_RECORD_LENGTH - 3,
      'individual/dark': MAX_PREFERENCE_RECORD_LENGTH - 1, 'individual/light': MAX_PREFERENCE_RECORD_LENGTH,
      'team/dark': MAX_PREFERENCE_RECORD_LENGTH - 7, 'team/light': MAX_PREFERENCE_RECORD_LENGTH - 6,
      'ledger/dark': MAX_PREFERENCE_RECORD_LENGTH - 5, 'ledger/light': MAX_PREFERENCE_RECORD_LENGTH - 4,
      'console/dark': MAX_PREFERENCE_RECORD_LENGTH - 4, 'console/light': MAX_PREFERENCE_RECORD_LENGTH - 3,
      'guide/dark': MAX_PREFERENCE_RECORD_LENGTH - 6, 'guide/light': MAX_PREFERENCE_RECORD_LENGTH - 5,
    });
    // Team in dark alone would still fit one unit beyond, but whether a scope persists does not depend on what is chosen.
    expect(record({ version: 2, layout: 'team', theme: 'dark', identity: beyond, origin: ORIGIN }).length).toBeLessThan(MAX_PREFERENCE_RECORD_LENGTH);
    const refusedStorage = new MemoryStorage();
    const refused = preferenceScope({ id: beyond }, environment(refusedStorage));
    expect(refused).toEqual({ kind: 'unscoped', reason: 'record-too-large' });
    for (const [layout, theme] of COMBINATIONS) expect(writeLayoutPreference(refused, layout, theme)).toEqual({ status: 'not-saved', reason: 'record-too-large' });
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
    expect(writeLayoutPreference(constructed, 'individual', 'light')).toEqual({ status: 'not-saved', reason: 'record-too-large' });
    expect(storage.writes).toEqual([]);
    expect(storage.values.get(key)).toBe(earlier);
    // Whatever the writer does save re-reads as the same stored layout and theme.
    expect(writeLayoutPreference(constructed, 'team')).toEqual({ status: 'saved' });
    expect(readLayoutPreference(constructed)).toEqual({ status: 'stored', layout: 'team', theme: 'dark' });
    expect(writeLayoutPreference(constructed, 'individual', 'dark')).toEqual({ status: 'saved' });
    expect((storage.values.get(key) ?? '').length).toBe(MAX_PREFERENCE_RECORD_LENGTH);
    expect(readLayoutPreference(constructed)).toEqual({ status: 'stored', layout: 'individual', theme: 'dark' });
  });

  // Covers all six layouts in both themes.
  it('round-trips every layout and theme for accepted ordinary, escaped and Unicode ids', () => {
    for (const id of ['fixture-admin', 'user 1', 'quote"and\\backslash', '"'.repeat(100), ...UNICODE_IDS]) {
      for (const [layout, theme] of COMBINATIONS) {
        const storage = new MemoryStorage();
        const scope = accountScope(id, storage);
        expect(writeLayoutPreference(scope, layout, theme), id).toEqual({ status: 'saved' });
        expect((storage.values.get(scope.key) ?? '').length).toBeLessThanOrEqual(MAX_PREFERENCE_RECORD_LENGTH);
        expect(readLayoutPreference(scope), id).toEqual({ status: 'stored', layout, theme });
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
      expect(read).toEqual({ status: 'unavailable', layout: 'classic', theme: 'dark' });
      expect(readNotice(read)?.short).toBe('Using Classic');
      expect(writeLayoutPreference(scope, 'team')).toEqual({ status: 'not-saved', reason: 'storage-refused' });
      expect(writeLayoutPreference(scope, 'team', 'light')).toEqual({ status: 'not-saved', reason: 'storage-refused' });
      expect(writeNotice('team', { status: 'not-saved', reason: 'storage-refused' })?.detail).toBe('Team applies in this tab until you reload; this browser did not allow the layout preference to be saved.');
      expect(writeNotice('team', { status: 'not-saved', reason: 'storage-refused' }, { theme: 'light' })?.detail).toBe('The Light theme applies in this tab until you reload; this browser did not allow the layout preference to be saved.');
    });
  });

  it('renders Classic and dark with the visible Using Classic notice through the default-environment hook and the chooser', () => {
    vi.stubGlobal('location', { origin: ORIGIN });
    vi.stubGlobal('localStorage', new MemoryStorage());
    withRefusedStorage(() => {
      let html = '';
      expect(() => { html = renderToStaticMarkup(createElement(DefaultChooserProbe, { user: { id: 'A', role: 'viewer' } })); }).not.toThrow();
      expect(html).toContain('<option value="classic" selected="">Classic</option>');
      expect(html).toContain('<option value="dark" selected="">Dark</option>');
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
    expect(readLayoutPreference(absent)).toEqual({ status: 'unscoped', layout: 'classic', theme: 'dark' });
    expect(readNotice(readLayoutPreference(absent))).toBeNull();
    expect(browserPreferenceEnvironment()).toEqual({ storage, origin: ORIGIN, storageRefused: false });
    expect(storage.reads).toEqual([]);
    expect(storage.writes).toEqual([]);
  });
});

describe('#264 layout chooser', () => {
  const noop = () => {};
  it('shows Dark selected when it is given no theme, after the layout select', () => {
    const html = renderToStaticMarkup(createElement(LayoutChooser, { value: 'classic', onChange: noop, notice: null }));
    expect(html.indexOf('data-rs-layout-chooser=""')).toBeLessThan(html.indexOf('data-rs-theme-chooser=""'));
    expect(html).toContain('<option value="dark" selected="">Dark</option><option value="light">Light</option>');
  });
});
