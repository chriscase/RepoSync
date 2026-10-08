import type { RepoImportMode } from '../importBaseline';
import { DEFAULT_REPO_IMPORT_MODE, importModeLabel } from '../importBaseline';

const inputClass =
  'w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent';

export default function ImportModeFields({
  mode,
  svnRevision,
  onModeChange,
  onRevisionChange,
  disabled = false,
}: {
  mode: RepoImportMode;
  svnRevision: string;
  onModeChange: (mode: RepoImportMode) => void;
  onRevisionChange: (value: string) => void;
  disabled?: boolean;
}) {
  return (
    <div className="space-y-3" data-testid="import-mode-fields">
      <p className="text-xs text-gray-400">
        Choose how the first Git baseline is built from SVN. Full history remains the default for
        existing repositories and API callers.
      </p>
      <div className="space-y-2">
        <label className="flex items-start gap-2 cursor-pointer">
          <input
            type="radio"
            name="repo-import-mode"
            data-testid="import-mode-full"
            checked={mode === DEFAULT_REPO_IMPORT_MODE}
            disabled={disabled}
            onChange={() => onModeChange('full')}
            className="mt-0.5 text-blue-600"
          />
          <span className="text-sm text-gray-300">
            <span className="font-medium">{importModeLabel('full')}</span>
            <span className="block text-xs text-gray-500 mt-0.5">
              Replay every SVN revision into Git (recommended default).
            </span>
          </span>
        </label>
        <label className="flex items-start gap-2 cursor-pointer">
          <input
            type="radio"
            name="repo-import-mode"
            data-testid="import-mode-snapshot"
            checked={mode === 'snapshot'}
            disabled={disabled}
            onChange={() => onModeChange('snapshot')}
            className="mt-0.5 text-amber-500"
          />
          <span className="text-sm text-gray-300">
            <span className="font-medium">{importModeLabel('snapshot')}</span>
            <span className="block text-xs text-gray-500 mt-0.5">
              Pin SVN HEAD or a selected revision once; earlier history is intentionally omitted.
            </span>
          </span>
        </label>
      </div>
      {mode === 'snapshot' && (
        <div>
          <label className="block text-xs text-gray-400 mb-1">SVN revision (HEAD or number)</label>
          <input
            type="text"
            className={inputClass}
            data-testid="import-svn-revision"
            value={svnRevision}
            disabled={disabled}
            onChange={(e) => onRevisionChange(e.target.value)}
            placeholder="HEAD"
          />
        </div>
      )}
    </div>
  );
}
