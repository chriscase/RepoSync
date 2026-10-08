import type { ImportBaselineFields } from '../importBaseline';
import {
  importBaselineFailureNotice,
  importHistoryNotice,
  importModeLabel,
  normalizeImportMode,
} from '../importBaseline';

export default function ImportHistoryNotice({
  fields,
  compact = false,
}: {
  fields: ImportBaselineFields;
  compact?: boolean;
}) {
  const mode = normalizeImportMode(fields.import_mode);
  const boundary = importHistoryNotice(fields);
  const failure = importBaselineFailureNotice(fields);

  if (boundary) {
    const revision =
      fields.starting_revision != null ? `r${fields.starting_revision}` : null;
    return (
      <div
        className={`rounded-lg border border-amber-700/60 bg-amber-950/30 ${
          compact ? 'p-2.5' : 'p-3'
        }`}
        data-testid="import-history-notice"
      >
        <p className={`font-medium text-gray-200 ${compact ? 'text-xs' : 'text-sm'}`}>
          {importModeLabel('snapshot')}
          {revision && (
            <span className="ml-2 font-mono text-amber-200/90">{revision}</span>
          )}
        </p>
        <p className={`mt-1 text-amber-100/90 ${compact ? 'text-[11px]' : 'text-xs'}`}>
          {boundary}
        </p>
      </div>
    );
  }

  if (failure) {
    return (
      <div
        className={`rounded-lg border border-red-700/60 bg-red-950/30 ${
          compact ? 'p-2.5' : 'p-3'
        }`}
        data-testid="import-baseline-failure"
      >
        <p className={`font-medium text-red-200 ${compact ? 'text-xs' : 'text-sm'}`}>
          Snapshot import refused
        </p>
        <p className={`mt-1 text-red-100/90 ${compact ? 'text-[11px]' : 'text-xs'}`}>
          {failure}
        </p>
      </div>
    );
  }

  if (mode === 'full' && fields.earlier_history_imported === false && fields.history_boundary) {
    return (
      <div
        className={`rounded-lg border border-gray-600 bg-gray-900/40 ${
          compact ? 'p-2.5' : 'p-3'
        }`}
        data-testid="import-history-notice"
      >
        <p className={`font-medium text-gray-200 ${compact ? 'text-xs' : 'text-sm'}`}>
          {importModeLabel('full')}
        </p>
        <p className={`mt-1 text-gray-300 ${compact ? 'text-[11px]' : 'text-xs'}`}>
          {fields.history_boundary}
        </p>
      </div>
    );
  }

  return null;
}
