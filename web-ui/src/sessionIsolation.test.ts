import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { QueryObserver, QueryClientProvider } from '@tanstack/react-query';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import { api, type RepositorySaveReceipt } from './api';
import { ClientSession, clientSession, sessionFetch, watchSessionStorage, accountStorageKey } from './session';
import { SessionQueryCache } from './sessionQueries';
import { ViewOwnership, ownedMutation } from './viewOwnership';
import { beginRepositorySave, RepositorySaveJournal, runRepositorySave, RepositoryEditorOwnership, repositoryEditorKey, repositorySaveComplete } from './repositorySave';
import { persistManagedRemovalReceipt, readAllManagedRemovalReceipts, type ManagedRemovalReceipt } from './managedRemoval';
import { persistBranchPairRemovalNotice, readPersistedBranchPairRemovalNotice } from './branchPairRemoval';
import Layout from './components/Layout';

class MemoryStorage {
  values = new Map<string, string>();
  getItem(key: string) { return this.values.get(key) ?? null; }
  setItem(key: string, value: string) { this.values.set(key, value); }
  removeItem(key: string) { this.values.delete(key); }
  get length() { return this.values.size; }
  key(index: number) { return [...this.values.keys()][index] ?? null; }
}
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(yes => { resolve = yes; }); return { promise, resolve }; }
const admin = { id: 'A', username: 'admin-A', role: 'admin' };
const member = { id: 'B', username: 'member-B', role: 'viewer' };
const fetchMock = vi.fn();
let storage: MemoryStorage; let cache: SessionQueryCache;
beforeEach(() => {
  storage = new MemoryStorage(); storage.setItem('session_token', 'synthetic-A'); storage.setItem('user', JSON.stringify(admin));
  vi.stubGlobal('localStorage', storage); vi.stubGlobal('sessionStorage', new MemoryStorage());
  vi.stubGlobal('window', { location: { pathname: '/repos/A', href: '/repos/A' }, history: { state: { usr: { message: 'A receipt' } }, replaceState: vi.fn() } });
  vi.stubGlobal('fetch', fetchMock); fetchMock.mockReset(); cache = new SessionQueryCache(clientSession());
});
afterEach(() => { cache.dispose(); vi.unstubAllGlobals(); });
function loginB() { const session = clientSession(); session.signIn({ token: 'synthetic-B', user: member }, session.snapshot()); }

