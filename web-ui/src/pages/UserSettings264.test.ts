import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import UserSettings, {
  CredentialCardView,
  CredentialStateMark,
  CredentialsReadFailure,
  type CredentialCardViewProps,
} from './UserSettings';
import { api, type CredentialSummary, type User } from '../api';

// Static-render checks (no DOM). Synthetic fixtures only.
const noop = () => {};
const saved: CredentialSummary = {
  id: 'cred-1',
  service: 'git_token',
  server_url: 'https://git.fixture.invalid',
  username: '',
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
};
const savedPassword: CredentialSummary = { ...saved, id: 'cred-2', service: 'svn_password', server_url: 'https://svn.fixture.invalid', username: 'fixture-user' };

function view(overrides: Partial<CredentialCardViewProps> = {}) {
  const props: CredentialCardViewProps = {
    title: 'Git Token',
    service: 'git_token',
    existing: undefined,
    showUsername: false,
    serverUrl: '',
    username: '',
    value: '',
    replacing: false,
    saving: false,
    deleting: false,
    testing: false,
    confirmingDelete: false,
    testResult: null,
    error: '',
    deleteError: '',
    notice: '',
    onServerUrl: noop,
    onUsername: noop,
    onValue: noop,
    onSave: noop,
    onReplace: noop,
    onCancelReplace: noop,
    onTest: noop,
    onAskDelete: noop,
    onConfirmDelete: noop,
    onCancelDelete: noop,
    ...overrides,
  };
  return renderToStaticMarkup(createElement(CredentialCardView, props));
}

function buttons(html: string) {
  return [...html.matchAll(/<button([^>]*)>([\s\S]*?)<\/button>/g)].map(([, attrs, text]) => ({
    text,
    disabled: / disabled=""/.test(attrs),
  }));
}
const button = (html: string, text: string) => buttons(html).find((b) => b.text === text);

describe('#264 S23 personal credential state mark', () => {
  it('shows a saved credential as a word and a mark', () => {
    const html = renderToStaticMarkup(createElement(CredentialStateMark, { configured: true }));
    expect(html).toMatch(/^<span class="rs-mark [^"]*"><svg[^>]*aria-hidden="true"[^>]*>[\s\S]*?<\/svg>Configured<\/span>$/);
  });

  it('shows a missing credential as a word and a mark', () => {
    const html = renderToStaticMarkup(createElement(CredentialStateMark, { configured: false }));
    expect(html).toMatch(/^<span class="rs-mark [^"]*"><svg[^>]*aria-hidden="true"[^>]*>[\s\S]*?<\/svg>Not configured<\/span>$/);
  });
});

describe('#264 G08.4 credential card: configured', () => {
  it('shows the state, never a secret field or a placeholder about it', () => {
    const html = view({ existing: saved, serverUrl: saved.server_url });
    expect(html).toContain('</svg>Configured</span>');
    expect(html).not.toContain('type="password"');
    expect(html).not.toContain('placeholder');
    expect(html).not.toContain('is set');
    expect(html).not.toContain('leave blank to keep current');
    expect(html).toContain('>Server URL</dt>');
    expect(html).toContain('>https://git.fixture.invalid</dd>');
  });

  it('offers Replace, Test Connection and Delete, and no Save until Replace', () => {
    const html = view({ existing: saved, serverUrl: saved.server_url });
    expect(buttons(html).map((b) => b.text)).toEqual(['Replace', 'Test Connection', 'Delete']);
    expect(buttons(html).every((b) => !b.disabled)).toBe(true);
    expect(html).not.toContain('<dialog');
  });

  it('shows the saved username for the SVN password card', () => {
    const html = view({ title: 'SVN Password', service: 'svn_password', showUsername: true, existing: savedPassword });
    expect(html).toContain('>Username</dt>');
    expect(html).toContain('>fixture-user</dd>');
  });
});

