import { AlertTriangle, CheckCircle2, CircleDashed, CircleHelp, Diamond, Hourglass, Lock, OctagonPause, PauseCircle, RefreshCw, type LucideIcon } from 'lucide-react';
import { type EffectiveHealth, type StatusQueryObservation } from '../statusObservation';
import { useStatusObservation } from '../useStatusObservation';

/** #264 S03: each health has its own shape as well as its own colour, so colour is never the only signal. */
const MARKS: Record<EffectiveHealth, { Icon: LucideIcon; tone: string }> = {
  idle: { Icon: CheckCircle2, tone: 'border-green-700 bg-green-900/30 text-green-300' },
  running: { Icon: RefreshCw, tone: 'border-cyan-700/50 bg-cyan-900/50 text-cyan-300' },
  initializing: { Icon: CircleDashed, tone: 'border-cyan-700/50 bg-cyan-900/50 text-cyan-300' },
  busy: { Icon: Hourglass, tone: 'border-cyan-700/50 bg-cyan-900/50 text-cyan-300' },
  disabled: { Icon: PauseCircle, tone: 'border-gray-600 bg-gray-800 text-gray-300' },
  paused: { Icon: OctagonPause, tone: 'border-yellow-700 bg-yellow-900/30 text-yellow-300' },
  conflict: { Icon: Diamond, tone: 'border-yellow-700 bg-yellow-900/30 text-yellow-300' },
  held: { Icon: Lock, tone: 'border-orange-700/50 bg-orange-900/50 text-orange-300' },
  error: { Icon: AlertTriangle, tone: 'border-red-700 bg-red-900/30 text-red-300' },
  unknown: { Icon: CircleHelp, tone: 'border-dashed border-gray-500 bg-gray-800 text-gray-300' },
};

/** A state mark: shape, colour and the health word. Display only. */
export function HealthMark({ health, label }: { health: EffectiveHealth; label: string }) {
  const { Icon, tone } = MARKS[health];
  return <span data-rs-health={health}
    className={`rs-mark inline-flex items-center gap-1 whitespace-nowrap rounded border px-1.5 py-0.5 text-xs font-medium ${tone}`}>
    <Icon aria-hidden="true" className="w-3.5 h-3.5 flex-shrink-0" />{label}
  </span>;
}

/**
 * Navigation uses the same failed/stale observation model as overview and detail. It shows what
 * the aggregate read returns (the worst health) and makes no request of its own. The text content
 * is unchanged ("<health>. <availability>."), so screen readers hear what they heard before.
 */
export default function SyncStatus({query}:{query:StatusQueryObservation}) {
  const view=useStatusObservation(query);
  return <div role="status" aria-live="polite" aria-atomic="true" className="rs-shell-status flex items-center space-x-2 text-sm text-gray-300">
    <HealthMark health={view.health} label={view.healthLabel}/>
    <span className="sr-only">. </span>
    <span>{view.availability}.</span>
  </div>;
}
