import type { CommitMapEntry } from '../api';
import { commitMapTargetDisplay } from '../commitMapTarget';

/** Git SHA cell for the commit-map tables; explicit when a row has no Git target (#54). */
export function CommitMapTargetCell({ entry }: { entry: Pick<CommitMapEntry, 'git_sha' | 'target_state'> }) {
  const display = commitMapTargetDisplay(entry);
  const tone = display.mono ? 'font-mono text-purple-400' : 'italic text-gray-400';
  return (
    <td className={`px-6 py-3 text-sm ${tone} truncate max-w-[200px]`} title={display.title}>
      {display.text}
    </td>
  );
}
