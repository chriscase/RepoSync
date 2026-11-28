import { useState } from 'react';

type Section =
  | 'overview'
  | 'getting-started'
  | 'dashboard'
  | 'repositories'
  | 'identity-mapping'
  | 'conflict-resolution'
  | 'configuration'
  | 'personal-mode'
  | 'deployment'
  | 'troubleshooting';

const sections: { id: Section; title: string }[] = [
  { id: 'overview', title: 'Overview' },
  { id: 'getting-started', title: 'Getting Started' },
  { id: 'dashboard', title: 'Dashboard' },
  { id: 'repositories', title: 'Managing Repositories' },
  { id: 'identity-mapping', title: 'Identity Mapping' },
  { id: 'conflict-resolution', title: 'Conflict Resolution' },
  { id: 'configuration', title: 'Configuration' },
  { id: 'personal-mode', title: 'Personal Branch Mode' },
  { id: 'deployment', title: 'Deployment' },
  { id: 'troubleshooting', title: 'Troubleshooting' },
];

function Screenshot({ alt, caption }: { alt: string; caption: string }) {
  return (
    <figure className="my-6">
      <div className="bg-gray-200 border-2 border-dashed border-gray-400 rounded-lg h-64 flex items-center justify-center">
        <div className="text-center text-gray-500">
          <svg
            className="mx-auto h-12 w-12 mb-2"
            fill="none"
            viewBox="0 0 24 24"
            strokeWidth={1.5}
            stroke="currentColor"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              d="m2.25 15.75 5.159-5.159a2.25 2.25 0 0 1 3.182 0l5.159 5.159m-1.5-1.5 1.409-1.409a2.25 2.25 0 0 1 3.182 0l2.909 2.909M3.75 21h16.5A2.25 2.25 0 0 0 22.5 19.5V4.5a2.25 2.25 0 0 0-2.25-2.25H3.75A2.25 2.25 0 0 0 1.5 4.5v15a2.25 2.25 0 0 0 2.25 2.25Z"
            />
          </svg>
          <p className="text-sm font-medium">{alt}</p>
        </div>
      </div>
      <figcaption className="mt-2 text-sm text-gray-500 text-center">
        {caption}
      </figcaption>
    </figure>
  );
}

function SectionOverview() {
  return (
    <div className="space-y-4">
      <p>
        RepoSync is a bidirectional SVN-to-Git synchronization bridge. It
        watches both systems, automatically syncs non-conflicting changes, and
        alerts you only when human intervention is needed.
      </p>
      <h3 className="rs-heading text-lg font-semibold text-gray-900">Two Modes</h3>
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div className="bg-blue-50 border border-blue-200 rounded-lg p-4">
          <h4 className="font-semibold text-blue-900">Team Mode</h4>
          <p className="mt-1 text-sm text-blue-800">
            Full bidirectional sync for entire teams. Runs as a server daemon
            with a web dashboard, identity mapping, and conflict notifications
            via Slack and email.
          </p>
        </div>
        <div className="bg-green-50 border border-green-200 rounded-lg p-4">
          <h4 className="font-semibold text-green-900">Personal Branch Mode</h4>
          <p className="mt-1 text-sm text-green-800">
            Individual developer bridge. Runs on your laptop, imports SVN
            history into a GitHub repo, and uses a PR-based workflow so every
            sync is reviewable.
          </p>
        </div>
      </div>
      <Screenshot
        alt="RepoSync Architecture Diagram"
        caption="High-level architecture showing bidirectional sync between SVN and Git"
      />
      <h3 className="rs-heading text-lg font-semibold text-gray-900">Key Features</h3>
      <ul className="list-disc list-inside space-y-1 text-gray-700">
        <li>Bidirectional sync — SVN commits appear in Git, Git pushes appear in SVN</li>
        <li>Author identity mapping — SVN usernames mapped to Git name+email</li>
        <li>Automatic conflict resolution for non-overlapping changes</li>
        <li>Web dashboard for monitoring sync status and resolving conflicts</li>
        <li>Slack and email notifications when human intervention is needed</li>
        <li>GitHub Enterprise and GitHub.com support</li>
        <li>Configurable SVN layouts — standard or custom paths</li>
        <li>Direct auto-sync between SVN and Git</li>
        <li>Crash recovery with transaction logging</li>
      </ul>
    </div>
  );
}

