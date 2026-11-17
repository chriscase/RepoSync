import { useLayoutEffect, useMemo, useRef } from 'react';
import { useLocation } from 'react-router-dom';
import { useMutation, type UseMutationOptions } from '@tanstack/react-query';
import { clientSession } from './session';
import { ViewOwnership, ownedMutation } from './viewOwnership';

/**
 * #264 (G04.31): `?section=` only chooses which part of the repository page is shown; the
 * page, its drafts and its pending actions are the same view. It is left out of the view's
 * identity so a section change does not orphan a pending action. Every other parameter
 * still identifies the view, and a path without `section` is returned unchanged.
 */
export function viewIdentityPath(pathname: string, search: string): string {
  const params = new URLSearchParams(search);
  if (!params.has('section')) return pathname + search;
  params.delete('section');
  const rest = params.toString();
  return pathname + (rest ? `?${rest}` : '');
}

export function useViewOwnership() {
  const { pathname, search } = useLocation(); const path = viewIdentityPath(pathname, search); const epoch = clientSession().snapshot().epoch;
  const current = useRef({ path, epoch }); current.current = { path, epoch };
  const owner = useMemo(() => new ViewOwnership({ path, epoch }, () => ({ ...current.current, path: typeof window === 'undefined' ? current.current.path : viewIdentityPath(window.location.pathname, window.location.search ?? ''), epoch: clientSession().snapshot().epoch })), [path, epoch]);
  useLayoutEffect(() => { owner.setActive(true); return () => owner.setActive(false); }, [owner]);
  return owner;
}
export function useViewMutation<TData = unknown, TError = Error, TVariables = void, TContext = unknown>(options: UseMutationOptions<TData, TError, TVariables, TContext>) {
  const owner = useViewOwnership();
  return useMutation(ownedMutation(options, owner));
}
