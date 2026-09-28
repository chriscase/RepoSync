import { Check, Plug, Download, ShieldCheck, Upload, CircleCheck, XCircle } from 'lucide-react';

// ---------------------------------------------------------------------------
// Phase definitions
// ---------------------------------------------------------------------------

interface PhaseDef {
  key: string;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  description: string;
}

const PHASES: PhaseDef[] = [
  { key: 'connecting', label: 'Connect', icon: Plug, description: 'Connecting to the SVN server and reading repository metadata...' },
  { key: 'importing', label: 'Import & Push', icon: Download, description: 'Converting SVN revisions to Git commits and pushing in batches...' },
  { key: 'verifying', label: 'Verify', icon: ShieldCheck, description: 'Verifying that all files match between SVN and the Git repository...' },
  { key: 'final_push', label: 'Final Push', icon: Upload, description: 'Pushing remaining commits and tags to the remote repository...' },
  { key: 'completed', label: 'Complete', icon: CircleCheck, description: 'Import finished successfully. Your SVN history is now in Git.' },
];

// Map each phase key to its index for ordering
const PHASE_INDEX: Record<string, number> = {};
PHASES.forEach((p, i) => { PHASE_INDEX[p.key] = i; });

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export default function ImportPhaseGraphic({ currentPhase }: { currentPhase: string }) {
  const currentIdx = PHASE_INDEX[currentPhase] ?? -1;
  const isFailed = currentPhase === 'failed';
  const isCancelled = currentPhase === 'cancelled';
  const isIdle = currentPhase === 'idle';

  const getNodeStyle = (idx: number) => {
    if (isFailed && idx === currentIdx) {
      return 'bg-red-600/20 border-red-500 text-red-400 ring-2 ring-red-500/30';
    }
    if (isCancelled && idx === currentIdx) {
      return 'bg-yellow-600/20 border-yellow-500 text-yellow-400';
    }
    if (idx < currentIdx) {
      // Completed phase
      return 'bg-emerald-600/20 border-emerald-500 text-emerald-400';
    }
    if (idx === currentIdx && !isIdle) {
      // Active phase
      return 'bg-blue-600/20 border-blue-500 text-blue-400 ring-2 ring-blue-500/40 animate-pulse';
    }
    // Future / idle
    return 'bg-gray-800 border-gray-600 text-gray-500';
  };

  const getConnectorStyle = (idx: number) => {
    if (idx < currentIdx) return 'bg-emerald-500';
    return 'bg-gray-700';
  };

  const getIconForNode = (idx: number, phase: PhaseDef) => {
    if (idx < currentIdx) {
      return <Check className="w-4 h-4" />;
    }
    if (isFailed && idx === currentIdx) {
      return <XCircle className="w-4 h-4" />;
    }
    const Icon = phase.icon;
    return <Icon className="w-4 h-4" />;
  };

  // Determine the description to show
  let description = '';
  if (isIdle) {
    description = 'Ready to begin the import process.';
  } else if (isFailed) {
    description = 'The import encountered an error. Check the log for details.';
  } else if (isCancelled) {
    description = 'The import was cancelled by the user.';
  } else if (currentIdx >= 0 && currentIdx < PHASES.length) {
    description = PHASES[currentIdx].description;
  }

  return (
    <div className="mb-6">
      {/* Phase nodes - horizontal on desktop, vertical on mobile */}
      <div className="hidden md:flex items-center justify-between">
        {PHASES.map((phase, i) => (
          <div key={phase.key} className="flex items-center flex-1 last:flex-none">
            {/* Node */}
            <div className="flex flex-col items-center">
              <div
                className={`w-10 h-10 rounded-lg border-2 flex items-center justify-center transition-all duration-300 ${getNodeStyle(i)}`}
              >
                {getIconForNode(i, phase)}
              </div>
              <span className={`mt-1.5 text-xs font-medium whitespace-nowrap ${
                i <= currentIdx && !isIdle ? 'text-gray-300' : 'text-gray-500'
              }`}>
                {phase.label}
              </span>
            </div>
            {/* Connector arrow */}
            {i < PHASES.length - 1 && (
              <div className="flex-1 flex items-center mx-2 mt-[-1.25rem]">
                <div className={`flex-1 h-0.5 ${getConnectorStyle(i)} transition-colors duration-300`} />
                <div className={`w-0 h-0 border-t-[4px] border-t-transparent border-b-[4px] border-b-transparent border-l-[6px] ${
                  i < currentIdx ? 'border-l-emerald-500' : 'border-l-gray-700'
                } transition-colors duration-300`} />
              </div>
            )}
          </div>
        ))}
      </div>

      {/* Mobile: vertical list */}
      <div className="md:hidden space-y-2">
        {PHASES.map((phase, i) => (
          <div key={phase.key} className="flex items-center space-x-3">
            <div
              className={`w-8 h-8 rounded-lg border-2 flex items-center justify-center flex-shrink-0 transition-all duration-300 ${getNodeStyle(i)}`}
            >
              {getIconForNode(i, phase)}
            </div>
            <span className={`text-sm font-medium ${
              i <= currentIdx && !isIdle ? 'text-gray-300' : 'text-gray-500'
            }`}>
              {phase.label}
            </span>
          </div>
        ))}
      </div>

      {/* Phase description */}
      {description && (
        <p className="text-sm text-gray-400 mt-4 text-center italic">{description}</p>
      )}
    </div>
  );
}
