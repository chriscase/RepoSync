import { describe, expect, it } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { SyncSettingsProblemNotice } from './RepoDetail';

describe('sync settings problem notice', () => {
  it('shows nothing when the stored settings are usable', () => {
    expect(renderToStaticMarkup(createElement(SyncSettingsProblemNotice, {}))).toBe('');
    expect(renderToStaticMarkup(createElement(SyncSettingsProblemNotice, { problem: null }))).toBe('');
  });
  it('says sync is skipped and names the setting', () => {
    const html = renderToStaticMarkup(
      createElement(SyncSettingsProblemNotice, { problem: 'git_branch must not be empty' }),
    );
    expect(html).toContain('role="alert"');
    expect(html).toContain('Sync is not running for this pair');
    expect(html).toContain('git_branch must not be empty');
  });
});
