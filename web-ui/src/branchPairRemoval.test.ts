import { accountStorageKey, clientSession } from './session';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import ManagedRemovalPanel from './components/ManagedRemovalPanel';
import { api } from './api';
import { readPendingRemovalReview, retainPendingRemovalReview, reviewedRemovalReady, reviewedRemovalDisplay, removalDependencyPreviewConfirmReady, readManagedRemovalReceipt, type ReviewedRemovalPreview, type ManagedRemovalStatus } from './managedRemoval';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  branchPairRemovalOutcome,
  clearPersistedBranchPairRemovalNotice,
  isNotFoundError,
  persistBranchPairRemovalNotice,
  readBranchPairRemovalNotice,
  readPersistedBranchPairRemovalNotice,
  removalDestination,
  repoDetailQueryKeys,
} from './branchPairRemoval';
import type { BranchPairRemovalNotice } from './branchPairRemoval';

describe('branchPairRemovalOutcome', () => {
  it.each(['queued', 'running', 'cancelling', 'canceling', 'accepted', 'pending', 'in_progress'])(
    'keeps %s operations unfinished even with ok: true',
    (state) => {
      expect(branchPairRemovalOutcome({ ok: true, state: ` ${state.toUpperCase()} ` })).toBe('in_progress');
      expect(branchPairRemovalOutcome({ ok: true, lifecycle: state })).toBe('in_progress');
    },
  );

  it('requires completion confirmation rather than an operation id or HTTP acceptance', () => {
    expect(branchPairRemovalOutcome({ operation_id: 'op-70' })).toBe('in_progress');
    expect(branchPairRemovalOutcome({ ok: false, operation_id: 'op-70' })).toBe('in_progress');
    expect(branchPairRemovalOutcome({ ok: true, operation_id: 'op-70', state: 'completed' })).toBe('completed');
    expect(branchPairRemovalOutcome({ ok: true, warnings: ['remote cleanup failed'] })).toBe('completed');
    expect(branchPairRemovalOutcome({})).toBe('rejected');
    expect(branchPairRemovalOutcome({ ok: false, state: 'failed' })).toBe('rejected');
    expect(branchPairRemovalOutcome({ state: 'completed' })).toBe('rejected');
  });
});

describe('removalDestination', () => {
  const viewed = { viewedId: 'child', targetId: 'child', parentId: 'parent', parentExists: true };

  it('keeps a surviving parent in place when deleting one of its children', () => {
    expect(removalDestination({ ...viewed, viewedId: 'parent' })).toBeNull();
    expect(removalDestination({ ...viewed, viewedId: 'parent', parentExists: false })).toBeNull();
  });

  it('uses the captured parent unless its absence is confirmed', () => {
    expect(removalDestination(viewed)).toBe('/repos/parent');
    expect(removalDestination({ ...viewed, parentExists: null })).toBe('/repos/parent');
    expect(removalDestination({ ...viewed, parentExists: false })).toBe('/repos');
    expect(removalDestination({ ...viewed, parentId: null })).toBe('/repos');
  });
});

describe('detail retirement and not-found classification', () => {
  it('includes every detail query and scopes cancellation to the removed pair', () => {
    expect(repoDetailQueryKeys('child')).toEqual([
      ['repo', 'child'], ['status', 'child'], ['sync-records', 'child'],
      ['commit-map', 'child'], ['audit', 'child'], ['repo-credentials', 'child'],
      ['branch-pairs', 'child'], ['import-status', 'child'],
    ]);
    expect(repoDetailQueryKeys('parent').every((key) => key[1] === 'parent')).toBe(true);
  });

  it.each([new Error('HTTP 404: missing'), 'repository not found', new Error('Repository Not Found')])(
    'recognizes a missing repository: %s', (error) => expect(isNotFoundError(error)).toBe(true),
  );

  it.each([null, undefined, '', new Error('HTTP 500'), 'HTTP 401: admin access required', '1404']) (
    'does not disguise other failures as absence: %s', (error) => expect(isNotFoundError(error)).toBe(false),
  );
});

