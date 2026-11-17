import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import Config, {
  IdentityMappingsView,
  NotificationsView,
  RemoveMappingConfirm,
  WebhookStateMark,
  type IdentityMappingsViewProps,
  type NotificationsViewProps,
} from './Config';
import { api, type AuthorMapping } from '../api';

// Static-render checks (no DOM). Synthetic fixtures only.
const noop = () => {};
const rows: AuthorMapping[] = [
  { svn_username: 'fixture-a', name: 'Fixture A', email: 'a@fixture.invalid' },
  { svn_username: 'fixture-b', name: 'Fixture B', email: 'b@fixture.invalid', github: 'fixture-b' },
];
const emptyDraft: AuthorMapping = { svn_username: '', name: '', email: '' };

function buttons(html: string) {
  return [...html.matchAll(/<button([^>]*)>([\s\S]*?)<\/button>/g)].map(([, attrs, text]) => ({
    text,
    disabled: / disabled=""/.test(attrs),
  }));
}
const button = (html: string, text: string) => buttons(html).find((b) => b.text === text);
const inputs = (html: string) => html.match(/<input[^>]*>/g) ?? [];

function mappingsView(overrides: Partial<IdentityMappingsViewProps> = {}) {
  const props: IdentityMappingsViewProps = {
    mappings: rows,
    loading: false,
    readError: '',
    saving: false,
    saveError: '',
    newMapping: emptyDraft,
    onNewMapping: noop,
    onAdd: noop,
    onRemove: noop,
    onRetry: noop,
    ...overrides,
  };
  return renderToStaticMarkup(createElement(IdentityMappingsView, props));
}

describe('#264 G11.1 author identity mappings', () => {
  it('lists the mappings with Remove and Add available', () => {
    const html = mappingsView();
    expect(html).toContain('<table');
    expect(html).toContain('>fixture-a</td>');
    expect(html).toContain('>fixture-b</td>');
    expect(buttons(html).filter((b) => b.text === 'Remove')).toEqual([
      { text: 'Remove', disabled: false },
      { text: 'Remove', disabled: false },
    ]);
    expect(button(html, 'Add')?.disabled).toBe(false);
    expect(inputs(html).every((input) => !input.includes('disabled=""'))).toBe(true);
    expect(html).not.toContain('role="alert"');
  });

  it('says so when the list is empty', () => {
    const html = mappingsView({ mappings: [] });
    expect(html).toContain('No mappings yet');
    expect(button(html, 'Add')?.disabled).toBe(false);
  });

  it('shows a failed read as failed and turns editing off', () => {
    const html = mappingsView({ mappings: undefined, readError: 'API error 500: fixture' });
    expect(html).toMatch(/<div role="alert" class="rs-bn rs-bn--danger[^"]*"><p class="font-medium">Mappings could not be loaded<\/p><p>API error 500: fixture<\/p>/);
    expect(html).toContain('They are unknown, so editing is turned off. Saving now would replace the whole list.');
    expect(button(html, 'Retry')?.disabled).toBe(false);
    expect(html).not.toContain('<table');
    expect(html).not.toContain('No mappings yet');
    expect(button(html, 'Remove')).toBeUndefined();
    expect(button(html, 'Add')?.disabled).toBe(true);
    expect(inputs(html)).toHaveLength(3);
    expect(inputs(html).every((input) => input.includes('disabled=""'))).toBe(true);
  });

  it('keeps a typed draft while editing is off', () => {
    const html = mappingsView({ mappings: undefined, readError: 'API error 500: fixture', newMapping: { svn_username: 'draft-user', name: '', email: '' } });
    expect(html).toContain('value="draft-user"');
  });

  it('keeps editing off while a retry is loading', () => {
    const html = mappingsView({ mappings: undefined, loading: true });
    expect(html).toContain('<p role="status" class="mb-4 text-sm text-gray-300">Loading...</p>');
    expect(html).not.toContain('<table');
    expect(button(html, 'Add')?.disabled).toBe(true);
  });

  it('holds Add and Remove while a save is running', () => {
    const html = mappingsView({ saving: true });
    expect(buttons(html).filter((b) => b.text === 'Remove').every((b) => b.disabled)).toBe(true);
    expect(button(html, 'Add')?.disabled).toBe(true);
  });

  it('shows a failed save with the client error text', () => {
    const html = mappingsView({ saveError: 'API error 500: fixture' });
    expect(html).toContain('<div role="alert" class="rs-bn rs-bn--danger mb-4 bg-red-900/40 border border-red-700 text-red-300 px-4 py-3 rounded text-sm space-y-1"><p class="font-medium">The mappings were not saved</p><p>API error 500: fixture</p></div>');
  });
});