function SectionGettingStarted() {
  return (
    <div className="space-y-4">
      <p>
        This guide walks you through setting up RepoSync in Team Mode with the
        web dashboard. For Personal Branch Mode, see the{' '}
        <button
          className="text-blue-600 hover:underline"
          onClick={() =>
            document
              .getElementById('nav-personal-mode')
              ?.click()
          }
        >
          Personal Branch Mode
        </button>{' '}
        section.
      </p>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Prerequisites</h3>
      <ul className="list-disc list-inside space-y-1 text-gray-700">
        <li>SVN client (<code className="bg-gray-100 px-1 rounded">svn</code> CLI) installed</li>
        <li>Git installed</li>
        <li>Network access to both your SVN server and GitHub</li>
        <li>A GitHub personal access token with <code className="bg-gray-100 px-1 rounded">repo</code> scope</li>
      </ul>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Installation</h3>
      <div className="space-y-3">
        <p className="font-medium text-gray-800">From binary release:</p>
        <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`curl -fsSL https://github.com/chriscase/RepoSync/releases/latest/download/install.sh | bash`}
        </pre>
        <p className="font-medium text-gray-800">From source:</p>
        <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`git clone https://github.com/chriscase/RepoSync.git
cd RepoSync
cargo build --release`}
        </pre>
        <p className="font-medium text-gray-800">Docker:</p>
        <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`docker pull ghcr.io/chriscase/reposync:latest`}
        </pre>
      </div>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Initial Configuration</h3>
      <p>
        After installation, generate a configuration file and edit it with your
        SVN and GitHub details:
      </p>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`reposync init --config /etc/reposync/config.toml
$EDITOR /etc/reposync/config.toml
$EDITOR /etc/reposync/authors.toml`}
      </pre>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Starting the Daemon</h3>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`# As a systemd service
sudo cp scripts/reposync.service /etc/systemd/system/
sudo systemctl enable --now reposync

# Or with Docker
docker run -d --name reposync -p 8080:8080 \\
  -v /etc/reposync:/etc/reposync:ro \\
  -v /var/lib/reposync:/var/lib/reposync \\
  --env-file /etc/reposync/env \\
  ghcr.io/chriscase/reposync:latest`}
      </pre>
      <p>
        Once the daemon is running, open{' '}
        <code className="bg-gray-100 px-1 rounded">http://your-server:8080</code>{' '}
        to access this dashboard.
      </p>
      <Screenshot
        alt="Login Screen"
        caption="The RepoSync login screen — enter the admin password configured in your environment"
      />
    </div>
  );
}

function SectionDashboard() {
  return (
    <div className="space-y-4">
      <p>
        The dashboard is your central view of all synchronization activity. It
        shows the current sync status, recent activity, and any issues that need
        attention.
      </p>
      <Screenshot
        alt="Dashboard Overview"
        caption="The main dashboard showing sync status, recent activity, and repository health"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Status Cards</h3>
      <p>
        The top row shows at-a-glance metrics: current sync state, last SVN
        revision synced, total syncs performed, active conflicts, and error
        count. When a repository filter is active, these reflect the selected
        repository.
      </p>
      <Screenshot
        alt="Status Cards"
        caption="Status cards showing sync state, revision info, and conflict counts"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Activity Feed</h3>
      <p>
        The activity feed shows recent sync operations in chronological order.
        Each entry includes the direction (SVN→Git or Git→SVN), the commit
        message, author mapping, and timestamp. Failed syncs are highlighted in
        red.
      </p>
      <Screenshot
        alt="Activity Feed"
        caption="Recent sync activity showing bidirectional commits with author mapping"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Import Progress</h3>
      <p>
        When a repository import is running, a progress card appears showing the
        current phase, revision count, and estimated time remaining.
      </p>
      <Screenshot
        alt="Import Progress"
        caption="Import progress card showing phase, revision count, and ETA during initial import"
      />
    </div>
  );
}

