import { useId } from 'react';
import { LayoutTemplate } from 'lucide-react';
import { LAYOUT_CATALOG, LAYOUT_IDS, isLayoutId, type LayoutId } from './layoutCatalog';
import type { LayoutPreferenceNotice } from './layoutPreference';

export interface LayoutChooserProps {
  readonly value: LayoutId;
  readonly onChange: (layout: LayoutId) => void;
  readonly notice: LayoutPreferenceNotice | null;
}

/**
 * #262 layout chooser. A native select keeps keyboard and screen-reader behavior
 * standard and stays compact in the Classic header. It sits in one fixed shell
 * slot, so it keeps its DOM node and focus while the presentation changes.
 */
export default function LayoutChooser({ value, onChange, notice }: LayoutChooserProps) {
  const id = useId();
  const selectId = `${id}-layout`;
  const hintId = `${id}-hint`;
  const noticeId = `${id}-notice`;
  return (
    <div className="rs-layout-chooser flex items-center gap-2">
      <label htmlFor={selectId} className="rs-shell-muted flex items-center" title="Layout">
        <LayoutTemplate aria-hidden="true" className="h-4 w-4" />
        <span className="sr-only">Layout</span>
      </label>
      <select
        id={selectId}
        value={value}
        onChange={event => { const next = event.target.value; if (isLayoutId(next)) onChange(next); }}
        aria-describedby={`${hintId} ${noticeId}`}
        className="rs-layout-chooser__select rounded-md border px-2 py-1 text-xs font-medium"
        data-rs-layout-chooser=""
      >
        {LAYOUT_IDS.map(layoutId => (
          <option key={layoutId} value={layoutId}>{LAYOUT_CATALOG[layoutId].label}</option>
        ))}
      </select>
      <span id={hintId} className="sr-only">Changes only how navigation is arranged. Pages, actions, permissions and server mode stay the same.</span>
      <p id={noticeId} role="status" className="rs-layout-chooser__notice text-xs" title={notice?.detail}>
        {notice && <>{notice.short}<span className="sr-only">. {notice.detail}</span></>}
      </p>
    </div>
  );
}