describe('#120 request and real QueryClient session boundaries', () => {
  it('cancels delayed A query and discards private caches before B can observe them', async () => {
    const old = cache.snapshot().client; const response = deferred<Response>(); fetchMock.mockReturnValueOnce(response.promise);
    old.setQueryData(['admin-users'], [{ email: 'admin-only@example.invalid' }]);
    const observer = new QueryObserver(old, { queryKey: ['repo', 'A'], queryFn: () => api.getRepo('A'), retry: false });
    const unsubscribe = observer.subscribe(() => {}); const pending = observer.refetch().catch(error => error);
    const signal = fetchMock.mock.calls[0][1].signal as AbortSignal;
    loginB(); expect(signal.aborted).toBe(true); expect(old.getQueryCache().getAll()).toHaveLength(0);
    const next = cache.snapshot().client; expect(next).not.toBe(old); expect(next.getQueryData(['admin-users'])).toBeUndefined();
    response.resolve(new Response('{"id":"A","name":"private"}')); await pending;
    expect(next.getQueryData(['repo', 'A'])).toBeUndefined(); expect(old.getQueryData(['repo', 'A'])).toBeUndefined(); unsubscribe();
  });
  it('clears cached admin mutations and the actual member navigation exposes no admin links', () => {
    const old = cache.snapshot().client; old.setQueryData(['admin-users'], ['private']); old.getMutationCache().build(old, { mutationKey: ['admin-edit'] });
    loginB(); expect(old.getMutationCache().getAll()).toHaveLength(0); expect(old.getQueryCache().getAll()).toHaveLength(0);
    const html = renderToStaticMarkup(createElement(QueryClientProvider, { client: cache.snapshot().client }, createElement(MemoryRouter, {}, createElement(Layout))));
    expect(html).toContain('member-B'); expect(html).not.toContain('href="/users"'); expect(html).not.toContain('href="/ldap"'); expect(html).not.toContain('private');
    expect(window.history.replaceState).toHaveBeenCalledWith({ usr: null }, '');
  });
  it.each(['headers', 'body'] as const)('refuses stale 401 during %s without expiring B', async stage => {
    const delayed = deferred<Response>(); const body = deferred<string>(); const incoming = new Response('', { status: 401 });
    if (stage === 'body') vi.spyOn(incoming, 'text').mockReturnValue(body.promise);
    fetchMock.mockReturnValueOnce(stage === 'headers' ? delayed.promise : Promise.resolve(incoming));
    const result = api.getMe().catch(error => error); await Promise.resolve(); loginB();
    delayed.resolve(incoming); body.resolve('{"error":"expired"}'); expect(await result).toBeInstanceOf(Error); expect(storage.getItem('session_token')).toBe('synthetic-B');
  });
  it('keeps explicit 403 and exact legacy role-only 401 denial distinct from an expired session', async () => {
    for (const [status, message] of [[403, 'forbidden'], [401, 'admin access required']] as const) {
      const before = cache.snapshot().client; fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ error: message }), { status }));
      await expect(api.getUsers()).rejects.toMatchObject({ status: 403 }); expect(storage.getItem('session_token')).toBe('synthetic-A'); expect(cache.snapshot().client).toBe(before);
    }
    fetchMock.mockResolvedValueOnce(new Response('{"error":"session expired"}', { status: 401 }));
    await expect(api.getMe()).rejects.toMatchObject({ status: 401 }); expect(storage.getItem('session_token')).toBeNull(); expect(storage.getItem('user')).toBeNull();
  });
  it('retires local logout immediately and late revocation cannot clear a replacement session', async () => {
    const delayed = deferred<Response>(); fetchMock.mockReturnValueOnce(delayed.promise); const result = api.logout().catch(error => error);
    expect(storage.getItem('session_token')).toBeNull(); expect(new Headers(fetchMock.mock.calls[0][1].headers).get('Authorization')).toBe('Bearer synthetic-A');
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ token: 'synthetic-A' }); loginB(); delayed.resolve(new Response('{"ok":true}')); await result;
    expect(storage.getItem('session_token')).toBe('synthetic-B');
  });
  it('public invalid login does not expire a session; a late login cannot replace a newer identity', async () => {
    fetchMock.mockResolvedValueOnce(new Response('{"error":"invalid password"}', { status: 401 }));
    await expect(api.login('synthetic', 'invalid')).rejects.toThrow('invalid password'); expect(storage.getItem('session_token')).toBe('synthetic-A');
    const delayed = deferred<Response>(); fetchMock.mockReturnValueOnce(delayed.promise); const result = api.login('old', 'synthetic').catch(error => error);
    loginB(); delayed.resolve(new Response('{"token":"stale","user":{"id":"stale","role":"admin"}}')); expect(await result).toBeInstanceOf(Error); expect(storage.getItem('session_token')).toBe('synthetic-B');
  });
  it('legacy login without user removes prior admin metadata and rotates even for the same token', async () => {
    const before = cache.snapshot(); fetchMock.mockResolvedValueOnce(new Response('{"token":"synthetic-A"}')); await api.login('', 'synthetic');
    expect(storage.getItem('user')).toBeNull(); expect(cache.snapshot().session.epoch).toBeGreaterThan(before.session.epoch); expect(cache.snapshot().client).not.toBe(before.client);
  });
  it('rejects old profile metadata after account change', () => {
    const session = clientSession(); const origin = session.snapshot(); loginB(); expect(() => session.updateUser(admin, origin)).toThrow('login session changed'); expect(JSON.parse(storage.getItem('user')!).id).toBe('B');
  });
  it('honors an explicit abort and releases an early 404 body without expiring the session', async () => {
    fetchMock.mockResolvedValueOnce(new Response('', { status: 404 })); expect(await api.getManagedRemoval('A')).toBeNull();
    const delayed = deferred<Response>(); fetchMock.mockReturnValueOnce(delayed.promise); const controller = new AbortController();
    const result = sessionFetch('/api/fixture', { signal: controller.signal }).catch(error => error); controller.abort(); expect((await result).name).toBe('AbortError'); delayed.resolve(new Response('late'));
    expect(storage.getItem('session_token')).toBe('synthetic-A');
  });
  it.each([202, 409])('keeps specialized managed-removal %s response semantics', async status => {
    fetchMock.mockResolvedValueOnce(new Response('{"ok":false,"state":"reconciliation_required","operation_id":"op-A"}', { status }));
    expect(await api.getManagedRemoval('A')).toMatchObject({ operation_id: 'op-A', state: 'reconciliation_required' }); expect(storage.getItem('session_token')).toBe('synthetic-A');
  });
});

