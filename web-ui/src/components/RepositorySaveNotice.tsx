import type { RepositorySaveAttempt } from '../repositorySave';
import { Link } from 'react-router-dom';

export default function RepositorySaveNotice({ attempt, error, pending }: {
  attempt: RepositorySaveAttempt | null; error: string | null; pending: boolean;
}) {
  if (!attempt && !error && !pending) return null;
  return <div className="rounded border border-yellow-700 bg-yellow-900/20 p-3 mb-4 text-sm text-gray-200" aria-live="polite">
    {attempt && <>
      <p>Recorded configuration stage: {attempt.configurationStage}. Credential stage: {attempt.credentialStage}.</p>
      {attempt.repoId && <p>{attempt.protocolFailure ? 'Unconfirmed response repository ID:' : 'Existing repository:'} <Link className="text-blue-300 underline" to={`/repos/${encodeURIComponent(attempt.repoId)}`}>{attempt.repoId}</Link>. This ID is retained; no new registration is silently created.</p>}
      <p>Resume confirms the earlier submitted snapshot; newer edits are not included until a new save. Closing or navigating away keeps this recovery record; it does not undo a saved configuration.</p>
      {attempt.credentialFields.length>0 && <p>Required credential fields: {attempt.credentialFields.join(', ')}. Secrets are kept only in memory; after reopening, re-enter them only if the previous save is not confirmed.</p>}
    </>}
    {pending && <p>Saving in order: configuration, then credentials. Neither stage is reported complete before confirmation.</p>}
    {error && <p role="alert" className="text-red-300 mt-2">{error}</p>}
  </div>;
}
