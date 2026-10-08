import type { ManagedRemovalReceipt } from '../managedRemoval';
import { managedRemovalStateLabel } from '../managedRemoval';

export default function ManagedRemovalReceiptNotice({ receipt }: { receipt: ManagedRemovalReceipt }) {
  const completed = receipt.state.toLowerCase() === 'completed';
  const tone = completed
    ? 'bg-blue-900/30 border-blue-700 text-blue-100'
    : 'bg-amber-900/30 border-amber-700 text-amber-100';

  return (
    <div
      data-testid="managed-removal-receipt-notice"
      className={`border rounded-lg p-4 text-sm ${tone}`}
      role="status"
    >
      <p className="font-medium">{managedRemovalStateLabel(receipt.state)}</p>
      {receipt.message && <p className="mt-1">{receipt.message}</p>}
      <p className="mt-1 font-mono text-xs">Operation {receipt.operationId}</p>
    </div>
  );
}
