import { useId } from 'react';
import ReactDiffViewer, { DiffMethod } from 'react-diff-viewer-continued';

/**
 * #264: the viewer keeps its light palette exactly where no page tokens apply (Classic,
 * Individual and Team in dark). Under data-rs-bridge, pageClasses.css defines the
 * --rs-diff-* variables from the page tokens, so the diff follows the layout and theme.
 */
const LIGHT_VARIABLES = {
  diffViewerBackground: 'var(--rs-diff-bg, #fff)',
  diffViewerColor: 'var(--rs-diff-text, #212529)',
  addedBackground: 'var(--rs-diff-added-bg, #e6ffed)',
  addedColor: 'var(--rs-diff-text, #24292e)',
  removedBackground: 'var(--rs-diff-removed-bg, #ffeef0)',
  removedColor: 'var(--rs-diff-text, #24292e)',
  changedBackground: 'var(--rs-diff-changed-bg, #fffbdd)',
  wordAddedBackground: 'var(--rs-diff-word-added-bg, #acf2bd)',
  wordRemovedBackground: 'var(--rs-diff-word-removed-bg, #fdb8c0)',
  addedGutterBackground: 'var(--rs-diff-added-gutter-bg, #cdffd8)',
  removedGutterBackground: 'var(--rs-diff-removed-gutter-bg, #ffdce0)',
  gutterBackground: 'var(--rs-diff-gutter-bg, #f7f7f7)',
  gutterBackgroundDark: 'var(--rs-diff-gutter-bg-dark, #f3f1f1)',
  highlightBackground: 'var(--rs-diff-highlight-bg, #fffbdd)',
  highlightGutterBackground: 'var(--rs-diff-highlight-gutter-bg, #fff5b1)',
  codeFoldGutterBackground: 'var(--rs-diff-fold-gutter-bg, #dbedff)',
  codeFoldBackground: 'var(--rs-diff-fold-bg, #f1f8ff)',
  emptyLineBackground: 'var(--rs-diff-empty-bg, #fafbfc)',
  gutterColor: 'var(--rs-diff-gutter-text, #212529)',
  addedGutterColor: 'var(--rs-diff-gutter-text, #212529)',
  removedGutterColor: 'var(--rs-diff-gutter-text, #212529)',
  codeFoldContentColor: 'var(--rs-diff-gutter-text, #212529)',
  diffViewerTitleBackground: 'var(--rs-diff-title-bg, #fafbfc)',
  diffViewerTitleColor: 'var(--rs-diff-text, #212529)',
  diffViewerTitleBorderColor: 'var(--rs-diff-title-border, #eee)',
};

const CODE_FONT = {
  fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
  fontSize: '13px',
  lineHeight: '1.5',
};

interface Props {
  oldValue: string;
  newValue: string;
  leftTitle: string;
  rightTitle: string;
  /**
   * #264 (G06.2): the common base from the same read, shown as a third panel under the
   * two-way diff. `null` means the server returned no base; `undefined` draws no panel.
   */
  baseValue?: string | null;
  baseTitle?: string;
}

/**
 * The base is shown as read, not diffed: the two-way diff still compares the two sides,
 * as before. It takes the same --rs-diff-* tokens as the viewer, so it follows the theme.
 */
export function CommonBasePanel({ value, title }: { value: string | null; title: string }) {
  const titleId = useId();
  return (
    <div
      role="region"
      aria-labelledby={titleId}
      style={{
        background: LIGHT_VARIABLES.diffViewerBackground,
        color: LIGHT_VARIABLES.diffViewerColor,
        borderTop: `1px solid ${LIGHT_VARIABLES.diffViewerTitleBorderColor}`,
      }}
    >
      <div
        id={titleId}
        style={{
          ...CODE_FONT,
          background: LIGHT_VARIABLES.diffViewerTitleBackground,
          color: LIGHT_VARIABLES.diffViewerTitleColor,
          borderBottom: `1px solid ${LIGHT_VARIABLES.diffViewerTitleBorderColor}`,
          padding: '8px 10px',
        }}
      >
        {title}
      </div>
      {value === null ? (
        <p style={{ padding: '8px 10px', fontSize: '13px' }}>The server returned no common base for this conflict.</p>
      ) : value === '' ? (
        <p style={{ padding: '8px 10px', fontSize: '13px' }}>The common base is empty.</p>
      ) : (
        // A focusable frame lets a keyboard scroll long lines.
        <pre tabIndex={0} style={{ ...CODE_FONT, margin: 0, padding: '8px 10px', overflowX: 'auto', whiteSpace: 'pre' }}>
          {value}
        </pre>
      )}
    </div>
  );
}

export default function DiffViewer({
  oldValue,
  newValue,
  leftTitle,
  rightTitle,
  baseValue,
  baseTitle = 'Common base',
}: Props) {
  return (
    <>
    <ReactDiffViewer
      oldValue={oldValue}
      newValue={newValue}
      splitView={true}
      leftTitle={leftTitle}
      rightTitle={rightTitle}
      compareMethod={DiffMethod.LINES}
      useDarkTheme={false}
      styles={{
        variables: { light: LIGHT_VARIABLES },
        contentText: CODE_FONT,
      }}
    />
    {baseValue !== undefined && <CommonBasePanel value={baseValue} title={baseTitle} />}
    </>
  );
}
