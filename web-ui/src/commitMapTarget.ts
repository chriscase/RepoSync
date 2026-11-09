import type { CommitMapEntry, CommitMapTargetState } from './api';

/** What a commit-map row shows in the Git SHA column, in words and for a tooltip. */
export interface CommitMapTargetDisplay {
  kind: CommitMapTargetState | 'unknown';
  text: string;
  title: string;
  mono: boolean;
}

const NO_TARGET_STATES: Record<Exclude<CommitMapTargetState, 'git_commit'>, Omit<CommitMapTargetDisplay, 'kind'>> = {
  filtered_no_target: {
    text: 'No Git commit (filtered)',
    title: 'This SVN revision was recorded as producing no Git commit, and RepoSync has a matching filtered no-target outcome.',
    mono: false,
  },
  no_target_unverified: {
    text: 'No Git target (unverified)',
    title: 'No Git SHA is recorded and no matching filtered no-target outcome was found. This row is not treated as synced.',
    mono: false,
  },
  legacy_empty_value: {
    text: 'Unknown (empty legacy value)',
    title: 'An older version stored an empty Git SHA here. It is neither a commit nor a no-target proof.',
    mono: false,
  },
};

/**
 * Render the Git side of a commit-map row. A row may have no Git SHA (#54);
 * the server says why in `target_state`. Never slices a missing SHA.
 */
export function commitMapTargetDisplay(entry: Pick<CommitMapEntry, 'git_sha' | 'target_state'>): CommitMapTargetDisplay {
  const sha = typeof entry.git_sha === 'string' ? entry.git_sha : null;
  const state = entry.target_state;
  if (state && state !== 'git_commit' && state in NO_TARGET_STATES) {
    return { kind: state, ...NO_TARGET_STATES[state] };
  }
  if (sha && (state === 'git_commit' || state === undefined)) {
    return { kind: 'git_commit', text: sha.substring(0, 12), title: sha, mono: true };
  }
  return {
    kind: 'unknown',
    text: 'Unknown',
    title: 'The server did not report a usable Git target for this row.',
    mono: false,
  };
}
