import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ImportStatus } from './api';
import { watchSetupImport } from './useSetupImport';

const idle: ImportStatus = {
  phase: 'idle', current_rev: 0, total_revs: 0, commits_created: 0, current_file_count: 0,
  lfs_unique_count: 0, files_skipped: 0, batches_pushed: 0, push_started_at: null,
  verification: null, errors: [], log_lines: [], started_at: null, completed_at: null,
  can_start: true, can_cancel: false, start_repository_id: 'repo-1',
};

function running(id: string): ImportStatus {
  return { ...idle, phase: 'importing', busy: true, lifecycle: 'running', operation_id: id,
    can_start: false, can_cancel: true };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}

function setup() {
  const client = {
    getImportStatus: vi.fn<(signal?: AbortSignal) => Promise<ImportStatus>>(),
    startImport: vi.fn<(id: string, signal?: AbortSignal) => Promise<{ ok: boolean; message: string; operation_id?: string }>>(),
    cancelImport: vi.fn<(id: string, signal?: AbortSignal) => Promise<{ ok: boolean; message: string; operation_id?: string }>>(),
  };
  const render = vi.fn<Parameters<typeof watchSetupImport>[1]>();
  return { client, render, view: () => render.mock.lastCall![0], mount: () => watchSetupImport(client, render) };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe('setup import observations and commands', () => {
  it('serializes reads, caps retry delay, and distrusts controls during a disconnect', async () => {
    const { client, view, mount } = setup();
    const pending = deferred<ImportStatus>();
    client.getImportStatus.mockResolvedValueOnce(running('A')).mockReturnValueOnce(pending.promise)
      .mockRejectedValue(new Error('offline'));
    const watcher = mount();
    await vi.advanceTimersByTimeAsync(7000);
    expect(client.getImportStatus).toHaveBeenCalledTimes(2);
    pending.resolve(running('A'));
    await vi.advanceTimersByTimeAsync(1500);
    expect(view().status?.operation_id).toBe('A');
    expect(view().statusError).toMatch(/offline/);
    await watcher.cancel('A');
    expect(client.cancelImport).not.toHaveBeenCalled();
    for (const delay of [3000, 6000, 12000, 12000]) {
      const count = client.getImportStatus.mock.calls.length;
      await vi.advanceTimersByTimeAsync(delay - 1);
      expect(client.getImportStatus).toHaveBeenCalledTimes(count);
      await vi.advanceTimersByTimeAsync(1);
      expect(client.getImportStatus).toHaveBeenCalledTimes(count + 1);
    }
    watcher.dispose();
  });

  it('rejects reads superseded by start and holds the old idle view until the new operation is observed', async () => {
    const { client, view, mount } = setup();
    const stale = deferred<ImportStatus>();
    client.getImportStatus.mockResolvedValueOnce(idle).mockReturnValueOnce(stale.promise)
      .mockResolvedValueOnce(idle).mockResolvedValue(running('new'));
    client.startImport.mockResolvedValue({ ok: true, operation_id: 'new', message: 'Queued' });
    const watcher = mount();
    await vi.advanceTimersByTimeAsync(1500);
    await watcher.start('previous-repository');
    expect(client.startImport).not.toHaveBeenCalled();
    await watcher.start('repo-1');
    expect(view().awaitingStatus).toBe(true);
    await watcher.start('repo-1');
    stale.resolve(running('old'));
    await vi.advanceTimersByTimeAsync(0);
    expect(view().status?.operation_id).toBeUndefined();
    expect(client.startImport).toHaveBeenCalledTimes(1);
    expect(client.startImport).toHaveBeenCalledWith('repo-1', expect.any(AbortSignal));
    await vi.advanceTimersByTimeAsync(1500);
    expect(view().status?.operation_id).toBe('new');
    expect(view().awaitingStatus).toBe(false);
    watcher.dispose();
  });

  it('pins the stop to the displayed id and accepts a newer operation without transferring the stop', async () => {
    const { client, view, mount } = setup();
    client.getImportStatus.mockResolvedValueOnce(running('A')).mockResolvedValueOnce(running('A'))
      .mockResolvedValue(running('B'));
    client.cancelImport.mockResolvedValue({ ok: true, operation_id: 'A', message: 'Stop requested' });
    const watcher = mount();
    await vi.advanceTimersByTimeAsync(0);
    await watcher.cancel('A');
    expect(view()).toMatchObject({ awaitingStatus: true, cancelling: true, actionMessage: 'Stop requested' });
    await watcher.cancel('A');
    await vi.advanceTimersByTimeAsync(1500);
    expect(view()).toMatchObject({ status: { operation_id: 'B' }, awaitingStatus: false, actionMessage: '' });
    await watcher.cancel('A');
    expect(client.cancelImport.mock.calls.map(([id]) => id)).toEqual(['A']);
    watcher.dispose();
  });

  it('requires the named capability and surfaces explicit refusals and unknown outcomes without replay', async () => {
    const { client, view, mount } = setup();
    client.getImportStatus.mockResolvedValueOnce({ ...running('A'), can_cancel: undefined })
      .mockResolvedValue(running('A'));
    client.cancelImport.mockResolvedValueOnce({ ok: false, message: 'Operation changed' })
      .mockRejectedValueOnce(new Error('offline'));
    const watcher = mount();
    await vi.advanceTimersByTimeAsync(0);
    await watcher.cancel('A');
    expect(client.cancelImport).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1500);
    await watcher.cancel('A');
    expect(view().actionError).toBe('Operation changed');
    await watcher.cancel('A');
    expect(view().actionError).toMatch(/Could not confirm.*offline/);
    await vi.advanceTimersByTimeAsync(12000);
    expect(client.cancelImport.mock.calls.map(([id]) => id)).toEqual(['A', 'A']);
    watcher.dispose();
  });

  it('aborts a read at deadline or disposal and ignores its late result', async () => {
    const { client, view, render, mount } = setup();
    const first = deferred<ImportStatus>();
    const second = deferred<ImportStatus>();
    client.getImportStatus.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const watcher = mount();
    const signals = client.getImportStatus.mock.calls;
    await vi.advanceTimersByTimeAsync(10000);
    expect(signals[0][0]?.aborted).toBe(true);
    expect(view().statusError).toMatch(/timed out/);
    await vi.advanceTimersByTimeAsync(3000);
    watcher.dispose();
    expect(signals[1][0]?.aborted).toBe(true);
    const count = render.mock.calls.length;
    first.resolve(idle);
    second.resolve(running('A'));
    await vi.advanceTimersByTimeAsync(60000);
    expect(render).toHaveBeenCalledTimes(count);
    expect(client.getImportStatus).toHaveBeenCalledTimes(2);
  });
  it('resumes reads after a hung POST and ignores a later acknowledgement', async () => {
    const { client, view, mount } = setup();
    const pending = deferred<{ ok: boolean; message: string; operation_id: string }>();
    client.getImportStatus.mockResolvedValueOnce(running('A')).mockResolvedValue(running('B'));
    client.cancelImport.mockReturnValueOnce(pending.promise);
    const watcher = mount();
    await vi.advanceTimersByTimeAsync(0);
    const action = watcher.cancel('A');
    await vi.advanceTimersByTimeAsync(10000);
    await action;
    expect(client.cancelImport.mock.calls[0][1]?.aborted).toBe(true);
    expect(view()).toMatchObject({ awaitingStatus: false, cancelling: false, status: { operation_id: 'B' } });
    expect(view().actionError).toMatch(/Could not confirm.*timed out/);
    pending.resolve({ ok: true, message: 'Stop requested', operation_id: 'A' });
    await vi.advanceTimersByTimeAsync(12000);
    expect(view().actionMessage).toBe('');
    expect(client.cancelImport).toHaveBeenCalledTimes(1);
    watcher.dispose();
  });

  it('surfaces a receipt without identity, then uses the current observation', async () => {
    const { client, view, mount } = setup();
    client.getImportStatus.mockResolvedValueOnce(idle).mockResolvedValue(running('started'));
    client.startImport.mockResolvedValueOnce({ ok: true, message: 'Accepted' });
    const watcher = mount();
    await vi.advanceTimersByTimeAsync(0);
    await watcher.start('repo-1');
    expect(view()).toMatchObject({ awaitingStatus: false, status: { operation_id: 'started' } });
    expect(view().actionError).toMatch(/Could not confirm the operation/);
    expect(view().actionMessage).toBe('');
    watcher.dispose();
  });

  it('reports an unconfirmed acknowledgement and keeps polling when the server returns only its old state', async () => {
    const { client, view, mount } = setup();
    client.getImportStatus.mockResolvedValue(idle);
    client.startImport.mockResolvedValueOnce({ ok: true, message: 'Accepted', operation_id: 'missing' });
    const watcher = mount();
    await vi.advanceTimersByTimeAsync(0);
    await watcher.start('repo-1');
    expect(view().awaitingStatus).toBe(true);
    await vi.advanceTimersByTimeAsync(3000);
    expect(view()).toMatchObject({ awaitingStatus: false, actionMessage: '', status: idle });
    expect(view().actionError).toMatch(/did not confirm/);
    expect(client.startImport).toHaveBeenCalledTimes(1);
    watcher.dispose();
  });

  it('drops the stop acknowledgement at terminal state and rejects a later retired operation snapshot', async () => {
    const { client, view, mount } = setup();
    const terminal = { ...running('A'), phase: 'cancelled' as const, lifecycle: 'cancelled' as const,
      busy: false, can_cancel: false };
    client.getImportStatus.mockResolvedValueOnce(running('A')).mockResolvedValueOnce({
      ...running('A'), lifecycle: 'cancel_requested', can_cancel: false,
    }).mockResolvedValueOnce(terminal).mockResolvedValueOnce(running('B')).mockResolvedValueOnce(running('A'));
    client.cancelImport.mockResolvedValue({ ok: true, message: 'Stop requested', operation_id: 'A' });
    const watcher = mount();
    await vi.advanceTimersByTimeAsync(0);
    await watcher.cancel('A');
    expect(view().actionMessage).toBe('Stop requested');
    await vi.advanceTimersByTimeAsync(1500);
    expect(view()).toMatchObject({ actionMessage: '', status: terminal });
    await vi.advanceTimersByTimeAsync(3000);
    expect(view().status?.operation_id).toBe('B');
    expect(view().statusError).toMatch(/earlier import/);
    watcher.dispose();
  });

  it('disposes a pending write and a remount observes the durable state independently', async () => {
    const { client, view, render, mount } = setup();
    const pending = deferred<{ ok: boolean; message: string; operation_id: string }>();
    client.getImportStatus.mockResolvedValueOnce(running('A')).mockResolvedValueOnce({
      ...running('A'), lifecycle: 'cancelling', can_cancel: false,
    });
    client.cancelImport.mockReturnValueOnce(pending.promise);
    const watcher = mount();
    await vi.advanceTimersByTimeAsync(0);
    const action = watcher.cancel('A');
    watcher.dispose();
    const before = render.mock.calls.length;
    await action;
    expect(client.cancelImport.mock.calls[0][1]?.aborted).toBe(true);
    pending.resolve({ ok: true, operation_id: 'A', message: 'Stop requested' });
    await vi.advanceTimersByTimeAsync(20000);
    expect(render).toHaveBeenCalledTimes(before);
    const reopened = mount();
    await vi.advanceTimersByTimeAsync(0);
    expect(view()).toMatchObject({ status: { operation_id: 'A', lifecycle: 'cancelling' }, actionMessage: '' });
    reopened.dispose();
  });

});
