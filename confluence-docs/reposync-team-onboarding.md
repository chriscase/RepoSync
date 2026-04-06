# RepoSync Team Onboarding Guide

## Document Information

| Field | Value |
|-------|-------|
| **Status** | Draft |
| **Author** | [Your Name] |
| **Date** | April 2026 |
| **Audience** | All team members using repositories synced by RepoSync |

---

## What is RepoSync?

RepoSync is a tool that keeps our SVN and GitHub Enterprise repositories in sync, automatically and continuously. When someone commits to SVN, that change appears in GitHub within seconds. When someone pushes to GitHub (or merges a pull request), that change appears in SVN within seconds.

**You don't need to install anything.** RepoSync runs on a server managed by [Sync Admin Name]. You just keep working the way you already do — commit to SVN or push to Git, and RepoSync handles the rest.

---

## What Changes for You?

### If You Primarily Use SVN

| Before | After |
|--------|-------|
| You commit to SVN | You commit to SVN — **no change** |
| Your commits are only in SVN | Your commits automatically appear in GitHub too |
| You can't see Git activity | You can browse Git history on GitHub Enterprise |
| Code reviews require emailing diffs or using a separate tool | Your SVN commits are visible as GitHub commits; teammates can comment on them |

**Bottom line: Nothing changes about your daily workflow.** You keep using `svn commit` as always. RepoSync mirrors your work to GitHub in the background.

### If You Primarily Use Git

| Before | After |
|--------|-------|
| You push to GitHub | You push to GitHub — **no change** |
| Your commits are only in GitHub | Your commits automatically appear in SVN too |
| You can't see SVN activity | SVN commits show up as GitHub commits in the same repo |
| PRs only affect the Git side | Merged PRs are automatically committed to SVN |

**Bottom line: Nothing changes about your daily workflow.** You keep using `git push` and pull requests as always. RepoSync mirrors your work to SVN in the background.

---

## How Sync Works (Simplified)

```
You commit to SVN                     You push to Git
       │                                      │
       ▼                                      ▼
  RepoSync detects                    RepoSync detects
  the new SVN commit                  the new Git commit
       │                                      │
       ▼                                      ▼
  Creates a matching                  Creates a matching
  Git commit on GitHub                SVN commit on the server
       │                                      │
       ▼                                      ▼
  Appears in GitHub                   Appears in SVN
  within ~15 seconds                  within ~15 seconds
```

**Key points:**

- Sync happens automatically every 15 seconds (or instantly via webhooks)
- Your name and email are preserved — the commit shows up as *your* commit, not as a bot
- The original commit message is preserved
- RepoSync is smart enough to not create duplicate commits (echo suppression)

---

## The Web Dashboard

RepoSync has a web dashboard where you can check sync status and resolve any issues.

**URL**: `https://[reposync-server-url]:8080` (ask your sync admin for the exact URL)

**Login**: Use the credentials provided by your sync admin.

### Dashboard Overview

When you log in, you'll see:

- **Sync Status**: Whether sync is running, idle, or has an issue
- **Last Sync Time**: When the last successful sync happened
- **Recent Activity**: The latest commits that were synced in both directions
- **Conflict Count**: How many conflicts (if any) need attention

### Pages You Might Use

| Page | What It Shows | When You'd Use It |
|------|--------------|-------------------|
| **Dashboard** | Overall sync status and recent activity | Quick check that everything is working |
| **Repositories** | List of synced repos with per-repo status | See status for a specific repository |
| **Conflicts** | List of conflicts waiting for resolution | When you're notified of a conflict |
| **Audit Log** | Full history of all sync operations | Investigating when a specific commit synced |

---

## Daily Workflows

### Workflow 1: SVN Developer — Commit as Usual

```bash
# Work on your files
svn update
# ... make changes ...
svn add newfile.txt        # if adding new files
svn commit -m "Fix login validation bug"
```

That's it. Within 15 seconds, your commit will appear in the GitHub Enterprise repository with your name as the author.

### Workflow 2: Git Developer — Push as Usual

```bash
# Work on your branch
git checkout -b fix-login-bug
# ... make changes ...
git add -A
git commit -m "Fix login validation bug"
git push origin fix-login-bug

# Create a pull request on GitHub Enterprise
# When the PR is merged to main, it syncs to SVN
```

That's it. Once your changes are on the default branch (e.g., `main`), they sync to SVN automatically.

### Workflow 3: Checking Sync Status

**Via the web dashboard:**
1. Go to the dashboard URL
2. Look at the "Last Sync" time and status indicators

**Via the CLI** (if you have access to the RepoSync server):
```bash
reposync status
```

### Workflow 4: Viewing Sync History

**Via the web dashboard:**
1. Go to **Audit Log**
2. Filter by repository, date, or action type
3. See which commits synced and in which direction

**Via the CLI:**
```bash
reposync audit --limit 20
```

---

## Conflict Resolution

### What Is a Conflict?

A conflict happens when **both SVN and Git change the same file** at roughly the same time, and the changes overlap in a way that can't be merged automatically.

This is uncommon in a small team, but it can happen. RepoSync will:

