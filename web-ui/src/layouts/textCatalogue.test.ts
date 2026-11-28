import { describe, expect, it } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { TEXT_CATALOGUE, type TextKey } from './textCatalogue';
import { VocabularyProvider } from './VocabularyProvider';
import { useText } from './vocabulary';
import { VOCABULARY_IDS } from './layoutCatalog';

// This project installs no Node type definitions (see LayoutFoundation.test.ts), so node:fs is
// loaded through a specifier the type checker does not resolve. Vitest runs these tests in Node.
const NODE_FS = 'node:fs';
const readSource = async (path: URL) => ((await import(/* @vite-ignore */ NODE_FS)) as { readFileSync(path: URL, encoding: 'utf8'): string }).readFileSync(path, 'utf8');

/**
 * Today's text, written out by hand (not derived from the catalogue): Classic, Individual and Team
 * show exactly this. Changing one of these values changes the existing vocabulary and must fail here.
 */
const EXISTING: Readonly<Record<TextKey, string>> = {
  'shell.layout': 'Layout',
  'shell.theme': 'Theme',
  'dashboard.scopeAll': 'All Repositories',
  'dashboard.manageRepositories': 'Manage Repositories →',
  'dashboard.records': 'Recent Sync Records',
  'dashboard.commitMap': 'Commit Map (SVN ↔ Git)',
  'dashboard.activity': 'Recent Activity',
  'server.title': 'Server Monitor',
  'import.manageRepositories': 'Manage Repos',
  'import.stop': 'Stop import',
  'repos.add': 'Add Repository',
  'repos.add.name': 'Repository Name',
  'repos.add.svn': 'SVN Configuration',
  'repos.add.git': 'Git Configuration',
  'repos.add.sync': 'Sync Settings',
  'repos.add.pollInterval': 'Poll Interval (seconds)',
  'repos.add.lfsThreshold': 'LFS Threshold (MB, 0 = disabled)',
  'field.svnUrl': 'SVN URL',
  'field.svnPassword': 'Password',
  'field.defaultBranch': 'Default Branch',
  'field.gitToken': 'Git Token',
  'field.pollInterval': 'Poll Interval',
  'field.lfsThreshold': 'LFS Threshold',
  'field.autoMerge': 'Auto Merge',
  'repo.section.svn': 'SVN Configuration',
  'repo.section.git': 'Git Configuration',
  'repo.section.sync': 'Sync Settings',
  'repo.section.records': 'Sync Records',
  'repo.section.commitMap': 'Commit Map (SVN ↔ Git)',
  'repo.section.audit': 'Recent Audit Log',
  'repo.section.pairs': 'Branch Pairs',
  'repo.section.stopOrRemove': 'Danger Zone',
  'repo.field.svnPassword': 'SVN Password',
  'repo.field.gitToken': 'Token',
  'repo.field.pollInterval': 'Poll Interval (s)',
  'repo.field.lfsThreshold': 'LFS Threshold (MB)',
  'repo.about.lastUpdated': 'Last Updated',
  'repo.about.createdBy': 'Created By',
  'repo.header.pause': 'Turn sync off',
  'repo.header.hook': 'Git Hook',
  'repo.paused.skip': 'Skip Commit',
  'account.profile': 'My Profile',
  'account.changePassword': 'Change Password',
  'account.credentials': 'My Credentials',
  'account.credential.svnPassword': 'SVN Password',
  'account.credential.svnToken': 'SVN Token',
  'account.credential.gitToken': 'Git Token',
  'config.identity': 'Author Identity Mappings',
  'config.notifications': 'Notifications — Microsoft Teams',
  'conflict.view.diff': 'Side-by-Side Diff',
  'conflict.view.edit': 'Manual Edit',
  'account.title': 'Settings',
  'config.title': 'Configuration',
};

