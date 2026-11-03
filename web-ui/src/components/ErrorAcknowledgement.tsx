import { useState } from 'react';
import { api } from '../api';
import { acknowledgementPrompt, type ErrorObservation, type ErrorScope } from '../errorAcknowledgement';
import { getStoredUser } from '../utils/auth';

export default function ErrorAcknowledgement({ scope, repositoryName, observation, count, onSuccess }: {
  scope: ErrorScope;
  repositoryName: string;
  observation: ErrorObservation | undefined;
  count: number;
  onSuccess: () => void;
}) {
  const [pending, setPending] = useState(false);
  const [feedback, setFeedback] = useState<{ error: boolean; text: string } | null>(null);
  // Legacy password sessions do not store a user object. The server checks the
  // current role on every request; local storage is only a presentation hint.
  const user = getStoredUser();
  if (user && user.role !== 'admin') return null;
  return (
    <div onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
      <button type="button" disabled={pending || !observation || count === 0}
        className="text-xs text-gray-300 border border-gray-600 rounded px-2 py-1 disabled:opacity-50"
        onClick={async () => {
          if (pending || !observation || !window.confirm(acknowledgementPrompt(scope, repositoryName))) return;
          setPending(true);
          setFeedback(null);
          try {
            const result = await api.acknowledgeErrors(scope, observation);
            setFeedback({ error: false, text: `Acknowledged ${result.acknowledged}. Audit evidence preserved.` });
            onSuccess();
          } catch (error) {
            setFeedback({ error: true, text: error instanceof Error ? error.message : 'Acknowledgement failed. Try again.' });
          } finally {
            setPending(false);
          }
        }}>
        {pending ? 'Acknowledging…' : 'Acknowledge'}
      </button>
      {feedback && <p role={feedback.error ? 'alert' : 'status'} className="text-xs mt-1">{feedback.text}</p>}
    </div>
  );
}