function SectionRepositories() {
  return (
    <div className="space-y-4">
      <p>
        RepoSync supports syncing multiple repositories simultaneously. Each
        repository has its own SVN URL, GitHub repo, identity mappings, and sync
        watermarks.
      </p>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Adding a Repository</h3>
      <p>
        From the dashboard or the Repositories page, click{' '}
        <span className="font-semibold">Add Repository</span> and provide:
      </p>
      <ul className="list-disc list-inside space-y-1 text-gray-700">
        <li><strong>Name</strong> — a friendly display name</li>
        <li><strong>SVN URL</strong> — the root URL of your SVN repository</li>
        <li><strong>SVN Username</strong> — service account for SVN access</li>
        <li><strong>GitHub Repo</strong> — in <code className="bg-gray-100 px-1 rounded">owner/name</code> format</li>
        <li><strong>GitHub Token</strong> — a PAT with repo scope</li>
        <li><strong>Default Branch</strong> — typically <code className="bg-gray-100 px-1 rounded">main</code></li>
      </ul>
      <Screenshot
        alt="Add Repository Form"
        caption="The repository setup form with SVN and GitHub connection details"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Test Connection</h3>
      <p>
        After entering credentials, use the <strong>Test Connection</strong>{' '}
        buttons to verify that RepoSync can reach both the SVN server and
        GitHub API before starting a sync.
      </p>
      <Screenshot
        alt="Test Connection Results"
        caption="Connection test showing successful SVN and GitHub connectivity"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Initial Import</h3>
      <p>
        For new repositories, run an initial import to bring the full SVN
        history into Git. This is a one-time operation that replays all SVN
        revisions as Git commits with proper author mapping.
      </p>
      <Screenshot
        alt="Initial Import"
        caption="Running an initial import — SVN history is replayed as Git commits"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Repository Detail</h3>
      <p>
        Click any repository to see its detail page with sync history,
        watermark state, recent errors, and per-repo configuration.
      </p>
      <Screenshot
        alt="Repository Detail Page"
        caption="Repository detail view showing sync history, watermarks, and configuration"
      />
    </div>
  );
}

function SectionIdentityMapping() {
  return (
    <div className="space-y-4">
      <p>
        SVN uses simple usernames (e.g. <code className="bg-gray-100 px-1 rounded">alice</code>)
        while Git uses name + email pairs. RepoSync maintains a mapping between
        the two so that commits are attributed correctly in both systems.
      </p>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Authors File</h3>
      <p>
        The mapping is defined in a TOML file (typically{' '}
        <code className="bg-gray-100 px-1 rounded">/etc/reposync/authors.toml</code>):
      </p>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`[authors]
alice = { name = "Alice Johnson", email = "alice@company.com", github = "alicej" }
bob   = { name = "Bob Williams", email = "bob@company.com", github = "bobw" }

[defaults]
email_domain = "company.com"`}
      </pre>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">How Mapping Works</h3>
      <div className="bg-gray-50 rounded-lg p-4 space-y-2">
        <p><strong>SVN → Git:</strong> When syncing an SVN commit by user{' '}
          <code className="bg-gray-100 px-1 rounded">alice</code>, RepoSync looks up the
          mapping and creates the Git commit with author{' '}
          <code className="bg-gray-100 px-1 rounded">Alice Johnson &lt;alice@company.com&gt;</code>.
        </p>
        <p><strong>Git → SVN:</strong> When syncing a Git commit by{' '}
          <code className="bg-gray-100 px-1 rounded">Alice Johnson</code>, RepoSync reverse-maps
          to SVN username <code className="bg-gray-100 px-1 rounded">alice</code> and sets the
          SVN revision property accordingly.
        </p>
      </div>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Unmapped Users</h3>
      <p>
        If an SVN username isn't in the mapping file, RepoSync falls back to{' '}
        <code className="bg-gray-100 px-1 rounded">username@email_domain</code> using the
        default domain from the config. This ensures syncs never fail due to
        missing mappings — but you should add explicit mappings for accurate
        attribution.
      </p>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">LDAP Integration</h3>
      <p>
        For large teams, RepoSync can optionally query LDAP/Active Directory to
        automatically resolve SVN usernames to name+email. Configure the LDAP
        connection in your config file under the{' '}
        <code className="bg-gray-100 px-1 rounded">[identity]</code> section.
      </p>
      <Screenshot
        alt="Identity Mapping Configuration"
        caption="Author mapping configuration showing SVN username to Git identity resolution"
      />
    </div>
  );
}

