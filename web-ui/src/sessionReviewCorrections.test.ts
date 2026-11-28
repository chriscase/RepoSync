import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MutationObserver, QueryClient, onlineManager } from '@tanstack/react-query';
import { api } from './api';
import { clientSession, sessionFetch, RetiredSessionError } from './session';
import { ViewOwnership, ownedMutation } from './viewOwnership';
import { removeBranchPairForView } from './branchPairRemoval';
import ImportProgressCard from './components/ImportProgressCard';

function deferred<T>() { let resolve!: (value: T) => void; let reject!: (error: Error) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
const fetchMock = vi.fn(); let client: QueryClient;
beforeEach(() => {
  const values = new Map([['session_token', 'synthetic-A'], ['user', JSON.stringify({ id: 'A', role: 'admin' })]]);
  vi.stubGlobal('localStorage', { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) });
  vi.stubGlobal('window', { location: { pathname: '/repos/child', search: '', href: '/repos/child' } });
  vi.stubGlobal('fetch', fetchMock); fetchMock.mockReset(); client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
});
afterEach(() => { onlineManager.setOnline(true); client.clear(); vi.unstubAllGlobals(); });
function removalOwner() {
  const session = clientSession(); let path = '/repos/child'; const origin = { epoch: session.snapshot().epoch, path };
  const owner = new ViewOwnership(origin, () => ({ epoch: session.snapshot().epoch, path }));
  return { owner, retireRoute: () => { path = '/repos/replacement'; }, replaceSession: () => { session.signIn({ token: 'synthetic-B', user: { id: 'B', role: 'admin' } }, session.snapshot()); } };
}
const input = { viewedId: 'child', target: { id: 'child', parent_id: 'parent' }, options: { delete_git: false, delete_svn: false } };
const transport = { lookupParent: api.getRepo, remove: api.deleteBranchPair };

describe('RS120-1 actual removal continuation', () => {
  it.each(['session', 'route', 'late-fulfilled-route'] as const)('never follows a retired %s parent lookup with DELETE or replacement-authorized calls', async transition => {
    const { owner, retireRoute, replaceSession } = removalOwner(); const parent = deferred<Response>(); fetchMock.mockReturnValueOnce(parent.promise);
    const result = removeBranchPairForView(input, owner, transport).catch(error => error);
    expect(fetchMock.mock.calls[0][0]).toBe('/api/repos/parent');
    if (transition === 'session') replaceSession(); else retireRoute();
    if (transition === 'route') parent.reject(new TypeError('lookup failed')); else parent.resolve(new Response('{"id":"parent"}'));
    expect(await result).toBeInstanceOf(Error); expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls.filter(call => call[1].method === 'DELETE')).toHaveLength(0);
    expect(fetchMock.mock.calls.filter(call => new Headers(call[1].headers).get('Authorization') === 'Bearer synthetic-B')).toHaveLength(0);
  });
  it.each([200, 404, 500])('preserves same-owner parent %s destination and real completed removal', async status => {
    const { owner } = removalOwner(); fetchMock.mockResolvedValueOnce(new Response(status === 200 ? '{"id":"parent"}' : '{"error":"lookup refused"}', { status }));
    fetchMock.mockResolvedValueOnce(new Response('{"ok":true,"state":"completed","operation_id":"genuine"}'));
    const result = await removeBranchPairForView(input, owner, transport);
    expect(result.destination).toBe(status === 404 ? '/repos' : '/repos/parent'); expect(result.outcome).toBe('completed'); expect(result.result.operation_id).toBe('genuine');
    expect(fetchMock).toHaveBeenCalledTimes(2); expect(fetchMock.mock.calls[1][1].method).toBe('DELETE'); expect(new Headers(fetchMock.mock.calls[1][1].headers).get('Authorization')).toBe('Bearer synthetic-A');
  });
  it('does not downgrade same-owner explicit cancellation to uncertain-parent permission to delete', async () => {
    const { owner } = removalOwner(); const remove = vi.fn();
    await expect(removeBranchPairForView(input, owner, { lookupParent: async () => { throw new DOMException('cancelled', 'AbortError'); }, remove })).rejects.toMatchObject({ name: 'AbortError' }); expect(remove).not.toHaveBeenCalled();
  });
  it('propagates an explicit retired-session error even if the current view itself has not changed', async () => {
    const { owner } = removalOwner(); const remove = vi.fn();
    await expect(removeBranchPairForView(input, owner, { lookupParent: async () => { throw new RetiredSessionError(); }, remove })).rejects.toBeInstanceOf(RetiredSessionError); expect(remove).not.toHaveBeenCalled();
  });
  it('retains a genuinely submitted result after route retirement and does not report it undone', async () => {
    const { owner, retireRoute } = removalOwner(); const deletion = deferred<Response>(); const sent = deferred<void>();
    fetchMock.mockResolvedValueOnce(new Response('{"id":"parent"}')).mockImplementationOnce(() => { sent.resolve(); return deletion.promise; });
    const result = removeBranchPairForView(input, owner, transport); await sent.promise; retireRoute(); deletion.resolve(new Response('{"ok":true,"state":"completed","operation_id":"already-sent"}'));
    expect(await result).toMatchObject({ outcome: 'completed', result: { operation_id: 'already-sent' } }); expect(owner.isCurrent()).toBe(false);
  });
  it('a session change after DELETE submission remains an unknown response, never a rollback claim or second send', async () => {
    const { owner, replaceSession } = removalOwner(); const deletion = deferred<Response>(); const sent = deferred<void>();
    fetchMock.mockResolvedValueOnce(new Response('{"id":"parent"}')).mockImplementationOnce(() => { sent.resolve(); return deletion.promise; });
    const result = removeBranchPairForView(input, owner, transport).catch(error => error); await sent.promise; replaceSession();
    expect(await result).toBeInstanceOf(Error); expect(fetchMock).toHaveBeenCalledTimes(2); deletion.resolve(new Response('{"ok":true,"state":"completed"}'));
  });
});

