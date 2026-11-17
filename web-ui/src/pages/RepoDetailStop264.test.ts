import { describe, expect, it } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { StopOperationConfirm } from './RepoDetail';

// #264 D11 and the interaction contract: "Stop operation…" asks once; nothing is sent until confirmed.
describe('#264 Stop operation… confirmation', () => {
  const noop = () => {};
  it('renders nothing until asked', () => {
    expect(renderToStaticMarkup(createElement(StopOperationConfirm, { open: false, pending: false, onConfirm: noop, onCancel: noop }))).toBe('');
  });
  it('says what stopping does not undo, uses the warning style like Stop import, and does not default to it', () => {
    const html = renderToStaticMarkup(createElement(StopOperationConfirm, { open: true, pending: false, onConfirm: noop, onCancel: noop }));
    expect(html).toContain('Stop this operation?');
    expect(html).toContain('it does not undo remote work');
    expect(html).toMatch(/<button type="button" data-dialog-cancel="[^"]*"[^>]*>Keep running<\/button>/);
    expect(html).toMatch(/data-testid="confirm-stop-operation"[^>]*class="rs-btn rs-btn--state[^"]*"[^>]*>Stop operation<\/button>/);
  });
  it('disables both buttons and says it is requesting the stop while pending', () => {
    const html = renderToStaticMarkup(createElement(StopOperationConfirm, { open: true, pending: true, onConfirm: noop, onCancel: noop }));
    expect(html.match(/<button[^>]*disabled=""/g)?.length).toBe(2);
    expect(html).toContain('Requesting stop…');
  });
  it('shows a failed stop inside the dialog', () => {
    const html = renderToStaticMarkup(createElement(StopOperationConfirm, { open: true, pending: false, error: 'Synthetic refusal', onConfirm: noop, onCancel: noop }));
    expect(html).toMatch(/role="alert"[^>]*>Synthetic refusal</);
  });
});
