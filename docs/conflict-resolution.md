# Conflict Resolution

RepoSync automatically handles most synchronization scenarios. Conflicts only arise when both SVN and Git have changes to the same file since the last sync.

## When Conflicts Occur

A conflict happens when:
- The **same lines** of a file are changed in both SVN and Git between sync cycles
- A file is **edited** on one side and **deleted** on the other
- A file is **renamed** differently on both sides
- A **binary file** is modified on both sides (binary files cannot be merged)

## What Doesn't Cause Conflicts

These situations are handled automatically:
- Different files changed on each side → both changes applied
- Same file but **different lines** changed → auto-merged via 3-way merge
- Changes on only one side → synced directly

## Conflict Types

| Type | Description | Auto-resolvable? |
|------|-------------|-----------------|
| Content | Same lines changed differently | No |
| Edit/Delete | One side edited, other deleted | No |
| Rename | Both sides renamed differently | No |
| Property | SVN properties vs .gitattributes | Sometimes |
| Branch | Branch created/deleted on both sides | Sometimes |
| Binary | Binary file modified on both sides | No |

## Resolution Methods

### Web Dashboard (Recommended)

1. Open the dashboard at `http://your-server:8080`
2. Click on **Conflicts** in the navigation
3. Select a conflict to see the 3-way diff:
   - **Left**: SVN version
   - **Center**: Base (common ancestor)
   - **Right**: Git version
   - **Bottom**: Merged result (editable)
4. Choose a resolution:
   - **Accept SVN** — use the SVN version, discard Git changes
   - **Accept Git** — use the Git version, discard SVN changes
   - **Manual Edit** — edit the merged result directly
   - **Defer** — skip for now, resolve later

### CLI

```bash
# List conflicts
reposync conflicts list

# View a specific conflict
reposync conflicts show abc-123-def

# Resolve by accepting one side
reposync conflicts resolve abc-123-def --accept svn
reposync conflicts resolve abc-123-def --accept git
```

### Notifications

When a conflict is detected, RepoSync sends alerts:
- **Slack**: Message to configured channel with file path, authors, and dashboard link
- **Email**: Notification to configured recipients

## How 3-Way Merge Works

RepoSync uses the last synced version as the **base** for merging:

```
Base (last synced version)
     ├── SVN changes (ours)
     └── Git changes (theirs)
```

If the changes don't overlap (different lines), they're merged automatically. If they do overlap, a conflict is created for manual resolution.

## What a Team Conflict Records

When a Team sync cycle holds a conflict, the conflict row records:

- the newest pending SVN revision and the newest pending Git commit that touch the file;
- the base text: the file at the SVN revision and Git commit both sides last agreed on, stored only when the SVN and Git copies there are identical;
- the SVN text at the newest pending SVN revision and the Git text at the pending Git tip. A side where the file no longer exists has no text.

Text is stored only when it is valid UTF-8, contains no NUL byte and is at most 1 MiB. Binary files, other encodings, larger files, Git LFS pointers and anything that could not be read leave that text empty. SVN text is read with keywords expanded, the same way RepoSync exports it to Git. The revision and commit are still recorded. Neither repository is changed while the conflict is held, and the recorded text is never used to merge automatically.

## Conflict Lifecycle

```
DETECTED → QUEUED → [User resolves] → RESOLVED → Applied to both repos
                  → DEFERRED (can be resolved later)
```

Deferred conflicts don't block other syncs. New changes to the same file will create a new conflict that supersedes the old one.

## Best Practices

1. **Resolve conflicts promptly** — deferred conflicts accumulate and become harder to resolve
2. **Communicate with your team** — if you see a conflict, talk to the other developer
3. **Use the web UI** — the 3-way diff view makes it much easier than CLI resolution
4. **Review the audit log** — understand the sequence of changes that led to the conflict
