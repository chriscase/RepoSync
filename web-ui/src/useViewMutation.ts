import { useLayoutEffect, useMemo, useRef } from 'react';
import { useLocation } from 'react-router-dom';
import { useMutation, type UseMutationOptions } from '@tanstack/react-query';
import { clientSession } from './session';
import { ViewOwnership, ownedMutation } from './viewOwnership';

export function useViewOwnership() {
  const { pathname, search } = useLocation(); const path = pathname + search; const epoch = clientSession().snapshot().epoch;
  const current = useRef({ path, epoch }); current.current = { path, epoch };
  const owner = useMemo(() => new ViewOwnership({ path, epoch }, () => ({ ...current.current, path: typeof window === 'undefined' ? current.current.path : window.location.pathname + (window.location.search ?? ''), epoch: clientSession().snapshot().epoch })), [path, epoch]);
  useLayoutEffect(() => { owner.setActive(true); return () => owner.setActive(false); }, [owner]);
  return owner;
}
export function useViewMutation<TData = unknown, TError = Error, TVariables = void, TContext = unknown>(options: UseMutationOptions<TData, TError, TVariables, TContext>) {
  const owner = useViewOwnership();
  return useMutation(ownedMutation(options, owner));
}
