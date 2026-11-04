import type { UseMutationOptions } from '@tanstack/react-query';
import { RetiredSessionError } from './session';

/** A retired route/session retains its server outcome but cannot drive the replacement view. */
export interface ViewIdentity { epoch: number; path: string }
export class ViewOwnership {
  private active = true;
  constructor(private readonly origin: ViewIdentity, private readonly current: () => ViewIdentity) {}
  isCurrent(): boolean { const value = this.current(); return this.active && value.epoch === this.origin.epoch && value.path === this.origin.path; }
  setActive(active: boolean): void { this.active = active; }
}

/** Used by the actual mutation hook, also at a queued mutation's send boundary. */
export function ownedMutation<TData, TError, TVariables, TContext>(options: UseMutationOptions<TData, TError, TVariables, TContext>, owner: ViewOwnership): UseMutationOptions<TData, TError, TVariables, TContext> {
  return { ...options,
    mutationFn: options.mutationFn ? (...args) => { if (!owner.isCurrent()) throw new RetiredSessionError(); return options.mutationFn!(...args); } : undefined,
    onSuccess: (...args) => owner.isCurrent() ? options.onSuccess?.(...args) : undefined,
    onError: (...args) => owner.isCurrent() ? options.onError?.(...args) : undefined,
    onSettled: (...args) => owner.isCurrent() ? options.onSettled?.(...args) : undefined,
  };
}
