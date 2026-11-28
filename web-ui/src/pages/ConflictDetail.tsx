import { useId } from 'react';
import { useViewMutation as useMutation } from '../useViewMutation';
import { useState } from 'react';
import { Link, useParams, useNavigate } from 'react-router-dom';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api, ApiError, type Conflict, type Repository } from '../api';
import DiffViewer from '../components/DiffViewer';
import Dialog from '../components/Dialog';
import { useText } from '../layouts/vocabulary';

/** The three resolutions the server accepts; the value is the request's `resolution`. */
export type ConflictChoice = 'accept_svn' | 'accept_git' | 'custom';

const CHOICE_LABEL: Record<ConflictChoice, string> = {
  accept_svn: 'Accept SVN',
  accept_git: 'Accept Git',
  custom: 'Apply Manual Edit',
};

const CHOICE_CLASS: Record<ConflictChoice, string> = {
  accept_svn: 'rs-btn rs-btn--state px-4 py-2 bg-purple-600 rs-text-on-state rounded-md hover:bg-purple-700 disabled:opacity-50 text-sm font-medium',
  accept_git: 'rs-btn rs-btn--primary px-4 py-2 bg-blue-600 rs-text-on-accent rounded-md hover:bg-blue-700 disabled:opacity-50 text-sm font-medium',
  custom: 'rs-btn rs-btn--state px-4 py-2 bg-green-700 rs-text-on-state rounded-md hover:bg-green-800 disabled:opacity-50 text-sm font-medium',
};

/** #264 (G06.1): the badge follows the real status value, as the conflict list does. */
const STATUS_CLASS: Record<string, string> = {
  detected: 'bg-red-900/50 text-red-300',
  queued: 'bg-yellow-900/50 text-yellow-300',
  deferred: 'bg-gray-700 text-gray-300',
  resolved: 'bg-green-900/50 text-green-300',
};

const STALE_REASON = 'Unavailable until the conflict is read again.';
const CHOOSE_REASON = 'Choose a decision first.';

export default function ConflictDetail() {
  const { id } = useParams<{ id: string }>();
  return <ConflictEditor key={id} />;
}

