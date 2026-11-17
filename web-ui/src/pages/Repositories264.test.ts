import { describe, expect, it } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { ConnectionTestResult, type ConnectionTestOutcome } from './Repositories';

const at = new Date(2026, 10, 17, 14, 1, 40);
const render = (result: ConnectionTestOutcome | null) => renderToStaticMarkup(createElement(ConnectionTestResult, { result }));

describe('#264 G03.3 connection test result in the Add Repository dialog', () => {
  it('keeps an empty live region in the page while idle or testing', () => {
    expect(render(null)).toBe('<span role="status"></span>');
  });

  it('shows a passed result with its mark, words for assistive technology, the message and the time', () => {
    const html = render({ ok: true, message: 'Repository Root: https://svn.fixture.invalid/project', testedAt: at });
    expect(html.startsWith('<span role="status">')).toBe(true);
    expect(html).toContain('class="ml-3 text-xs text-green-300"');
    expect(html).toMatch(/<svg[^>]*class="lucide lucide-circle-check[^"]*"[^>]*aria-hidden="true"/);
    expect(html).toContain('<span class="sr-only">Connection test passed: </span>');
    expect(html).toContain('Repository Root: https://svn.fixture.invalid/project');
    expect(html).toContain(` · tested <time dateTime="${at.toISOString()}">14:01:40</time>`);
    expect(html).not.toContain('text-red-300');
  });

  it('shows a failed result with a different mark and the message unchanged', () => {
    const message = 'API error 502: upstream unreachable';
    const html = render({ ok: false, message, testedAt: at });
    expect(html).toContain('class="ml-3 text-xs text-red-300"');
    expect(html).toMatch(/<svg[^>]*class="lucide lucide-circle-x[^"]*"[^>]*aria-hidden="true"/);
    expect(html).toContain('<span class="sr-only">Connection test failed: </span>');
    expect(html).toContain(message);
    expect(html).not.toContain('text-green-300');
    expect(html).not.toContain('lucide-circle-check');
  });

  it('pads the local time to hours, minutes and seconds', () => {
    const html = render({ ok: true, message: 'ok', testedAt: new Date(2026, 0, 2, 9, 5, 3) });
    expect(html).toContain('>09:05:03</time>');
  });
});
