import type { ManagedRemovalStatus } from '../managedRemoval';
import { managedRemovalStateLabel } from '../managedRemoval';

export default function ManagedRemovalPanel({
  status,
  onRetry,
  retryPending,
  onRestore,
  restorePending,
}: {
  status: ManagedRemovalStatus;
  onRetry?: () => void;
  retryPending?: boolean;
  onRestore?: () => void;
  restorePending?: boolean;
}) {
  const state = `${status.state}`.toLowerCase();
  const tone =
    state === 'completed'
      ? 'bg-blue-900/30 border-blue-700 text-blue-100'
      : state === 'failed' || state === 'reconciliation_required'
        ? 'bg-red-900/30 border-red-700 text-red-100'
        : 'bg-amber-900/30 border-amber-700 text-amber-100';

  return (
    <div
      data-testid="managed-removal-panel"
      data-state={state}
      data-operation-id={status.operation_id}
      className={`border rounded-lg p-4 text-sm space-y-2 ${tone}`}
      role="status"
    >
      <p className="font-medium">{managedRemovalStateLabel(status.state)}</p>
      <p>{status.message}</p>
      <p className="font-mono text-xs">Operation {status.operation_id}</p>
      <p className="text-xs" data-testid="managed-removal-remote-outcome">
        Remote Git: {status.remote_git} · SVN: {status.remote_svn}.
        {status.partial_cleanup?.retry_is_local_cleanup_only
          ? ' Retry only retries owned local cleanup; recorded remote outcomes are kept.'
          : ' Managed removal does not delete remotes unless you separately authorize branch deletion.'}
      </p>
      {status.partial_cleanup && (
        <div
          data-testid="managed-removal-partial-cleanup"
          className="text-xs border border-current/30 rounded p-2 space-y-1"
        >
          <p className="font-medium">Partial cleanup receipt</p>
          {status.partial_cleanup.outcome_detail && (
            <p>{status.partial_cleanup.outcome_detail}</p>
          )}
          <p>
            Registration still listed: {status.partial_cleanup.registration_listed ? 'yes' : 'no'}
          </p>
        </div>
      )}
      {status.recovery && (
        <div
          data-testid="managed-removal-recovery"
          className="text-xs border border-current/30 rounded p-2 space-y-1"
        >
          <p className="font-medium">
            {status.recovery.restore_supported
              ? 'Recovery metadata (restore available)'
              : 'Recovery metadata (restore not supported)'}
          </p>
          <p>
            {status.recovery.name}: maps={status.recovery.commit_map_count}, tip r
            {status.recovery.last_svn_rev} / {status.recovery.last_git_sha.slice(0, 8)}
          </p>
          <p>{status.recovery.retention}</p>
          {status.recovery.restore_supported
            && status.state === 'completed'
            && status.registration_listed === false
            && onRestore && (
            <button
              type="button"
              data-testid="managed-removal-restore"
              disabled={restorePending}
              onClick={onRestore}
              className="mt-2 px-3 py-1.5 rounded-md bg-blue-800 hover:bg-blue-700 disabled:opacity-50 text-xs font-medium"
            >
              {restorePending ? 'Restoring registration…' : 'Restore registration (stays disabled)'}
            </button>
          )}
        </div>
      )}
      {status.retryable && onRetry && (
        <button
          type="button"
          data-testid="managed-removal-retry"
          disabled={retryPending}
          onClick={onRetry}
          className="mt-2 px-3 py-1.5 rounded-md bg-gray-800 hover:bg-gray-700 disabled:opacity-50 text-xs font-medium"
        >
          {retryPending ? 'Retrying local cleanup…' : 'Retry local cleanup'}
        </button>
      )}
    </div>
  );
}
