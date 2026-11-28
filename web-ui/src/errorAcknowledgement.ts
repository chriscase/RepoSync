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

/** #264 G02.5: the prompt above is shown in the shared dialog; these are its other texts. */
export const acknowledgementTitle = 'Acknowledge these errors?';

export function acknowledgementConfirmLabel(count: number): string {
  return `Acknowledge ${count} ${count === 1 ? 'error' : 'errors'}`;
}

export interface AcknowledgementFeedback { error: boolean; text: string }

export function acknowledgedFeedback(acknowledged: number): AcknowledgementFeedback {
  return { error: false, text: `Acknowledged ${acknowledged}. Audit evidence preserved.` };
}

export function acknowledgementFailure(error: unknown): AcknowledgementFeedback {
  return { error: true, text: error instanceof Error ? error.message : 'Acknowledgement failed. Try again.' };
}
