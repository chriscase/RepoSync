# RepoSync: Importing SVN History to GitHub Enterprise

This guide documents importing an SVN repository's branch history into a GitHub Enterprise Server repository using the RepoSync setup wizard.

## Scenario

- **SVN Source**: Internal SVN server with a multi-branch repository
- **Git Target**: GitHub Enterprise Server (private repo)
- **Goal**: Import all commits from a specific SVN branch into Git with original author attribution
- **Direction**: Read-only from SVN — no writes to the SVN repository

## Prerequisites

1. **RepoSync daemon** running on a Linux server that can reach both the SVN server and GitHub Enterprise
2. **SVN credentials** (username + password) with read access to the repository
3. **GitHub Enterprise Personal Access Token** with `repo` scope
   - Create at: `https://<your-ghe-host>/settings/tokens/new`
   - Select scope: `repo` (Full control of private repositories)
4. **An empty GitHub Enterprise repository** to receive the imported commits
   - Create at: `https://<your-ghe-host>/new`

## Example Configuration

This example imports the `sls_engr_trunk` branch from an internal SVN server into GitHub Enterprise.

### SVN Repository Details

| Field | Value |
|-------|-------|
| Repository URL | `http://orw-bsd-svn-01.wv.mentorg.com:8080/svn2/sdd/edmsls/branches/SLS/sls_engr_trunk` |
| Username | `chrisc` |
| Layout | Standard |

**Important**: Point the URL directly at the specific branch you want to import, not the repository root. This ensures only the history for that branch is imported.

### Git (GitHub Enterprise) Details

| Field | Value |
|-------|-------|
| Provider | GitHub / GitHub Enterprise |
| API URL | `https://github.siemens.cloud/api/v3` |
| Repository | `chris-case/EDM-Server-Load-Simulator` |
| Default Branch | `main` |

**API URL format**: For GitHub Enterprise Server, the API URL follows the pattern `https://<hostname>/api/v3`. This is different from public GitHub which uses `https://api.github.com`.

### Sync Settings

| Field | Value |
|-------|-------|
| Auto Merge | Yes |
| Sync Tags | Yes |
| LFS Threshold | 10 MB (optional) |

## Step-by-Step Setup

### 1. Open the Setup Wizard

Navigate to `http://<reposync-server>:8080/setup` in your browser.

### 2. SVN Repository (Step 2)

- Enter the full URL to the specific SVN branch you want to import
- Enter your SVN username
- Enter your SVN password in the password field
  - The password is stored securely in the server's database
  - It is **never** written to the TOML configuration file
- Leave layout as "Standard" — it doesn't matter when pointing directly at a branch

### 3. Git Provider (Step 3)

- Select "GitHub / GitHub Enterprise"
- Set the API URL to `https://<your-ghe-host>/api/v3`
- Enter the repository in `owner/repo` format
- Enter your Personal Access Token in the token field
  - Like the SVN password, this is stored securely in the database
- Default branch is typically `main`

### 4. Sync Settings (Step 4)

- Enable **Auto Merge** and **Sync Tags** as desired
- Set **LFS Threshold** if you have large binary files (e.g., 10 MB)
  - Requires `git-lfs` to be installed on the server: `sudo dnf install git-lfs`
  - Files exceeding the threshold will be tracked via Git LFS
- Set **Max File Size** to skip files above a certain size (0 = no limit)

### 5. Identity Mapping (Step 5)

- Set **Fallback Email Domain** (e.g., `mentorg.com`)
  - Unmapped SVN users get emails like `username@mentorg.com`
- Add explicit mappings for known SVN usernames:
  - SVN Username → Git Name + Email
  - Example: `chrisc` → `Chris Case` / `chrisc@mentorg.com`

### 6. Server & Auth (Step 6)

- Listen address: `0.0.0.0:8080`
- Auth mode: Simple (password)
- Poll interval: `30` seconds (for ongoing sync after import)
- Data directory: `/home/<user>/reposync-data`

### 7. Review (Step 7)

- Review all settings in the summary cards
- Click **"Save Configuration to Server"**
- The server validates and saves the configuration
- Warnings will appear if Git LFS is not installed (non-blocking)

### 8. Import (Step 8)

- Click **"Start Full Import"**
- Watch the progress:
  - **Progress bar** shows percentage complete
  - **Stats cards** show revisions processed, commits created, files imported, LFS files tracked
  - **Operation log** shows each revision being imported in real-time with color coding:
    - Green `[ok]`: Successfully imported
    - Blue `[info]`: Status messages
    - Yellow `[warn]`: Warnings (skipped files, etc.)
    - Red `[error]`: Errors
- The import can be cancelled at any time with the "Cancel Import" button
- On completion, click **"Go to Dashboard"** to see the imported data

## How It Works

The import process is **read-only on SVN**. For each SVN revision:

1. `svn info` — Reads repository metadata (HEAD revision)
2. `svn log` — Reads the full commit history
3. `svn export` — Exports the file tree at each revision (no `.svn` directories)
4. Files are copied into the local Git working tree with policy enforcement:
   - LFS tracking for files exceeding the threshold
   - Ignore patterns to skip unwanted files
   - Max file size to skip oversized files
5. A Git commit is created with:
   - **Author**: The original SVN committer (mapped via identity config)
   - **Committer**: "RepoSync" (the tool that performed the import)
   - **Message**: Original SVN commit message + metadata trailer
6. After all revisions are imported, all commits are pushed to the remote in one batch

## Handling Multiple SVN Branches

Currently, the import targets a single SVN path (URL). To import a specific branch:

- Point the SVN URL directly at the branch path:
  `http://svn-server/repos/project/branches/feature-branch`
- Each import creates commits on the configured Git branch (`main` by default)

To import multiple SVN branches into separate Git branches, run the wizard multiple times with different SVN URLs and Git branch settings.

## Troubleshooting

### "Failed to get SVN info"
- Check that the SVN URL is correct and reachable from the server
- Verify credentials — try `svn info --username <user> <url>` on the server

### "Failed to clone git repo"
- The daemon now handles this gracefully by creating an empty repo
- Ensure the GHE token has `repo` scope
- Check that the server can reach GitHub Enterprise: `curl https://<ghe-host>/api/v3`

### "Git LFS not available"
- Install git-lfs: `sudo dnf install git-lfs` (RHEL/Rocky) or `sudo apt install git-lfs`
- This is a warning, not an error — files will be committed directly without LFS

### Daemon won't start
- Check logs: `tail -50 /tmp/reposync.log`
- The daemon now handles missing git repos gracefully
- Ensure the data directory exists and is writable

## Security Notes

- SVN passwords and Git tokens entered in the wizard are stored in the SQLite database on the server
- They are **never** written to the TOML configuration file
- The TOML file only contains environment variable names as references
- For production use, consider:
  - Running the web UI over HTTPS (via Caddy or nginx reverse proxy)
  - Restricting network access to the daemon port
  - Using a dedicated service account for SVN access
