import { useEffect, useRef, useState } from 'react';
import { api, type ImportStatus } from './api';

const pollInterval = 1500;
const maximumRetryDelay = 12000;
const requestTimeout = 10000;
const maximumUnconfirmedReads = 3;

interface SetupImportView {
  status: ImportStatus | null;
  statusError: string;
  actionError: string;
  actionMessage: string;
  starting: boolean;
  cancelling: boolean;
  awaitingStatus: boolean;
}

const initialView: SetupImportView = {
  status: null, statusError: '', actionError: '', actionMessage: '',
  starting: false, cancelling: false, awaitingStatus: false,
};

export function canStartSetupImport(status: ImportStatus | null) {
  return status?.can_start === true && !!status.start_repository_id;
}

export function canCancelSetupImport(status: ImportStatus | null) {
  return status?.can_cancel === true && !!status.operation_id;
}

interface Receipt {
  action: 'start' | 'cancel';
  operationId: string;
  previousId?: string;
  unconfirmedReads: number;
}

function terminal(status: ImportStatus) {
  if (status.lifecycle) {
    return ['completed', 'cancelled', 'failed', 'reconciliation_required'].includes(status.lifecycle);
  }
  return ['completed', 'cancelled', 'failed'].includes(status.phase);
}

function observesReceipt(status: ImportStatus, receipt: Receipt) {
  if (!status.operation_id) return false;
  if (status.operation_id !== receipt.operationId) {
    // After a fast completion the next read can already describe a successor.
    // A read of the preceding operation cannot confirm a newly accepted start.
    return status.operation_id !== receipt.previousId;
  }
  if (receipt.action === 'start') return true;
  return status.cancelling === true ||
    status.lifecycle === 'cancel_requested' || status.lifecycle === 'cancelling' ||
    terminal(status);
}

type SetupImportApi = Pick<typeof api, 'getImportStatus' | 'startImport' | 'cancelImport'>;

export function watchSetupImport(client: SetupImportApi, render: (view: SetupImportView) => void) {
  let view = initialView;
  let mounted = true;
  let pollTimer: ReturnType<typeof setTimeout>;
  let deadline: ReturnType<typeof setTimeout>;
  let currentRead: AbortController | undefined;
  let currentWrite: AbortController | undefined;
  const superseded = new Set<string>();
  let retryDelay = pollInterval;
  let receipt: Receipt | undefined;

  function update(changes: Partial<SetupImportView>) {
    view = { ...view, ...changes };
    render(view);
  }

  function stopReading() {
    clearTimeout(pollTimer);
    clearTimeout(deadline);
    currentRead?.abort();
    currentRead = undefined;
  }

  function failedRead(message: string) {
    update({ statusError: message });
    retryDelay = Math.min(retryDelay * 2, maximumRetryDelay);
    pollTimer = setTimeout(read, retryDelay);
  }

  function observe(status: ImportStatus) {
    if (status.operation_id && superseded.has(status.operation_id)) {
      failedRead('Status returned an earlier import operation. Checking again.');
      return;
    }
    retryDelay = pollInterval;
    const confirmed = !receipt || observesReceipt(status, receipt);
    if (receipt && !confirmed && ++receipt.unconfirmedReads < maximumUnconfirmedReads) {
      update({ statusError: '' });
    } else {
      const previousId = view.status?.operation_id;
      const replaced = previousId && status.operation_id && status.operation_id !== previousId;
      if (replaced) superseded.add(previousId);
      if (receipt && confirmed && status.operation_id !== receipt.operationId) {
        superseded.add(receipt.operationId);
      }
      const clearMessage = !confirmed || terminal(status) || replaced ||
        (receipt && status.operation_id !== receipt.operationId);
      receipt = undefined;
      update({
        status, statusError: '', awaitingStatus: false, cancelling: false,
        ...(clearMessage ? { actionMessage: '' } : {}),
        ...(!confirmed ? { actionError: 'Current import status did not confirm the request.' } : {}),
      });
    }
    pollTimer = setTimeout(read, pollInterval);
  }

  async function read() {
    const request = new AbortController();
    currentRead = request;
    deadline = setTimeout(() => {
      stopReading();
      failedRead('Import status timed out. Retrying.');
    }, requestTimeout);

    try {
      const status = await client.getImportStatus(request.signal);
      if (request !== currentRead) return;
      clearTimeout(deadline);
      currentRead = undefined;
      observe(status);
    } catch (error) {
      if (request !== currentRead) return;
      clearTimeout(deadline);
      currentRead = undefined;
      const detail = error instanceof Error ? error.message : 'Request failed';
      failedRead(`Could not read import status. ${detail}`);
    }
  }

  async function act(action: 'start' | 'cancel', targetId: string) {
    if (!mounted || view.starting || view.cancelling || view.awaitingStatus || view.statusError) return;
    const observed = view.status;
    if (action === 'start' && (!canStartSetupImport(observed) || targetId !== observed?.start_repository_id)) return;
    if (action === 'cancel' && (targetId !== observed?.operation_id ||
      !canCancelSetupImport(observed))) return;

    stopReading();
    update({
      actionError: '', actionMessage: '', awaitingStatus: true,
      starting: action === 'start', cancelling: action === 'cancel',
    });
    const request = new AbortController();
    currentWrite = request;
    const timeout = setTimeout(() => request.abort(), requestTimeout);
    try {
      // Aborting the HTTP request cannot undo an accepted write. Resume reads
      // after an uncertain response, and leave any retry to a fresh user action.
      const interrupted = new Promise<never>((_, reject) => {
        request.signal.addEventListener('abort', () => reject(new Error('Request timed out.')), { once: true });
      });
      const write = action === 'cancel'
        ? client.cancelImport(targetId, request.signal)
        : client.startImport(targetId, request.signal);
      const result = await Promise.race([write, interrupted]);
      if (!mounted) return;
      if (!result.ok) {
        update({ actionError: result.message || 'The server refused the import request.' });
      } else if (!result.operation_id || (action === 'cancel' && result.operation_id !== targetId)) {
        update({ actionError: 'Could not confirm the operation for this request. Checking import status.' });
      } else {
        receipt = { action, operationId: result.operation_id, previousId: observed?.operation_id, unconfirmedReads: 0 };
        update({ actionMessage: result.message });
      }
    } catch (error) {
      if (!mounted) return;
      const detail = error instanceof Error ? error.message : 'Connection failed';
      update({ actionError: `Could not confirm the import request. ${detail}` });
    } finally {
      clearTimeout(timeout);
      currentWrite = undefined;
    }
    if (mounted) {
      update({ starting: false, cancelling: receipt?.action === 'cancel' });
      void read();
    }
  }

  void read();
  return {
    start: (repositoryId: string) => act('start', repositoryId),
    cancel: (operationId: string) => act('cancel', operationId),
    dispose: () => {
      mounted = false;
      stopReading();
      currentWrite?.abort();
    },
  };
}

export function useSetupImport() {
  const [view, setView] = useState(initialView);
  const watcher = useRef<ReturnType<typeof watchSetupImport> | null>(null);

  useEffect(() => {
    const current = watchSetupImport(api, setView);
    watcher.current = current;
    return () => {
      current.dispose();
      watcher.current = null;
    };
  }, []);

  return {
    ...view,
    start: async (repositoryId: string) => { await watcher.current?.start(repositoryId); },
    cancel: async (operationId: string) => { await watcher.current?.cancel(operationId); },
  };
}
