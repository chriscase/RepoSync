import { describe, expect, it } from 'vitest';
import { commitMapTargetDisplay } from './commitMapTarget';

const sha = 'a'.repeat(40);

describe('#54 commit-map target display', () => {
  it('shows a short SHA for a Git commit', () => {
    expect(commitMapTargetDisplay({ git_sha: sha, target_state: 'git_commit' })).toEqual({
      kind: 'git_commit',
      text: 'aaaaaaaaaaaa',
      title: sha,
      mono: true,
    });
  });

  it.each([
    ['filtered_no_target', 'No Git commit (filtered)'],
    ['no_target_unverified', 'No Git target (unverified)'],
  ] as const)('renders %s in words for a null SHA', (state, text) => {
    const display = commitMapTargetDisplay({ git_sha: null, target_state: state });
    expect(display.kind).toBe(state);
    expect(display.text).toBe(text);
    expect(display.mono).toBe(false);
  });

  it('labels a historical empty-string SHA instead of showing a blank cell', () => {
    const display = commitMapTargetDisplay({ git_sha: '', target_state: 'legacy_empty_value' });
    expect(display.kind).toBe('legacy_empty_value');
    expect(display.text).toBe('Unknown (empty legacy value)');
  });

  it('never claims a commit when the state is missing and the SHA is null', () => {
    expect(commitMapTargetDisplay({ git_sha: null }).kind).toBe('unknown');
    expect(commitMapTargetDisplay({ git_sha: null, target_state: 'git_commit' }).kind).toBe('unknown');
  });

  it('keeps showing SHAs from an older server that sends no target_state', () => {
    expect(commitMapTargetDisplay({ git_sha: sha }).text).toBe('aaaaaaaaaaaa');
  });

  it('does not let a no-target state be overridden by a stray SHA', () => {
    expect(commitMapTargetDisplay({ git_sha: sha, target_state: 'no_target_unverified' }).kind).toBe('no_target_unverified');
  });
});
