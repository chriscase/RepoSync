import { describe, it, expect } from 'vitest';
import { createElement, type ReactElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import type { WizardSetupConfig } from '../api';
import SetupWizard, {
  DEFAULT_DATA,
  GitStep,
  IdentityStep,
  ReviewStep,
  SavedConfigGate,
  ServerAuthStep,
  SvnStep,
  SyncStep,
  savedConfigFields,
  savedConfigView,
  validateStep,
  type SavedConfigRead,
  type WizardData,
} from './SetupWizard';

const noop = () => {};
const MARKER = 'Collected but not sent today';
const wizard = (over: Partial<WizardData> = {}): WizardData => ({ ...DEFAULT_DATA, ...over });
const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const count = (html: string, text: string) => html.split(text).length - 1;

function stepElement(step: number, data: WizardData, errors: Record<string, string>): ReactElement {
  switch (step) {
    case 1: return createElement(SvnStep, { data, update: noop, errors, svnPasswordSaved: false, svnPasswordDirty: false, onSvnPasswordChange: noop });
    case 2: return createElement(GitStep, { data, update: noop, errors, gitTokenSaved: false, gitTokenDirty: false, onGitTokenChange: noop });
    case 3: return createElement(SyncStep, { data, update: noop, errors });
    case 4: return createElement(IdentityStep, { data, update: noop, errors });
    case 5: return createElement(ServerAuthStep, { data, update: noop, errors });
    default: throw new Error(`no form step ${step}`);
  }
}

/** Data that fails every client check on every step. */
const invalid = wizard({
  svn_url: 'kiln',
  svn_username: ' ',
  svn_trunk_path: ' ',
  git_api_url: 'ftp://git.example.org',
  git_repo: 'kiln-controller',
  lfs_threshold_mb: -1,
  identity_email_domain: 'not a domain',
  web_listen: '8080',
  daemon_poll_interval: 0,
  daemon_data_dir: ' ',
  web_admin_password_env: ' ',
  web_auth_mode: 'simple',
});

describe('#264 G12.9 setup validation runs each check on the step that shows its field', () => {
  it('keeps the same rules and messages, now on the step that shows each field', () => {
    expect(validateStep(1, invalid)).toEqual({
      svn_url: 'Must start with svn://, http://, or https://',
      svn_username: 'Username is required',
      svn_trunk_path: 'Trunk path is required',
    });
    expect(validateStep(2, invalid)).toEqual({
      git_api_url: 'Must be a valid HTTP(S) URL',
      git_repo: 'Must be in owner/repo format',
    });
    expect(validateStep(3, invalid)).toEqual({ lfs_threshold_mb: 'LFS threshold must be non-negative' });
    expect(validateStep(4, invalid)).toEqual({ identity_email_domain: 'Must be a valid domain (e.g. company.com)' });
    expect(validateStep(5, invalid)).toEqual({
      web_listen: 'Must be in host:port format (e.g. 0.0.0.0:8080)',
      daemon_poll_interval: 'Must be at least 1 second',
      daemon_data_dir: 'Data directory is required',
      web_admin_password_env: 'Admin password env var is required for simple auth',
    });
    for (const step of [0, 6, 7]) expect(validateStep(step, invalid)).toEqual({});
  });

  it('no longer blocks a step on a field that another step shows', () => {
    // Sync Settings with only a bad poll interval and a bad domain.
    expect(validateStep(3, wizard({ daemon_poll_interval: 0, identity_email_domain: 'nope' }))).toEqual({});
    // Identity Mapping with only a negative LFS threshold.
    expect(validateStep(4, wizard({ lfs_threshold_mb: -5 }))).toEqual({});
    // Server & Auth with only a bad domain and a negative LFS threshold.
    expect(validateStep(5, wizard({ identity_email_domain: 'nope', lfs_threshold_mb: -5 }))).toEqual({});
  });

  it('still accepts valid values on the steps that now hold the moved checks', () => {
    expect(validateStep(3, wizard({ lfs_threshold_mb: 0 }))).toEqual({});
    expect(validateStep(4, wizard({ identity_email_domain: 'example.org' }))).toEqual({});
    expect(validateStep(4, wizard({ identity_email_domain: '' }))).toEqual({});
    expect(validateStep(5, wizard({ daemon_poll_interval: 1 }))).toEqual({});
  });

  it.each([1, 2, 3, 4, 5])('step %i draws every error it reports next to a field on that step', step => {
    const errors = validateStep(step, invalid);
    expect(Object.keys(errors).length).toBeGreaterThan(0);
    const html = renderToStaticMarkup(stepElement(step, invalid, errors));
    expect(count(html, 'role="alert"')).toBe(Object.keys(errors).length);
    for (const message of Object.values(errors)) {
      const alert = html.match(new RegExp(`<p id="([^"]+)" role="alert"[^>]*>${escapeRe(message)}</p>`));
      expect(alert, message).toBeTruthy();
      const errorId = alert![1];
      const input = html.match(new RegExp(`<input[^>]*aria-describedby="${escapeRe(errorId)}"[^>]*>`))?.[0];
      expect(input, message).toBeTruthy();
      expect(input).toContain('aria-invalid="true"');
      const inputId = input!.match(/ id="([^"]+)"/)?.[1];
      expect(html).toContain(`for="${inputId}"`);
    }
  });

  it('keeps the LFS threshold help tied to its field when there is no error', () => {
    const html = renderToStaticMarkup(stepElement(3, wizard(), {}));
    const input = html.match(/<input[^>]*placeholder="0 = disabled"[^>]*>/)?.[0];
    expect(input).toBeTruthy();
    expect(input).not.toContain('aria-invalid');
    const helpId = input!.match(/aria-describedby="([^"]+)"/)?.[1];
    expect(html).toContain(`id="${helpId}" class="text-xs text-gray-400 mt-1">Files larger than this are tracked via Git LFS. Set 0 to disable.</p>`);
    expect(html).not.toContain('role="alert"');
  });
});