function ConflictEditor() {
  const t = useText();
  const fieldId = useId();
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [mergedContent, setMergedContent] = useState('');
  const [actionError, setActionError] = useState('');
  const [activeTab, setActiveTab] = useState<'diff' | 'edit'>('diff');
  // #264 (G06.4, D19): nothing is preselected; a choice is confirmed once before it is sent.
  const [choice, setChoice] = useState<ConflictChoice | null>(null);
  const [confirming, setConfirming] = useState(false);

  const { data: conflict, isError, error, refetch } = useQuery({
    queryKey: ['conflict', id],
    queryFn: () => api.getConflict(id!),
    enabled: !!id,
  });
  // A re-read after a failure has no data and no error until it settles; keep showing the
  // failure meanwhile instead of flipping back to loading on every poll.
  const [lastReadError, setLastReadError] = useState<Error | null>(null);
  if (error && error !== lastReadError) setLastReadError(error);

  const resolveMutation = useMutation({
    mutationFn: ({ resolution, content }: { resolution: string; content?: string }) =>
      api.resolveConflict(id!, resolution, content),
    onMutate: () => { setActionError(''); },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['conflicts'] });
      queryClient.invalidateQueries({ queryKey: ['conflict', id] });
      navigate('/conflicts');
    },
    onError: (err: Error) => {
      setConfirming(false);
      setActionError(`Resolution failed: ${err.message}`);
    },
  });

  const deferMutation = useMutation({
    mutationFn: () => api.deferConflict(id!),
    onMutate: () => { setActionError(''); },
    onError: (err: Error) => { setActionError(`Deferral failed: ${err.message}`); },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['conflicts'] });
      navigate('/conflicts');
    },
  });

  if (!conflict) {
    // #264 (G06.7): a failed or missing read is shown as such, never as endless loading.
    const readError = error ?? lastReadError;
    if (readError) return <ConflictReadFailure error={readError} onRetry={() => void refetch()} />;
    return <div role="status" className="text-center py-8 text-gray-400">Loading...</div>;
  }

  const isResolved = conflict.status === 'resolved';
  // Earlier data stays on screen after a failed refresh, marked as such; actions wait for a fresh read.
  const stale = isError;
  const busy = resolveMutation.isPending || deferMutation.isPending;
  // Read from the list's cache only: this page makes no request for repository names.
  const repoName = conflict.repo_id
    ? queryClient.getQueryData<Repository[]>(['repos'])?.find((r) => r.id === conflict.repo_id)?.name
    : undefined;
  const recordDecision = () => {
    if (!choice) return;
    resolveMutation.mutate(choice === 'custom' ? { resolution: 'custom', content: mergedContent } : { resolution: choice });
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <button
            onClick={() => navigate('/conflicts')}
            className="text-blue-400 hover:text-blue-300 text-sm mb-2"
          >
            &larr; Back to Conflicts
          </button>
          <h1 className="rs-title text-2xl font-bold text-gray-100">
            <code>{conflict.file_path}</code>
          </h1>
        </div>
        <ConflictStatusMark status={conflict.status} />
      </div>

      {stale && (
        <div role="alert" className="rs-bn rs-bn--warn bg-amber-900/30 border border-amber-700 rounded-lg p-3 text-amber-100 text-sm flex flex-wrap items-center gap-3">
          <span>The latest read of this conflict failed: {error?.message}. The details below are from an earlier read.</span>
          <button type="button" onClick={() => void refetch()} className="rs-btn px-3 py-1 rounded-md bg-gray-700 text-gray-200 hover:bg-gray-600">Retry</button>
        </div>
      )}

      {/* Metadata */}
      <div className="rs-sec bg-gray-800 shadow rounded-lg p-4 grid grid-cols-2 md:grid-cols-5 gap-4 text-sm border border-gray-700">
        <div>
          <span className="text-gray-400">Repository</span>
          <p className="truncate text-gray-200">
            <ConflictRepository repoId={conflict.repo_id ?? null} repoName={repoName} />
          </p>
        </div>
        <div>
          <span className="text-gray-400">Type</span>
          <p className="font-medium capitalize text-gray-200">{conflict.conflict_type}</p>
        </div>
        <div>
          <span className="text-gray-400">SVN Revision</span>
          <p className="font-mono text-gray-200">{conflict.svn_revision ?? '-'}</p>
        </div>
        <div>
          <span className="text-gray-400">Git SHA</span>
          <p className="font-mono truncate text-gray-200">{conflict.git_hash ?? '-'}</p>
        </div>
        <div>
          <span className="text-gray-400">Created</span>
          <p className="text-gray-200">{new Date(conflict.created_at).toLocaleString()}</p>
        </div>
      </div>

      {/* Tabs */}
      {!isResolved && (
        <div className="border-b border-gray-700">
          <nav aria-label="Conflict view" className="-mb-px flex space-x-8">
            <button
              aria-pressed={activeTab === 'diff'} onClick={() => setActiveTab('diff')}
              className={`py-4 px-1 border-b-2 font-medium text-sm ${
                activeTab === 'diff'
                  ? 'border-blue-500 text-blue-400'
                  : 'border-transparent text-gray-300 hover:text-gray-100 hover:border-gray-600'
              }`}
            >
              {t('conflict.view.diff')}
            </button>
            <button
              aria-pressed={activeTab === 'edit'}
              onClick={() => {
                setActiveTab('edit');
                if (!mergedContent) {
                  setMergedContent(conflict.git_content ?? conflict.svn_content ?? '');
                }
              }}
              className={`py-4 px-1 border-b-2 font-medium text-sm ${
                activeTab === 'edit'
                  ? 'border-blue-500 text-blue-400'
                  : 'border-transparent text-gray-300 hover:text-gray-100 hover:border-gray-600'
              }`}
            >
              {t('conflict.view.edit')}
            </button>
          </nav>
        </div>
      )}

      {/* Content */}
      {activeTab === 'diff' ? (
        <div className="rs-sec bg-gray-800 shadow rounded-lg overflow-hidden border border-gray-700">
          <DiffViewer
            oldValue={conflict.svn_content ?? ''}
            newValue={conflict.git_content ?? ''}
            leftTitle="SVN Version"
            rightTitle="Git Version"
            baseValue={conflict.base_content}
            baseTitle="Common base"
          />
        </div>
      ) : (
        <div className="rs-sec bg-gray-800 shadow rounded-lg p-4 border border-gray-700">
          <label className="block text-sm font-medium text-gray-300 mb-2" htmlFor={`${fieldId}-1`}>
            Edit merged content:
          </label>
          <textarea
            value={mergedContent}
            onChange={(e) => setMergedContent(e.target.value)}
            className="rs-in w-full h-96 font-mono text-sm border border-gray-600 bg-gray-900 text-gray-200 rounded-md p-3 focus:ring-blue-500 focus:border-blue-500" id={`${fieldId}-1`}
          />
        </div>
      )}

      {actionError && <p role="alert" className="text-red-300">{actionError}</p>}
      {/* Resolution Actions */}
      {!isResolved && (
        <>
          <ResolutionActions
            headingId={`${fieldId}-resolution`}
            status={conflict.status}
            deferredAt={conflict.resolved_at}
            choice={choice}
            manualOffered={activeTab === 'edit' || choice === 'custom'}
            busy={busy}
            stale={stale}
            pendingText={resolveMutation.isPending ? 'Recording decision…' : deferMutation.isPending ? 'Recording deferral…' : ''}
            onChoose={setChoice}
            onRecord={() => setConfirming(true)}
            onDefer={() => deferMutation.mutate()}
          />
          <DecisionConfirmation
            isOpen={confirming && choice !== null}
            conflict={conflict}
            choice={choice}
            repoName={repoName}
            busy={resolveMutation.isPending}
            stale={stale}
            onConfirm={recordDecision}
            onCancel={() => setConfirming(false)}
          />
        </>
      )}

      {/* Resolution info */}
      {isResolved && (
        <div className="rs-bn rs-bn--ok bg-green-900/30 border border-green-700 rounded-lg p-4">
          <p className="text-green-300">
            Resolved as <strong>{conflict.resolution}</strong>
            {conflict.resolved_by && <> by {conflict.resolved_by}</>}
            {conflict.resolved_at && (
              <> on {new Date(conflict.resolved_at).toLocaleString()}</>
            )}
          </p>
        </div>
      )}
    </div>
  );
}

