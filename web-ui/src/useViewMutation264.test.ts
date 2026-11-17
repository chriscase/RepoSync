import { describe, expect, it } from 'vitest';
import { viewIdentityPath } from './useViewMutation';

// #264 G04.31: a repository section change is the same view; other parameters still identify it.
describe('#264 view identity ignores the repository section', () => {
  it('returns the path unchanged when there is no section parameter', () => {
    expect(viewIdentityPath('/repos/A', '')).toBe('/repos/A');
    expect(viewIdentityPath('/audit', '?page=2&action=sync_cycle')).toBe('/audit?page=2&action=sync_cycle');
  });
  it('treats every section of one repository as one view', () => {
    const base = viewIdentityPath('/repos/A', '');
    for (const section of ['overview', 'activity', 'branches', 'operations', 'settings', 'unknown']) {
      expect(viewIdentityPath('/repos/A', `?section=${section}`)).toBe(base);
    }
  });
  it('keeps other parameters and other repositories distinct', () => {
    expect(viewIdentityPath('/repos/A', '?section=settings&x=1')).toBe('/repos/A?x=1');
    expect(viewIdentityPath('/repos/B', '?section=settings')).not.toBe(viewIdentityPath('/repos/A', '?section=settings'));
  });
});