describe('#264 G12.4 and G12.6 mark the fields that are collected but not sent today', () => {
  it('step 4 marks the pull request settings, and only when they are shown', () => {
    const pr = renderToStaticMarkup(stepElement(3, wizard({ sync_mode: 'pr' }), {}));
    expect(count(pr, MARKER)).toBe(1);
    const heading = pr.indexOf('Pull Request Settings');
    expect(heading).toBeGreaterThan(-1);
    expect(pr.indexOf(MARKER)).toBeGreaterThan(pr.indexOf('Auto-merge PRs'));
    expect(pr.indexOf('Auto-merge PRs')).toBeGreaterThan(heading);
    expect(pr).toContain('class="rs-mark');

    const direct = renderToStaticMarkup(stepElement(3, wizard({ sync_mode: 'direct' }), {}));
    expect(direct).not.toContain('Pull Request Settings');
    expect(direct).not.toContain(MARKER);
  });

  it.each(['simple', 'github_oauth', 'both'] as const)('step 6 marks authentication mode, OAuth and notification fields (%s)', mode => {
    const html = renderToStaticMarkup(stepElement(5, wizard({ web_auth_mode: mode }), {}));
    expect(count(html, MARKER)).toBe(2);
    const first = html.indexOf(MARKER);
    const second = html.indexOf(MARKER, first + 1);
    const daemon = html.indexOf('Daemon Settings');
    const notifications = html.indexOf('Notifications');
    expect(first).toBeGreaterThan(html.indexOf('Authentication Mode'));
    expect(first).toBeLessThan(daemon);
    expect(html.indexOf('Authentication mode and the OAuth fields')).toBeGreaterThan(first);
    expect(html.indexOf('Authentication mode and the OAuth fields')).toBeLessThan(daemon);
    if (mode !== 'simple') expect(first).toBeGreaterThan(html.indexOf('Allowed GitHub Users'));
    expect(second).toBeGreaterThan(html.indexOf('Email Recipients'));
    expect(second).toBeGreaterThan(notifications);
  });

  it('adds no marker to the steps that have none', () => {
    for (const step of [1, 2, 4]) expect(renderToStaticMarkup(stepElement(step, wizard({ sync_mode: 'pr' }), {}))).not.toContain(MARKER);
  });
});