/** The real status value, coloured by what it is; an unrecognised value is shown as read, in neutral grey. */
export function ConflictStatusMark({ status }: { status: string }) {
  return (
    <span
      className={`rs-mark inline-flex items-center px-3 py-1 rounded-full text-sm font-medium ${
        STATUS_CLASS[status] ?? 'bg-gray-700 text-gray-300'
      }`}
    >
      {status}
    </span>
  );
}

/** #264 (G06.1): the read carries repo_id; the name comes from the list's cache when it is there. */
export function ConflictRepository({ repoId, repoName }: { repoId: string | null; repoName?: string }) {
  if (!repoId) return <span className="text-gray-400">Not recorded</span>;
  return (
    <Link to={`/repos/${encodeURIComponent(repoId)}`} className="text-blue-400 hover:text-blue-300 underline">
      {repoName ?? repoId}
    </Link>
  );
}

/** #264 (G06.7): not found and read failure, each with the error text the client produced. */
export function ConflictReadFailure({ error, onRetry }: { error: Error; onRetry: () => void }) {
  if (error instanceof ApiError && error.status === 404) {
    return (
      <div role="status" className="rs-sec bg-gray-800 shadow rounded-lg p-8 text-center border border-gray-700 space-y-2">
        <p className="text-gray-100 text-lg font-medium">Conflict not found</p>
        <p className="text-gray-300 text-sm">It may belong to a repository that was removed.</p>
        <p className="text-gray-500 text-xs">{error.message}</p>
        <Link to="/conflicts" className="inline-block text-blue-400 hover:text-blue-300 text-sm">
          Back to Conflicts
        </Link>
      </div>
    );
  }
  return (
    <div role="alert" className="rs-bn rs-bn--danger bg-red-900/30 border border-red-700 rounded-lg p-4 text-red-300 text-sm space-y-2">
      <p className="font-medium">The conflict could not be loaded.</p>
      <p>{error.message}</p>
      <div className="flex items-center gap-4">
        <button type="button" onClick={onRetry} className="rs-btn px-3 py-1 rounded-md bg-gray-700 text-gray-200 hover:bg-gray-600">
          Retry
        </button>
        <Link to="/conflicts" className="text-blue-400 hover:text-blue-300">
          Back to Conflicts
        </Link>
      </div>
    </div>
  );
}

interface ResolutionActionsProps {
  headingId: string;
  status: string;
  deferredAt: string | null;
  choice: ConflictChoice | null;
  manualOffered: boolean;
  busy: boolean;
  stale: boolean;
  pendingText: string;
  onChoose: (choice: ConflictChoice) => void;
  onRecord: () => void;
  onDefer: () => void;
}

/**
 * #264 (G06.4): the existing labels choose; "Record decision…" asks once before the existing
 * call. "Defer" records a deferral at once, as before.
 */
