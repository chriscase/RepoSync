import type { QueryClient } from '@tanstack/react-query';
import type { SyncStatus } from './api';
import { errorStatusQuery } from './errorStatus';
import { statusObservationView, type StatusQueryObservation } from './statusObservation';

export interface ScopedSkipObservation {repoId?:string; detailLive:boolean; administrator:boolean; query:StatusQueryObservation}
/** Re-read the actual cache at confirmation/mutation time, including before React commits a refetch render. */
export function latestScopedSkipObservation(client:QueryClient,state:ScopedSkipObservation):ScopedSkipObservation {
  const cached=client.getQueryState<SyncStatus>(errorStatusQuery(state.repoId).queryKey);
  return {...state,query:cached ? {data:cached.data,error:cached.error,isError:cached.status==='error',dataUpdatedAt:cached.dataUpdatedAt}:{}};
}
/** A display guard only. The API still checks the exact tip and current writer/ownership rules. */
export function canConfirmObservedSkip(state:ScopedSkipObservation,repoId:string,now=Date.now()):boolean {
  const view=statusObservationView(state.query,now);
  return !!repoId && state.repoId===repoId && state.detailLive && state.administrator
    && state.query.data?.state==='error_paused' && state.query.data.observation?.scope==='repository'
    && state.query.data.observation.repository_id===repoId && view.health==='paused' && view.canAct;
}
/** Used at the real mutation entry as well as the open-dialog confirmation boundary. */
export function runObservedSkip<T>(read:()=>ScopedSkipObservation,repoId:string,action:()=>Promise<T>,now=Date.now()):Promise<T> {
  if(!canConfirmObservedSkip(read(),repoId,now))throw new Error('Current scoped status does not permit Skip. Refresh and inspect the retained evidence; no request was sent.');
  return action();
}
