import React from 'react';
import type { ActivityEvent } from '@appforgeapps/uiforge';
import {
  RefreshCw,
  AlertTriangle,
  CheckCircle,
  XCircle,
  Zap,
  Play,
  LogIn,
  Settings,
  GitCommit,
  ArrowRight,
} from 'lucide-react';
import { RepoBadge, DirectionBadge, ActionBadge, SuccessIndicator } from './Badges';

/**
 * Icon color mapping matching the existing ActionBadge color scheme.
 */
const ICON_CONFIG: Record<string, { icon: typeof RefreshCw; className: string }> = {
  sync_cycle:        { icon: RefreshCw,     className: 'text-cyan-400' },
  conflict_detected: { icon: AlertTriangle, className: 'text-red-400' },
  conflict_resolved: { icon: CheckCircle,   className: 'text-green-400' },
  sync_error:        { icon: XCircle,       className: 'text-red-400' },
  webhook_received:  { icon: Zap,           className: 'text-yellow-400' },
  daemon_started:    { icon: Play,          className: 'text-emerald-400' },
  auth_login:        { icon: LogIn,         className: 'text-indigo-400' },
  config_updated:    { icon: Settings,      className: 'text-orange-400' },
};

/**
 * Custom icon renderer for audit events in UIForgeActivityStream.
 */
export function renderAuditIcon(event: ActivityEvent): React.ReactNode {
  const action = (event.metadata?.action as string) ?? event.type;
  const config = ICON_CONFIG[action];
  if (config) {
    const Icon = config.icon;
    return <Icon size={16} className={config.className} />;
  }
  return <GitCommit size={16} className="text-gray-400" />;
}

/**
 * Custom event renderer for audit entries in UIForgeActivityStream.
 * Preserves the rich badge display from the original Dashboard.
 */
export function renderAuditEvent(event: ActivityEvent): React.ReactNode {
  const meta = event.metadata ?? {};
  const action = meta.action as string | undefined;
  const success = meta.success as boolean | undefined;
  const direction = meta.direction as string | null | undefined;
  const svnRev = meta.svn_rev as number | null | undefined;
  const gitSha = meta.git_sha as string | null | undefined;
  const author = meta.author as string | null | undefined;
  const repoName = meta.repository as string | undefined;

  return (
    <div className="flex items-center justify-between w-full gap-2 min-w-0">
      <div className="flex items-center gap-2 min-w-0 flex-1">
        {success !== undefined && <SuccessIndicator success={success} />}
        {repoName && repoName !== 'default' && <RepoBadge name={repoName} />}
        {direction && <DirectionBadge direction={direction} />}
        {action && <ActionBadge action={action} />}
        <span className="text-sm text-gray-200 line-clamp-2" title={event.description ?? event.title}>
          {event.description ?? event.title}
        </span>
        {author && (
          <span className="text-sm text-gray-400 flex-shrink-0">by {author}</span>
        )}
      </div>
      <div className="flex items-center gap-2 flex-shrink-0 text-xs text-gray-500">
        {svnRev != null && (
          <span className="font-mono text-blue-400">r{svnRev}</span>
        )}
        {gitSha && (
          <span className="font-mono text-purple-400">
            {gitSha.substring(0, 8)}
          </span>
        )}
        {event.timestamp && (
          <span className="text-gray-500" title={String(event.timestamp)}>
            {formatTimeAgo(String(event.timestamp))}
          </span>
        )}
      </div>
    </div>
  );
}

function formatTimeAgo(dateStr: string): string {
  try {
    const date = new Date(dateStr);
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffSec = Math.floor(diffMs / 1000);
    if (diffSec < 60) return `${diffSec}s ago`;
    const diffMin = Math.floor(diffSec / 60);
    if (diffMin < 60) return `${diffMin}m ago`;
    const diffHr = Math.floor(diffMin / 60);
    if (diffHr < 24) return `${diffHr}h ago`;
    const diffDay = Math.floor(diffHr / 24);
    return `${diffDay}d ago`;
  } catch {
    return dateStr;
  }
}

/**
 * Custom icon renderer for sync record events.
 */
export function renderSyncRecordIcon(event: ActivityEvent): React.ReactNode {
  const direction = event.metadata?.direction as string | undefined;
  if (direction === 'svn_to_git') {
    return <ArrowRight size={16} className="text-blue-400" />;
  }
  return <ArrowRight size={16} className="text-purple-400" />;
}

/**
 * Custom event renderer for sync records in UIForgeActivityStream.
 */
export function renderSyncRecordEvent(event: ActivityEvent): React.ReactNode {
  const meta = event.metadata ?? {};
  const direction = meta.direction as string | undefined;
  const svnRev = meta.svn_rev as number | null | undefined;
  const gitSha = meta.git_sha as string | null | undefined;
  const author = meta.author as string | undefined;
  const status = meta.status as string | undefined;
  const repoName = meta.repository as string | undefined;

  const statusColor =
    status === 'applied'
      ? 'text-green-400'
      : status === 'failed'
        ? 'text-red-400'
        : 'text-yellow-400';

  const statusSymbol =
    status === 'applied'
      ? '\u2713'
      : status === 'failed'
        ? '\u2717'
        : '\u25CB';

  return (
    <div className="flex items-center justify-between w-full gap-2 min-w-0">
      <div className="flex items-center gap-2 min-w-0 flex-1">
        <span className={`text-xs font-bold ${statusColor}`}>{statusSymbol}</span>
        {repoName && repoName !== 'default' && <RepoBadge name={repoName} />}
        {direction && <DirectionBadge direction={direction} />}
        <span className="text-sm text-gray-200 line-clamp-2">{event.title}</span>
      </div>
      <div className="flex items-center gap-2 flex-shrink-0 text-xs text-gray-500">
        {author && <span className="text-gray-400">{author}</span>}
        {svnRev != null && (
          <span className="font-mono text-blue-400">r{svnRev}</span>
        )}
        {gitSha && (
          <span className="font-mono text-purple-400">
            {gitSha.substring(0, 8)}
          </span>
        )}
        {event.timestamp && (
          <span title={String(event.timestamp)}>
            {formatTimeAgo(String(event.timestamp))}
          </span>
        )}
      </div>
    </div>
  );
}