function SectionConflictResolution() {
  return (
    <div className="space-y-4">
      <p>
        When the same file is modified on both sides between sync cycles,
        RepoSync detects the conflict and attempts automatic resolution. If
        auto-merge fails, the conflict is flagged for human review.
      </p>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Automatic Resolution</h3>
      <p>
        RepoSync uses three-way merge with the last synced state as the common
        ancestor. Non-overlapping changes (different lines of the same file) are
        merged automatically. Overlapping changes create a conflict.
      </p>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Conflict Types</h3>
      <div className="space-y-2">
        <div className="flex items-start space-x-3 p-3 bg-yellow-50 rounded-lg">
          <span className="text-yellow-600 font-bold text-lg">!</span>
          <div>
            <p className="font-medium text-yellow-900">Content Conflict</p>
            <p className="text-sm text-yellow-800">Same lines modified on both sides. Requires manual review.</p>
          </div>
        </div>
        <div className="flex items-start space-x-3 p-3 bg-orange-50 rounded-lg">
          <span className="text-orange-600 font-bold text-lg">!</span>
          <div>
            <p className="font-medium text-orange-900">Edit/Delete Conflict</p>
            <p className="text-sm text-orange-800">File edited on one side, deleted on the other.</p>
          </div>
        </div>
        <div className="flex items-start space-x-3 p-3 bg-red-50 rounded-lg">
          <span className="text-red-600 font-bold text-lg">!</span>
          <div>
            <p className="font-medium text-red-900">Binary Conflict</p>
            <p className="text-sm text-red-800">Binary files can't be merged — one side must be chosen.</p>
          </div>
        </div>
      </div>

      <Screenshot
        alt="Conflict List"
        caption="The conflicts page showing pending conflicts with file paths and conflict types"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Resolving Conflicts</h3>
      <p>
        Click a conflict to see a side-by-side diff of the SVN and Git versions.
        Choose to accept the SVN version, the Git version, or provide a manual
        merge. Once resolved, the fix is synced to both systems.
      </p>
      <Screenshot
        alt="Conflict Detail with Diff"
        caption="Side-by-side diff view for resolving a content conflict"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Notifications</h3>
      <p>
        When a conflict requires human intervention, RepoSync can notify your
        team via Slack webhook or email. Configure notification channels in the{' '}
        <code className="bg-gray-100 px-1 rounded">[notifications]</code> section of your
        config file.
      </p>
    </div>
  );
}

function SectionConfiguration() {
  return (
    <div className="space-y-4">
      <p>
        RepoSync is configured via a TOML file (default location:{' '}
        <code className="bg-gray-100 px-1 rounded">/etc/reposync/config.toml</code>). The
        Configuration page in the dashboard shows the current settings and
        allows editing key values.
      </p>
      <Screenshot
        alt="Configuration Page"
        caption="The configuration page showing current sync settings"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Key Configuration Sections</h3>

      <h4 className="font-semibold text-gray-800 mt-4">[daemon]</h4>
      <p className="text-gray-700">
        Controls the sync polling interval, log level, and data directory.
      </p>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`[daemon]
poll_interval_secs = 15
log_level = "info"
data_dir = "/var/lib/reposync"`}
      </pre>

      <h4 className="font-semibold text-gray-800 mt-4">[svn]</h4>
      <p className="text-gray-700">
        SVN repository URL, credentials, and layout configuration.
      </p>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`[svn]
url = "https://svn.company.com/repos/project"
username = "sync-service"
password_env = "REPOSYNC_SVN_PASSWORD"
layout = "standard"   # or "custom"`}
      </pre>

      <h4 className="font-semibold text-gray-800 mt-4">[github]</h4>
      <p className="text-gray-700">
        GitHub API URL, repository, token, and webhook configuration.
      </p>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`[github]
api_url = "https://api.github.com"
repo = "org/project"
token_env = "REPOSYNC_GITHUB_TOKEN"
webhook_secret_env = "REPOSYNC_WEBHOOK_SECRET"
default_branch = "main"`}
      </pre>

      <h4 className="font-semibold text-gray-800 mt-4">[sync]</h4>
      <p className="text-gray-700">
        Merge behavior and what is synchronized.
      </p>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`[sync]
mode = "direct"       # reserved: sync always pushes directly today
auto_merge = true
sync_branches = true
sync_tags = true`}
      </pre>

      <h4 className="font-semibold text-gray-800 mt-4">[notifications]</h4>
      <p className="text-gray-700">Slack and email notification settings for conflict alerts.</p>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`[notifications]
slack_webhook_url_env = "REPOSYNC_SLACK_WEBHOOK"
email_smtp = "smtp.company.com:587"
email_from = "reposync@company.com"
email_recipients = ["team@company.com"]`}
      </pre>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Environment Variables</h3>
      <p>
        Secrets are stored in environment variables, not in the config file.
        Create an env file at{' '}
        <code className="bg-gray-100 px-1 rounded">/etc/reposync/env</code>:
      </p>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`REPOSYNC_SVN_PASSWORD=your-svn-password
REPOSYNC_GITHUB_TOKEN=your-github-token
REPOSYNC_ADMIN_PASSWORD=your-dashboard-password
REPOSYNC_SESSION_SECRET=<random-hex-string>
REPOSYNC_WEBHOOK_SECRET=your-webhook-secret`}
      </pre>
    </div>
  );
}

