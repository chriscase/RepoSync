import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from './api';
import { clientSession } from './session';
import { shouldRetireBranchPair } from './branchPairRemoval';
import { managedRemovalReceiptFromStatus, managedRemovalStatusFromReceipt, readPendingRemovalReview, retainPendingRemovalReview, reviewedRemovalReady, reviewedRemovalDisplay, type ManagedRemovalStatus, type ReviewedRemovalPreview } from './managedRemoval';
import ManagedRemovalPanel from './components/ManagedRemovalPanel';

const preview = (archive = true): ReviewedRemovalPreview => ({ protocol: 2, repo_id: 'child', review_pin: 'a'.repeat(64), archive_pair: archive, delete_git: true, delete_svn: false,
  git: { remote_url: 'https://git.example.invalid/repo.git', reference: 'refs/heads/child', expected_sha: 'b'.repeat(40) }, svn: null,
  dependency_preview: { repo_id: 'child', repo_name: 'Child', parent: { id: 'parent', name: 'Parent' }, children: [], parent_removal_blocked: false, block_reason: null, credentials: [], managed_local_path: 'repos/child', sibling_local_paths_preserved: [], shared_git_registrations: [] }, retention: 'Retained archive' });
const completed = (): ManagedRemovalStatus => ({ archive_pair: true, ok: true, action: 'managed_remove', protocol: 2, state: 'completed', operation_id: 'archive-op', message: 'Pair archived', remote_git: 'deleted', remote_svn: 'untouched', restore_supported: false, retryable: false, registration_listed: true,
  reviewed: { git: { phase: 'verified_deleted', errors: [] }, svn: { phase: 'not_requested', errors: [] }, cleanup_started: true, cleanup_completed: true, cleanup_errors: [] } });

describe('single-side pair archive', () => {
  beforeEach(() => {
    const values = new Map([['session_token', 'synthetic-archive'], ['user', JSON.stringify({ id: 'archive-admin', role: 'admin' })]]);
    vi.stubGlobal('localStorage', { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key), key: (i: number) => [...values.keys()][i] ?? null, get length() { return values.size; } });
    vi.stubGlobal('sessionStorage', { getItem: () => null, removeItem() {} });
    clientSession().snapshot();
  });
  afterEach(() => vi.unstubAllGlobals());

  it('pins archive disposition as well as both deletion choices', () => {
    const options = { archive_pair: true, delete_git: true, delete_svn: false };
    expect(reviewedRemovalReady(preview(), 'child', options, false)).toBe(true);
    expect(reviewedRemovalReady(preview(false), 'child', options, false)).toBe(false);
    expect(reviewedRemovalReady(preview(), 'child', { ...options, archive_pair: false }, false)).toBe(false);
    expect(reviewedRemovalReady({ ...preview(), delete_svn: true }, 'child', { ...options, delete_svn: true }, false)).toBe(false);
    expect(reviewedRemovalReady({ ...preview(), archive_pair: 'true' } as unknown as ReviewedRemovalPreview, 'child', options, false)).toBe(false);
  });

  it('does not reinterpret a retained legacy single-side removal as archival', async () => {
    const legacy = preview(false); delete legacy.archive_pair;
    retainPendingRemovalReview('child', legacy);
    const recovered = readPendingRemovalReview('child')!;
    expect(recovered.archive_pair).toBeUndefined();
    const send = vi.fn().mockRejectedValue(new Error('lost reply')); vi.stubGlobal('fetch', send);
    await expect(api.approveManagedRemoval('child', recovered)).rejects.toThrow('lost reply');
    expect(JSON.parse(send.mock.calls[0][1].body).archive_pair).toBe(false);
    expect(readPendingRemovalReview('child')?.archive_pair).toBeUndefined();
  });

  it('preserves the archive intent through lost response and same-request retry', async () => {
    const send = vi.fn().mockRejectedValueOnce(new Error('lost reply')).mockResolvedValueOnce(new Response(JSON.stringify(completed()), { status: 200 }));
    vi.stubGlobal('fetch', send);
    await expect(api.approveManagedRemoval('child', preview())).rejects.toThrow('lost reply');
    expect(readPendingRemovalReview('child')?.archive_pair).toBe(true);
    await api.approveManagedRemoval('child', readPendingRemovalReview('child')!);
    const bodies = send.mock.calls.map(([, init]) => JSON.parse(init.body));
    expect(bodies[0]).toEqual(bodies[1]); expect(bodies[0].archive_pair).toBe(true);
    expect(readPendingRemovalReview('child')).toBeNull();
  });

  it('keeps the retained pair page instead of navigating as if it were removed', () => {
    expect(shouldRetireBranchPair(completed())).toBe(false);
    expect(shouldRetireBranchPair({ ok: true, state: 'completed', registration_listed: true })).toBe(false);
    expect(shouldRetireBranchPair({ ok: true, state: 'completed', registration_listed: false })).toBe(true);
    expect(shouldRetireBranchPair({ ok: true, state: 'completed' })).toBe(true);
    expect(shouldRetireBranchPair({ ok: true, state: 'running', registration_listed: true })).toBe(false);
  });

  it.each([false, true])('renders retained evidence, never local deletion or restore (SVN deleted=%s)', svn => {
    const status = completed();
    if (svn) { [status.remote_git, status.remote_svn] = [status.remote_svn, status.remote_git]; [status.reviewed!.git, status.reviewed!.svn] = [status.reviewed!.svn, status.reviewed!.git]; }
    const recovered = managedRemovalStatusFromReceipt(managedRemovalReceiptFromStatus('child', status));
    expect(recovered.archive_pair).toBe(true);
    const html = renderToStaticMarkup(React.createElement(ManagedRemovalPanel, { status: recovered, onRestore() {} }));
    expect(html).toContain('Pair archived; survivor and history retained');
    expect(html).toContain('local data and history retained');
    expect(html).not.toContain('Owned local cleanup'); expect(html).not.toContain('managed-removal-restore');
    expect(reviewedRemovalDisplay({ ...status, registration_listed: false }).known).toBe(false);
    expect(reviewedRemovalDisplay({ ...status, restore_supported: true }).known).toBe(false);
  });
});
