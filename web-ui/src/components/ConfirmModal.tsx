import Dialog from './Dialog';

interface ConfirmModalProps {
  isOpen: boolean;
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
  variant?: 'danger' | 'default';
  busy?: boolean;
  error?: string;
}

export function ConfirmModal({ isOpen, title, message, confirmLabel = 'Confirm', cancelLabel = 'Cancel', onConfirm, onCancel, variant = 'default', busy = false, error }: ConfirmModalProps) {
  return <Dialog isOpen={isOpen} title={title} description={message} onClose={onCancel} busy={busy}>
    {error && <p role="alert" className="mb-3 text-red-300">{error}</p>}
    <div className="flex justify-end space-x-3">
      <button type="button" data-dialog-cancel disabled={busy} onClick={onCancel}
        className="px-4 py-2 rounded bg-gray-700 hover:bg-gray-600 text-gray-200">{cancelLabel}</button>
      <button type="button" disabled={busy} onClick={onConfirm}
        className={variant === 'danger'
          // A destructive confirmation keeps its danger fill in every layout.
          ? 'rs-btn rs-btn--danger px-4 py-2 rounded rs-text-on-danger bg-red-600 hover:bg-red-700'
          : 'rs-btn rs-btn--primary px-4 py-2 rounded rs-text-on-accent bg-blue-600 hover:bg-blue-700'}>{confirmLabel}</button>
    </div>
  </Dialog>;
}
