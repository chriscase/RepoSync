export type ErrorScope = { kind: 'all' } | { kind: 'repository'; repo_id: string };

export interface ErrorObservation {
  through_id: string;
  observed_at: string;
}

export function errorScope(repoId: string | undefined): ErrorScope {
  return repoId === undefined ? { kind: 'all' } : { kind: 'repository', repo_id: repoId };
}

export function acknowledgementPrompt(scope: ErrorScope, repositoryName: string): string {
  const target = scope.kind === 'all' ? 'All Repositories (global scope)' : `repository “${repositoryName}”`;
  return `Acknowledge the displayed recent errors for ${target}? Original audit evidence will be preserved. New errors will remain visible.`;
}