describe('#264 G08.4 credential card: Replace step', () => {
  const replacing = { existing: saved, serverUrl: saved.server_url, replacing: true };

  it('opens a blank secret field with Save unavailable and a way back', () => {
    const html = view(replacing);
    const field = html.match(/<input type="password"[^>]*>/)?.[0] ?? '';
    expect(field).toContain('value=""');
    expect(field).toContain('placeholder="Enter token"');
    const fieldId = field.match(/ id="([^"]+)"/)?.[1];
    expect(html).toContain(`for="${fieldId}">Token</label>`);
    const help = field.match(/aria-describedby="([^"]+)"/)?.[1];
    expect(html).toContain(`<p id="${help}" class="mt-1 text-xs text-gray-400">Enter the new token. Save stays unavailable while this field is blank.</p>`);
    expect(button(html, 'Save')?.disabled).toBe(true);
    expect(button(html, 'Cancel')?.disabled).toBe(false);
    expect(button(html, 'Delete')).toBeUndefined();
    expect(button(html, 'Test Connection')).toBeUndefined();
    expect(html).toContain('</svg>Configured</span>');
  });

  it('keeps the server URL editable, filled with the saved one', () => {
    const html = view(replacing);
    expect(html).toMatch(/<input type="text" placeholder="https:\/\/svn\.example\.com or https:\/\/github\.com"[^>]*value="https:\/\/git\.fixture\.invalid"/);
  });

  it('allows Save only once a value is typed', () => {
    expect(button(view({ ...replacing, value: '   ' }), 'Save')?.disabled).toBe(true);
    expect(button(view({ ...replacing, value: 'fixture-secret' }), 'Save')?.disabled).toBe(false);
  });

  it('shows Saving... and blocks a second save while a save is running', () => {
    const html = view({ ...replacing, value: 'fixture-secret', saving: true });
    expect(button(html, 'Saving...')?.disabled).toBe(true);
    expect(button(html, 'Cancel')?.disabled).toBe(true);
  });
});

describe('#264 G08.4 credential card: not configured', () => {
  it('shows the fields with Save unavailable while the secret is blank', () => {
    const html = view({ serverUrl: 'https://git.fixture.invalid' });
    expect(html).toContain('</svg>Not configured</span>');
    expect(html).toContain('placeholder="Enter token"');
    expect(html).toMatch(/for="[^"]+">Token<\/label>/);
    expect(buttons(html).map((b) => b.text)).toEqual(['Save']);
    expect(button(html, 'Save')?.disabled).toBe(true);
  });

  it('needs both a server URL and a secret to save', () => {
    expect(button(view({ value: 'fixture-secret' }), 'Save')?.disabled).toBe(true);
    expect(button(view({ serverUrl: 'https://git.fixture.invalid', value: 'fixture-secret' }), 'Save')?.disabled).toBe(false);
  });

  it('labels the SVN password field Password and shows the username field', () => {
    const html = view({ title: 'SVN Password', service: 'svn_password', showUsername: true });
    expect(html).toMatch(/for="[^"]+">Password<\/label>/);
    expect(html).toMatch(/for="[^"]+">Username<\/label>/);
    expect(html).toContain('placeholder="Enter password"');
  });
});

