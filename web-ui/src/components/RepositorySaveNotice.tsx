import { repositorySaveConflict, type RepositorySaveAttempt } from '../repositorySave';
import { Link } from 'react-router-dom';

export default function RepositorySaveNotice({ attempt, error, pending, onLoadCurrent, onUseReviewed, reviewed }: {
  attempt: RepositorySaveAttempt | null; error: string | null; pending: boolean;
  onLoadCurrent?:()=>void; onUseReviewed?:()=>void; reviewed?:boolean;
}) {
  if (!attempt && !error && !pending) return null;
  return <div className="rounded border border-yellow-700 bg-yellow-900/20 p-3 mb-4 text-sm text-gray-200" aria-live="polite">
    {attempt && <>
      <p>Recorded configuration stage: {attempt.configurationStage}. Credential stage: {attempt.credentialStage}.</p>
      {attempt.repoId && <p>{attempt.protocolFailure ? 'Unconfirmed response repository ID:' : 'Existing repository:'} <Link className="text-blue-300 underline" to={`/repos/${encodeURIComponent(attempt.repoId)}`}>{attempt.repoId}</Link>. This ID is retained; no new registration is silently created.</p>}
      {!!attempt.priorAttempts?.length && <details><summary>Earlier save outcomes retained ({attempt.priorAttempts.length})</summary>
        {attempt.priorAttempts.map(previous=><p key={previous.configRequestId}>Configuration request {previous.configRequestId}: {previous.configurationStage}; credential request {previous.credentialRequestId}: {previous.credentialStage}. Earlier unknown outcomes are not relabeled as rolled back.</p>)}
      </details>}
      <p>Resume confirms the earlier submitted snapshot; newer edits are not included until a new save. Closing or navigating away keeps this recovery record; it does not undo a saved configuration.</p>
      {attempt.credentialFields.length>0 && <p>Required credential fields: {attempt.credentialFields.join(', ')}. Secrets are kept only in memory; after reopening, re-enter them only if the previous save is not confirmed.</p>}
    </>}
    {attempt && repositorySaveConflict(attempt) && attempt.repoId && <div className="mt-2 space-y-2">
      <p>Resume cannot bypass this conflict. Review current settings on the retained ID before a deliberate new save. Earlier receipts and unknown outcomes remain recorded; no rollback is assumed. Re-enter required credentials only if you intend a new credential write.</p>
      <button type="button" disabled={pending} onClick={onLoadCurrent} className="underline text-blue-300">Load current settings for review</button>
      {reviewed && <button type="button" disabled={pending} onClick={onUseReviewed} className="block underline text-blue-300">Use these reviewed settings for a new save on this ID</button>}
    </div>}
    {pending && <p>Saving in order: configuration, then credentials. Neither stage is reported complete before confirmation.</p>}
    {error && <p role="alert" className="text-red-300 mt-2">{error}</p>}
  </div>;
}
