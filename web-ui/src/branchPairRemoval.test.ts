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
  const key = 'reposync.branchPairRemovalReceipt';
  const notice: BranchPairRemovalNotice = {
    outcome: 'completed', message: 'Registration removed', warnings: ['Git branch cleanup failed'],
    operationId: 'op-70', name: 'Child pair',
  };

  beforeEach(() => {
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
