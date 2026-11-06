import { useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { ChevronDown, ChevronRight, HardDrive, Wifi, Cpu, MemoryStick } from 'lucide-react';
import { api, type SystemMetrics } from '../api';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatBytes(bytes: number): string {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}

function formatElapsedSecs(secs: number): string {
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

function barColor(percent: number, invert = false): string {
  // invert: true means higher percent = worse (e.g. usage), false means higher = better (e.g. free)
  const effective = invert ? percent : 100 - percent;
  if (effective < 20) return 'bg-emerald-500';
  if (effective < 50) return 'bg-yellow-500';
  return 'bg-red-500';
}

function diskBarColor(freePercent: number): string {
  if (freePercent > 50) return 'bg-emerald-500';
  if (freePercent >= 20) return 'bg-yellow-500';
  return 'bg-red-500';
}

// ---------------------------------------------------------------------------
// Metric Card
// ---------------------------------------------------------------------------

function MetricCard({
  icon,
  label,
  value,
  subValue,
  barPercent,
  barColorClass,
}: {
  icon: React.ReactNode;
  label: string;
  value: React.ReactNode;
  subValue?: React.ReactNode;
  barPercent: number;
  barColorClass: string;
}) {
  return (
    <div className="bg-gray-800/60 border border-gray-700 rounded-lg p-4">
      <div className="flex items-center space-x-2 mb-2">
        {icon}
        <span className="text-xs font-medium text-gray-400 uppercase tracking-wider">{label}</span>
      </div>
      <div className="text-lg font-bold text-gray-100 font-mono mb-1">{value}</div>
      {subValue && <div className="text-xs text-gray-300 mb-2">{subValue}</div>}
      <div className="w-full h-1.5 bg-gray-700 rounded-full overflow-hidden">
        <div
          className={`h-full rounded-full transition-all duration-500 ${barColorClass}`}
          style={{ width: `${Math.min(100, Math.max(0, barPercent))}%` }}
        />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// ServerMonitor
// ---------------------------------------------------------------------------

export default function ServerMonitor() {
  const [open, setOpen] = useState(true);

  const { data: metrics } = useQuery<SystemMetrics>({
    queryKey: ['system-metrics'],
    queryFn: api.getSystemMetrics,
    refetchInterval: 15000,
    refetchIntervalInBackground: false,
  });

  // Use server-provided rates (computed from /proc/net/dev deltas server-side)
  const netRate = {
    up: metrics?.net_up_bytes_per_sec ?? 0,
    down: metrics?.net_down_bytes_per_sec ?? 0,
  };

  if (!metrics) {
    return (
      <div className="bg-gray-900 border border-gray-700 rounded-lg">
        <button
          aria-expanded={open}
          onClick={() => setOpen(o => !o)}
          className="w-full flex items-center justify-between px-4 py-3 text-sm font-semibold text-gray-300 hover:text-gray-100 transition-colors"
        >
          <span>Server Monitor</span>
          {open ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
        </button>
        {open && (
          <div className="px-4 pb-4 text-sm text-gray-300 italic">Loading metrics...</div>
        )}
      </div>
    );
  }

  const freePercent = metrics.disk_total_bytes > 0
    ? (metrics.disk_free_bytes / metrics.disk_total_bytes) * 100
    : 0;
  const usedDisk = metrics.disk_total_bytes - metrics.disk_free_bytes;

  const cpuMaxLoad = Math.max(metrics.cpu_load_1m, metrics.cpu_load_5m, metrics.cpu_load_15m, 1);
  const cpuBarPercent = Math.min(100, (metrics.cpu_load_1m / Math.max(cpuMaxLoad, 4)) * 100);

  return (
    <div className="bg-gray-900 border border-gray-700 rounded-lg">
      <button
        aria-expanded={open}
          onClick={() => setOpen(o => !o)}
        className="w-full flex items-center justify-between px-4 py-3 text-sm font-semibold text-gray-300 hover:text-gray-100 transition-colors"
      >
        <div className="flex items-center space-x-2">
          <span>Server Monitor</span>
          {metrics.git_push_active && (
            <span className="flex items-center space-x-1.5 text-xs text-emerald-400 font-normal">
              <span className="relative flex h-2 w-2">
                <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75" />
                <span className="relative inline-flex rounded-full h-2 w-2 bg-emerald-500" />
              </span>
              <span>Push Active</span>
            </span>
          )}
          {!metrics.git_push_active && metrics.svn_active && (
            <span className="flex items-center space-x-1.5 text-xs text-blue-400 font-normal">
              <span className="relative flex h-2 w-2">
                <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-blue-400 opacity-75" />
                <span className="relative inline-flex rounded-full h-2 w-2 bg-blue-500" />
              </span>
              <span>SVN Active</span>
            </span>
          )}
        </div>
        {open ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
      </button>

      {open && (
        <div className="px-4 pb-4">
          <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
            {/* Disk */}
            <MetricCard
              icon={<HardDrive className="w-4 h-4 text-gray-300" />}
              label="Disk"
              value={`${formatBytes(metrics.disk_free_bytes)} free`}
              subValue={`${formatBytes(usedDisk)} / ${formatBytes(metrics.disk_total_bytes)}`}
              barPercent={100 - freePercent}
              barColorClass={diskBarColor(freePercent)}
            />

            {/* Network */}
            <MetricCard
              icon={<Wifi className="w-4 h-4 text-gray-300" />}
              label="Network"
              value={
                metrics.git_push_active ? (
                  <span className="flex items-center space-x-2">
                    <span className="relative flex h-2.5 w-2.5">
                      <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75" />
                      <span className="relative inline-flex rounded-full h-2.5 w-2.5 bg-emerald-500" />
                    </span>
                    <span className="text-emerald-300">
                      Push{' '}
                      {metrics.git_push_elapsed_secs != null && (
                        <span className="text-emerald-400/80">
                          ({formatElapsedSecs(metrics.git_push_elapsed_secs)})
                        </span>
                      )}
                    </span>
                  </span>
                ) : metrics.svn_active ? (
                  <span className="flex items-center space-x-2">
                    <span className="relative flex h-2.5 w-2.5">
                      <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-blue-400 opacity-75" />
                      <span className="relative inline-flex rounded-full h-2.5 w-2.5 bg-blue-500" />
                    </span>
                    <span className="text-blue-300">SVN Active</span>
                  </span>
                ) : (netRate.up > 1024 || netRate.down > 1024) ? (
                  <span className="text-cyan-300">Active</span>
                ) : (
                  <span className="text-gray-400">Idle</span>
                )
              }
              subValue={
                <span>
                  ↑ {formatBytes(netRate.up)}/s &nbsp; ↓ {formatBytes(netRate.down)}/s
                </span>
              }
              barPercent={Math.min(100, (netRate.up + netRate.down) / (10 * 1024 * 1024) * 100)}
              barColorClass={
                metrics.git_push_active ? 'bg-emerald-500' :
                metrics.svn_active ? 'bg-blue-500' :
                (netRate.up + netRate.down) > 1024 ? 'bg-cyan-500' : 'bg-gray-600'
              }
            />

            {/* CPU */}
            <MetricCard
              icon={<Cpu className="w-4 h-4 text-gray-300" />}
              label="CPU"
              value={metrics.cpu_load_1m.toFixed(2)}
              subValue={`${metrics.cpu_load_1m.toFixed(2)} / ${metrics.cpu_load_5m.toFixed(2)} / ${metrics.cpu_load_15m.toFixed(2)}`}
              barPercent={cpuBarPercent}
              barColorClass={barColor(cpuBarPercent, true)}
            />

            {/* RAM */}
            <MetricCard
              icon={<MemoryStick className="w-4 h-4 text-gray-300" />}
              label="RAM"
              value={`${formatBytes(metrics.mem_used_bytes)} / ${formatBytes(metrics.mem_total_bytes)}`}
              subValue={`${metrics.mem_usage_percent.toFixed(1)}% used`}
              barPercent={metrics.mem_usage_percent}
              barColorClass={barColor(metrics.mem_usage_percent, true)}
            />
          </div>
        </div>
      )}
    </div>
  );
}