describe('branch-pair removal notices', () => {
  let key: string;
  const notice: BranchPairRemovalNotice = {
    outcome: 'completed', message: 'Registration removed', warnings: ['Git branch cleanup failed'],
    operationId: 'op-70', name: 'Child pair',
  };

  beforeEach(() => {
    vi.stubGlobal('localStorage', {getItem: () => null});
    key = accountStorageKey('reposync.branchPairRemovalReceipt');
    const values = new Map<string, string>();
    vi.stubGlobal('sessionStorage', {
      getItem: vi.fn((item: string) => values.get(item) ?? null),
      setItem: vi.fn((item: string, value: string) => values.set(item, value)),
      removeItem: vi.fn((item: string) => values.delete(item)),
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it.each(['completed', 'in_progress'] as const)('preserves %s warnings and operation details through navigation and storage', (outcome) => {
    const receipt = { ...notice, outcome };
    expect(readBranchPairRemovalNotice({ branchPairRemoval: receipt })).toEqual(receipt);
    persistBranchPairRemovalNotice(receipt);
    expect(readPersistedBranchPairRemovalNotice()).toEqual(receipt);
  });

  it.each([null, undefined, 'invalid', {}, { branchPairRemoval: null }, { branchPairRemoval: { outcome: 'failed' } }])(
    'ignores unrelated or invalid route state: %s', (state) => expect(readBranchPairRemovalNotice(state)).toBeNull(),
  );

  it('normalizes missing message and non-array warnings without inventing completion', () => {
    const state = { branchPairRemoval: { outcome: 'in_progress', warnings: 'invalid' } };
    expect(readBranchPairRemovalNotice(state)).toEqual({
      outcome: 'in_progress', message: '', warnings: [], operationId: undefined, name: undefined,
    });
    sessionStorage.setItem(key, JSON.stringify(state.branchPairRemoval));
    expect(readPersistedBranchPairRemovalNotice()).toEqual(readBranchPairRemovalNotice(state));
  });

  it.each(['invalid JSON', 'null', '{}', '{"outcome":"failed"}'])(
    'ignores an invalid persisted receipt: %s', (raw) => {
      sessionStorage.setItem(key, raw);
      expect(readPersistedBranchPairRemovalNotice()).toBeNull();
    },
  );

  it('clears only the removal receipt', () => {
    expect(readPersistedBranchPairRemovalNotice()).toBeNull();
    sessionStorage.setItem('unrelated', 'keep');
    persistBranchPairRemovalNotice(notice);
    clearPersistedBranchPairRemovalNotice();
    expect(readPersistedBranchPairRemovalNotice()).toBeNull();
    expect(sessionStorage.getItem('unrelated')).toBe('keep');
  });

  it('tolerates unavailable session storage', () => {
    const unavailable = () => { throw new Error('storage unavailable'); };
    vi.stubGlobal('sessionStorage', { getItem: unavailable, setItem: unavailable, removeItem: unavailable });
    expect(() => persistBranchPairRemovalNotice(notice)).not.toThrow();
    expect(readPersistedBranchPairRemovalNotice()).toBeNull();
    expect(() => clearPersistedBranchPairRemovalNotice()).not.toThrow();
  });
});

describe('reviewed removal request ownership and recovery', () => {
  const review = (): ReviewedRemovalPreview => ({ protocol: 2, repo_id: 'child', review_pin: 'a'.repeat(64), delete_git: true, delete_svn: false,
    git: { remote_url: 'https://git.example.invalid/repo.git', reference: 'refs/heads/child', expected_sha: 'b'.repeat(40) }, svn: null,
    dependency_preview: { repo_id: 'child', repo_name: 'Child', parent: { id: 'parent', name: 'Parent' }, children: [], parent_removal_blocked: false, block_reason: null, credentials: [], managed_local_path: 'repos/child', sibling_local_paths_preserved: [], shared_git_registrations: [] }, retention: 'Indefinite retained evidence' });
  let values: Map<string, string>;
  beforeEach(() => {
    values = new Map([['session_token', 'synthetic-a'], ['user', JSON.stringify({ id: 'account-a', role: 'admin' })]]);
    vi.stubGlobal('localStorage', { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key), key: (i: number) => [...values.keys()][i] ?? null, get length() { return values.size; } });
    vi.stubGlobal('sessionStorage', { getItem: () => null, removeItem: () => undefined });
    clientSession().snapshot();
  });
  afterEach(() => vi.unstubAllGlobals());

  it('requires available dependencies and the exact reviewed target and choices', () => {
    expect(removalDependencyPreviewConfirmReady({ isLoading: false, isFetching: false, isError: true })).toBe(false);
    expect(reviewedRemovalReady(review(), 'child', { delete_git: true, delete_svn: false }, false)).toBe(true);
    expect(reviewedRemovalReady(review(), 'other', { delete_git: true, delete_svn: false }, false)).toBe(false);
    expect(reviewedRemovalReady(review(), 'child', { delete_git: false, delete_svn: false }, false)).toBe(false);
    expect(reviewedRemovalReady(review(), 'child', { delete_git: true, delete_svn: false }, true)).toBe(false);
  });

  it('retains the original request after a lost reply and recovers the same idempotency identity', async () => {
    const send = vi.fn().mockRejectedValueOnce(new Error('lost response')).mockResolvedValueOnce(new Response(JSON.stringify({ ok: true, action: 'managed_remove', state: 'completed', operation_id: 'server-operation', protocol: 2, message: 'Removed', remote_git: 'deleted', remote_svn: 'untouched', restore_supported: true, retryable: false, registration_listed: false }), { status: 200 }));
    vi.stubGlobal('fetch', send);
    await expect(api.approveManagedRemoval('child', review())).rejects.toThrow('lost response');
    expect(readPendingRemovalReview('child')?.review_pin).toBe(review().review_pin);
    await expect(api.approveManagedRemoval('child', { ...review(), review_pin: 'c'.repeat(64) })).rejects.toThrow('Resolve the retained');
    expect(send).toHaveBeenCalledTimes(1);
    await api.approveManagedRemoval('child', readPendingRemovalReview('child')!);
    expect(send).toHaveBeenCalledTimes(2);
    const bodies = send.mock.calls.map(([, init]) => JSON.parse(init.body));
    expect(bodies[0]).toEqual(bodies[1]);
    expect(readPendingRemovalReview('child')).toBeNull();
    expect(readManagedRemovalReceipt('child')?.operationId).toBe('server-operation');
  });

  it('does not adopt another account or repository pending request', () => {
    retainPendingRemovalReview('child', review());
    expect(readPendingRemovalReview('other')).toBeNull();
    localStorage.setItem('session_token', 'synthetic-b'); localStorage.setItem('user', JSON.stringify({ id: 'account-b', role: 'admin' }));
    expect(readPendingRemovalReview('child')).toBeNull();
    localStorage.setItem('session_token', 'synthetic-a'); localStorage.setItem('user', JSON.stringify({ id: 'account-a', role: 'admin' }));
    expect(readPendingRemovalReview('child')?.review_pin).toBe(review().review_pin);
  });

  it('retires only an authoritatively rejected request, allowing a fresh review', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ ok: false, state: 'blocked', request_rejected: true, error: 'stale review' }), { status: 409 })));
    await expect(api.approveManagedRemoval('child', review())).rejects.toThrow('stale review');
    expect(readPendingRemovalReview('child')).toBeNull();
    retainPendingRemovalReview('child', { ...review(), review_pin: 'd'.repeat(64) });
    expect(readPendingRemovalReview('child')?.review_pin).toBe('d'.repeat(64));
  });

  it('does not send after account replacement during asynchronous journal loading', async () => {
    const send = vi.fn(); vi.stubGlobal('fetch', send);
    const pending = api.approveManagedRemoval('child', review());
    localStorage.setItem('session_token', 'replacement'); localStorage.setItem('user', JSON.stringify({ id: 'replacement', role: 'admin' }));
    await expect(pending).rejects.toThrow('login session changed');
    expect(send).not.toHaveBeenCalled(); expect(readPendingRemovalReview('child')).toBeNull();
  });

  it('rechecks the original view at the actual send boundary', async () => {
    const send = vi.fn(); vi.stubGlobal('fetch', send); let active = true;
    const pending = api.approveManagedRemoval('child', review(), { assertCurrent() { if (!active) throw new Error('retired route'); } });
    active = false; await expect(pending).rejects.toThrow('retired route'); expect(send).not.toHaveBeenCalled();
  });

  it('fails before sending when the pending request cannot be retained', async () => {
    const send = vi.fn(); vi.stubGlobal('fetch', send);
    vi.stubGlobal('localStorage', { getItem: (key: string) => values.get(key) ?? null, setItem() { throw new Error('storage full'); }, removeItem() {} });
    await expect(api.approveManagedRemoval('child', review())).rejects.toThrow('storage full'); expect(send).not.toHaveBeenCalled();
  });

  it('retains the original approval when its returned receipt cannot be stored', async () => {
    const send = vi.fn().mockImplementation(async () => {
      vi.stubGlobal('localStorage', { getItem: (key: string) => values.get(key) ?? null,
        setItem() { throw new Error('receipt storage full'); }, removeItem: (key: string) => values.delete(key) });
      return new Response(JSON.stringify({ ok: true, action: 'managed_remove', state: 'completed', operation_id: 'accepted-operation', protocol: 2,
        message: 'Removed', remote_git: 'deleted', remote_svn: 'untouched', restore_supported: true, retryable: false, registration_listed: false }), { status: 200 });
    });
    vi.stubGlobal('fetch', send);
    await expect(api.approveManagedRemoval('child', review())).rejects.toThrow('receipt storage full');
    expect(send).toHaveBeenCalledTimes(1);
    expect(readPendingRemovalReview('child')?.review_pin).toBe(review().review_pin);
    expect(readManagedRemovalReceipt('child')).toBeNull();
  });
});

