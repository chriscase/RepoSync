import { type StatusQueryObservation } from '../statusObservation';
import { useStatusObservation } from '../useStatusObservation';

/** Navigation uses the same failed/stale observation model as overview and detail. */
export default function SyncStatus({query}:{query:StatusQueryObservation}) {
  const view=useStatusObservation(query);
  const color=view.health==='idle' ? 'bg-green-400' : ['held','paused','error','conflict'].includes(view.health) ? 'bg-yellow-400':'bg-gray-500';
  return <div role="status" aria-live="polite" aria-atomic="true" className="flex items-center space-x-2 text-sm text-gray-300">
    <span aria-hidden="true" className={`inline-block w-2 h-2 rounded-full ${color}`}/>
    <span>{view.healthLabel}. {view.availability}.</span>
  </div>;
}
