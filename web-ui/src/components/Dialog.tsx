import { useEffect, useLayoutEffect, useId, useRef, type ReactNode } from 'react';

const useModalLayoutEffect = typeof window === 'undefined' ? useEffect : useLayoutEffect;

interface DialogProps {
  isOpen?: boolean;
  title: ReactNode;
  description: ReactNode;
  onClose: () => void;
  busy?: boolean;
  children: ReactNode;
  className?: string;
  testId?: string;
}

/** Native modal containment makes the rest of the document inert, including to AT. */
export default function Dialog({ isOpen = true, title, description, onClose, busy = false, children, className = '', testId }: DialogProps) {
  const id = useId();
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (!isOpen) return;
    const dialog = ref.current;
    if (!dialog) return;
    const previous = document.activeElement;
    dialog.showModal();
    // A destructive action is never the default focused control.
    const cancel = dialog.querySelector<HTMLElement>('[data-dialog-cancel]:not(:disabled)');
    (cancel ?? dialog.querySelector<HTMLElement>(`#${CSS.escape(id)}-title`))?.focus();
    return () => {
      const active = document.activeElement;
      // #264: when this dialog was opened from inside another dialog, that dialog takes focus
      // while this one unmounts; focus parked on the enclosing dialog is still ours to restore.
      const ownedFocus = dialog.contains(active) || active === document.body
        || (active instanceof HTMLDialogElement && previous instanceof Node && active !== dialog && active.contains(previous));
      dialog.close();
      // Never steal focus from a replacement route/session. If the completed
      // action removed its trigger, return to the current main landmark.
      if (!ownedFocus && document.activeElement !== document.body) return;
      if (previous instanceof HTMLElement && previous.isConnected && !previous.matches(':disabled')) previous.focus();
      else document.querySelector<HTMLElement>('main[tabindex="-1"]')?.focus();
    };
  }, [isOpen, id]);

  // A currently focused control can become disabled when a request starts or a
  // fresh safety observation refuses confirmation. Keep that transition inside.
  useModalLayoutEffect(() => {
    const dialog = ref.current;
    if (isOpen && dialog?.open && (!dialog.contains(document.activeElement) || (document.activeElement instanceof HTMLElement && document.activeElement.matches(':disabled')))) dialog.focus();
  });

  if (!isOpen) return null;
  return <dialog ref={ref} tabIndex={-1} data-testid={testId} aria-labelledby={`${id}-title`}
    aria-describedby={`${id}-description`} aria-modal="true" aria-busy={busy || undefined}
    // #264: a confirmation can open inside another dialog. React passes a nested dialog's
    // cancel (Escape) up to its ancestors, so each dialog handles only its own.
    onCancel={event => { if (event.target !== event.currentTarget) return; event.preventDefault(); event.stopPropagation(); if (!busy) onClose(); }}
    onKeyDownCapture={event => {
      if (event.key !== 'Tab') return;
      const dialog = event.currentTarget;
      const controls = Array.from(dialog.querySelectorAll<HTMLElement>('button, [href], input:not([type="hidden"]), select, textarea, [tabindex]'))
        .filter(element => element.tabIndex >= 0 && !element.matches(':disabled, [inert]') && element.getClientRects().length > 0 && getComputedStyle(element).visibility !== 'hidden');
      const first = controls[0], last = controls[controls.length - 1];
      if (!first) { event.preventDefault(); dialog.focus(); return; }
      if (!controls.includes(document.activeElement as HTMLElement) || (event.shiftKey ? document.activeElement === first : document.activeElement === last)) {
        event.preventDefault();
        (event.shiftKey ? last : first)?.focus();
      }
    }}
    className={`app-dialog m-auto max-h-[90vh] w-[calc(100%-2rem)] max-w-lg overflow-y-auto rounded-lg border border-gray-600 bg-gray-800 p-6 text-gray-100 shadow-xl ${className}`}>
    <h2 id={`${id}-title`} tabIndex={-1} className="rs-heading text-lg font-semibold mb-2">{title}</h2>
    <div id={`${id}-description`} className="text-sm text-gray-300 mb-4">{description}</div>
    {busy && <p role="status" className="mb-3 text-sm text-gray-300">Request in progress. Please wait before closing this dialog.</p>}
    {children}
  </dialog>;
}
