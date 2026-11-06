import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from './api';
import { acknowledgementPrompt, errorScope } from './errorAcknowledgement';

const observed = { through_id: '9007199254740993', observed_at: '2026-11-03T12:00:00Z' };
let storage: Map<string, string>;
const fetchMock = vi.fn();
beforeEach(() => {
  storage = new Map([['session_token', 'session'], ['user', '{"role":"admin"}']]);
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
    removeItem: (key: string) => storage.delete(key),
  });
  vi.stubGlobal('window', { location: { pathname: '/dashboard', href: '/dashboard' } });
  vi.stubGlobal('fetch', fetchMock);
  fetchMock.mockReset();
});
afterEach(() => vi.unstubAllGlobals());

describe('#114 scoped acknowledgement client', () => {
  it('submits the selected repository and exact observed boundary without numeric rounding', async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify({ ok: true, acknowledged: 2 })));
    expect(await api.acknowledgeErrors(errorScope('repo-a'), observed)).toEqual({ ok: true, acknowledged: 2 });
    const [url, options] = fetchMock.mock.calls[0];
    expect(url).toBe('/api/status/acknowledge-errors');
    expect(options.method).toBe('POST');
    expect(new Headers(options.headers).get('Authorization')).toBe('Bearer session');
    expect(JSON.parse(options.body)).toEqual({ scope: { kind: 'repository', repo_id: 'repo-a' }, observation: observed });
  });
  it('makes all-repositories explicit in both request and confirmation', async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify({ ok: true, acknowledged: 0 })));
    await api.acknowledgeErrors(errorScope(undefined), observed);
    expect(JSON.parse(fetchMock.mock.calls[0][1].body).scope).toEqual({ kind: 'all' });
    expect(acknowledgementPrompt(errorScope(undefined), '')).toContain('All Repositories (global scope)');
    expect(acknowledgementPrompt(errorScope('repo-a'), 'A')).toContain('repository “A”');
    expect(acknowledgementPrompt(errorScope('repo-a'), 'A')).toContain('evidence will be preserved');
  });
  it('exposes permission and storage errors for feedback without logging out', async () => {
    for (const status of [403, 500]) {
      fetchMock.mockResolvedValue(new Response('{"error":"acknowledgement failed"}', { status }));
      await expect(api.acknowledgeErrors(errorScope('repo-a'), observed)).rejects.toThrow(`API error ${status}`);
      expect(storage.get('session_token')).toBe('session');
      expect(window.location.href).toBe('/dashboard');
    }
  });
  it('distinguishes expired authentication from permission refusal', async () => {
    fetchMock.mockResolvedValue(new Response('{"error":"unauthorized"}', { status: 401 }));
    await expect(api.acknowledgeErrors(errorScope('repo-a'), observed)).rejects.toThrow('Session expired');
    expect(storage.has('session_token')).toBe(false);
    expect(storage.has('user')).toBe(false);
    expect(window.location.href).toBe('/login');
  });
});