function SectionPersonalMode() {
  return (
    <div className="space-y-4">
      <p>
        Personal Branch Mode lets individual developers work in Git while their
        team stays on SVN. It runs on your laptop and uses a PR-based workflow
        so every sync is reviewable.
      </p>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Quick Start</h3>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`reposync personal init           # Interactive setup wizard
reposync personal import --full  # Import SVN history to GitHub
reposync personal start          # Start sync daemon`}
      </pre>
      <Screenshot
        alt="Personal Mode Init Wizard"
        caption="The interactive init wizard configuring SVN and GitHub connections"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">How It Works</h3>
      <ol className="list-decimal list-inside space-y-2 text-gray-700">
        <li>
          <strong>Init</strong> creates a config file at{' '}
          <code className="bg-gray-100 px-1 rounded">~/.config/reposync/personal.toml</code>{' '}
          with your SVN and GitHub details.
        </li>
        <li>
          <strong>Import</strong> replays the full SVN history as Git commits
          with proper author mapping, then pushes to your GitHub repo.
        </li>
        <li>
          <strong>Start</strong> launches a background daemon that polls for new
          SVN commits and creates PRs in your GitHub repo for review.
        </li>
      </ol>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">PR-Based Workflow</h3>
      <p>
        Unlike Team Mode's direct sync, Personal Mode creates GitHub Pull
        Requests for each batch of SVN changes. This lets you review what's
        coming from SVN before merging into your Git branch.
      </p>
      <Screenshot
        alt="Personal Mode PR Workflow"
        caption="SVN changes appear as GitHub PRs for review before merging"
      />

      <h3 className="rs-heading text-lg font-semibold text-gray-900">CLI Commands</h3>
      <div className="overflow-x-auto">
        <table className="rs-tbl min-w-full divide-y divide-gray-200">
          <thead className="bg-gray-50">
            <tr>
              <th className="px-4 py-3 text-left text-xs font-medium text-gray-500 uppercase">Command</th>
              <th className="px-4 py-3 text-left text-xs font-medium text-gray-500 uppercase">Description</th>
            </tr>
          </thead>
          <tbody className="rs-surface-white divide-y divide-gray-200 text-sm">
            <tr><td className="px-4 py-2 font-mono">reposync personal init</td><td className="px-4 py-2">Interactive setup wizard</td></tr>
            <tr><td className="px-4 py-2 font-mono">reposync personal import --full</td><td className="px-4 py-2">Full SVN history import</td></tr>
            <tr><td className="px-4 py-2 font-mono">reposync personal start</td><td className="px-4 py-2">Start background sync daemon</td></tr>
            <tr><td className="px-4 py-2 font-mono">reposync personal stop</td><td className="px-4 py-2">Stop the daemon</td></tr>
            <tr><td className="px-4 py-2 font-mono">reposync personal status</td><td className="px-4 py-2">Show sync status and watermarks</td></tr>
            <tr><td className="px-4 py-2 font-mono">reposync personal log</td><td className="px-4 py-2">Show recent sync log entries</td></tr>
            <tr><td className="px-4 py-2 font-mono">reposync personal doctor</td><td className="px-4 py-2">Diagnose configuration issues</td></tr>
          </tbody>
        </table>
      </div>
    </div>
  );
}