export function ResolutionActions({
  headingId, status, deferredAt, choice, manualOffered, busy, stale, pendingText, onChoose, onRecord, onDefer,
}: ResolutionActionsProps) {
  const reason = stale ? STALE_REASON : choice ? '' : CHOOSE_REASON;
  const choices: ConflictChoice[] = manualOffered ? ['accept_svn', 'accept_git', 'custom'] : ['accept_svn', 'accept_git'];
  return (
    <section aria-labelledby={headingId} className="rs-sec bg-gray-800 shadow rounded-lg p-4 border border-gray-700 space-y-4">
      <h2 id={headingId} className="rs-heading text-lg font-semibold text-gray-100">Resolution</h2>
      {status === 'deferred' && (
        <p className="text-sm text-gray-300">
          Deferred{deferredAt && <> on {new Date(deferredAt).toLocaleString()}</>}. It still needs a decision.
        </p>
      )}
      <div role="group" aria-labelledby={headingId} className="flex flex-wrap gap-3">
        {choices.map((value) => {
          const pressed = choice === value;
          return (
            <button
              key={value}
              type="button"
              aria-pressed={pressed}
              onClick={() => onChoose(value)}
              disabled={busy}
              className={`${CHOICE_CLASS[value]}${pressed ? ' ring-4 ring-blue-500/40' : ''}`}
            >
              {pressed && <span aria-hidden="true">✓ </span>}
              {CHOICE_LABEL[value]}
            </button>
          );
        })}
      </div>
      <div className="rs-bn rs-bn--info bg-blue-900/30 border border-blue-700 rounded-lg p-3 text-sm text-blue-100">
        <p className="font-medium">At this version RepoSync records your decision. It does not write the chosen content to SVN or Git.</p>
        <p className="mt-1">Applying resolved content is open work (issue #73). Make the change in the repositories yourself.</p>
      </div>
      <div className="flex flex-wrap items-center gap-3">
        <button
          type="button"
          onClick={onDefer}
          disabled={busy || stale}
          aria-describedby={stale ? `${headingId}-reason` : undefined}
          className="px-4 py-2 bg-gray-700 text-gray-300 rounded-md hover:bg-gray-600 disabled:opacity-50 text-sm"
        >
          Defer
        </button>
        <span className="flex-1" />
        <p role="status" className="text-sm text-gray-300">{pendingText}</p>
        {reason && <span id={`${headingId}-reason`} className="text-sm text-gray-400">{reason}</span>}
        <button
          type="button"
          onClick={onRecord}
          disabled={busy || !!reason}
          aria-describedby={reason ? `${headingId}-reason` : undefined}
          className="rs-btn rs-btn--primary px-4 py-2 bg-blue-600 rs-text-on-accent rounded-md hover:bg-blue-700 disabled:opacity-50 text-sm font-medium"
        >
          Record decision…
        </button>
      </div>
    </section>
  );
}

interface DecisionConfirmationProps {
  isOpen: boolean;
  conflict: Conflict;
  choice: ConflictChoice | null;
  repoName?: string;
  busy: boolean;
  stale: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/** #264 (G06.4): one confirmation naming the target and stating that only a record is made (#73). */
export function DecisionConfirmation({ isOpen, conflict, choice, repoName, busy, stale, onConfirm, onCancel }: DecisionConfirmationProps) {
  if (!choice) return null;
  const svn = conflict.svn_revision != null ? `SVN version (r${conflict.svn_revision})` : 'SVN version';
  const git = conflict.git_hash ? `Git version (${conflict.git_hash.slice(0, 12)})` : 'Git version';
  const notChosen = choice === 'accept_svn' ? git : choice === 'accept_git' ? svn : `${svn} and ${git}`;
  return (
    <Dialog
      isOpen={isOpen}
      title="Record this decision?"
      description="This marks the conflict resolved in RepoSync. It does not change either repository."
      onClose={onCancel}
      busy={busy}
    >
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm mb-4">
        <dt className="text-gray-400">File</dt>
        <dd className="font-mono break-all text-gray-200">{conflict.file_path}</dd>
        <dt className="text-gray-400">Repository</dt>
        <dd className="break-all text-gray-200">{conflict.repo_id ? repoName ?? conflict.repo_id : 'Not recorded'}</dd>
        <dt className="text-gray-400">Decision</dt>
        <dd className="font-medium text-gray-100">{CHOICE_LABEL[choice]}</dd>
        <dt className="text-gray-400">Not chosen</dt>
        <dd className="text-gray-200">{notChosen}</dd>
      </dl>
      {choice === 'custom' && (
        <p className="text-sm text-gray-300 mb-4">The edited text is sent with the request. At this version the server does not store or apply it.</p>
      )}
      {stale && <p className="text-sm text-amber-200 mb-4">{STALE_REASON}</p>}
      <div className="flex justify-end space-x-3">
        <button type="button" data-dialog-cancel disabled={busy} onClick={onCancel}
          className="px-4 py-2 rounded bg-gray-700 hover:bg-gray-600 text-gray-200">Go back</button>
        <button type="button" disabled={busy || stale} onClick={onConfirm}
          className="rs-btn rs-btn--primary px-4 py-2 rounded rs-text-on-accent bg-blue-600 hover:bg-blue-700 disabled:opacity-50">Record decision</button>
      </div>
    </Dialog>
  );
}