/** The plain vocabulary of Ledger, Console and Guide, from the #263 crosswalk and the #264 brief. */
const PLAIN: Readonly<Record<TextKey, string>> = {
  'shell.layout': 'Layout',
  'shell.theme': 'Theme',
  'dashboard.scopeAll': 'All repositories',
  'dashboard.manageRepositories': 'Manage repositories →',
  'dashboard.records': 'Recent sync records',
  'dashboard.commitMap': 'Commit map (SVN ⇄ Git)',
  'dashboard.activity': 'Recent activity',
  'server.title': 'Server',
  'import.manageRepositories': 'Manage repositories',
  'import.stop': 'Stop import…',
  'repos.add': 'Add repository',
  'repos.add.name': 'Name',
  'repos.add.svn': 'SVN source',
  'repos.add.git': 'Git target',
  'repos.add.sync': 'Sync settings',
  'repos.add.pollInterval': 'Poll interval (seconds)',
  'repos.add.lfsThreshold': 'LFS threshold (MB, 0 = disabled)',
  'field.svnUrl': 'URL',
  'field.svnPassword': 'SVN password',
  'field.defaultBranch': 'Branch',
  'field.gitToken': 'Git token',
  'field.pollInterval': 'Poll interval',
  'field.lfsThreshold': 'LFS threshold',
  'field.autoMerge': 'Auto merge',
  'repo.section.svn': 'SVN source',
  'repo.section.git': 'Git target',
  'repo.section.sync': 'Sync policy',
  'repo.section.records': 'Sync records',
  'repo.section.commitMap': 'Commit map (SVN ⇄ Git)',
  'repo.section.audit': 'Recent audit log',
  'repo.section.pairs': 'Branch pairs',
  'repo.section.stopOrRemove': 'Stop or remove',
  'repo.field.svnPassword': 'SVN password',
  'repo.field.gitToken': 'Git token',
  'repo.field.pollInterval': 'Poll interval (s)',
  'repo.field.lfsThreshold': 'LFS threshold (MB)',
  'repo.about.lastUpdated': 'Last updated',
  'repo.about.createdBy': 'Created by',
  'repo.header.pause': 'Pause sync…',
  'repo.header.hook': 'Download Git pre-commit hook',
  'repo.paused.skip': 'Skip commits…',
  'account.profile': 'My profile',
  'account.changePassword': 'Change password',
  'account.credentials': 'My credentials',
  'account.credential.svnPassword': 'SVN password',
  'account.credential.svnToken': 'SVN token',
  'account.credential.gitToken': 'Git token',
  'config.identity': 'Author identity mappings',
  'config.notifications': 'Notifications: Microsoft Teams',
  'conflict.view.diff': 'Side-by-side diff',
  'conflict.view.edit': 'Manual edit',
  'account.title': 'My account',
  'config.title': 'Identity & notifications',
};

/** The only entries the crosswalk keeps the same in both vocabularies. */
const UNCHANGED: readonly TextKey[] = ['shell.layout', 'shell.theme'];

const keys = Object.keys(TEXT_CATALOGUE) as TextKey[];