describe('#264 G11.1 Remove asks first', () => {
  const confirm = (overrides: Partial<Parameters<typeof RemoveMappingConfirm>[0]> = {}) =>
    renderToStaticMarkup(createElement(RemoveMappingConfirm, { mapping: rows[0], busy: false, error: '', onConfirm: noop, onCancel: noop, ...overrides }));

  it('names the mapping, with Cancel as the default and a named destructive action', () => {
    const html = confirm();
    expect(html).toContain('<dialog');
    expect(html).toContain('>Remove this mapping?</h2>');
    expect(html).toContain('SVN username fixture-a, Git author Fixture A &lt;a@fixture.invalid&gt;.');
    expect(html).toMatch(/<button type="button" data-dialog-cancel="true"[^>]*>Cancel<\/button>/);
    expect(html).toMatch(/<button type="button" class="rs-btn rs-btn--danger [^"]*rs-text-on-danger bg-red-600[^"]*">Remove mapping<\/button>/);
  });

  it('holds both buttons while saving and shows a failed removal in the dialog', () => {
    const busy = confirm({ busy: true });
    expect(button(busy, 'Cancel')?.disabled).toBe(true);
    expect(button(busy, 'Remove mapping')?.disabled).toBe(true);
    expect(confirm({ error: 'API error 500: fixture' })).toContain('<p role="alert" class="mb-3 text-red-300">API error 500: fixture</p>');
  });

  it('draws nothing until a mapping is chosen', () => {
    expect(confirm({ mapping: null })).toBe('');
  });
});

function notificationsView(overrides: Partial<NotificationsViewProps> = {}) {
  const props: NotificationsViewProps = {
    read: { state: 'set' },
    showSetup: false,
    onToggleSetup: noop,
    teamsUrl: '',
    onTeamsUrl: noop,
    replacing: false,
    saving: false,
    testing: false,
    message: null,
    confirmingRemove: false,
    removing: false,
    removeError: '',
    onRetry: noop,
    onSave: noop,
    onReplace: noop,
    onCancelReplace: noop,
    onTest: noop,
    onAskRemove: noop,
    onConfirmRemove: noop,
    onCancelRemove: noop,
    ...overrides,
  };
  return renderToStaticMarkup(createElement(NotificationsView, props));
}
const webhookButtons = (html: string) => buttons(html).map((b) => b.text).filter((text) => !text.includes('Setup Instructions'));

describe('#264 S23 webhook state mark', () => {
  it.each([[true, 'Set'], [false, 'Not set']] as const)('set %s shows the word %s and a mark', (set, word) => {
    const html = renderToStaticMarkup(createElement(WebhookStateMark, { set }));
    expect(html).toMatch(new RegExp(`^<span class="rs-mark [^"]*"><svg[^>]*aria-hidden="true"[^>]*>[\\s\\S]*?</svg>${word}</span>$`));
  });
});