1. Detect the conflict
2. Pause syncing for the affected file(s)
3. Notify the team via Slack/email (if configured)
4. Queue the conflict for manual resolution

### How to Resolve a Conflict

**Via the web dashboard** (recommended):

1. Go to **Conflicts** in the navigation
2. Click on the conflict to see details
3. You'll see a side-by-side diff showing the SVN version and the Git version
4. Choose a resolution:
   - **Accept SVN**: Keep the SVN version, discard the Git changes
   - **Accept Git**: Keep the Git version, discard the SVN changes
   - **Rebase**: Re-apply one side's changes on top of the other
5. Click **Resolve**

**Via the CLI:**

```bash
# List conflicts
reposync conflicts list

# View details
reposync conflicts show <conflict-id>

# Resolve
reposync conflicts resolve <conflict-id> --accept git
# or
reposync conflicts resolve <conflict-id> --accept svn
```

### Preventing Conflicts

- **Communicate with your team** about which files you're working on
- **Commit/push frequently** — smaller, more frequent changes are less likely to conflict
- **Pull/update before starting work** — make sure you have the latest version

---

## Identity Mapping

RepoSync maps your SVN username to your Git name and email so that commits are attributed correctly on both sides.

**Example:**

- Your SVN username is `jsmith`
- In the mapping file, this is configured as: `John Smith <jsmith@yourcompany.com>`
- When you commit to SVN as `jsmith`, it appears in GitHub as authored by `John Smith <jsmith@yourcompany.com>`
- When you push to GitHub as `John Smith`, it appears in SVN as committed by `jsmith`

If your commits are showing up with the wrong name or as an unknown user, let the sync admin know so they can update the mapping file.

---

## FAQ

### General

**Q: Do I need to install RepoSync on my computer?**
A: No. RepoSync runs on a server. You don't need to install or configure anything on your machine.

**Q: Do I need to change how I commit to SVN or push to Git?**
A: No. You keep working exactly the way you do now. RepoSync operates transparently in the background.

**Q: How quickly do changes sync?**
A: Typically within 15 seconds. If webhooks are configured, it can be nearly instant.

**Q: Can I commit to both SVN and Git?**
A: Yes, but be careful not to change the same file at the same time in both systems. If you do, RepoSync will detect a conflict and ask for manual resolution.

### Commits and History

**Q: Will my commit messages be preserved?**
A: Yes, the original commit message is carried over exactly.

**Q: Will my name show up correctly on synced commits?**
A: Yes, as long as your identity is in the mapping file. If not, it will fall back to `your-svn-username@yourcompany.com`. Let the sync admin know if your name isn't showing up correctly.

**Q: What about branches?**
A: RepoSync can sync branches between SVN and Git. Whether this is enabled depends on the configuration. Ask the sync admin about the current setup.

### Conflicts

**Q: How often do conflicts happen?**
A: Rarely, especially with a small team. Conflicts only occur when two people change the same file at the same time on different sides (one in SVN, one in Git). If your team mostly works on one side, conflicts are very unlikely.

**Q: What happens if there's a conflict?**
A: Sync pauses for the affected file(s) until someone resolves the conflict. Other files and other repositories continue syncing normally. The team is notified via Slack/email.

**Q: Who should resolve conflicts?**
A: Ideally, one of the people who made the conflicting changes. The sync admin can also resolve conflicts if needed.

### Troubleshooting

**Q: My commit didn't show up on the other side. What do I do?**
A: Wait 30 seconds and check again. If it still hasn't appeared:
1. Check the web dashboard for errors
2. Check if there's an unresolved conflict blocking sync
3. Ask the sync admin to check the daemon logs

**Q: I see duplicate commits. Is that normal?**
A: No. RepoSync has echo suppression to prevent this. If you see duplicates, report it to the sync admin.

**Q: The dashboard shows an error. What should I do?**
A: Note the error message and notify the sync admin. Common issues include expired tokens or network connectivity problems. These don't affect your ability to commit — your commits will sync once the issue is resolved.

---

## Who to Contact

| Role | Person | Contact |
|------|--------|---------|
| **Sync Admin** | [Name] | [Email/Slack handle] |
| **Backup Admin** | [Name] | [Email/Slack handle] |

For urgent issues (sync completely stopped, data loss suspected), contact the sync admin directly.

For non-urgent questions or feature requests, post in [team Slack channel / email list].

---

## Glossary

| Term | Definition |
|------|-----------|
| **Sync cycle** | One round of checking both SVN and Git for new changes and applying them |
| **Watermark** | The last synced position (SVN revision number or Git commit SHA) — tells RepoSync where it left off |
| **Echo suppression** | The mechanism that prevents RepoSync from re-syncing its own commits (avoiding infinite loops) |
| **Conflict** | When both SVN and Git change the same file at the same time and the changes can't be merged automatically |
| **Identity mapping** | The translation between SVN usernames and Git author name+email pairs |
| **Poll interval** | How often RepoSync checks for new changes (default: every 15 seconds) |
| **Webhook** | A notification sent by GitHub or SVN to RepoSync when a commit happens (faster than polling) |
