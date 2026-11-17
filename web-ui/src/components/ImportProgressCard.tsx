import ImportProgressMeter from './ImportProgressMeter';
import Dialog from './Dialog';
import { useViewMutation as useMutation } from '../useViewMutation';
import { sessionFetch } from '../session';
import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Clock, ArrowRight, Terminal, CheckCircle2 } from 'lucide-react';
import { api, type ImportStatus } from '../api';
import {
  DEFAULT_REPO_IMPORT_MODE,
  buildStartImportBody,
  startImportButtonLabel,
  validateSvnRevisionInput,
  type RepoImportMode,
} from '../importBaseline';
import ImportHistoryNotice from './ImportHistoryNotice';
import ImportModeFields from './ImportModeFields';
import { getStoredUser } from '../utils/auth';
import { importStateLabel } from '../importProgress';
import { useText } from '../layouts/vocabulary';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatElapsed(startedAt: string | null, endedAt?: string | null): string {
  if (!startedAt) return '--:--';
  const start = new Date(startedAt).getTime();
  const end = endedAt ? new Date(endedAt).getTime() : Date.now();
  const secs = Math.max(0, Math.floor((end - start) / 1000));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (h > 0) return `${h}h ${m}m ${s}s`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

function phaseDotColor(phase: string): string {
  if (phase === 'completed') return 'bg-emerald-400';
  if (phase === 'failed') return 'bg-red-400';
  if (phase === 'cancelled') return 'bg-yellow-400';
  if (phase === 'idle') return 'bg-gray-500';
  return 'bg-blue-400 animate-pulse';
}

// The five import phases in order
const PHASE_STEPS = [
  { key: 'connecting', label: 'Connect' },
  { key: 'importing', label: 'Import' },
  { key: 'verifying', label: 'Verify' },
  { key: 'final_push', label: 'Push' },
  { key: 'completed', label: 'Complete' },
] as const;

function phaseIndex(phase: string): number {
  const idx = PHASE_STEPS.findIndex((s) => s.key === phase);
  return idx >= 0 ? idx : -1;
}

// ---------------------------------------------------------------------------
// Phase Dots
// ---------------------------------------------------------------------------

function PhaseDots({ phase }: { phase: string }) {
  const currentIdx = phaseIndex(phase);
  const isFailed = phase === 'failed';
  const isCancelled = phase === 'cancelled';

  return (
    <div className="flex items-center space-x-1">
      {PHASE_STEPS.map((step, i) => {
        let dotClass = 'bg-gray-600'; // future
        if (isFailed || isCancelled) {
          dotClass = i <= currentIdx ? (isFailed ? 'bg-red-400' : 'bg-yellow-400') : 'bg-gray-600';
        } else if (i < currentIdx) {
          dotClass = 'bg-emerald-400'; // past
        } else if (i === currentIdx) {
          dotClass = 'bg-blue-400 animate-pulse'; // current
          if (phase === 'completed') dotClass = 'bg-emerald-400';
        }

        return (
          <div key={step.key} className="flex flex-col items-center">
            <div className={`w-2 h-2 rounded-full ${dotClass}`} title={step.label} />
            <span className="text-[9px] text-gray-300 mt-0.5 leading-none">{step.label}</span>
          </div>
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Component (self-fetching)
// ---------------------------------------------------------------------------

interface ImportProgressProps {
  repoId?: string;
  repoName?: string;
  hideIfIdle?: boolean;
  defaultImportMode?: RepoImportMode;
  defaultSvnRevision?: string;
  /** #264 page regions: the region name and the sections it shows in, put on the card's root. */
  regionName?: string;
  regionSections?: string;
}

/** Subject changes retire local feedback, private preferences and mutation observers together. */
export default function ImportProgressCard(props: ImportProgressProps = {}) {
  return <ImportProgressSubject key={props.repoId ? `repository:${props.repoId}` : 'global'} {...props} />;
}

function ImportProgressSubject({
  repoId,
  repoName,
  hideIfIdle = false,
  defaultImportMode = DEFAULT_REPO_IMPORT_MODE,
  defaultSvnRevision = 'HEAD',
  regionName,
  regionSections,
}: ImportProgressProps) {
  const regionAttributes = regionName ? { 'data-rs-region': regionName, 'data-rs-sections': regionSections ?? '' } : {};
  const t = useText();
  const queryClient = useQueryClient();
  const admin = getStoredUser()?.role === 'admin';
  const [importMode, setImportMode] = useState<RepoImportMode>(defaultImportMode);
  const [svnRevision, setSvnRevision] = useState(defaultSvnRevision);
  const [startError, setStartError] = useState<string | null>(null);
  // G04.13: the operation a stop confirmation is open for.
  const [stopTarget, setStopTarget] = useState<string | null>(null);

  useEffect(() => {
    setImportMode(defaultImportMode);
    setSvnRevision(defaultSvnRevision);
  }, [defaultImportMode, defaultSvnRevision, repoId]);
  const { data: status, isError } = useQuery<ImportStatus>({
    queryKey: ['import-status', repoId || 'global'],
    queryFn: async () => {
      if (repoId) {
        const token = localStorage.getItem('session_token');
        const res = await sessionFetch(`/api/repos/${repoId}/import/status`, {
          headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}) },
        });
        if (!res.ok) throw new Error(`Import status unavailable (${res.status})`);
        return res.json();
      }
      return api.getImportStatus();
    },
    refetchInterval: 2000,
    refetchIntervalInBackground: false,
  });

  const start = useMutation({
    mutationFn: async () => {
      if (!repoId) throw new Error('Repository id required');
      setStartError(null);
      let body: ReturnType<typeof buildStartImportBody>;
      try {
        body = buildStartImportBody(importMode, svnRevision);
      } catch (e) {
        throw e instanceof Error ? e : new Error(String(e));
      }
      const res = await sessionFetch(`/api/repos/${repoId}/import`, {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${localStorage.getItem('session_token')}`,
          'Content-Type': 'application/json',
          'X-Request-ID': crypto.randomUUID(),
        },
        body: JSON.stringify(body),
      });
      const payload = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(payload.error || `Import start failed (${res.status})`);
      }
      return payload as { operation_id: string };
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['import-status', repoId] });
      queryClient.invalidateQueries({ queryKey: ['repo', repoId] });
    },
    onError: (e: Error) => {
      setStartError(e.message);
      queryClient.invalidateQueries({ queryKey: ['import-status', repoId] });
    },
  });
  const cancel = useMutation({
    mutationFn: async (operationId: string) => {
      const res = await sessionFetch(`/api/repos/${repoId}/import/${operationId}/cancel`, {
        method: 'POST', headers: { Authorization: `Bearer ${localStorage.getItem('session_token')}` },
      });
      if (!res.ok) throw new Error((await res.json()).error || `Cancellation failed (${res.status})`);
      return res.json();
    },
    onSuccess: () => {
      setStopTarget(null);
      queryClient.invalidateQueries({ queryKey: ['import-status', repoId] });
    },
  });
  const reconcile = useMutation({
    mutationFn: async (operationId: string) => {
      const res = await sessionFetch(`/api/repos/${repoId}/import/${operationId}/reconcile`, {
        method: 'POST', headers: { Authorization: `Bearer ${localStorage.getItem('session_token')}` },
      });
      if (!res.ok) throw new Error((await res.json()).error || `Remote verification failed (${res.status})`);
      return res.json() as Promise<{ lifecycle: string; remaining_reason: string | null }>;
    },
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['import-status', repoId] }),
  });

  // No data yet from API
  if (!status) {
    return (
      <div className="rs-sec bg-gray-800 border border-gray-700 rounded-xl p-5 shadow-lg" {...regionAttributes}>
        <div className="text-sm text-gray-300 italic">{isError ? 'Import status unavailable' : 'Loading import status...'}</div>
      </div>
    );
  }

  // Never ran an import — hide entirely if hideIfIdle
  const neverRan = status.phase === 'idle' && !status.started_at;
  if (neverRan) {
    if (hideIfIdle && !status.can_start) return null;
    return (
      <div className="rs-sec bg-gray-800 border border-gray-700 rounded-xl p-5 shadow-lg" {...regionAttributes}>
        <div className="flex items-center justify-between">
          <div>
            <h3 className="rs-heading text-sm font-semibold text-gray-200">
            SVN Import{repoName && <span className="text-blue-400 ml-1">— {repoName}</span>}
          </h3>
            <p className="text-xs text-gray-300 mt-1">No import history</p>
            {repoId && admin && status.can_start && (
              <div className="mt-3 space-y-3">
                <ImportModeFields
                  mode={importMode}
                  svnRevision={svnRevision}
                  onModeChange={setImportMode}
                  onRevisionChange={setSvnRevision}
                  disabled={start.isPending}
                  radioGroupName={repoId ? `repo-import-mode-${repoId}` : 'repo-import-mode'}
                />
                <button
                  type="button"
                  data-testid="start-repo-import"
                  onClick={() => start.mutate()}
                  disabled={
                    start.isPending
                    || (importMode === 'snapshot'
                      && Boolean(validateSvnRevisionInput(svnRevision)))
                  }
                  className="rs-btn rs-btn--primary rounded bg-blue-600 px-3 py-1 text-xs rs-text-on-accent disabled:opacity-50"
                >
                  {start.isPending ? 'Starting…' : startImportButtonLabel(importMode)}
                </button>
              </div>
            )}
            {(start.isError || startError) && (
              <p className="mt-2 text-xs text-red-400">{startError ?? start.error?.message}</p>
            )}
          </div>
          <a
            href="/repos"
            className="flex items-center space-x-1 text-sm text-blue-400 hover:text-blue-300 transition-colors"
          >
            <span>{t('import.manageRepositories')}</span>
            <ArrowRight className="w-4 h-4" />
          </a>
        </div>
      </div>
    );
  }

  // Completed and old — hide if requested (> 1 hour ago)
  if (status.phase === 'completed' && hideIfIdle && status.completed_at) {
    const completedMs = new Date(status.completed_at).getTime();
    const hourAgo = Date.now() - 60 * 60 * 1000;
    if (completedMs < hourAgo) return null;
  }

  // Completed state — show success summary
  if (status.phase === 'completed') {
    return (
      <div className="rs-sec bg-gray-800 border border-gray-700 rounded-xl p-5 shadow-lg" {...regionAttributes}>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center space-x-2">
            <CheckCircle2 className="w-5 h-5 text-emerald-400" />
            <h3 className="rs-heading text-sm font-semibold text-gray-200">
              SVN Import Complete{repoName && <span className="text-blue-400 ml-1">— {repoName}</span>}
            </h3>
          </div>
          <div className="flex items-center space-x-1 text-xs text-gray-300">
            <Clock className="w-3.5 h-3.5" />
            <span>{formatElapsed(status.started_at, status.completed_at)}</span>
          </div>
        </div>
        <ImportProgressMeter status={status} name={repoName} />
        <div className="grid grid-cols-4 gap-3 mb-3">
          <StatCell label="Revisions" value={`${status.total_revs}`} />
          <StatCell label="Commits" value={`${status.commits_created}`} />
          <StatCell label="Batches" value={`${status.batches_pushed}`} />
          <StatCell label="LFS Files" value={`${status.lfs_unique_count}`} />
        </div>
        <div className="mb-3">
          <ImportHistoryNotice fields={status} />
        </div>
        {status.outcome_detail && <p className="mb-3 text-xs text-emerald-300">{status.outcome_detail}</p>}
        {repoId && (
          <a
            href={`/repos/${repoId}`}
            className="flex items-center justify-center space-x-1 text-sm text-blue-400 hover:text-blue-300 transition-colors"
          >
            <span>View Repository</span>
            <ArrowRight className="w-4 h-4" />
          </a>
        )}
      </div>
    );
  }

  // Active / failed / cancelled — full card
  const lastLogLines = (status.log_lines ?? []).slice(-5);
  /** A stop is sent only for the operation the confirmation names, while it is still queued or running. */
  const stoppable = (operationId: string) => !!repoId && admin && status.operation_id === operationId
    && (status.lifecycle === 'queued' || status.lifecycle === 'running');

  return (
    <div className="rs-sec bg-gray-800 border border-gray-700 rounded-xl p-5 shadow-lg" {...regionAttributes}>
      {/* Header row */}
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center space-x-2">
          <span className={`w-2.5 h-2.5 rounded-full ${phaseDotColor(status.phase)}`} />
          <h3 className="rs-heading text-sm font-semibold text-gray-200">
            SVN Import{repoName && <span className="text-blue-400 ml-1">— {repoName}</span>}
          </h3>
          <span className="rs-mark text-xs text-gray-200 bg-gray-700 px-2 py-0.5 rounded-full">
            {importStateLabel(status)}
          </span>
        </div>
        <div className="flex items-center space-x-1 text-xs text-gray-300">
          <Clock className="w-3.5 h-3.5" />
          <span>{formatElapsed(status.started_at)}</span>
        </div>
      </div>

      {/* Phase dots */}
      <div className="flex justify-center mb-4">
        <PhaseDots phase={status.phase} />
      </div>

      {/* Progress bar */}
      <ImportProgressMeter status={status} name={repoName} />
      <div className="text-right text-xs text-gray-300 mb-3 font-mono">
        {status.current_rev} / {status.total_revs}
      </div>

      {/* Stats row */}
      <div className="grid grid-cols-4 gap-3 mb-4">
        <StatCell label="Revisions" value={`${status.current_rev}/${status.total_revs}`} />
        <StatCell label="Commits" value={`${status.commits_created}`} />
        <StatCell label="Batches" value={`${status.batches_pushed}`} />
        <StatCell label="LFS Files" value={`${status.lfs_unique_count}`} />
      </div>
      <div className="mb-3">
        <ImportHistoryNotice fields={status} compact />
      </div>
      {status.operation_id && <p className="mb-2 text-xs text-gray-400 font-mono">Operation {status.operation_id}</p>}
      <p className="mb-2 text-xs text-gray-400 font-mono" data-testid="import-reported-state">
        Reported lifecycle: {status.lifecycle ?? 'not reported'} · phase: {status.phase}
      </p>
      {status.last_local_svn_rev != null && (
        <p className="mb-2 text-xs text-gray-400">Local through SVN r{status.last_local_svn_rev};
          remote confirmed through {status.last_confirmed_svn_rev == null ? 'none' : `r${status.last_confirmed_svn_rev}`}.</p>
      )}
      {status.lifecycle === 'reconciliation_required' && (
        <div className="mb-2 space-y-1 text-xs text-gray-400 font-mono break-all">
          {status.last_local_git_sha && <p>Local Git: {status.last_local_git_sha}</p>}
          {status.last_confirmed_git_sha && <p>Confirmed Git: {status.last_confirmed_git_sha}</p>}
          {status.intended_ref && status.intended_git_sha && (
            <p>Publication intent: {status.intended_ref} → {status.intended_git_sha}</p>
          )}
        </div>
      )}
      {status.outcome_detail && <p className="mb-2 text-xs text-yellow-300">{status.outcome_detail}</p>}
      {(status.lifecycle === 'cancelled' || status.lifecycle === 'reconciliation_required') && (
        <p className="mb-3 text-xs text-yellow-300">Stopping does not undo commits already published.
          This repository stays held until its local and remote history is reconciled.</p>
      )}
      {repoId && admin && status.operation_id && (status.lifecycle === 'queued' || status.lifecycle === 'running') && (
        <button type="button" data-testid="stop-import"
          onClick={() => { cancel.reset(); setStopTarget(status.operation_id!); }} disabled={cancel.isPending}
          className="rs-btn rs-btn--quiet mb-3 rounded border border-yellow-500 px-3 py-1 text-xs text-yellow-200 disabled:opacity-50">
          {cancel.isPending ? 'Requesting stop…' : t('import.stop')}
        </button>
      )}
      {cancel.isError && !stopTarget && <p role="alert" className="mb-3 text-xs text-red-400">{cancel.error.message}</p>}
      {stopTarget && (
        <StopImportDialog
          operationId={stopTarget}
          repoName={repoName}
          allowed={stoppable(stopTarget)}
          pending={cancel.isPending}
          error={cancel.isError ? cancel.error.message : null}
          onConfirm={() => { if (stoppable(stopTarget)) cancel.mutate(stopTarget); }}
          onClose={() => setStopTarget(null)}
        />
      )}
      {repoId && admin && status.operation_id && status.lifecycle === 'reconciliation_required' && (
        <button type="button" onClick={() => reconcile.mutate(status.operation_id!)} disabled={reconcile.isPending}
          className="rs-btn rs-btn--quiet mb-3 rounded border border-blue-500 px-3 py-1 text-xs text-blue-200 disabled:opacity-50">
          {reconcile.isPending ? 'Verifying remote…' : 'Verify remote'}
        </button>
      )}
      {reconcile.isError && <p role="alert" className="mb-3 text-xs text-red-400">{reconcile.error.message}</p>}
      {reconcile.data?.remaining_reason && <p className="mb-3 text-xs text-yellow-300">{reconcile.data.remaining_reason}</p>}

      {/* Mini terminal */}
      {lastLogLines.length > 0 && (
        <div aria-label="Recent import log" aria-live="off" className="bg-gray-950 border border-gray-700 rounded-lg p-3 mb-4">
          <div className="flex items-center space-x-1.5 mb-2">
            <Terminal className="w-3 h-3 text-gray-300" />
            <span className="text-[10px] text-gray-300 uppercase tracking-wider">Log</span>
          </div>
          <div className="space-y-0.5 font-mono text-xs leading-relaxed max-h-[100px] overflow-y-auto">
            {lastLogLines.map((line, i) => (
              <div key={i} className="text-gray-400 truncate">{line}</div>
            ))}
          </div>
        </div>
      )}

      {/* Link: a repository's import lands on its Operations section (G04.31). */}
      {repoId ? (
        <Link
          to={`/repos/${repoId}?section=operations`}
          className="flex items-center justify-center space-x-1 text-sm text-blue-400 hover:text-blue-300 transition-colors"
        >
          <span>View Full Import</span>
          <ArrowRight className="w-4 h-4" />
        </Link>
      ) : (
        <a
          href="/"
          className="flex items-center justify-center space-x-1 text-sm text-blue-400 hover:text-blue-300 transition-colors"
        >
          <span>View Full Import</span>
          <ArrowRight className="w-4 h-4" />
        </a>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

/** G04.13: “Stop import” asks once and says what stopping does not undo; then the same call. */
export function StopImportDialog({ operationId, repoName, allowed, pending, error, onConfirm, onClose }: {
  operationId: string;
  repoName?: string;
  allowed: boolean;
  pending: boolean;
  error: string | null;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Dialog title="Stop import?" description={`Request a stop for this SVN import${repoName ? ` of ${repoName}` : ''}.`}
      onClose={onClose} busy={pending} testId="stop-import-modal">
      <div className="rs-dlg space-y-3 text-sm text-gray-300">
        <p>Stopping does not undo commits already published.</p>
        <p>This repository stays held until its local and remote history is reconciled.</p>
        <p className="text-xs text-gray-400 font-mono break-all">Operation {operationId}</p>
        {!allowed && (
          <p role="status" className="text-yellow-300">This import is no longer queued or running. No stop request will be sent.</p>
        )}
        {error && <p role="alert" className="text-red-400">{error}</p>}
        <div className="flex items-center justify-end gap-3 pt-1">
          <button type="button" onClick={onClose} data-dialog-cancel disabled={pending}
            className="rs-btn rs-btn--quiet px-4 py-2 rounded-lg border border-gray-600 text-gray-300 rs-hover-text-strong text-sm font-medium transition-colors">
            Cancel
          </button>
          <button type="button" data-testid="confirm-stop-import" onClick={onConfirm} disabled={pending || !allowed}
            className="rs-btn rs-btn--state px-4 py-2 rounded-lg bg-yellow-600 hover:bg-yellow-700 disabled:opacity-50 rs-text-on-state text-sm font-medium transition-colors">
            {pending ? 'Requesting stop…' : 'Stop import'}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function StatCell({ label, value }: { label: string; value: string }) {
  return (
    <div className="text-center">
      <div className="text-base font-bold text-gray-100 font-mono">{value}</div>
      <div className="text-[10px] text-gray-300 uppercase tracking-wider">{label}</div>
    </div>
  );
}