function SectionDeployment() {
  return (
    <div className="space-y-4">
      <p>
        RepoSync can be deployed as a systemd service, a Docker container, or
        run directly. All methods use the same configuration file.
      </p>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">systemd</h3>
      <p>
        The recommended method for Linux servers. The service file runs as a
        dedicated <code className="bg-gray-100 px-1 rounded">reposync</code> user with
        security hardening (no new privileges, protected system paths).
      </p>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`sudo cp scripts/reposync.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now reposync

# Check status
sudo systemctl status reposync
sudo journalctl -u reposync -f`}
      </pre>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Docker</h3>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`docker run -d \\
  --name reposync \\
  -p 8080:8080 \\
  -v /etc/reposync:/etc/reposync:ro \\
  -v /var/lib/reposync:/var/lib/reposync \\
  --env-file /etc/reposync/env \\
  ghcr.io/chriscase/reposync:latest`}
      </pre>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Docker Compose</h3>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`services:
  reposync:
    image: ghcr.io/chriscase/reposync:latest
    ports:
      - "8080:8080"
    volumes:
      - /etc/reposync:/etc/reposync:ro
      - reposync-data:/var/lib/reposync
    env_file:
      - /etc/reposync/env
    restart: unless-stopped

volumes:
  reposync-data:`}
      </pre>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Health Check</h3>
      <p>
        The daemon exposes a health endpoint at{' '}
        <code className="bg-gray-100 px-1 rounded">/api/status/health</code> for
        load balancer and monitoring integration.
      </p>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">File Paths</h3>
      <div className="overflow-x-auto">
        <table className="rs-tbl min-w-full divide-y divide-gray-200">
          <thead className="bg-gray-50">
            <tr>
              <th className="px-4 py-3 text-left text-xs font-medium text-gray-500 uppercase">Path</th>
              <th className="px-4 py-3 text-left text-xs font-medium text-gray-500 uppercase">Purpose</th>
            </tr>
          </thead>
          <tbody className="rs-surface-white divide-y divide-gray-200 text-sm">
            <tr><td className="px-4 py-2 font-mono">/etc/reposync/config.toml</td><td className="px-4 py-2">Main configuration</td></tr>
            <tr><td className="px-4 py-2 font-mono">/etc/reposync/authors.toml</td><td className="px-4 py-2">Author identity mappings</td></tr>
            <tr><td className="px-4 py-2 font-mono">/etc/reposync/env</td><td className="px-4 py-2">Environment secrets</td></tr>
            <tr><td className="px-4 py-2 font-mono">/var/lib/reposync/</td><td className="px-4 py-2">SQLite database and sync state</td></tr>
          </tbody>
        </table>
      </div>
    </div>
  );
}