// Captured from the exact original 3794-byte panel and original helper Git objects at 9afd72913bd83eb5b445e3a1c459626a1918c0ba.
const LEGACY_PANEL_VECTORS: Array<{ status: ManagedRemovalStatus; pending: boolean; html: string }> = [
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "queued",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "untouched",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "untouched",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      }
    },
    "pending": false,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"queued\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-amber-900/30 border-amber-700 text-amber-100\" role=\"status\"><p class=\"font-medium\">Removal queued</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: untouched · SVN: untouched. Managed removal does not delete remotes unless you separately authorize branch deletion.</p><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retry local cleanup</button></div>"
  },
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "queued",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "untouched",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "untouched",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      }
    },
    "pending": true,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"queued\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-amber-900/30 border-amber-700 text-amber-100\" role=\"status\"><p class=\"font-medium\">Removal queued</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: untouched · SVN: untouched. Managed removal does not delete remotes unless you separately authorize branch deletion.</p><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" disabled=\"\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retrying local cleanup…</button></div>"
  },
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "cancelling",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "untouched",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "untouched",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      }
    },
    "pending": false,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"cancelling\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-amber-900/30 border-amber-700 text-amber-100\" role=\"status\"><p class=\"font-medium\">Cancelling in-flight work</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: untouched · SVN: untouched. Managed removal does not delete remotes unless you separately authorize branch deletion.</p><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retry local cleanup</button></div>"
  },
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "cancelling",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "untouched",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "untouched",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      }
    },
    "pending": true,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"cancelling\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-amber-900/30 border-amber-700 text-amber-100\" role=\"status\"><p class=\"font-medium\">Cancelling in-flight work</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: untouched · SVN: untouched. Managed removal does not delete remotes unless you separately authorize branch deletion.</p><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" disabled=\"\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retrying local cleanup…</button></div>"
  },
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "running",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "untouched",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "untouched",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      }
    },
    "pending": false,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"running\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-amber-900/30 border-amber-700 text-amber-100\" role=\"status\"><p class=\"font-medium\">Cleaning owned local data</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: untouched · SVN: untouched. Managed removal does not delete remotes unless you separately authorize branch deletion.</p><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retry local cleanup</button></div>"
  },
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "running",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "untouched",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "untouched",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      }
    },
    "pending": true,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"running\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-amber-900/30 border-amber-700 text-amber-100\" role=\"status\"><p class=\"font-medium\">Cleaning owned local data</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: untouched · SVN: untouched. Managed removal does not delete remotes unless you separately authorize branch deletion.</p><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" disabled=\"\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retrying local cleanup…</button></div>"
  },
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "failed",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "deleted",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "deleted",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      },
      "partial_cleanup": {
        "outcome_detail": "Original cleanup failure",
        "registration_listed": true,
        "remote_git": "deleted",
        "remote_svn": "untouched",
        "retry_is_local_cleanup_only": true
      }
    },
    "pending": false,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"failed\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-red-900/30 border-red-700 text-red-100\" role=\"status\"><p class=\"font-medium\">Local cleanup failed</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: deleted · SVN: untouched. Retry only retries owned local cleanup; recorded remote outcomes are kept.</p><div data-testid=\"managed-removal-partial-cleanup\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Partial cleanup receipt</p><p>Original cleanup failure</p><p>Registration still listed: yes</p></div><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retry local cleanup</button></div>"
  },
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "failed",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "deleted",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "deleted",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      },
      "partial_cleanup": {
        "outcome_detail": "Original cleanup failure",
        "registration_listed": true,
        "remote_git": "deleted",
        "remote_svn": "untouched",
        "retry_is_local_cleanup_only": true
      }
    },
    "pending": true,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"failed\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-red-900/30 border-red-700 text-red-100\" role=\"status\"><p class=\"font-medium\">Local cleanup failed</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: deleted · SVN: untouched. Retry only retries owned local cleanup; recorded remote outcomes are kept.</p><div data-testid=\"managed-removal-partial-cleanup\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Partial cleanup receipt</p><p>Original cleanup failure</p><p>Registration still listed: yes</p></div><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" disabled=\"\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retrying local cleanup…</button></div>"
  },
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "reconciliation_required",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "deleted",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "deleted",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      },
      "partial_cleanup": {
        "outcome_detail": "Original cleanup failure",
        "registration_listed": true,
        "remote_git": "deleted",
        "remote_svn": "untouched",
        "retry_is_local_cleanup_only": true
      }
    },
    "pending": false,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"reconciliation_required\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-red-900/30 border-red-700 text-red-100\" role=\"status\"><p class=\"font-medium\">Blocked by unresolved external effect</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: deleted · SVN: untouched. Retry only retries owned local cleanup; recorded remote outcomes are kept.</p><div data-testid=\"managed-removal-partial-cleanup\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Partial cleanup receipt</p><p>Original cleanup failure</p><p>Registration still listed: yes</p></div><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retry local cleanup</button></div>"
  },
  {
    "status": {
      "ok": false,
      "action": "managed_remove",
      "state": "reconciliation_required",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "deleted",
      "remote_svn": "untouched",
      "restore_supported": false,
      "retryable": true,
      "registration_listed": true,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "deleted",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      },
      "partial_cleanup": {
        "outcome_detail": "Original cleanup failure",
        "registration_listed": true,
        "remote_git": "deleted",
        "remote_svn": "untouched",
        "retry_is_local_cleanup_only": true
      }
    },
    "pending": true,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"reconciliation_required\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-red-900/30 border-red-700 text-red-100\" role=\"status\"><p class=\"font-medium\">Blocked by unresolved external effect</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: deleted · SVN: untouched. Retry only retries owned local cleanup; recorded remote outcomes are kept.</p><div data-testid=\"managed-removal-partial-cleanup\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Partial cleanup receipt</p><p>Original cleanup failure</p><p>Registration still listed: yes</p></div><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p></div><button type=\"button\" data-testid=\"managed-removal-retry\" disabled=\"\" class=\"mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium\">Retrying local cleanup…</button></div>"
  },
  {
    "status": {
      "ok": true,
      "action": "managed_remove",
      "state": "completed",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "untouched",
      "remote_svn": "untouched",
      "restore_supported": true,
      "retryable": false,
      "registration_listed": false,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "untouched",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      }
    },
    "pending": false,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"completed\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-blue-900/30 border-blue-700 text-blue-100\" role=\"status\"><p class=\"font-medium\">Removed from RepoSync</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: untouched · SVN: untouched. Managed removal does not delete remotes unless you separately authorize branch deletion.</p><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p><button type=\"button\" data-testid=\"managed-removal-restore\" class=\"mt-2 px-3 py-1.5 rounded-md bg-blue-800 hover:bg-blue-700 disabled:opacity-50 text-xs font-medium\">Restore registration (stays disabled)</button></div></div>"
  },
  {
    "status": {
      "ok": true,
      "action": "managed_remove",
      "state": "completed",
      "operation_id": "legacy-operation",
      "message": "Retained original outcome",
      "remote_git": "untouched",
      "remote_svn": "untouched",
      "restore_supported": true,
      "retryable": false,
      "registration_listed": false,
      "recovery": {
        "operation_id": "legacy-operation",
        "name": "Legacy repository",
        "last_svn_rev": 4,
        "last_git_sha": "1234567890abcdef",
        "commit_map_count": 3,
        "remote_git": "untouched",
        "remote_svn": "untouched",
        "restore_supported": true,
        "retention": "retained legacy recovery"
      }
    },
    "pending": true,
    "html": "<div data-testid=\"managed-removal-panel\" data-state=\"completed\" data-operation-id=\"legacy-operation\" class=\"border rounded-lg p-4 text-sm space-y-2 bg-blue-900/30 border-blue-700 text-blue-100\" role=\"status\"><p class=\"font-medium\">Removed from RepoSync</p><p>Retained original outcome</p><p class=\"font-mono text-xs\">Operation legacy-operation</p><p class=\"text-xs\" data-testid=\"managed-removal-remote-outcome\">Remote Git: untouched · SVN: untouched. Managed removal does not delete remotes unless you separately authorize branch deletion.</p><div data-testid=\"managed-removal-recovery\" class=\"text-xs border border-current/30 rounded p-2 space-y-1\"><p class=\"font-medium\">Recovery metadata (restore available)</p><p>Legacy repository: maps=3, tip r4 / 12345678</p><p>retained legacy recovery</p><button type=\"button\" data-testid=\"managed-removal-restore\" disabled=\"\" class=\"mt-2 px-3 py-1.5 rounded-md bg-blue-800 hover:bg-blue-700 disabled:opacity-50 text-xs font-medium\">Restoring registration…</button></div></div>"
  }
];