describe('#264 E3 text catalogue', () => {
  it('is frozen, entry by entry, and every entry has exactly a non-empty existing and plain text', () => {
    expect(Object.isFrozen(TEXT_CATALOGUE)).toBe(true);
    for (const key of keys) {
      const entry = TEXT_CATALOGUE[key];
      expect(Object.isFrozen(entry), key).toBe(true);
      expect(Object.keys(entry).sort(), key).toEqual(['existing', 'plain']);
      expect(entry.existing.trim(), key).not.toBe('');
      expect(entry.plain.trim(), key).not.toBe('');
      expect(entry.existing, key).toBe(entry.existing.trim());
      expect(entry.plain, key).toBe(entry.plain.trim());
    }
    expect(Reflect.set(TEXT_CATALOGUE['repos.add'], 'existing', 'Add repository')).toBe(false);
    expect(Reflect.set(TEXT_CATALOGUE, 'repos.add', { existing: 'x', plain: 'x' })).toBe(false);
  });

  it('declares every key once in the source', async () => {
    const source = await readSource(new URL('./textCatalogue.ts', import.meta.url));
    const declared = Array.from(source.matchAll(/^\s*'([a-zA-Z.]+)': \{ existing:/gm), match => match[1]);
    expect(declared).toEqual(keys);
    expect(new Set(declared).size).toBe(declared.length);
  });

  it('lists every existing value exactly as today', () => {
    expect(Object.fromEntries(keys.map(key => [key, TEXT_CATALOGUE[key].existing]))).toEqual(EXISTING);
  });

  it('lists every plain value', () => {
    expect(Object.fromEntries(keys.map(key => [key, TEXT_CATALOGUE[key].plain]))).toEqual(PLAIN);
  });

  it('differs between the vocabularies only where the crosswalk changes the label', () => {
    for (const key of keys) {
      expect(TEXT_CATALOGUE[key].existing === TEXT_CATALOGUE[key].plain, key).toBe(UNCHANGED.includes(key));
    }
  });

  it('holds rendered characters, never HTML entities', () => {
    for (const key of keys) for (const vocabulary of VOCABULARY_IDS) expect(TEXT_CATALOGUE[key][vocabulary], key).not.toMatch(/&[a-zA-Z#0-9]+;/);
    expect(TEXT_CATALOGUE['dashboard.commitMap'].existing).toContain('↔');
    expect(TEXT_CATALOGUE['dashboard.manageRepositories'].existing.endsWith('→')).toBe(true);
  });

  it('holds no D11 correction and no safety or truthful-status sentence', () => {
    const forbidden = ['Trigger Sync', 'Request sync', 'Retry', 'Resume sync…', 'Cancel operation', 'Stop operation…', 'Sync configured enabled', 'Sync configured disabled', 'Turn sync on', 'Apply Manual Edit', 'Logout', 'Sign out'];
    for (const key of keys) for (const vocabulary of VOCABULARY_IDS) expect(forbidden, key).not.toContain(TEXT_CATALOGUE[key][vocabulary]);
    for (const key of keys) for (const vocabulary of VOCABULARY_IDS) expect(TEXT_CATALOGUE[key][vocabulary], key).not.toMatch(/\.$/);
  });

  it('is used by the pages: every key except the shell entries appears as t(key) in a page or component', async () => {
    const files = ['../pages/Dashboard.tsx', '../pages/Repositories.tsx', '../pages/RepoDetail.tsx', '../pages/UserSettings.tsx', '../pages/Config.tsx', '../pages/ConflictDetail.tsx', '../components/ServerMonitor.tsx', '../components/ImportProgressCard.tsx'];
    const sources = await Promise.all(files.map(file => readSource(new URL(file, import.meta.url))));
    const used = new Set(sources.flatMap(source => Array.from(source.matchAll(/\bt\('([a-zA-Z.]+)'\)/g), match => match[1])));
    expect([...used].filter(key => !(key in TEXT_CATALOGUE))).toEqual([]);
    expect(keys.filter(key => !used.has(key))).toEqual(['shell.layout', 'shell.theme']);
    // The pages no longer carry the replaced literals as text; the HTML entity forms are gone too.
    const joined = sources.join('\n');
    for (const literal of ['Recent Sync Records', 'Commit Map (SVN &harr; Git)', 'Manage Repositories &rarr;', '>Repository Name<', 'Server Monitor</span>', 'Manage Repos</span>', '>My Credentials<', 'Side-by-Side Diff\n']) {
      expect(joined, literal).not.toContain(literal);
    }
  });
});

describe('#264 E3 useText', () => {
  function Probe({ keys: probeKeys }: { keys: readonly TextKey[] }) {
    const t = useText();
    return createElement('ul', null, probeKeys.map(key => createElement('li', { key }, t(key))));
  }
  const render = (vocabulary?: 'existing' | 'plain') => {
    const probe = createElement(Probe, { keys });
    return renderToStaticMarkup(vocabulary ? createElement(VocabularyProvider, { vocabulary, children: probe }) : probe);
  };
  const items = (html: string) => Array.from(html.matchAll(/<li>([^<]*)<\/li>/g), match => match[1].replace(/&amp;/g, '&'));

  it('returns the existing text outside the shell and under the existing vocabulary', () => {
    expect(render()).toBe(render('existing'));
    expect(items(render('existing'))).toEqual(keys.map(key => EXISTING[key]));
  });

  it('returns the plain text under the plain vocabulary', () => {
    expect(items(render('plain'))).toEqual(keys.map(key => PLAIN[key]));
  });
});
