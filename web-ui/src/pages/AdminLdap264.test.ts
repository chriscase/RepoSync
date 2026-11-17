import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import AdminLdap, { BindPasswordStatus, type BindPasswordRead } from './AdminLdap';
import { api, type LdapConfig } from '../api';

const status = (read: BindPasswordRead, saved: boolean, typed: boolean) =>
  renderToStaticMarkup(createElement(BindPasswordStatus, { id: 'bind-state', read, saved, typed }));

const config: LdapConfig = {
  enabled: true,
  url: 'ldaps://ldap.fixture.invalid:3269',
  base_dn: 'dc=fixture,dc=invalid',
  search_filter: '(&(objectClass=user)(name={0}))',
  display_name_attr: 'displayname',
  email_attr: 'mail',
  group_attr: 'memberOf',
  bind_dn: 'cn=svc,dc=fixture,dc=invalid',
  bind_password_set: true,
};

describe('#264 S23 directory bind password state', () => {
  it.each<[BindPasswordRead, boolean, boolean, string, string]>([
    ['read', true, false, 'Saved', 'A password is saved.'],
    ['read', true, true, 'Replacing', 'A password is saved. A new one is typed in the field.'],
    ['read', false, false, 'Not saved', 'No password is saved.'],
    ['read', false, true, 'Not saved', 'No password is saved.'],
    ['checking', true, false, 'Checking', 'Reading whether a password is saved…'],
    ['checking', false, true, 'Checking', 'Reading whether a password is saved…'],
    ['unreadable', true, false, 'Not known', 'Whether a password is saved could not be read.'],
    ['unreadable', false, false, 'Not known', 'Whether a password is saved could not be read.'],
    ['unreadable', false, true, 'Not known', 'Whether a password is saved could not be read.'],
  ])('read %s, saved %s, typed %s shows the word %s', (read, saved, typed, word, sentence) => {
    const html = status(read, saved, typed);
    expect(html).toMatch(new RegExp(`<span class="rs-mark [^"]*"><svg[^>]*aria-hidden="true"[^>]*>.*?</svg>${word}</span>`));
    expect(html).toContain(`<span>${sentence}</span>`);
    expect(html).toContain('id="bind-state"');
    expect(html).toContain('aria-live="polite"');
    expect(html).not.toContain('Leave blank to keep current');
    expect(html).not.toContain('placeholder');
  });

  it('never shows an unread or re-reading state as saved or as not saved', () => {
    for (const read of ['checking', 'unreadable'] as const) {
      for (const saved of [true, false]) {
        const html = status(read, saved, false);
        expect(html).not.toContain('>Saved</span>');
        expect(html).not.toContain('>Not saved</span>');
      }
    }
  });
});

describe('#264 G10 directory page', () => {
  let client: QueryClient;
  const fetchMock = vi.fn();
  beforeEach(() => {
    client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    const entries = new Map([['session_token', 'fixture-token'], ['user', '{"id":"actor","username":"actor","role":"admin"}']]);
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => entries.get(key) ?? null,
      setItem: (key: string, value: string) => entries.set(key, value),
      removeItem: (key: string) => entries.delete(key),
      get length() { return entries.size; },
      key: (index: number) => Array.from(entries.keys())[index] ?? null,
    });
    vi.stubGlobal('window', { location: { pathname: '/ldap', href: '/ldap' } });
    vi.stubGlobal('fetch', fetchMock);
    fetchMock.mockReset();
  });
  afterEach(() => { client.clear(); vi.unstubAllGlobals(); });

  const renderPage = () => renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(AdminLdap)));

  // The fourth group, "Service account", is headed by the page's existing "Service Account
  // (Optional)" heading, so only three headings are added (a fourth would read twice).
  it('adds the three missing group headings as derived elements, in order, between the existing fields', () => {
    client.setQueryData(['ldap-config'], config);
    const html = renderPage();
    const headings = [...html.matchAll(/<h2 data-rs-derived="" class="([^"]*)">([^<]*)<\/h2>/g)];
    expect(headings.map((match) => match[2])).toEqual(['Directory sign-in', 'Connection', 'User mapping']);
    for (const [, className] of headings) expect(className).toContain('rs-heading');
    expect(html).not.toMatch(/<h2 data-rs-derived=""[^>]*(hidden|aria-hidden)/);

    const order = [
      '>Directory sign-in</h2>',
      'id="ldap-enabled"',
      '>Connection</h2>',
      '>Server URL</label>',
      '>Base DN</label>',
      '>User Search Filter</label>',
      '>User mapping</h2>',
      '>Display Name Attribute</label>',
      '>Email Attribute</label>',
      '>Group Membership Attribute</label>',
      'Service Account (Optional)',
      '>Bind DN</label>',
      'Bind Password',
    ];
    const positions = order.map((needle) => html.indexOf(needle));
    expect(positions.every((position) => position >= 0)).toBe(true);
    expect([...positions].sort((a, b) => a - b)).toEqual(positions);
  });

  it('keeps the first field flush with the top of the form when the first heading is hidden', () => {
    client.setQueryData(['ldap-config'], config);
    const html = renderPage();
    expect(html).toContain('<h2 data-rs-derived="" class="rs-heading text-sm font-semibold text-gray-300 mb-3 [&amp;+*]:!mt-0">Directory sign-in</h2>');
  });

  it('describes the bind password field with its state as a word and a mark', () => {
    client.setQueryData(['ldap-config'], config);
    const html = renderPage();
    const field = html.match(/<input type="password"[^>]*>/)?.[0] ?? '';
    const describedBy = field.match(/aria-describedby="([^"]+)"/)?.[1];
    expect(describedBy).toBeTruthy();
    expect(html).toContain(`<p id="${describedBy}" aria-live="polite" data-secret-state="saved"`);
    expect(field).toContain('placeholder=');
  });

  it('shows the state as not saved when the settings say so', () => {
    client.setQueryData(['ldap-config'], { ...config, bind_password_set: false });
    expect(renderPage()).toContain('data-secret-state="not-saved"');
  });

  it('shows the state as not known when the settings could not be read', async () => {
    client = new QueryClient({ defaultOptions: { queries: { retry: false, retryOnMount: false, staleTime: Infinity } } });
    fetchMock.mockResolvedValueOnce(new Response('{"error":"fixture failure"}', { status: 500 }));
    await expect(client.fetchQuery({ queryKey: ['ldap-config'], queryFn: api.getLdapConfig })).rejects.toThrow();
    const html = renderPage();
    expect(html).toContain('data-secret-state="unknown"');
    expect(html).toContain('Whether a password is saved could not be read.');
  });
});
