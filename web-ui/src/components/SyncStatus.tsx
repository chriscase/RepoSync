import { type SyncStatus as SyncStatusType } from '../api';
import { syncStateVisual } from '../utils/syncStateColor';

interface Props {
  status: SyncStatusType;
}

export default function SyncStatus({ status }: Props) {
  const visual = syncStateVisual(status.state);
  const stateColor =
    visual === 'healthy'
      ? 'bg-green-400'
      : visual === 'error'
        ? 'bg-red-400'
        : visual === 'attention'
          ? 'bg-yellow-400'
          : visual === 'active'
            ? 'bg-blue-400'
            : 'bg-gray-500';

  const stateLabel =
    status.state.charAt(0).toUpperCase() + status.state.slice(1);

  return (
    <div className="flex items-center space-x-2 text-sm text-gray-300">
      <span className={`inline-block w-2 h-2 rounded-full ${stateColor}`} />
      <span>{stateLabel}</span>
      {status.last_sync_at && (
        <span className="text-gray-500">
          Last sync: {new Date(status.last_sync_at).toLocaleTimeString()}
        </span>
      )}
    </div>
  );
}
