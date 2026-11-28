import { describe, expect, it } from 'vitest';
import { syncReplyLabel } from './RepoDetail';

describe('Sync Now reply label', () => {
  it('says triggered only when the request was queued', () => {
    expect(syncReplyLabel({ queued: true })).toBe('Sync Triggered');
    expect(syncReplyLabel({})).toBe('Sync Triggered');
  });
  it('names the wait when a requested sync just started', () => {
    expect(syncReplyLabel({ queued: false, retry_after_secs: 12 })).toBe('Sync just started, wait 12s');
  });
  it('does not claim a sync for a pair that was not queued', () => {
    expect(syncReplyLabel({ queued: false, retry_after_secs: null })).toBe('Not queued');
  });
});
