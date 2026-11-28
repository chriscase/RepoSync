import { describe, expect, it } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { SyncSettingsProblemNotice, storedListToText } from './RepoDetail';

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
    expect(html).toContain('A saved setting stops sync for this pair');
    expect(html).toContain('git_branch must not be empty');
  });
});

describe('stored path rules in the edit form', () => {
  it('shows a stored list one entry per line', () => {
    expect(storedListToText('["src/","docs/"]')).toBe('src/\ndocs/');
    expect(storedListToText(null)).toBe('');
    expect(storedListToText('[]')).toBe('');
  });
  it('still opens for a stored value that is not a list, so it can be corrected', () => {
    expect(storedListToText('"src/"')).toBe('src/');
    expect(storedListToText('{}')).toBe('{}');
    expect(storedListToText('src/')).toBe('src/');
    expect(storedListToText('[1,2]')).toBe('1\n2');
  });
});
