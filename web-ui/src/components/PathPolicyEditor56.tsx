import { useId, useRef, useState } from 'react';
import {
  POLICY_FIELDS, inboundFilteringNote, type PathPolicyDraft, type PolicyFieldKey,
  addPolicyEntry, beginPolicyEdit, clearPolicyField, effectivePathPolicy, hasSurroundingWhitespace, parsePreviewPaths,
  policyEntryNoun, pathPolicyIssues, policyFieldIssues, policyFieldLabel, policyFieldSummary, policyFieldWarnings,
  effectiveRewrite, previewMappedPath, previewReasonText, removePolicyEntry, undoPolicyField, updatePolicyEntry,
} from '../pathPolicy56';

const buttonClass = 'rs-btn rs-btn--quiet inline-flex items-center px-2.5 py-1 rounded-md border border-gray-600 text-xs text-gray-200 hover:border-gray-400 rs-hover-text-strong focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-60';
const inputClass = 'rs-in flex-1 min-w-0 bg-gray-700 border border-gray-600 rounded-md px-3 py-1.5 text-sm text-gray-100 font-mono focus:outline-none focus:ring-2 focus:ring-blue-500 aria-[invalid=true]:border-red-400';

interface PathPolicyEditor56Props {
  draft: PathPolicyDraft;
  /** Absent: read-only display. Present: every edit returns a new draft. */
  onChange?: (next: PathPolicyDraft) => void;
  initialPreview?: string;
  initiallyExpanded?: boolean;
  serverError?: string;
}

type FocusTarget = { field: PolicyFieldKey; index: number } | null;

