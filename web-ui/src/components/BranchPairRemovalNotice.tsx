import type { BranchPairRemovalNotice as Notice } from '../branchPairRemoval';

export default function BranchPairRemovalNotice({ notice }: { notice: Notice }) {
  const queued = notice.outcome === 'in_progress';
  const partial = notice.warnings.length > 0;
  const tone = queued || partial
    ? 'bg-amber-900/30 border-amber-700 text-amber-100'
    : 'bg-blue-900/30 border-blue-700 text-blue-100';
  const title = queued
    ? 'Removal is not complete.'
    : partial
      ? 'Branch pair removed with cleanup warnings.'
      : 'Branch pair removed.';

  return (
    <div
      data-testid="branch-pair-removal-notice"
      data-outcome={notice.outcome}
      className={`border rounded-lg p-4 text-sm ${tone}`}
      role="status"
    >
      <p className="font-medium">{title}</p>
      {notice.name && <p className="mt-1">{notice.name}</p>}
      {notice.message && <p className="mt-1">{notice.message}</p>}
      {notice.operationId && (
        <p className="mt-1 font-mono text-xs">Operation {notice.operationId}</p>
      )}
      {partial && (
        <ul className="mt-2 list-disc pl-5 space-y-1">
          {notice.warnings.map((warning) => (
            <li key={warning}>{warning}</li>
          ))}
        </ul>
      )}
    </div>
  );
}