describe('#120 cross-tab invalidation', () => {
  it('storage logout cancels requests, clears cached sensitive observations and notifies the other tab', async () => {
    const events = new EventTarget(); const stop = watchSessionStorage(events as unknown as Window); const old = cache.snapshot().client;
    old.setQueryData(['repo', 'A'], { private: true }); const delayed = deferred<Response>(); fetchMock.mockReturnValueOnce(delayed.promise); const result = api.getMe().catch(error => error);
    let notifications = 0; const unsubscribe = cache.subscribe(() => { notifications++; }); storage.removeItem('session_token'); storage.removeItem('user');
    events.dispatchEvent(Object.assign(new Event('storage'), { key: 'session_token' })); expect(cache.snapshot().session.token).toBeNull(); expect(notifications).toBe(1); expect(old.getQueryCache().getAll()).toHaveLength(0);
    delayed.resolve(new Response('{"id":"A"}')); expect(await result).toBeInstanceOf(Error); unsubscribe(); stop();
  });
  it('ignores unrelated storage, but role changes and storage.clear retire session caches', () => {
    const events = new EventTarget(); const stop = watchSessionStorage(events as unknown as Window); const old = cache.snapshot().client;
    events.dispatchEvent(Object.assign(new Event('storage'), { key: 'unrelated-draft' })); expect(cache.snapshot().client).toBe(old);
    storage.setItem('user', JSON.stringify({ ...admin, role: 'viewer' })); events.dispatchEvent(Object.assign(new Event('storage'), { key: 'user' })); expect(cache.snapshot().client).not.toBe(old);
    storage.values.clear(); events.dispatchEvent(Object.assign(new Event('storage'), { key: null })); expect(cache.snapshot().session.token).toBeNull(); stop();
  });
  it('checks storage before the delayed event arrives so A cannot populate a B cache', async () => {
    const delayed = deferred<Response>(); fetchMock.mockReturnValueOnce(delayed.promise); const result = api.getMe().catch(error => error);
    storage.setItem('session_token', 'synthetic-B'); storage.setItem('user', JSON.stringify(member)); delayed.resolve(new Response('{"id":"A"}')); expect(await result).toBeInstanceOf(Error); expect(cache.snapshot().session.token).toBe('synthetic-B');
  });
  it('authenticates and logs out in documented HTTP contexts without crypto.randomUUID', () => {
    const secureRandom = crypto.getRandomValues.bind(crypto);
    const random = vi.fn(secureRandom);
    vi.stubGlobal('crypto', { getRandomValues: random });
    const session = clientSession(); const before = cache.snapshot();
    expect(() => session.signIn({ token: 'synthetic-B', user: member }, session.snapshot())).not.toThrow();
    const signedIn = session.snapshot();
    expect(signedIn.token).toBe('synthetic-B'); expect(signedIn.boundary).toMatch(/^[0-9a-f]{32}$/);
    expect(cache.snapshot().client).not.toBe(before.client);
    expect(() => session.end(signedIn)).not.toThrow();
    expect(session.snapshot().token).toBeNull(); expect(session.snapshot().boundary).not.toBe(signedIn.boundary);
    const loggedOut = session.snapshot(); session.signIn({ token: 'synthetic-B', user: member }, loggedOut);
    expect(session.snapshot().epoch).toBeGreaterThan(loggedOut.epoch); expect(session.snapshot().boundary).not.toBe(loggedOut.boundary);
    expect(random).toHaveBeenCalledTimes(3); expect(random.mock.calls.every(call => call[0]?.byteLength === 16)).toBe(true);
  });
  it('a storage failure cannot bind a new token to old admin metadata and still retires the old cache', () => {
    const memory = new MemoryStorage(); memory.setItem('session_token', 'A'); memory.setItem('user', JSON.stringify(admin)); const session = new ClientSession(memory); let notifications = 0; session.subscribe(() => notifications++);
    vi.spyOn(memory, 'setItem').mockImplementation(() => { throw new Error('Storage unavailable'); }); expect(() => session.signIn({ token: 'B', user: member }, session.snapshot())).toThrow('Storage unavailable');
    expect(memory.getItem('session_token')).toBeNull(); expect(memory.getItem('user')).toBeNull(); expect(notifications).toBe(1);
  });
});