describe('RS120-2 actual MutationObserver pending option replacement', () => {
  it.each(['success', 'error'] as const)('A %s cannot acquire B navigation, cache invalidation or feedback from setOptions', async outcome => {
    let path = '/conflicts/A'; const ownerA = new ViewOwnership({ epoch: 0, path }, () => ({ epoch: 0, path }));
    const gate = deferred<string>(); const sent = deferred<void>(); const oldFeedback = vi.fn(); const replacement = vi.fn();
    const observer = new MutationObserver(client, ownedMutation({ mutationFn: async () => { sent.resolve(); return gate.promise; }, onSuccess: oldFeedback, onError: oldFeedback, onSettled: oldFeedback }, ownerA));
    const unsubscribe = observer.subscribe(() => {}); const pending = observer.mutate(undefined).catch(error => error); await sent.promise;
    path = '/conflicts/B'; ownerA.setActive(false); const ownerB = new ViewOwnership({ epoch: 0, path }, () => ({ epoch: 0, path }));
    observer.setOptions(ownedMutation({ mutationFn: async () => 'B', onSuccess: replacement, onError: replacement, onSettled: replacement }, ownerB));
    expect(observer.getCurrentResult().isIdle).toBe(true);
    if (outcome === 'success') gate.resolve('A genuine outcome'); else gate.reject(new Error('A failure'));
    await pending; expect(oldFeedback).not.toHaveBeenCalled(); expect(replacement).not.toHaveBeenCalled(); expect(observer.getCurrentResult().data).toBeUndefined(); expect(observer.getCurrentResult().error).toBeNull(); unsubscribe();
  });
  it('paused A send cannot adopt B mutationFn and a fresh intentional B action still works', async () => {
    onlineManager.setOnline(false); let path = '/conflicts/A'; const ownerA = new ViewOwnership({ epoch: 0, path }, () => ({ epoch: 0, path })); const sendA = vi.fn(async () => 'A'); const sendB = vi.fn(async () => 'B'); const feedback = vi.fn();
    const observer = new MutationObserver(client, ownedMutation({ mutationFn: sendA, onSuccess: feedback, onError: feedback }, ownerA)); const unsubscribe = observer.subscribe(() => {});
    const pending = observer.mutate(undefined).catch(error => error); await vi.waitFor(() => expect(observer.getCurrentResult().isPaused).toBe(true));
    path = '/conflicts/B'; ownerA.setActive(false); const ownerB = new ViewOwnership({ epoch: 0, path }, () => ({ epoch: 0, path })); observer.setOptions(ownedMutation({ mutationFn: sendB, onSuccess: feedback }, ownerB));
    onlineManager.setOnline(true); await client.resumePausedMutations(); expect(await pending).toBeInstanceOf(Error); expect(sendA).not.toHaveBeenCalled(); expect(sendB).not.toHaveBeenCalled(); expect(feedback).not.toHaveBeenCalled();
    expect(await observer.mutate(undefined)).toBe('B'); expect(sendB).toHaveBeenCalledTimes(1); expect(feedback).toHaveBeenCalledTimes(1); unsubscribe();
  });
  it('normal same-owner callback updates retain the expected TanStack behavior', async () => {
    const owner = new ViewOwnership({ epoch: 0, path: '/conflicts/A' }, () => ({ epoch: 0, path: '/conflicts/A' })); const gate = deferred<string>(); const sent = deferred<void>(); const old = vi.fn(); const current = vi.fn();
    const observer = new MutationObserver(client, ownedMutation({ mutationFn: async () => { sent.resolve(); return gate.promise; }, onSuccess: old }, owner)); const unsubscribe = observer.subscribe(() => {});
    const pending = observer.mutate(undefined); await sent.promise; observer.setOptions(ownedMutation({ mutationFn: async () => 'same owner', onSuccess: current }, owner)); gate.resolve('actual A result');
    expect(await pending).toBe('actual A result'); expect(old).not.toHaveBeenCalled(); expect(current).toHaveBeenCalledTimes(1); unsubscribe();
  });
});

