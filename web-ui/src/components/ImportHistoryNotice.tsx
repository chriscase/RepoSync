import type { ImportBaselineFields } from '../importBaseline';
import { importHistoryNotice, importModeLabel, normalizeImportMode } from '../importBaseline';

export default function ImportHistoryNotice({
  fields,
  compact = false,
}: {
  fields: ImportBaselineFields;
  compact?: boolean;
}) {
  const mode = normalizeImportMode(fields.import_mode);
  const notice = importHistoryNotice(fields);
  if (!notice && mode !== 'snapshot') return null;

  const revision =
    fields.starting_revision != null ? `r${fields.starting_revision}` : null;

  return (
    <div
      className={`rounded-lg border ${
        mode === 'snapshot'
          ? 'border-amber-700/60 bg-amber-950/30'
          : 'border-gray-600 bg-gray-900/40'
      } ${compact ? 'p-2.5' : 'p-3'}`}
      data-testid="import-history-notice"
    >
      <p className={`font-medium text-gray-200 ${compact ? 'text-xs' : 'text-sm'}`}>
        {importModeLabel(mode)}
        {revision && (
          <span className="ml-2 font-mono text-amber-200/90">{revision}</span>
        )}
      </p>
      {notice && (
        <p className={`mt-1 text-amber-100/90 ${compact ? 'text-[11px]' : 'text-xs'}`}>
          {notice}
        </p>
      )}
      {mode === 'snapshot' && fields.earlier_history_imported === false && (
        <p className={`mt-1 text-gray-400 ${compact ? 'text-[10px]' : 'text-[11px]'}`}>
          Bidirectional sync applies from this baseline forward; omitted SVN revisions are not replayed.
        </p>
      )}
    </div>
  );
}
