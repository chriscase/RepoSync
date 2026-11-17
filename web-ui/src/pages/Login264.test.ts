import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import Login, { DirectorySignInHint, type DirectoryRead } from './Login';

const hint = (read: DirectoryRead) => renderToStaticMarkup(createElement(DirectorySignInHint, { read }));

describe('#264 G01.2 sign-in directory hint', () => {
  it('shows no hint while the read is pending or after it failed', () => {
    expect(hint({ state: 'pending' })).toBe('');
    expect(hint({ state: 'failed' })).toBe('');
  });

  it('shows the existing banner wording for a completed read with directory sign-in on and a domain', () => {
    const html = hint({ state: 'loaded', info: { ldap_enabled: true, ldap_domain: 'corp.example' } });
    expect(html).toContain('<span class="font-medium">Corporate login enabled</span>');
    expect(html).toContain('(corp.example)');
    expect(html).toContain('Sign in with your network credentials');
    expect(html).toContain('rs-bn rs-bn--info bg-blue-900/30 border border-blue-700/50 text-blue-300');
  });

  it.each<[string, unknown]>([
    ['switched off', { ldap_enabled: false, ldap_domain: 'corp.example' }],
    ['a string instead of true', { ldap_enabled: 'true', ldap_domain: 'corp.example' }],
    ['a number instead of true', { ldap_enabled: 1, ldap_domain: 'corp.example' }],
    ['no flag', { ldap_domain: 'corp.example' }],
    ['no domain', { ldap_enabled: true, ldap_domain: null }],
    ['an empty domain', { ldap_enabled: true, ldap_domain: '' }],
    ['a blank domain', { ldap_enabled: true, ldap_domain: '   ' }],
    ['a non-text domain', { ldap_enabled: true, ldap_domain: 42 }],
    ['null', null],
    ['text', 'ldap_enabled'],
    ['a list', [true]],
  ])('shows no false hint for a completed read with %s', (_label, info) => {
    expect(hint({ state: 'loaded', info })).toBe('');
  });
});

describe('#264 G01.2 sign-in page before the read completes', () => {
  beforeEach(() => {
    const entries = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => entries.get(key) ?? null,
      setItem: (key: string, value: string) => entries.set(key, value),
      removeItem: (key: string) => entries.delete(key),
      get length() { return entries.size; },
      key: (index: number) => Array.from(entries.keys())[index] ?? null,
    });
  });
  afterEach(() => { vi.unstubAllGlobals(); });

  it('keeps the form, the plain placeholders and no banner', () => {
    const html = renderToStaticMarkup(createElement(MemoryRouter, { initialEntries: ['/login'] }, createElement(Login)));
    expect(html).toContain('Sign in to the sync dashboard');
    expect(html).toContain('placeholder="Username"');
    expect(html).toContain('placeholder="Password"');
    expect(html).toContain('>Sign in</button>');
    expect(html).not.toContain('Corporate login enabled');
    expect(html).not.toContain('Network username');
    expect(html).not.toContain('Network password');
    expect(html).not.toContain('rs-bn--info');
  });
});
