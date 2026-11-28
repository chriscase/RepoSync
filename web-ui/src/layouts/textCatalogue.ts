/**
 * #264 E3: one text catalogue with two vocabularies.
 *
 * `existing` is the text Classic, Individual and Team show, byte for byte as before
 * #264. `plain` is the vocabulary of the #263 crosswalk, used by Ledger, Console and
 * Guide. Safety and truthful-status sentences are never entries here: they are the
 * source's own and identical in every layout (decision D17). The acceptance lab finds
 * controls by their existing labels, so an existing entry changes only with it.
 */
export interface CatalogueEntry {
  readonly existing: string;
  readonly plain: string;
}

function entries<K extends string>(value: Record<K, CatalogueEntry>): Readonly<Record<K, CatalogueEntry>> {
  for (const entry of Object.values<CatalogueEntry>(value)) Object.freeze(entry);
  return Object.freeze(value);
}

export const TEXT_CATALOGUE = entries({
  'shell.layout': { existing: 'Layout', plain: 'Layout' },
  'shell.theme': { existing: 'Theme', plain: 'Theme' },

  // Dashboard (pages/Dashboard.tsx). The scope label is both the select's option and the context label.
  'dashboard.scopeAll': { existing: 'All Repositories', plain: 'All repositories' },
  'dashboard.manageRepositories': { existing: 'Manage Repositories →', plain: 'Manage repositories →' },
  'dashboard.records': { existing: 'Recent Sync Records', plain: 'Recent sync records' },
  'dashboard.commitMap': { existing: 'Commit Map (SVN ↔ Git)', plain: 'Commit map (SVN ⇄ Git)' },
  'dashboard.activity': { existing: 'Recent Activity', plain: 'Recent activity' },

  // Server panel (components/ServerMonitor.tsx), on the dashboard and the repository page.
  'server.title': { existing: 'Server Monitor', plain: 'Server' },

  // Import card (components/ImportProgressCard.tsx). The stop dialog's own buttons are not entries.
  'import.manageRepositories': { existing: 'Manage Repos', plain: 'Manage repositories' },
  'import.stop': { existing: 'Stop import', plain: 'Stop import…' },

  // Add Repository (pages/Repositories.tsx): the page button, the empty-state button and the dialog title.
  'repos.add': { existing: 'Add Repository', plain: 'Add repository' },
  'repos.add.name': { existing: 'Repository Name', plain: 'Name' },
  'repos.add.svn': { existing: 'SVN Configuration', plain: 'SVN source' },
  'repos.add.git': { existing: 'Git Configuration', plain: 'Git target' },
  'repos.add.sync': { existing: 'Sync Settings', plain: 'Sync settings' },
  'repos.add.pollInterval': { existing: 'Poll Interval (seconds)', plain: 'Poll interval (seconds)' },
  'repos.add.lfsThreshold': { existing: 'LFS Threshold (MB, 0 = disabled)', plain: 'LFS threshold (MB, 0 = disabled)' },

  // Field labels shared by the Add Repository dialog and the repository page (crosswalk, Settings).
  'field.svnUrl': { existing: 'SVN URL', plain: 'URL' },
  'field.svnPassword': { existing: 'Password', plain: 'SVN password' },
  'field.defaultBranch': { existing: 'Default Branch', plain: 'Branch' },
  'field.gitToken': { existing: 'Git Token', plain: 'Git token' },
  'field.pollInterval': { existing: 'Poll Interval', plain: 'Poll interval' },
  'field.lfsThreshold': { existing: 'LFS Threshold', plain: 'LFS threshold' },
  'field.autoMerge': { existing: 'Auto Merge', plain: 'Auto merge' },

  // Repository page (pages/RepoDetail.tsx): section headings.
  'repo.section.svn': { existing: 'SVN Configuration', plain: 'SVN source' },
  'repo.section.git': { existing: 'Git Configuration', plain: 'Git target' },
  'repo.section.sync': { existing: 'Sync Settings', plain: 'Sync policy' },
  'repo.section.records': { existing: 'Sync Records', plain: 'Sync records' },
  'repo.section.commitMap': { existing: 'Commit Map (SVN ↔ Git)', plain: 'Commit map (SVN ⇄ Git)' },
  'repo.section.audit': { existing: 'Recent Audit Log', plain: 'Recent audit log' },
  'repo.section.pairs': { existing: 'Branch Pairs', plain: 'Branch pairs' },
  'repo.section.stopOrRemove': { existing: 'Danger Zone', plain: 'Stop or remove' },
  // Repository page: field labels whose existing text differs from the Add Repository dialog's.
  'repo.field.svnPassword': { existing: 'SVN Password', plain: 'SVN password' },
  'repo.field.gitToken': { existing: 'Token', plain: 'Git token' },
  'repo.field.pollInterval': { existing: 'Poll Interval (s)', plain: 'Poll interval (s)' },
  'repo.field.lfsThreshold': { existing: 'LFS Threshold (MB)', plain: 'LFS threshold (MB)' },
  'repo.about.lastUpdated': { existing: 'Last Updated', plain: 'Last updated' },
  'repo.about.createdBy': { existing: 'Created By', plain: 'Created by' },
  // Repository page: controls. The header pause entry is used only where a repository (not a
  // branch pair) shows “Turn sync off”; “Turn sync on” is not an entry.
  'repo.header.pause': { existing: 'Turn sync off', plain: 'Pause sync…' },
  'repo.header.hook': { existing: 'Git Hook', plain: 'Download Git pre-commit hook' },
  'repo.paused.skip': { existing: 'Skip Commit', plain: 'Skip commits…' },

  // My account (pages/UserSettings.tsx). The “Change Password” button is not an entry.
  'account.profile': { existing: 'My Profile', plain: 'My profile' },
  'account.changePassword': { existing: 'Change Password', plain: 'Change password' },
  'account.credentials': { existing: 'My Credentials', plain: 'My credentials' },
  'account.credential.svnPassword': { existing: 'SVN Password', plain: 'SVN password' },
  'account.credential.svnToken': { existing: 'SVN Token', plain: 'SVN token' },
  'account.credential.gitToken': { existing: 'Git Token', plain: 'Git token' },

  // Identity & notifications (pages/Config.tsx).
  'config.identity': { existing: 'Author Identity Mappings', plain: 'Author identity mappings' },
  'config.notifications': { existing: 'Notifications — Microsoft Teams', plain: 'Notifications: Microsoft Teams' },

  // Conflict page (pages/ConflictDetail.tsx): the view toggle. “Apply Manual Edit” is not an entry.
  'conflict.view.diff': { existing: 'Side-by-Side Diff', plain: 'Side-by-side diff' },
  'conflict.view.edit': { existing: 'Manual Edit', plain: 'Manual edit' },
  // Page titles that name the navigation entry.
  'account.title': { existing: 'Settings', plain: 'My account' },
  'config.title': { existing: 'Configuration', plain: 'Identity & notifications' },
});

export type TextKey = keyof typeof TEXT_CATALOGUE;