describe('RS120-3 actual import subject identity and observers', () => {
  it('the actual card remounts repository-local error/pending/preferences for same-path A/B/global changes', () => {
    const a = ImportProgressCard({ repoId: 'A', defaultImportMode: 'snapshot', defaultSvnRevision: '17' }); const b = ImportProgressCard({ repoId: 'B', defaultImportMode: 'full', defaultSvnRevision: 'HEAD' }); const global = ImportProgressCard();
    expect(a.type).toBe(b.type); expect(a.key).not.toBe(b.key); expect(global.key).not.toBe(a.key); expect(ImportProgressCard({ repoId: 'global' }).key).not.toBe(global.key);
    expect(a.props.defaultImportMode).toBe('snapshot'); expect(a.props.defaultSvnRevision).toBe('17'); expect(b.props.defaultImportMode).toBe('full'); expect(b.props.defaultSvnRevision).toBe('HEAD');
  });
  it.each(['success', 'error'] as const)('same pathname repo_id change cannot put A %s feedback/pending state into B', async outcome => {
    const pathname = '/'; let search = '?repo_id=A'; const current = () => ({ epoch: 0, path: pathname + search }); const ownerA = new ViewOwnership(current(), current);
    const gate = deferred<Response>(); const sent = deferred<void>(); fetchMock.mockImplementationOnce(() => { sent.resolve(); return gate.promise; }); const aFeedback = vi.fn(); const bFeedback = vi.fn();
    const a = new MutationObserver(client, ownedMutation({ mutationFn: () => sessionFetch('/api/repos/A/import', { method: 'POST' }).then(async r => { const value = await r.json(); if (!r.ok) throw new Error(value.error); return value; }), onSuccess: aFeedback, onError: aFeedback }, ownerA));
    const stopA = a.subscribe(() => {}); const pending = a.mutate(undefined).catch(error => error); await sent.promise;
    search = '?repo_id=B'; ownerA.setActive(false); const ownerB = new ViewOwnership(current(), current);
    const b = new MutationObserver(client, ownedMutation({ mutationFn: () => sessionFetch('/api/repos/B/import', { method: 'POST' }).then(r => r.json()), onSuccess: bFeedback, onError: bFeedback }, ownerB)); const stopB = b.subscribe(() => {});
    gate.resolve(new Response(outcome === 'success' ? '{"operation_id":"durable-A"}' : '{"error":"A failed"}', { status: outcome === 'success' ? 200 : 500 })); await pending;
    expect(aFeedback).not.toHaveBeenCalled(); expect(bFeedback).not.toHaveBeenCalled(); expect(b.getCurrentResult().isIdle).toBe(true); expect(b.getCurrentResult().error).toBeNull(); expect(b.getCurrentResult().data).toBeUndefined();
    fetchMock.mockResolvedValueOnce(new Response('{"operation_id":"durable-B"}')); expect(await b.mutate(undefined)).toEqual({ operation_id: 'durable-B' }); expect(fetchMock.mock.calls[1][0]).toBe('/api/repos/B/import'); expect(bFeedback).toHaveBeenCalledTimes(1); stopA(); stopB();
  });
});