describe('#264 G08.4 credential card: results', () => {
  it('shows a failed save with the client error text, linked to the fields', () => {
    const message = 'API error 400: {"error":"fixture"}';
    const html = view({ serverUrl: 'https://git.fixture.invalid', value: 'fixture-secret', error: message });
    const alert = html.match(/<div class="rs-bn rs-bn--danger[^"]*" id="([^"]+)" role="alert">([^<]*)<\/div>/);
    expect(alert?.[2]).toBe(message.replace(/"/g, '&quot;'));
    expect(html).toContain(`aria-describedby="${alert?.[1]}"`);
  });

  it('shows the test result as a failure with the client error text', () => {
    const html = view({ existing: saved, testResult: { ok: false, message: 'API error 405: ' } });
    expect(html).toMatch(/<div role="alert" class="rs-bn rs-bn--danger [^"]*">API error 405: <\/div>/);
    expect(html).not.toContain('rs-bn--ok');
  });

  it('only shows a test result as passed when the reply says ok', () => {
    const html = view({ existing: saved, testResult: { ok: true, message: 'fixture reply' } });
    expect(html).toMatch(/<div role="status" class="rs-bn rs-bn--ok [^"]*">fixture reply<\/div>/);
  });

  it('keeps a status region for the outcome of Save and Delete', () => {
    expect(view({ existing: saved })).toContain('<p role="status"></p>');
    expect(view({ existing: saved, notice: 'Saved' })).toContain('<p role="status" class="mt-3 text-sm text-green-400">Saved</p>');
  });
});

describe('#264 G08.4 credential card: Delete asks first', () => {
  it('draws the confirmation with Cancel as the default and a named destructive action', () => {
    const html = view({ existing: saved, confirmingDelete: true });
    expect(html).toContain('<dialog');
    expect(html).toContain('>Delete your Git Token?</h2>');
    expect(html).toContain('The saved token for https://git.fixture.invalid will be deleted. This cannot be undone.');
    expect(html).toMatch(/<button type="button" data-dialog-cancel="true"[^>]*>Cancel<\/button>/);
    expect(html).toMatch(/<button type="button" class="rs-btn rs-btn--danger [^"]*rs-text-on-danger bg-red-600[^"]*">Delete token<\/button>/);
  });

  it('names the password for the SVN password card', () => {
    const html = view({ title: 'SVN Password', service: 'svn_password', showUsername: true, existing: savedPassword, confirmingDelete: true });
    expect(html).toContain('>Delete your SVN Password?</h2>');
    expect(html).toContain('>Delete password</button>');
  });

  it('holds both dialog buttons while deleting and shows a failed delete in the dialog', () => {
    const busy = view({ existing: saved, confirmingDelete: true, deleting: true });
    expect(busy).toContain('aria-busy="true"');
    expect(button(busy, 'Cancel')?.disabled).toBe(true);
    expect(button(busy, 'Delete token')?.disabled).toBe(true);
    const failed = view({ existing: saved, confirmingDelete: true, deleteError: 'API error 500: fixture' });
    expect(failed).toContain('<p role="alert" class="mb-3 text-red-300">API error 500: fixture</p>');
  });

  it('draws no dialog for a card with nothing saved', () => {
    expect(view({ confirmingDelete: true })).not.toContain('<dialog');
  });
});

describe('#264 G08.4 credential read failure', () => {
  it('says the state is unknown when nothing loaded', () => {
    const html = renderToStaticMarkup(createElement(CredentialsReadFailure, { message: 'API error 500: fixture', stale: false, onRetry: noop }));
    expect(html).toMatch(/^<div role="alert"/);
    expect(html).toContain('Credentials could not be loaded');
    expect(html).toContain('<p>API error 500: fixture</p>');
    expect(html).toContain('Their state is unknown, so the cards are not shown.');
    expect(html).toContain('>Retry</button>');
  });

  it('says the cards may be out of date when an earlier read is still shown', () => {
    const html = renderToStaticMarkup(createElement(CredentialsReadFailure, { message: 'API error 500: fixture', stale: true, onRetry: noop }));
    expect(html).toContain('The cards below show the last state that loaded, which may be out of date.');
  });
});

describe('#264 G08.4 settings page wiring', () => {
  const me: User = { id: 'fixture-id', username: 'fixture', display_name: 'Fixture', email: 'fixture@fixture.invalid', role: 'user' } as User;
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
    vi.stubGlobal('window', { location: { pathname: '/settings', href: '/settings' } });
    vi.stubGlobal('fetch', fetchMock);
    fetchMock.mockReset();
    client.setQueryData(['me'], me);
  });
  afterEach(() => { client.clear(); vi.unstubAllGlobals(); });

  const renderPage = () => renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(UserSettings)));
  const failRead = () => {
    fetchMock.mockResolvedValueOnce(new Response('{"error":"fixture failure"}', { status: 500 }));
    return client.fetchQuery({ queryKey: ['credentials', me.id], queryFn: () => api.getUserCredentials(me.id), staleTime: 0 });
  };

  it('shows a failed read as failed, never as Not configured', async () => {
    await expect(failRead()).rejects.toThrow();
    const html = renderPage();
    expect(html).toContain('Credentials could not be loaded');
    expect(html).toContain('API error 500: {&quot;error&quot;:&quot;fixture failure&quot;}');
    expect(html).not.toContain('Not configured');
    expect(html).not.toContain('<input type="password" placeholder="Enter');
  });

  it('keeps the cards and says they may be out of date when a later read fails', async () => {
    client.setQueryData(['credentials', me.id], [saved]);
    await expect(failRead()).rejects.toThrow();
    const html = renderPage();
    expect(html).toContain('which may be out of date');
    expect([...html.matchAll(/>(SVN Password|SVN Token|Git Token)<\/h3>/g)].map((m) => m[1])).toEqual(['SVN Password', 'SVN Token', 'Git Token']);
  });

  it('draws each card from the read', () => {
    client.setQueryData(['credentials', me.id], [saved]);
    const html = renderPage();
    expect(html).not.toContain('Credentials could not be loaded');
    expect((html.match(/<\/svg>Configured<\/span>/g) || []).length).toBe(1);
    expect((html.match(/<\/svg>Not configured<\/span>/g) || []).length).toBe(2);
  });

  it('shows loading, not the cards, before the first read', () => {
    const html = renderPage();
    expect(html).toContain('<div class="text-center py-8 text-gray-400" role="status">Loading credentials...</div>');
    expect(html).not.toContain('Not configured');
  });
});
