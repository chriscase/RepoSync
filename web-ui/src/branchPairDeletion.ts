/** Query string for branch-pair DELETE — shared contract for UI and tests. */

export interface BranchPairRemoteDeletionOpts {
  delete_git?: boolean;
  delete_svn?: boolean;
}

export function buildBranchPairDeleteQuery(
  opts?: BranchPairRemoteDeletionOpts,
): URLSearchParams {
  const params = new URLSearchParams();
  params.set('explicit_remote_deletion_opts', 'true');
  params.set('delete_git', String(opts?.delete_git ?? false));
  params.set('delete_svn', String(opts?.delete_svn ?? false));
  return params;
}
