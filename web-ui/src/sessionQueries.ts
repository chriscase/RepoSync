import { QueryClient } from '@tanstack/react-query';
import { type ClientSession, type SessionSnapshot } from './session';

export class SessionQueryCache {
  private value: { session: SessionSnapshot; client: QueryClient };
  private listeners = new Set<() => void>();
  private unsubscribe: () => void;
  constructor(session: ClientSession) {
    this.value = { session: session.snapshot(), client: this.create() };
    this.unsubscribe = session.subscribe(() => {
      const next = session.snapshot();
      let client = this.value.client;
      if (next.epoch !== this.value.session.epoch) {
        // Cancellation retires query observers. Mutation transport is separately aborted by ClientSession.
        void client.cancelQueries(); client.clear(); client = this.create();
        // BrowserRouter must not inherit another session's receipt/draft navigation state.
        if (typeof window !== 'undefined' && window.history?.state?.usr) {
          window.history.replaceState({ ...window.history.state, usr: null }, '');
        }
      }
      this.value = { session: next, client };
      for (const notify of this.listeners) notify();
    });
  }
  private create(): QueryClient { return new QueryClient({ defaultOptions: { queries: { refetchInterval: 5000, retry: 2 } } }); }
  readonly snapshot = () => this.value;
  readonly subscribe = (notify: () => void) => { this.listeners.add(notify); return () => { this.listeners.delete(notify); }; };
  dispose(): void { this.unsubscribe(); void this.value.client.cancelQueries(); this.value.client.clear(); }
}
