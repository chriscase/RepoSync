import { useSyncExternalStore, type ReactNode } from 'react';
import { QueryClientProvider } from '@tanstack/react-query';
import { clientSession, watchSessionStorage } from './session';
import { SessionQueryCache } from './sessionQueries';

let cache: SessionQueryCache | undefined;
function applicationCache() {
  if (!cache) { cache = new SessionQueryCache(clientSession()); watchSessionStorage(); }
  return cache;
}
export default function SessionBoundary({ children }: { children: ReactNode }) {
  const owner = applicationCache();
  const { session, client } = useSyncExternalStore(owner.subscribe, owner.snapshot, owner.snapshot);
  // Remount forms, private inputs and observers as well as replacing the cache.
  return <QueryClientProvider key={session.epoch} client={client}>{children}</QueryClientProvider>;
}