describe('#120 route-save and recovery ownership', () => {
  it('A-to-B while saving retains genuine A confirmation without closing or navigating B', async () => {
    const session = clientSession(); const origin = session.snapshot(); let path = '/repos/A'; let targetId = 'A';
    const view = new ViewOwnership({ epoch: origin.epoch, path }, () => ({ epoch: session.snapshot().epoch, path }));
    const owner = new RepositoryEditorOwnership({ owner: 'A', targetId, session: origin.token, epoch: origin.epoch }, () => ({ owner: 'A', targetId, session: session.snapshot().token, epoch: session.snapshot().epoch }));
    const journal = new RepositorySaveJournal('A', 'repo:A', storage); const attempt = beginRepositorySave('A', 'update', { name: 'A draft' }, {}, 'A', 'old');
    const gate = deferred<Response>(); const started = deferred<void>(); fetchMock.mockImplementationOnce(() => { started.resolve(); return gate.promise; });
    const save = () => runRepositorySave(attempt, {}, journal, { configuration: (a, signal) => api.updateRepo(a.repoId!, a.configuration, { requestId: a.configRequestId, expectedUpdatedAt: a.expectedUpdatedAt, signal }), credentials: vi.fn(), lookup: vi.fn() });
    const close = vi.fn(); const navigate = vi.fn();
    const client = cache.snapshot().client;
    const mutation = client.getMutationCache().build(client, ownedMutation({ mutationFn: save, onSuccess: () => { close(); navigate('/repos/A'); } }, view));
    const pending = mutation.execute(undefined); await started.promise; path = '/repos/B'; targetId = 'B';
    const receipt: RepositorySaveReceipt = { version: 1, request_id: attempt.configRequestId, kind: 'configuration', repo_id: 'A', configuration_updated_at: '2026-11-04T02:00:00Z', credential_fields: [], propagated_repositories: 0 };
    gate.resolve(new Response(JSON.stringify({ id: 'A', save_receipt: receipt }))); const result = await pending;
    expect(owner.isCurrent()).toBe(false);
    expect(repositorySaveComplete(result)).toBe(true); expect(journal.read()?.repoId).toBe('A'); expect(journal.read()?.configurationReceipt).toEqual(receipt); expect(close).not.toHaveBeenCalled(); expect(navigate).not.toHaveBeenCalled();
    expect(new RepositorySaveJournal('A', 'repo:B', storage).read()).toBeNull();
  });
  it('a queued old-route mutation refuses to send and cannot call replacement-view callbacks', async () => {
    let path = '/repos/A'; const view = new ViewOwnership({ epoch: 0, path }, () => ({ epoch: 0, path }));
    const send = vi.fn(async () => 'effect'); const callback = vi.fn(); const client = cache.snapshot().client;
    const mutation = client.getMutationCache().build(client, ownedMutation({ mutationFn: send, onSuccess: callback, onError: callback, onSettled: callback }, view));
    path = '/repos/B'; await expect(mutation.execute(undefined)).rejects.toThrow('login session changed'); expect(send).not.toHaveBeenCalled(); expect(callback).not.toHaveBeenCalled();
  });
  it('logout during submitted save preserves unknown outcome/request ID and never sends A credentials as B', async () => {
    const session = clientSession(); const origin = session.snapshot(); const gate = deferred<Response>(); fetchMock.mockReturnValueOnce(gate.promise);
    const journal = new RepositorySaveJournal('A', 'create', storage); const attempt = beginRepositorySave('A', 'create', { name: 'A draft' }, { git_token: 'synthetic-secret' }); const credentials = vi.fn();
    const save = runRepositorySave(attempt, { git_token: 'synthetic-secret' }, journal, { configuration: (a, signal) => api.createRepo(a.configuration, a.configRequestId, signal), credentials, lookup: vi.fn() }).catch(error => error);
    session.end(origin); loginB(); const failure = await save; expect(failure.attempt.configRequestId).toBe(attempt.configRequestId); expect(journal.read()?.configurationStage).toBe('unknown'); expect(credentials).not.toHaveBeenCalled();
    expect(new RepositorySaveJournal('B', 'create', storage).read()).toBeNull(); expect(JSON.stringify([...storage.values.values()])).not.toContain('synthetic-secret'); gate.resolve(new Response('{"id":"A-created"}'));
  });
  it('same-actor draft and known ID survive reauthentication without reviving the previous private editor', () => {
    const journal = new RepositorySaveJournal('A', 'repo:A', storage); const state = beginRepositorySave('A', 'update', { name: 'retained draft' }, { git_token: 'synthetic-secret' }, 'A', 'old'); journal.write(state);
    const first = clientSession().snapshot(); clientSession().end(first); clientSession().signIn({ token: first.token!, user: admin }, clientSession().snapshot());
    expect(journal.read()?.repoId).toBe('A'); expect(journal.read()?.configuration.name).toBe('retained draft'); expect(JSON.stringify([...storage.values.values()])).not.toContain('synthetic-secret');
    expect(repositoryEditorKey({ owner: 'A', targetId: 'A', session: first.token, epoch: first.epoch })).not.toBe(repositoryEditorKey({ owner: 'A', targetId: 'A', session: first.token, epoch: clientSession().snapshot().epoch }));
  });
  it('partitions admin removal displays while preserving old unscoped evidence and A receipts', () => {
    const receipt = { repoId: 'A', operationId: 'op-A', state: 'failed', message: 'A private recovery', updated_at: 'now' } as ManagedRemovalReceipt;
    storage.setItem('reposync.managedRemovalReceipt.legacy', JSON.stringify({ ...receipt, repoId: 'legacy' })); persistManagedRemovalReceipt(receipt); persistBranchPairRemovalNotice({ outcome: 'in_progress', message: 'A pending', warnings: [] });
    expect(readAllManagedRemovalReceipts().map(value => value.repoId)).toEqual(['A']); const key = accountStorageKey('reposync.managedRemovalReceipt.'); loginB();
    expect(readAllManagedRemovalReceipts()).toEqual([]); expect(readPersistedBranchPairRemovalNotice()).toBeNull(); expect(storage.getItem(`${key}A`)).not.toBeNull(); expect(storage.getItem('reposync.managedRemovalReceipt.legacy')).not.toBeNull();
    clientSession().end(); expect(readAllManagedRemovalReceipts()).toEqual([]); expect(readPersistedBranchPairRemovalNotice()).toBeNull();
  });
});
