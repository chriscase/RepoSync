import type { UseMutationOptions } from '@tanstack/react-query';
import { RetiredSessionError } from './session';

/** A retired route/session retains its server outcome but cannot drive the replacement view. */
export interface ViewIdentity { readonly epoch: number; readonly path: string }
export class ViewOwnership {
  private active = true;
  readonly mutationKey: readonly unknown[];
  constructor(private readonly origin: ViewIdentity, private readonly current: () => ViewIdentity) { this.mutationKey = ['reposync-view-mutation', origin.epoch, origin.path]; }
  isCurrent(): boolean { const value = this.current(); return this.active && value.epoch === this.origin.epoch && value.path === this.origin.path; }
  assertCurrent(): void { if (!this.isCurrent()) throw new RetiredSessionError(); }
  setActive(active: boolean): void { this.active = active; }
}

/** Used by the actual mutation hook, also at a queued mutation's send boundary. */
export function ownedMutation<TData, TError, TVariables, TContext>(options: UseMutationOptions<TData, TError, TVariables, TContext>, owner: ViewOwnership): UseMutationOptions<TData, TError, TVariables, TContext> {
  return { ...options,
    // TanStack resets its pending observer instead of replacing that invocation's
    // options when this immutable identity key changes. Same-owner updates still work.
    mutationKey: [...owner.mutationKey, options.mutationKey ?? []],
    mutationFn: options.mutationFn ? (...args) => { owner.assertCurrent(); return options.mutationFn!(...args); } : undefined,
    onSuccess: (...args) => owner.isCurrent() ? options.onSuccess?.(...args) : undefined,
    onError: (...args) => owner.isCurrent() ? options.onError?.(...args) : undefined,
    onSettled: (...args) => owner.isCurrent() ? options.onSettled?.(...args) : undefined,
  };
}