describe('#264 G11.2 Teams webhook', () => {
  it('shows a stored webhook as Set, with Replace, Test and Remove and no field', () => {
    const html = notificationsView();
    expect(html).toContain('<span class="block text-sm text-gray-400">Teams Webhook URL</span>');
    expect(html).toContain('</svg>Set</span>');
    expect(webhookButtons(html)).toEqual(['Replace', 'Test', 'Remove']);
    expect(inputs(html)).toHaveLength(0);
    expect(html).not.toContain('<dialog');
  });

  it('opens a blank field on Replace, with Save unavailable until a value is typed', () => {
    const html = notificationsView({ replacing: true });
    expect(html).toContain('<label class="block text-sm text-gray-400" for="teams-webhook-url">Teams Webhook URL</label>');
    expect(inputs(html)).toHaveLength(1);
    expect(inputs(html)[0]).toContain('value=""');
    expect(html).toContain('</svg>Set</span>');
    expect(webhookButtons(html)).toEqual(['Save', 'Cancel']);
    expect(button(html, 'Save')?.disabled).toBe(true);
    expect(button(notificationsView({ replacing: true, teamsUrl: '  ' }), 'Save')?.disabled).toBe(true);
    expect(button(notificationsView({ replacing: true, teamsUrl: 'https://hook.fixture.invalid/x' }), 'Save')?.disabled).toBe(false);
  });

  it('shows a missing webhook as Not set with the field, and Save unavailable while blank', () => {
    const html = notificationsView({ read: { state: 'not_set' } });
    expect(html).toContain('</svg>Not set</span>');
    expect(html).toContain('for="teams-webhook-url">Teams Webhook URL</label>');
    expect(webhookButtons(html)).toEqual(['Save']);
    expect(button(html, 'Save')?.disabled).toBe(true);
  });

  it('offers no Test beside a typed value that is not saved, and Test in the Set state', () => {
    const typed = 'https://hook.fixture.invalid/x';
    const html = notificationsView({ read: { state: 'not_set' }, teamsUrl: typed, message: { type: 'error', text: 'Save failed' } });
    expect(webhookButtons(html)).toEqual(['Save']);
    expect(button(html, 'Save')?.disabled).toBe(false);
    const field = inputs(html)[0] ?? '';
    expect(field).toContain(`value="${typed}"`);
    const feedback = html.match(/<p id="([^"]+)" role="alert"[^>]*>Save failed<\/p>/)?.[1];
    expect(feedback).toBeTruthy();
    expect(field).toContain(`aria-describedby="${feedback}"`);
    // Replacing a saved webhook: the typed value is not saved yet, so no Test.
    const replacing = notificationsView({ replacing: true, teamsUrl: typed });
    expect(webhookButtons(replacing)).toEqual(['Save', 'Cancel']);
    expect(button(replacing, 'Test')).toBeUndefined();
    expect(button(notificationsView({ replacing: true, teamsUrl: typed, testing: true }), 'Sending...')).toBeUndefined();
    // Set and not being replaced: Test is offered.
    const set = notificationsView({ read: { state: 'set' }, replacing: false, teamsUrl: '' });
    expect(webhookButtons(set)).toEqual(['Replace', 'Test', 'Remove']);
    expect(button(set, 'Test')?.disabled).toBe(false);
    expect(button(notificationsView({ testing: true }), 'Sending...')?.disabled).toBe(true);
  });

  it('turns saving off after a failed read and shows the client error text', () => {
    const html = notificationsView({ read: { state: 'failed', error: 'API error 500: fixture' } });
    expect(html).toMatch(/<div role="alert" class="rs-bn rs-bn--danger[^"]*"><p class="font-medium">The notification setting could not be read<\/p><p>API error 500: fixture<\/p>/);
    expect(html).toContain('It is unknown, so saving is turned off and an existing webhook is not cleared.');
    expect(webhookButtons(html)).toEqual(['Retry']);
    expect(inputs(html)).toHaveLength(0);
    expect(html).not.toContain('</svg>Set</span>');
    expect(html).not.toContain('</svg>Not set</span>');
  });

  it('turns saving off while the setting is being read', () => {
    const html = notificationsView({ read: { state: 'loading' } });
    expect(html).toContain('<p role="status" class="text-sm text-gray-300">Loading...</p>');
    expect(webhookButtons(html)).toEqual([]);
    expect(inputs(html)).toHaveLength(0);
  });

  it('confirms Remove with Cancel as the default and a named destructive action', () => {
    const html = notificationsView({ confirmingRemove: true });
    expect(html).toContain('>Remove the Teams webhook?</h2>');
    expect(html).toContain('The saved Teams webhook URL will be removed. To set it again you will need the webhook URL from Teams.');
    expect(html).toMatch(/<button type="button" data-dialog-cancel="true"[^>]*>Cancel<\/button>/);
    expect(html).toMatch(/<button type="button" class="rs-btn rs-btn--danger [^"]*rs-text-on-danger bg-red-600[^"]*">Remove webhook<\/button>/);
    const failed = notificationsView({ confirmingRemove: true, removeError: 'Save failed (500)' });
    expect(failed).toContain('<p role="alert" class="mb-3 text-red-300">Save failed (500)</p>');
    const busy = notificationsView({ confirmingRemove: true, removing: true });
    expect(button(busy, 'Remove webhook')?.disabled).toBe(true);
  });

  it('never opens the Remove confirmation unless the webhook is known to be set', () => {
    for (const read of [{ state: 'not_set' }, { state: 'loading' }, { state: 'failed', error: 'x' }] as const) {
      expect(notificationsView({ read, confirmingRemove: true })).not.toContain('<dialog');
    }
  });

  it('keeps the existing feedback wording', () => {
    expect(notificationsView({ message: { type: 'success', text: 'Saved' } })).toMatch(/<p id="[^"]+" role="status" class="text-xs mt-1 text-green-400">Saved<\/p>/);
    expect(notificationsView({ read: { state: 'not_set' }, message: { type: 'error', text: 'Save failed' } })).toMatch(/<p id="[^"]+" role="alert" class="text-xs mt-1 text-red-400">Save failed<\/p>/);
  });
});