describe('removal panel preserves legacy output and separates reviewed stages', () => {
  it.each(LEGACY_PANEL_VECTORS)('preserves the complete legacy rendered result: $status.state / pending=$pending', ({ status, pending, html }) => {
    expect(renderToStaticMarkup(React.createElement(ManagedRemovalPanel, { status, onRetry() {}, onRestore() {}, retryPending: pending, restorePending: pending }))).toBe(html);
  });
  const reviewed = (): ManagedRemovalStatus => ({ ok: false, action: 'managed_remove', protocol: 2, state: 'queued', operation_id: 'reviewed-operation', message: 'Original reviewed operation retained', remote_git: 'pending', remote_svn: 'untouched', restore_supported: false, retryable: true, registration_listed: true,
    reviewed: { git: { phase: 'prepared', errors: [] }, svn: { phase: 'not_requested', errors: [] }, cleanup_started: false, cleanup_completed: false, cleanup_errors: [] } });
  it('does not call pending remote work a local-cleanup retry', () => {
    const status = reviewed(); const html = renderToStaticMarkup(React.createElement(ManagedRemovalPanel, { status, onRetry() {} }));
    expect(html).toContain('Approved target; deletion not started'); expect(html).toContain('Owned local cleanup: Not started');
    expect(html).toContain('Continue original reviewed operation'); expect(html).not.toContain('Retry owned local cleanup');
  });
  it('shows cleanup-only continuation only with consistent verified remote receipts and retains ordered errors', () => {
    const status = reviewed(); status.state = 'failed'; status.remote_git = 'deleted'; status.reviewed!.git.phase = 'verified_deleted'; status.reviewed!.cleanup_started = true; status.reviewed!.cleanup_errors = ['first original cause', 'second cleanup cause'];
    const html = renderToStaticMarkup(React.createElement(ManagedRemovalPanel, { status, onRetry() {} }));
    expect(reviewedRemovalDisplay(status).remoteDone).toBe(true); expect(html).toContain('Retry owned local cleanup');
    expect(html.indexOf('first original cause')).toBeLessThan(html.indexOf('second cleanup cause'));
    expect(html).toContain('Registration still listed: yes');
  });
  it.each(['future_phase', 'reconciliation_required', 'intent_recorded'])('never invents completion from %s', phase => {
    const status = reviewed(); status.reviewed!.git.phase = phase; status.remote_git = phase === 'future_phase' ? 'deleted' : 'reconciliation_required';
    const html = renderToStaticMarkup(React.createElement(ManagedRemovalPanel, { status, onRetry() {} }));
    expect(reviewedRemovalDisplay(status).remoteDone).toBe(false); expect(html).not.toContain('Retry owned local cleanup');
  });
  it('refuses contradictory completion or missing cleanup evidence', () => {
    const status = reviewed(); status.state = 'completed'; status.ok = true;
    expect(reviewedRemovalDisplay(status).known).toBe(false);
    status.state = 'queued'; status.ok = false; delete status.reviewed!.cleanup_errors;
    expect(reviewedRemovalDisplay(status).known).toBe(false);
  });
});