/** #56 collapsed, keyboard-operable path-policy lists with inline validation and a Git→SVN preview. */
export default function PathPolicyEditor56({ draft, onChange, initialPreview = '', initiallyExpanded = false, serverError }: PathPolicyEditor56Props) {
  const base = useId();
  const editable = !!onChange;
  const [expanded, setExpanded] = useState<Record<PolicyFieldKey, boolean>>({ allowed_paths: initiallyExpanded, blocked_patterns: initiallyExpanded });
  const [sectionOpen, setSectionOpen] = useState(initiallyExpanded);
  const [inbound, setInbound] = useState(false);
  const [samples, setSamples] = useState(initialPreview);
  const focusTarget = useRef<FocusTarget>(null);
  const change = (next: PathPolicyDraft) => onChange?.(next);
  const open = (field: PolicyFieldKey, value: boolean) => setExpanded(previous => ({ ...previous, [field]: value }));
  const claimFocus = (field: PolicyFieldKey, index: number) => (element: HTMLElement | null) => {
    const target = focusTarget.current;
    if (element && target && target.field === field && target.index === index) { element.focus(); focusTarget.current = null; }
  };

  const effective = effectivePathPolicy(draft);
  const paths = parsePreviewPaths(samples);
  const rewrite = effectiveRewrite(draft, effective.ok ? effective.allowed : []);
  const direction = inbound ? 'SVN→Git' : 'Git→SVN';
  const issue = effective.ok ? (pathPolicyIssues(draft).length ? 'Fix the path list before previewing these settings.' : rewrite.problem) : undefined;
  const outcomes = effective.ok && !issue ? paths.map(path => previewMappedPath(path, effective.allowed, effective.blocked, rewrite.prefix, inbound)) : [];
  const canEditRewrite = editable && draft.rewrite.origin !== 'unsupported' && draft.rewrite.origin !== 'unknown';
  const changeRewrite = (mode: PathPolicyDraft['rewrite']['mode'], prefix: string) =>
    change({...draft, rewrite: {...draft.rewrite, mode, prefix}});
  const included = outcomes.filter(outcome => outcome.included).length;

  return (
    <section aria-labelledby={`${base}-heading`} className="col-span-full space-y-3" data-testid="path-policy-56">
      <h3 id={`${base}-heading`} className="rs-heading text-sm font-semibold text-gray-100">
        <button type="button" className={buttonClass} aria-expanded={sectionOpen || !!serverError}
          aria-controls={`${base}-body`} onClick={() => setSectionOpen(!sectionOpen)}>Path policy</button>
      </h3>
      <div id={`${base}-body`} hidden={!sectionOpen && !serverError} className="space-y-3">
      {serverError && <p id={`${base}-server-error`} role="alert" className="text-xs text-red-300">{serverError}</p>}
      <div role="note" aria-labelledby={`${base}-limits`} className="rs-bn rs-bn--warn rounded-md border border-amber-700/60 bg-amber-900/20 px-3 py-2 text-xs text-amber-100">
        <p id={`${base}-limits`} className="font-medium">Limitations in this build</p>
        <ul className="list-disc pl-4">
          <li>{rewrite.prefix ? 'A Team strip prefix requires a fresh projected snapshot into an empty Git destination. It does not rewrite existing Git history.' : inboundFilteringNote[draft.inboundFiltering]}</li>
          <li>Scope and blocked patterns always use SVN paths. Rewriting is supported for new paused direct root registrations without automatic conflict merges or LFS.</li>
        </ul>
      </div>
      {draft.notices.map(notice => <p key={notice} className="text-xs text-amber-200">{notice}</p>)}

      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        {POLICY_FIELDS.map(field => {
          const value = draft[field];
          const { origin } = value;
          const id = `${base}-${field}`;
          const label = policyFieldLabel(field);
          const noun = policyEntryNoun(field);
          const plural = policyEntryNoun(field, 2);
          const issues = editable ? policyFieldIssues(field, value) : [];
          const listIssue = issues.find(issue => issue.index === null);
          const forced = issues.length > 0;
          const isOpen = expanded[field] || forced;
          const shown = value.mode === 'edit' ? value.entries : value.mode === 'clear' ? [] : origin.kind === 'known' ? origin.entries : [];
          const warnings = value.mode === 'edit' ? policyFieldWarnings(field, value.entries) : [];
          const canClear = origin.kind === 'malformed' || (origin.kind === 'known' && origin.stored === 'list');
          return (
            <div key={field} role="group" aria-labelledby={`${id}-heading`} aria-describedby={`${id}-summary`}
              className="rounded-md border border-gray-700 bg-gray-900/40 p-3 space-y-2" data-testid={`path-policy-${field}`}>
              <div className="flex items-center justify-between gap-2">
                <h4 id={`${id}-heading`} className="text-sm font-medium text-gray-200">{label}</h4>
                {origin.kind !== 'unavailable' && (
                  <button type="button" className={buttonClass} aria-expanded={isOpen} aria-controls={`${id}-panel`}
                    disabled={forced} onClick={() => open(field, !isOpen)}>
                    {isOpen ? 'Hide' : 'Show'}<span className="sr-only"> {plural}</span>
                  </button>
                )}
              </div>
              <p id={`${id}-summary`} className="text-xs text-gray-300">
                {policyFieldSummary(field, value)}{forced ? ` — ${issues.length} to fix before saving` : ''}
              </p>
              {origin.kind !== 'known' && value.mode === 'keep' && (
                <p className={`text-xs ${origin.kind === 'malformed' ? 'text-amber-200' : 'text-gray-300'}`}>{origin.reason}</p>
              )}
              {origin.kind !== 'unavailable' && (
                <div id={`${id}-panel`} hidden={!isOpen} className="space-y-2">
                  {editable && value.mode === 'edit' ? (
                    <ol className="space-y-2">
                      {value.entries.map((entry, index) => {
                        const issue = issues.find(candidate => candidate.index === index);
                        const inputId = `${id}-entry-${index}`;
                        return (
                          <li key={index} className="space-y-1">
                            <label htmlFor={inputId} className="block text-xs text-gray-300">{noun.charAt(0).toUpperCase() + noun.slice(1)} {index + 1}</label>
                            <div className="flex gap-2">
                              <input id={inputId} type="text" className={inputClass} value={entry} spellCheck={false} autoComplete="off"
                                aria-invalid={issue ? true : undefined} aria-describedby={issue ? `${inputId}-error` : undefined}
                                ref={claimFocus(field, index)}
                                onChange={event => change(updatePolicyEntry(draft, field, index, event.target.value))} />
                              <button type="button" className={buttonClass} onClick={() => {
                                focusTarget.current = { field, index: value.entries.length > 1 ? Math.max(index - 1, 0) : -1 };
                                change(removePolicyEntry(draft, field, index));
                              }}>Remove<span className="sr-only"> {noun} {index + 1}</span></button>
                            </div>
                            {issue && <p id={`${inputId}-error`} role="alert" className="text-xs text-red-300">{issue.message}</p>}
                          </li>
                        );
                      })}
                    </ol>
                  ) : shown.length ? (
                    <ul className="flex flex-wrap gap-1" aria-label={`Stored ${plural}`}>
                      {shown.map((entry, index) => (
                        <li key={index}><code className="rounded bg-gray-800 px-1.5 py-0.5 text-xs text-gray-100">{entry}</code></li>
                      ))}
                    </ul>
                  ) : <p className="text-xs text-gray-300">No entries.</p>}
                  {listIssue && <p role="alert" className="text-xs text-red-300">{listIssue.message}</p>}
                  {warnings.length > 0 && (
                    <ul className="list-disc pl-4 text-xs text-amber-200">{warnings.map(warning => <li key={warning}>{warning}</li>)}</ul>
                  )}
                </div>
              )}
              {editable && origin.kind !== 'unavailable' && (
                <div className="flex flex-wrap gap-2">
                  {value.mode === 'keep' && (
                    <button type="button" className={buttonClass} onClick={() => { open(field, true); change(beginPolicyEdit(draft, field)); }}>
                      {origin.kind === 'malformed' ? 'Replace with a new list' : 'Edit'}<span className="sr-only"> {plural}</span>
                    </button>
                  )}
                  {value.mode === 'edit' && (
                    <button type="button" className={buttonClass} ref={claimFocus(field, -1)} onClick={() => {
                      focusTarget.current = { field, index: value.entries.length };
                      open(field, true);
                      change(addPolicyEntry(draft, field));
                    }}>Add<span className="sr-only"> {noun}</span></button>
                  )}
                  {value.mode !== 'clear' && canClear && (
                    <button type="button" className={buttonClass} onClick={() => change(clearPolicyField(draft, field))}>
                      Clear<span className="sr-only"> stored {plural}</span>
                    </button>
                  )}
                  {value.mode !== 'keep' && (
                    <button type="button" className={buttonClass} onClick={() => change(undoPolicyField(draft, field))}>
                      Undo<span className="sr-only"> {plural} changes</span>
                    </button>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>

      <div role="group" aria-labelledby={`${base}-rewrite-heading`} className="rounded-md border border-gray-700 bg-gray-900/40 p-3 space-y-2">
        <h4 id={`${base}-rewrite-heading`} className="text-sm font-medium text-gray-200">Strip SVN prefix</h4>
        {draft.rewrite.mode === 'edit' ? (
          <>
            <label htmlFor={`${base}-rewrite`} className="block text-xs text-gray-300">SVN directory prefix</label>
            <input id={`${base}-rewrite`} className={inputClass} value={draft.rewrite.prefix}
              aria-invalid={!!issue || !!serverError || undefined}
              aria-describedby={issue ? `${base}-rewrite-error` : serverError ? `${base}-server-error` : undefined}
              onChange={event => changeRewrite('edit', event.target.value)} />
          </>
        ) : <p className="text-xs text-gray-300">{draft.rewrite.mode === 'clear' ? 'Will be cleared on save'
          : draft.rewrite.saved || (canEditRewrite && draft.rewrite.origin === 'malformed' ? 'Malformed stored rule' : draft.rewrite.origin === 'unset' ? 'No rewrite' : 'Rewrite editing not available from this server')}</p>}
        {issue && draft.rewrite.mode === 'edit' && <p id={`${base}-rewrite-error`} role="alert" className="text-xs text-red-300">{issue}</p>}
        {canEditRewrite && <div className="flex gap-2">
          {draft.rewrite.mode === 'keep' && <button type="button" className={buttonClass} onClick={() => changeRewrite('edit', draft.rewrite.saved)}>Edit strip prefix</button>}
          {draft.rewrite.mode !== 'clear' && ['configured', 'malformed'].includes(draft.rewrite.origin) &&
            <button type="button" className={buttonClass} onClick={() => changeRewrite('clear', '')}>Clear strip prefix</button>}
          {draft.rewrite.mode !== 'keep' && <button type="button" className={buttonClass} onClick={() => changeRewrite('keep', draft.rewrite.saved)}>Undo strip prefix changes</button>}
        </div>}
      </div>

      <div role="group" aria-labelledby={`${base}-preview-heading`} className="rounded-md border border-gray-700 bg-gray-900/40 p-3 space-y-2">
        <h4 id={`${base}-preview-heading`} className="text-sm font-medium text-gray-200">{direction} preview</h4>
        <p className="text-xs text-gray-300">Uses the lists as they would be after saving and the same matching as the sync engine. Nothing is sent to the server.</p>
        {draft.inboundFiltering === 'managed_incremental' && <label className="block text-xs text-gray-300">
          Preview direction{' '}
          <select className={inputClass} value={inbound ? 'inbound' : 'outbound'} onChange={event => setInbound(event.target.value === 'inbound')}>
            <option value="outbound">Git to SVN</option>
            <option value="inbound">SVN to Git</option>
          </select>
        </label>}
        <label htmlFor={`${base}-samples`} className="block text-xs text-gray-300">Sample {inbound ? 'SVN' : 'Git'} paths (one per line)</label>
        <textarea id={`${base}-samples`} rows={3} value={samples} onChange={event => setSamples(event.target.value)}
          className="rs-in w-full bg-gray-700 border border-gray-600 rounded-md px-3 py-2 text-sm text-gray-100 font-mono focus:outline-none focus:ring-2 focus:ring-blue-500"
          placeholder={'src/main.rs\nbuild/output.exe'} spellCheck={false} />
        {!effective.ok ? (
          <p className="text-xs text-amber-200">{effective.reason}</p>
        ) : issue ? <p className="text-xs text-amber-200">{issue}</p> : outcomes.length > 0 && (
          <table className="rs-tbl w-full text-xs text-left" data-testid="path-policy-preview">
            <caption className="sr-only">{direction} preview results</caption>
            <thead><tr><th scope="col" className="py-1 pr-2 text-gray-300">{inbound ? 'SVN' : 'Git'} path</th><th scope="col" className="py-1 text-gray-300">Result</th></tr></thead>
            <tbody>
              {outcomes.map((outcome, index) => (
                <tr key={index} className="border-t border-gray-700">
                  <td className="py-1 pr-2">
                    <code className="text-gray-100 whitespace-pre">{outcome.path}</code>
                    {hasSurroundingWhitespace(outcome.path) && <span className="block text-gray-300">Includes leading or trailing whitespace</span>}
                  </td>
                  <td className={`py-1 ${outcome.included ? 'text-green-300' : 'text-amber-200'}`}>{('invalid' in outcome && outcome.invalid) ? 'Excluded: not a canonical relative file path'
                      : outcome.included ? `Included in ${direction}` : outcome.reason ? previewReasonText(outcome.reason) : 'Excluded: the prefix directory is not a file'}
                    {outcome.included && rewrite.prefix && <code className="block whitespace-pre">{outcome.target}</code>}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <p role="status" className="text-xs text-gray-300">
          {effective.ok && outcomes.length
            ? `${included} of ${outcomes.length} sample ${outcomes.length === 1 ? 'path' : 'paths'} included in ${direction}.`
            : ''}
        </p>
      </div>
      </div>
    </section>
  );
}
