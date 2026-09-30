import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Clock, ArrowRight, Terminal, CheckCircle2 } from 'lucide-react';
import { api, type ImportStatus } from '../api';
import { getStoredUser } from '../utils/auth';

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

function phaseLabel(phase: string): string {
  const labels: Record<string, string> = {
    idle: 'Idle',
    connecting: 'Connecting',
    importing: 'Importing',
    verifying: 'Verifying',
    final_push: 'Final Push',
    completed: 'Completed',
    failed: 'Failed',
    cancelled: 'Cancelled',
  };
  return labels[phase] ?? phase;
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
            <span className="text-[9px] text-gray-500 mt-0.5 leading-none">{step.label}</span>
          </div>
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Component (self-fetching)
// ---------------------------------------------------------------------------

export default function ImportProgressCard({ repoId, repoName, hideIfIdle = false }: { repoId?: string; repoName?: string; hideIfIdle?: boolean } = {}) {
  const queryClient = useQueryClient();
  const admin = getStoredUser()?.role === 'admin';
  const { data: status, isError } = useQuery<ImportStatus>({
    queryKey: ['import-status', repoId || 'global'],
    queryFn: async () => {
      if (repoId) {
        const token = localStorage.getItem('session_token');
        const res = await fetch(`/api/repos/${repoId}/import/status`, {
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
      const res = await fetch(`/api/repos/${repoId}/import`, {
        method: 'POST', headers: { Authorization: `Bearer ${localStorage.getItem('session_token')}`,
          'X-Request-ID': crypto.randomUUID() },
      });
      if (!res.ok) throw new Error((await res.json()).error || `Import start failed (${res.status})`);
      return res.json() as Promise<{ operation_id: string }>;
    },
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['import-status', repoId] }),
  });
  const cancel = useMutation({
    mutationFn: async (operationId: string) => {
      const res = await fetch(`/api/repos/${repoId}/import/${operationId}/cancel`, {
        method: 'POST', headers: { Authorization: `Bearer ${localStorage.getItem('session_token')}` },
      });
      if (!res.ok) throw new Error((await res.json()).error || `Cancellation failed (${res.status})`);
      return res.json();
    },
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['import-status', repoId] }),
  });
  const reconcile = useMutation({
    mutationFn: async (operationId: string) => {
      const res = await fetch(`/api/repos/${repoId}/import/${operationId}/reconcile`, {
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
      <div className="bg-gray-800 border border-gray-700 rounded-xl p-5 shadow-lg">
        <div className="text-sm text-gray-500 italic">{isError ? 'Import status unavailable' : 'Loading import status...'}</div>
      </div>
    );
  }

  // Never ran an import — hide entirely if hideIfIdle
  const neverRan = status.phase === 'idle' && !status.started_at;
  if (neverRan) {
    if (hideIfIdle && !status.can_start) return null;
    return (
      <div className="bg-gray-800 border border-gray-700 rounded-xl p-5 shadow-lg">
        <div className="flex items-center justify-between">
          <div>
            <h3 className="text-sm font-semibold text-gray-200">
            SVN Import{repoName && <span className="text-blue-400 ml-1">— {repoName}</span>}
          </h3>
            <p className="text-xs text-gray-500 mt-1">No import history</p>
            {repoId && admin && status.can_start && (
              <button type="button" onClick={() => start.mutate()} disabled={start.isPending}
                className="mt-3 rounded bg-blue-600 px-3 py-1 text-xs text-white disabled:opacity-50">
                {start.isPending ? 'Starting…' : 'Start full import'}
              </button>
            )}
            {start.isError && <p className="mt-2 text-xs text-red-400">{start.error.message}</p>}
          </div>
          <a
            href="/repos"
            className="flex items-center space-x-1 text-sm text-blue-400 hover:text-blue-300 transition-colors"
          >
            <span>Manage Repos</span>
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
      <div className="bg-gray-800 border border-gray-700 rounded-xl p-5 shadow-lg">
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center space-x-2">
            <CheckCircle2 className="w-5 h-5 text-emerald-400" />
            <h3 className="text-sm font-semibold text-gray-200">
              SVN Import Complete{repoName && <span className="text-blue-400 ml-1">— {repoName}</span>}
            </h3>
          </div>
          <div className="flex items-center space-x-1 text-xs text-gray-500">
            <Clock className="w-3.5 h-3.5" />
            <span>{formatElapsed(status.started_at, status.completed_at)}</span>
          </div>
        </div>
        <div className="w-full h-2 bg-gray-700 rounded-full overflow-hidden mb-3">
          <div className="h-full rounded-full bg-emerald-500 w-full" />
        </div>
        <div className="grid grid-cols-4 gap-3 mb-3">
          <StatCell label="Revisions" value={`${status.total_revs}`} />
          <StatCell label="Commits" value={`${status.commits_created}`} />
          <StatCell label="Batches" value={`${status.batches_pushed}`} />
          <StatCell label="LFS Files" value={`${status.lfs_unique_count}`} />
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
  const percentage =
    status.total_revs > 0
      ? Math.round((status.current_rev / status.total_revs) * 100)
      : 0;

  const barColor =
    status.phase === 'failed'
      ? 'bg-red-500'
      : status.phase === 'cancelled'
        ? 'bg-yellow-500'
        : 'bg-blue-500';

  const lastLogLines = (status.log_lines ?? []).slice(-5);

  return (
    <div className="bg-gray-800 border border-gray-700 rounded-xl p-5 shadow-lg">
      {/* Header row */}
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center space-x-2">
          <span className={`w-2.5 h-2.5 rounded-full ${phaseDotColor(status.phase)}`} />
          <h3 className="text-sm font-semibold text-gray-200">
            SVN Import{repoName && <span className="text-blue-400 ml-1">— {repoName}</span>}
          </h3>
          <span className="text-xs text-gray-400 bg-gray-700 px-2 py-0.5 rounded-full">
            {status.lifecycle === 'cancel_requested' || status.lifecycle === 'cancelling'
              ? 'Cancellation requested — stopping' : status.lifecycle === 'reconciliation_required'
                ? 'Reconciliation required' : phaseLabel(status.phase)}
          </span>
        </div>
        <div className="flex items-center space-x-1 text-xs text-gray-500">
          <Clock className="w-3.5 h-3.5" />
          <span>{formatElapsed(status.started_at)}</span>
        </div>
      </div>

      {/* Phase dots */}
      <div className="flex justify-center mb-4">
        <PhaseDots phase={status.phase} />
      </div>

      {/* Progress bar */}
      <div className="w-full h-2 bg-gray-700 rounded-full overflow-hidden mb-1">
        <div
          className={`h-full rounded-full transition-all duration-500 ease-out ${barColor}`}
          style={{ width: `${percentage}%` }}
        />
      </div>
      <div className="text-right text-xs text-gray-500 mb-3 font-mono">
        {status.current_rev} / {status.total_revs}
      </div>

      {/* Stats row */}
      <div className="grid grid-cols-4 gap-3 mb-4">
        <StatCell label="Revisions" value={`${status.current_rev}/${status.total_revs}`} />
        <StatCell label="Commits" value={`${status.commits_created}`} />
        <StatCell label="Batches" value={`${status.batches_pushed}`} />
        <StatCell label="LFS Files" value={`${status.lfs_unique_count}`} />
      </div>
      {status.operation_id && <p className="mb-2 text-xs text-gray-400 font-mono">Operation {status.operation_id}</p>}
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
        <button type="button" onClick={() => cancel.mutate(status.operation_id!)} disabled={cancel.isPending}
          className="mb-3 rounded border border-yellow-500 px-3 py-1 text-xs text-yellow-200 disabled:opacity-50">
          {cancel.isPending ? 'Requesting stop…' : 'Stop import'}
        </button>
      )}
      {cancel.isError && <p className="mb-3 text-xs text-red-400">{cancel.error.message}</p>}
      {repoId && admin && status.operation_id && status.lifecycle === 'reconciliation_required' && (
        <button type="button" onClick={() => reconcile.mutate(status.operation_id!)} disabled={reconcile.isPending}
          className="mb-3 rounded border border-blue-500 px-3 py-1 text-xs text-blue-200 disabled:opacity-50">
          {reconcile.isPending ? 'Verifying remote…' : 'Verify remote'}
        </button>
      )}
      {reconcile.isError && <p className="mb-3 text-xs text-red-400">{reconcile.error.message}</p>}
      {reconcile.data?.remaining_reason && <p className="mb-3 text-xs text-yellow-300">{reconcile.data.remaining_reason}</p>}

      {/* Mini terminal */}
      {lastLogLines.length > 0 && (
        <div className="bg-gray-950 border border-gray-700 rounded-lg p-3 mb-4">
          <div className="flex items-center space-x-1.5 mb-2">
            <Terminal className="w-3 h-3 text-gray-500" />
            <span className="text-[10px] text-gray-500 uppercase tracking-wider">Log</span>
          </div>
          <div className="space-y-0.5 font-mono text-xs leading-relaxed max-h-[100px] overflow-y-auto">
            {lastLogLines.map((line, i) => (
              <div key={i} className="text-gray-400 truncate">{line}</div>
            ))}
          </div>
        </div>
      )}

      {/* Link */}
      <a
        href="/"
        className="flex items-center justify-center space-x-1 text-sm text-blue-400 hover:text-blue-300 transition-colors"
      >
        <span>View Full Import</span>
        <ArrowRight className="w-4 h-4" />
      </a>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function StatCell({ label, value }: { label: string; value: string }) {
  return (
    <div className="text-center">
      <div className="text-base font-bold text-gray-100 font-mono">{value}</div>
      <div className="text-[10px] text-gray-500 uppercase tracking-wider">{label}</div>
    </div>
  );
}