describe('#264 G11 configuration page wiring', () => {
  let client: QueryClient;
  const fetchMock = vi.fn();
  beforeEach(() => {
    client = new QueryClient({ defaultOptions: { queries: { retry: false, retryOnMount: false, staleTime: Infinity } } });
    const entries = new Map<string, string>([['session_token', 'fixture-token']]);
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => entries.get(key) ?? null,
      setItem: (key: string, value: string) => entries.set(key, value),
      removeItem: (key: string) => entries.delete(key),
      get length() { return entries.size; },
      key: (index: number) => Array.from(entries.keys())[index] ?? null,
    });
    vi.stubGlobal('window', { location: { pathname: '/config', href: '/config' } });
    vi.stubGlobal('fetch', fetchMock);
    fetchMock.mockReset();
    // The router and the view-ownership hook use layout effects, which React reports on the server.
    const original = console.error;
    vi.spyOn(console, 'error').mockImplementation((...args: unknown[]) => {
      if (String(args[0]).includes('useLayoutEffect does nothing on the server')) return;
      original(...args);
    });
  });
  afterEach(() => { client.clear(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

  const renderPage = () => renderToStaticMarkup(
    createElement(MemoryRouter, { initialEntries: ['/config'] }, createElement(QueryClientProvider, { client }, createElement(Config))),
  );
  const failRead = () => {
    fetchMock.mockResolvedValueOnce(new Response('{"error":"fixture failure"}', { status: 500 }));
    return client.fetchQuery({ queryKey: ['identityMappings'], queryFn: api.getIdentityMappings, staleTime: 0 });
  };

  it('shows a failed read as failed, not as an empty table, and keeps the rest of the page', async () => {
    await expect(failRead()).rejects.toThrow();
    const html = renderPage();
    expect(html).toContain('Mappings could not be loaded');
    expect(html).toContain('API error 500: {&quot;error&quot;:&quot;fixture failure&quot;}');
    expect(html).not.toContain('<table');
    expect(button(html, 'Add')?.disabled).toBe(true);
    expect(html).toContain('Notifications — Microsoft Teams');
  });

  it('turns editing off when a later read fails, even with an earlier list in memory', async () => {
    client.setQueryData(['identityMappings'], rows);
    await expect(failRead()).rejects.toThrow();
    const html = renderPage();
    expect(html).toContain('Mappings could not be loaded');
    expect(html).not.toContain('fixture-a');
    expect(button(html, 'Remove')).toBeUndefined();
    expect(button(html, 'Add')?.disabled).toBe(true);
  });

  it('draws the list from a successful read with editing on', () => {
    client.setQueryData(['identityMappings'], rows);
    const html = renderPage();
    expect(html).not.toContain('Mappings could not be loaded');
    expect(html).toContain('>fixture-a</td>');
    expect(button(html, 'Add')?.disabled).toBe(false);
    expect(html).not.toContain('<dialog');
  });

  it('keeps the page-level loading line for the first read', () => {
    expect(renderPage()).toBe('<div class="text-center py-8 text-gray-300">Loading...</div>');
  });
});
