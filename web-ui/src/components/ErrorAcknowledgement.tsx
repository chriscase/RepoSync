import { useState } from 'react';
import { api } from '../api';
import Dialog from './Dialog';
import {
  acknowledgedFeedback, acknowledgementConfirmLabel, acknowledgementFailure, acknowledgementPrompt, acknowledgementTitle,
  type AcknowledgementFeedback, type ErrorObservation, type ErrorScope,
} from '../errorAcknowledgement';
import { getStoredUser } from '../utils/auth';

/**
 * #264 G02.5: the confirmation that used to be the browser prompt, in the shared dialog.
 * The description is the existing prompt, unchanged. Cancel is focused first.
 */
export function ErrorAcknowledgementDialog({ isOpen, scope, repositoryName, count, available, pending, error, onConfirm, onCancel }: {
  isOpen: boolean;
  scope: ErrorScope;
  repositoryName: string;
  /** The count shown when the dialog was opened: what the confirmation acknowledges. */
  count: number;
  /** False when the page no longer has a current error observation to acknowledge. */
  available: boolean;
  pending: boolean;
  error?: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return <Dialog isOpen={isOpen} title={acknowledgementTitle} description={acknowledgementPrompt(scope, repositoryName)}
    onClose={onCancel} busy={pending} className="rs-dlg">
    {error && <p role="alert" className="mb-3 text-sm text-red-300">{error}</p>}
    {!available && !pending && <p role="status" className="mb-3 text-sm text-yellow-300">
      The error status is no longer current, so nothing can be acknowledged now. Cancel and try again once it has been read.
    </p>}
    <div className="flex justify-end space-x-3">
      <button type="button" data-dialog-cancel disabled={pending} onClick={onCancel}
        className="rs-btn rs-btn--quiet px-4 py-2 rounded bg-gray-700 hover:bg-gray-600 text-gray-200">Cancel</button>
      <button type="button" disabled={pending || !available} onClick={onConfirm}
        className="rs-btn rs-btn--primary px-4 py-2 rounded rs-text-on-accent bg-blue-600 hover:bg-blue-700 disabled:opacity-50">
        {acknowledgementConfirmLabel(count)}
      </button>
    </div>
  </Dialog>;
}

export default function ErrorAcknowledgement({ scope, repositoryName, observation, count, onSuccess }: {
  scope: ErrorScope;
  repositoryName: string;
  observation: ErrorObservation | undefined;
  count: number;
  onSuccess: () => void;
}) {
  const [pending, setPending] = useState(false);
  const [feedback, setFeedback] = useState<AcknowledgementFeedback | null>(null);
  // The observation and count on screen when the person chose Acknowledge. The browser prompt
  // blocked the page, so the call always carried that observation; the dialog keeps it the same.
  const [confirming, setConfirming] = useState<{ observation: ErrorObservation; count: number } | null>(null);
  const [dialogError, setDialogError] = useState<string | null>(null);
  // Legacy password sessions do not store a user object. The server checks the
  // current role on every request; local storage is only a presentation hint.
  const user = getStoredUser();
  if (user && user.role !== 'admin') return null;
  const acknowledge = async () => {
    if (pending || !confirming) return;
    setPending(true);
    setFeedback(null);
    setDialogError(null);
    try {
      const result = await api.acknowledgeErrors(scope, confirming.observation);
      setFeedback(acknowledgedFeedback(result.acknowledged));
      setConfirming(null);
      onSuccess();
    } catch (error) {
      const failure = acknowledgementFailure(error);
      setFeedback(failure);
      setDialogError(failure.text);
    } finally {
      setPending(false);
    }
  };
  return (
    <div onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
      <button type="button" disabled={pending || !observation || count === 0}
        className="rs-btn rs-btn--quiet text-xs text-gray-300 border border-gray-600 rounded px-2 py-1 disabled:opacity-50"
        onClick={() => {
          if (pending || !observation) return;
          setDialogError(null);
          setConfirming({ observation, count });
        }}>
        {pending ? 'Acknowledging…' : 'Acknowledge'}
      </button>
      {feedback && !confirming && <p role={feedback.error ? 'alert' : 'status'} className="text-xs mt-1">{feedback.text}</p>}
      <ErrorAcknowledgementDialog isOpen={confirming !== null} scope={scope} repositoryName={repositoryName}
        count={confirming?.count ?? count} available={!!observation && count > 0} pending={pending}
        error={dialogError ?? undefined} onConfirm={() => { void acknowledge(); }}
        onCancel={() => { if (!pending) setConfirming(null); }} />
    </div>
  );
}