describe('#264 G12.10 a failed read of the saved configuration turns the form off until a read succeeds', () => {
  it('offers Save only after a successful read and turns the form off only after a failed one', () => {
    expect(savedConfigView('reading')).toEqual({ formOff: false, canSave: false });
    expect(savedConfigView('loaded')).toEqual({ formOff: false, canSave: true });
    expect(savedConfigView('failed')).toEqual({ formOff: true, canSave: false });
  });

  const gate = (read: SavedConfigRead) => renderToStaticMarkup(
    createElement(SavedConfigGate, { read, onRetry: noop, children: stepElement(1, wizard(), {}) }),
  );

  it('shows the failure with the client error text and a disabled form', () => {
    const html = gate({ status: 'failed', error: 'API error 500: <synthetic>', retrying: false });
    const alert = html.match(/<div id="([^"]+)" role="alert">(.*?)<\/div>/);
    expect(alert).toBeTruthy();
    expect(alert![2]).toContain('The saved configuration could not be loaded');
    expect(alert![2]).toContain('API error 500: &lt;synthetic&gt;');
    expect(alert![2]).toContain('The form is turned off until the saved configuration is read.');
    expect(html).toContain(`<fieldset disabled="" aria-describedby="${alert![1]}"`);
    expect(html.indexOf('role="alert"')).toBeLessThan(html.indexOf('<fieldset'));
    expect(html).toMatch(/<button type="button" class="rs-btn rs-btn--quiet[^"]*">Retry<\/button>/);
    // The fields are still drawn (inside the disabled fieldset), showing what the page holds.
    expect(html.indexOf('Repository URL')).toBeGreaterThan(html.indexOf('<fieldset'));
  });

  it('disables Retry and says so while a retry is reading', () => {
    const html = gate({ status: 'failed', error: 'API error 502: ', retrying: true });
    expect(html).toMatch(/<button type="button" disabled=""[^>]*>Retry<\/button>/);
    expect(html).toContain('role="status" class="text-xs text-gray-400">Reading the saved configuration…</span>');
    expect(html).toContain('<fieldset disabled=""');
  });

  it.each(['reading', 'loaded'] as const)('leaves the form on and shows no failure while %s', status => {
    const html = gate({ status });
    expect(html).toContain('<fieldset class="min-w-0 disabled:opacity-60">');
    expect(html).not.toContain('<fieldset disabled');
    expect(html).not.toMatch(/<button[^>]*disabled=""[^>]*>Retry/);
    expect(html).toContain('Test Connection</button>');
    expect(html).not.toMatch(/<button[^>]*disabled=""[^>]*>Test Connection/);
    expect(html).not.toContain('role="alert"');
    expect(html).not.toContain('could not be loaded');
  });

  const review = (readStatus: SavedConfigRead['status']) => renderToStaticMarkup(createElement(ReviewStep, {
    data: wizard(), configSaved: false, saveError: '', saveWarnings: [], onSave: noop, readStatus,
  }));
  const saveButton = (html: string) => html.match(/<button[^>]*>Save Configuration to Server<\/button>/)?.[0];

  it('offers Save on the Review step only after a successful read', () => {
    expect(saveButton(review('loaded'))).not.toContain('disabled=""');
    expect(review('loaded')).not.toContain('Waiting for the saved configuration');
    expect(saveButton(review('failed'))).toContain('disabled=""');
    const reading = review('reading');
    expect(saveButton(reading)).toContain('disabled=""');
    expect(reading).toContain('<p role="status" class="mb-4 text-sm text-gray-400">Waiting for the saved configuration to be read.</p>');
  });

  it('renders the page with the form on and no failure before the read settles', () => {
    const html = renderToStaticMarkup(createElement(MemoryRouter, null, createElement(SetupWizard)));
    expect(html).toContain('RepoSync Setup');
    expect(html).toMatch(/<button class="rs-btn rs-btn--primary[^"]*">Get Started<\/button>/);
    expect(html).toContain('<fieldset class="min-w-0 disabled:opacity-60">');
    expect(html).not.toContain('could not be loaded');
  });

  it('maps a successful read into the wizard as before', () => {
    const cfg: WizardSetupConfig = {
      svn_url: 'svn://svn.example.org/repos/kiln', svn_username: 'sync-service', svn_layout: 'standard', svn_trunk_path: 'trunk', svn_password_set: true,
      git_provider: 'gitea', git_api_url: 'http://git.example.org/api/v1', git_repo: 'makers/kiln', git_branch: 'develop', git_token_set: false,
      sync_mode: 'pr', auto_merge: false, sync_tags: false, lfs_threshold: 5 * 1024 * 1024,
      email_domain: 'example.org',
      listen: '127.0.0.1:9090', auth_mode: 'both', poll_interval: 30, log_level: 'debug', data_dir: '/srv/reposync', admin_password_set: true,
    };
    expect(savedConfigFields(cfg)).toEqual({
      fields: {
        svn_url: 'svn://svn.example.org/repos/kiln', svn_username: 'sync-service', svn_layout: 'standard', svn_trunk_path: 'trunk',
        git_provider: 'gitea', git_api_url: 'http://git.example.org/api/v1', git_repo: 'makers/kiln', git_default_branch: 'develop',
        sync_mode: 'pr', sync_auto_merge: false, sync_tags: false, lfs_threshold_mb: 5,
        identity_email_domain: 'example.org',
        web_listen: '127.0.0.1:9090', web_auth_mode: 'both', daemon_poll_interval: 30, daemon_log_level: 'debug', daemon_data_dir: '/srv/reposync',
      },
      svnPasswordSet: true,
      gitTokenSet: false,
      adminPasswordSet: true,
    });
    const empty = savedConfigFields({ ...cfg, svn_url: '', git_repo: '', lfs_threshold: 0, poll_interval: 0, email_domain: '' });
    for (const key of ['svn_url', 'git_repo', 'lfs_threshold_mb', 'daemon_poll_interval', 'identity_email_domain']) expect(empty.fields).not.toHaveProperty(key);
  });
});
