export function RepoBadge({ name }: { name: string }) {
  return (
    <span className="rs-mark inline-flex items-center px-2 py-0.5 rounded text-xs font-medium bg-blue-900/40 text-blue-300 border border-blue-700/30 truncate max-w-[120px]">
      {name}
    </span>
  );
}

export function DirectionBadge({ direction }: { direction: string }) {
  const isToGit = direction === 'svn_to_git';
  return (
    <span
      className={`rs-mark inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
        isToGit
          ? 'bg-blue-900/50 text-blue-300'
          : 'bg-purple-900/50 text-purple-300'
      }`}
    >
      {isToGit ? 'SVN \u2192 Git' : 'Git \u2192 SVN'}
    </span>
  );
}

const ACTION_COLORS: Record<string, string> = {
  sync_cycle: 'bg-cyan-900/50 text-cyan-300',
  conflict_detected: 'bg-red-900/50 text-red-300',
  conflict_resolved: 'bg-green-900/50 text-green-300',
  sync_error: 'bg-red-900/50 text-red-300',
  webhook_received: 'bg-yellow-900/50 text-yellow-300',
  daemon_started: 'bg-emerald-900/50 text-emerald-300',
  auth_login: 'bg-indigo-900/50 text-indigo-300',
  config_updated: 'bg-orange-900/50 text-orange-300',
};

export function ActionBadge({ action }: { action: string }) {
  const label = action.replace(/_/g, ' ');
  return (
    <span
      className={`rs-mark inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
        ACTION_COLORS[action] ?? 'bg-gray-700 text-gray-300'
      }`}
    >
      {label}
    </span>
  );
}

export function SuccessIndicator({ success }: { success: boolean }) {
  return (
    <span
      className={`inline-block w-2 h-2 rounded-full ${
        success ? 'bg-green-400' : 'bg-red-400'
      }`}
      role="img"
      aria-label={success ? 'Success' : 'Failed'}
      title={success ? 'Success' : 'Failed'}
    />
  );
}