function SectionTroubleshooting() {
  return (
    <div className="space-y-4">
      <h3 className="rs-heading text-lg font-semibold text-gray-900">Common Issues</h3>

      <div className="space-y-4">
        <div className="border border-gray-200 rounded-lg p-4">
          <h4 className="font-semibold text-gray-900">Sync cycle shows "no changes" but SVN has new commits</h4>
          <p className="mt-1 text-sm text-gray-700">
            Check the SVN watermark in the audit log. If the watermark is ahead
            of the actual SVN HEAD, the import may have set an incorrect value.
            Use the repository detail page to inspect and reset watermarks.
          </p>
        </div>

        <div className="border border-gray-200 rounded-lg p-4">
          <h4 className="font-semibold text-gray-900">Authentication failures with GitHub Enterprise</h4>
          <p className="mt-1 text-sm text-gray-700">
            Ensure your <code className="bg-gray-100 px-1 rounded">api_url</code> is set to{' '}
            <code className="bg-gray-100 px-1 rounded">https://github.company.com/api/v3</code>{' '}
            (not the web URL). The token needs <code className="bg-gray-100 px-1 rounded">repo</code>{' '}
            scope at minimum.
          </p>
        </div>

        <div className="border border-gray-200 rounded-lg p-4">
          <h4 className="font-semibold text-gray-900">SVN hook not triggering real-time sync</h4>
          <p className="mt-1 text-sm text-gray-700">
            Webhook-triggered sync requires a post-commit hook on the SVN server
            that sends a POST to{' '}
            <code className="bg-gray-100 px-1 rounded">/webhook/svn</code> with the webhook
            secret. Without this, sync relies on polling (default: every 15
            seconds).
          </p>
        </div>

        <div className="border border-gray-200 rounded-lg p-4">
          <h4 className="font-semibold text-gray-900">Duplicate commits after restart</h4>
          <p className="mt-1 text-sm text-gray-700">
            RepoSync uses echo detection (<code className="bg-gray-100 px-1 rounded">[reposync]</code>{' '}
            markers in commit messages) to prevent re-syncing its own commits.
            If you see duplicates, check that the watermarks in the database are
            correct and haven't been reset.
          </p>
        </div>

        <div className="border border-gray-200 rounded-lg p-4">
          <h4 className="font-semibold text-gray-900">Large files blocked by policy</h4>
          <p className="mt-1 text-sm text-gray-700">
            RepoSync enforces file size limits and can route large files to Git
            LFS. Check the <code className="bg-gray-100 px-1 rounded">[options]</code> section
            of your config for <code className="bg-gray-100 px-1 rounded">max_file_size</code>{' '}
            and <code className="bg-gray-100 px-1 rounded">lfs_threshold</code> settings.
          </p>
        </div>
      </div>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Diagnostic Commands</h3>
      <pre className="bg-gray-900 text-gray-100 rounded-lg p-4 overflow-x-auto text-sm">
{`# Check daemon status and logs
sudo systemctl status reposync
sudo journalctl -u reposync --since "1 hour ago"

# Personal mode diagnostics
reposync personal doctor
reposync personal status

# Database inspection
sqlite3 /var/lib/reposync/reposync.db "SELECT * FROM watermarks;"
sqlite3 /var/lib/reposync/reposync.db "SELECT * FROM kv_state;"`}
      </pre>

      <h3 className="rs-heading text-lg font-semibold text-gray-900">Getting Help</h3>
      <p>
        If you're stuck, file an issue at{' '}
        <a
          href="https://github.com/chriscase/RepoSync/issues"
          target="_blank"
          rel="noopener noreferrer"
          className="text-blue-600 hover:underline"
        >
          github.com/chriscase/RepoSync/issues
        </a>.
      </p>
    </div>
  );
}

const sectionComponents: Record<Section, () => JSX.Element> = {
  'overview': SectionOverview,
  'getting-started': SectionGettingStarted,
  'dashboard': SectionDashboard,
  'repositories': SectionRepositories,
  'identity-mapping': SectionIdentityMapping,
  'conflict-resolution': SectionConflictResolution,
  'configuration': SectionConfiguration,
  'personal-mode': SectionPersonalMode,
  'deployment': SectionDeployment,
  'troubleshooting': SectionTroubleshooting,
};

export default function Documentation() {
  const [activeSection, setActiveSection] = useState<Section>('overview');
  const ActiveContent = sectionComponents[activeSection];

  return (
    <div className="rs-docs flex gap-6">
      {/* Sidebar navigation */}
      <nav className="rs-docs-nav w-56 flex-shrink-0">
        <div className="sticky top-6">
          <h2 className="rs-heading text-xs font-semibold text-gray-500 uppercase tracking-wider mb-3">
            Documentation
          </h2>
          <ul className="space-y-1">
            {sections.map((section) => (
              <li key={section.id}>
                <button
                  id={`nav-${section.id}`}
                  onClick={() => setActiveSection(section.id)}
                  className={`w-full text-left px-3 py-2 rounded-md text-sm transition-colors ${
                    activeSection === section.id
                      ? 'bg-blue-50 text-blue-700 font-medium'
                      : 'text-gray-600 hover:bg-gray-50 hover:text-gray-900'
                  }`}
                >
                  {section.title}
                </button>
              </li>
            ))}
          </ul>
        </div>
      </nav>

      {/* Main content */}
      <div className="flex-1 min-w-0">
        <div className="rs-surface-white shadow rounded-lg p-8">
          <h1 className="rs-title text-2xl font-bold text-gray-900 mb-6">
            {sections.find((s) => s.id === activeSection)?.title}
          </h1>
          <div className="prose prose-gray max-w-none text-gray-700">
            <ActiveContent />
          </div>
        </div>
      </div>
    </div>
  );
}
