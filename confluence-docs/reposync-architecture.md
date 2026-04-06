# RepoSync Architecture & Design

## Document Information

| Field | Value |
|-------|-------|
| **Status** | Draft |
| **Author** | [Your Name] |
| **Date** | April 2026 |
| **Audience** | Engineering team members who need to understand how RepoSync works |

---

## System Overview

RepoSync is a Rust-based server daemon that provides continuous, bidirectional synchronization between an SVN repository and a GitHub Enterprise repository. It runs as a background service on a dedicated VM or container.

```
                    ┌─────────────────────────────────────────────┐
                    │           RepoSync Daemon (Rust)            │
                    │                                             │
                    │   SVN Watcher ──▶ Sync Engine ◀── Git      │
                    │   (polling +      (state        Watcher    │
                    │    webhooks)       machine)     (webhooks   │
                    │                      │          + polling)  │
                    │   Identity    Conflict          Web UI      │
                    │   Mapper      Resolution        (React      │
                    │   (LDAP/file) Pipeline          dashboard)  │
                    │                                             │
                    │   ┌─────────────────────────────────────┐   │
                    │   │ SQLite: commit map, conflicts,      │   │
                    │   │         watermarks, audit log        │   │
                    │   └─────────────────────────────────────┘   │
                    └──────────┬──────────────────┬───────────────┘
                               │                  │
                          SVN Server       GitHub Enterprise
```

**Key design principles:**

- **No data loss**: Every state transition is logged to SQLite with WAL (Write-Ahead Logging). On crash or restart, the daemon reads the last state and resumes.
- **Echo suppression**: When the daemon pushes a synced commit, it records the mapping so it can recognize and skip its own work when the resulting webhook arrives (preventing infinite loops).
- **Identity preservation**: SVN usernames are mapped to Git name+email and back, preserving authorship across both systems.

---

## Component Architecture

RepoSync is organized as a Rust workspace with five crates:

| Crate | Type | Purpose |
|-------|------|---------|
| `reposync-core` | Library | Shared sync logic, SVN/Git clients, identity mapping, conflict handling, database |
| `reposync-daemon` | Binary | Server daemon entry point, scheduler, signal handling, startup |
| `reposync-web` | Library | Axum HTTP server, REST API endpoints, WebSocket, static file serving |
| `reposync-cli` | Binary | Management CLI (`reposync status`, `reposync conflicts`, etc.) |
| `reposync-personal` | Binary | Personal Branch Mode engine (not used in Team Mode) |

Additionally:

| Component | Technology | Purpose |
|-----------|-----------|---------|
| `web-ui/` | React 18 + TypeScript + Vite | Web dashboard frontend |
| SQLite | Bundled (rusqlite) | All persistent state |
| Tokio | Async runtime | Concurrent polling and request handling |
| Axum | Web framework | REST API + WebSocket + static files |

---

## Sync Engine State Machine

The sync engine is the heart of RepoSync. It operates as a state machine with crash recovery:

```
IDLE ──(poll timer / webhook)──▶ DETECTING
  ▲                                  │
  │                         ┌────────┴────────┐
  │                         ▼                  ▼
  │                   NO CONFLICT        CONFLICT FOUND
  │                         │                  │
  │                         ▼                  ▼
  │                     APPLYING       QUEUED FOR RESOLUTION
  │                         │                  │
  │                         ▼                  ▼ (user resolves)
  └──────────────────── COMMITTED      RESOLUTION APPLIED ──▶ COMMITTED
```

**How a sync cycle works:**

1. **Trigger**: The scheduler fires every `poll_interval_secs` (default: 15 seconds), or a webhook arrives from GitHub/SVN.
2. **Lock**: The engine acquires an atomic lock to prevent concurrent sync cycles.
3. **Detect**: Fetch new SVN revisions since the last watermark; fetch new Git commits since the last watermark.
4. **Compare**: The conflict detector checks if both sides changed the same files.
5. **Apply or Queue**:
   - If no overlapping files changed: apply both sides automatically.
   - If overlapping files changed: attempt a 3-way merge (base = last synced version).
     - Merge succeeds → apply automatically.
     - Merge fails → queue the conflict for manual resolution and send notifications (Slack/email).
6. **Record**: Update watermarks, commit map, and audit log in SQLite.
7. **Release**: Release the sync lock.

---

## Data Flow: SVN to Git

```
SVN commit by developer
        │
        ▼
RepoSync polls SVN (or receives SVN post-commit webhook)
        │
        ▼
Detects new revision(s) since last SVN watermark
        │
        ▼
For each new SVN revision:
  1. Read SVN diff (changed files + content)
  2. Map SVN username → Git author (via authors.toml or LDAP)
  3. Apply changes to local Git working copy
  4. Create Git commit:
     - Author: original developer (mapped identity)
     - Committer: RepoSync daemon (for audit trail)
     - Message: original SVN commit message
  5. Push to GitHub Enterprise
  6. Record SVN rev ↔ Git SHA in commit_map table
  7. Update SVN watermark
```

## Data Flow: Git to SVN

```
Git push / PR merge on GitHub Enterprise
        │
        ▼
RepoSync receives GitHub webhook (or polls GitHub API)
        │
        ▼
Checks commit_map — is this an echo of our own sync?
        │
  ┌─────┴─────┐
  YES          NO
  │            │
  ▼            ▼
 SKIP      For each new Git commit:
             1. Read Git diff (changed files + content)
             2. Map Git author → SVN username
             3. Check out SVN working copy, apply changes
             4. Commit to SVN:
                - svn:author set to mapped SVN username
                - Message: original Git commit message
             5. Record Git SHA ↔ SVN rev in commit_map table
             6. Update Git watermark
```

---

## Echo Suppression

Echo suppression prevents infinite sync loops. Without it:

1. Developer commits to SVN
2. RepoSync syncs it to Git (creates a Git commit)
3. GitHub sends a webhook for the new Git commit
4. RepoSync sees the webhook and tries to sync it back to SVN
5. This creates a duplicate SVN commit, triggering another webhook... (infinite loop)

**How it works**: Every synced commit is recorded in the `commit_map` table with both the SVN revision number and the Git SHA. When a webhook arrives, the daemon checks if the commit SHA already exists in the map. If it does, it's an echo and is skipped.

---

## Conflict Detection and Resolution

```
Changes detected on both SVN and Git
        │
        ▼
Do they modify the same files?
        │
   NO ──┤── YES
   │         │
   ▼         ▼
Auto-apply  Attempt 3-way merge
both sides  (base = last synced version)
                │
         ┌──────┴──────┐
      SUCCESS        FAILURE
         │              │
         ▼              ▼
    Auto-apply    Queue for manual resolution
                        │
                  ┌─────┼─────┐
                  ▼     ▼     ▼
                Slack  Email  Web UI
                        │
                  User resolves:
                  • Accept SVN version
                  • Accept Git version
                  • Rebase
```

**Resolution options:**

| Strategy | Description |
|----------|-------------|
| **Accept SVN** | Discard Git changes, keep SVN version |
| **Accept Git** | Discard SVN changes, keep Git version |
| **Rebase** | Re-apply one side's changes on top of the other |

Conflicts can be resolved via the web dashboard or the CLI (`reposync conflicts resolve <id> --accept git`).

---

## Identity Mapping

SVN and Git represent author identity differently:

- **SVN**: Simple username string (e.g., `jsmith`)
- **Git**: Name + email (e.g., `John Smith <jsmith@company.com>`)
- **Git** also has separate Author and Committer fields

RepoSync maps identities using these sources (in priority order):

1. **authors.toml** — explicit manual mapping file
2. **LDAP/Active Directory** — automatic lookup (optional, configured in `config.toml`)
3. **Email domain fallback** — `username@{email_domain}` (e.g., `jsmith@company.com`)

**Author vs. Committer distinction:**

| Direction | Git Author Field | Git Committer Field |
|-----------|-----------------|-------------------|
| SVN → Git | Original developer (mapped from SVN username) | RepoSync daemon service account |
| Git → SVN | `svn:author` property set to mapped SVN username | N/A (SVN has no committer concept) |

This preserves the audit trail: you can always see who originally wrote the code and that RepoSync performed the sync.

---

## Database Schema

RepoSync uses SQLite with WAL mode for all persistent state. Key tables:

| Table | Purpose |
|-------|---------|
| `commit_map` | Bidirectional SVN revision ↔ Git SHA mapping. Used for echo suppression and history tracking. |
| `watermarks` | Last synced position for each direction (SVN revision number, Git commit SHA). Determines where to start scanning for new changes. |
| `conflicts` | Queue of unresolved conflicts with file paths, diff content, and resolution status (`detected`, `resolved`, `deferred`). |
| `audit_log` | Complete timestamped history of all sync operations (direction, status, author, repository, error details). |
| `repositories` | Per-repository configuration including credentials, watermarks, sync status, and statistics. |
| `users` | Web dashboard user accounts with bcrypt-hashed passwords and roles. |
| `sessions` | Active web dashboard sessions with HMAC-SHA256 signed tokens and expiration. |

**Crash recovery**: Because every state transition is written to SQLite with WAL journaling, the daemon can resume from its last known state after an unexpected restart. No commits are lost.

---

## Web Server and API

The daemon embeds an Axum HTTP server that serves both the REST API and the React frontend.

### REST API Endpoints

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/api/status/health` | GET | Health check (returns `{"ok": true}`) |
| `/api/status` | GET | Current sync engine status and statistics |
| `/api/repos` | GET/POST | List and create repositories |
| `/api/repos/:id` | GET/PUT/DELETE | Repository detail, update, and deletion |
| `/api/repos/:id/import` | POST | Trigger SVN history import for a repository |
| `/api/conflicts` | GET | List active conflicts |
| `/api/conflicts/:id` | GET/POST | View conflict detail and submit resolution |
| `/api/audit` | GET | Sync history with filtering by repo, action, date |
| `/api/auth/login` | POST | Authenticate and create session |
| `/api/auth/logout` | POST | End session |
| `/api/users` | GET/POST | User management (admin only) |
| `/api/config` | GET/PUT | View and update daemon configuration |
| `/api/setup` | GET/POST | Initial setup wizard |
| `/api/webhooks/github` | POST | GitHub webhook receiver |
| `/api/webhooks/svn` | POST | SVN post-commit webhook receiver |

### WebSocket

The server provides a WebSocket endpoint for real-time updates. Connected browsers receive push notifications for:

- Sync cycle completions
- New conflicts detected
- Conflict resolutions
- Import progress updates

### Authentication

The web dashboard supports multiple authentication modes:

| Mode | Description |
|------|-------------|
| **Simple** | Single admin password (set via `REPOSYNC_ADMIN_PASSWORD` environment variable) |
| **GitHub OAuth** | Authenticate via GitHub Enterprise OAuth app; optionally restrict to a specific org |
| **Both** | Either simple password or GitHub OAuth |

Sessions are managed with HMAC-SHA256 signed cookies with configurable expiration.

---

## Security Model

| Layer | Mechanism |
|-------|-----------|
| **Secrets** | All credentials passed via environment variables, never in config files |
| **Credential storage** | Per-repo credentials encrypted at rest with AES-GCM |
| **Webhook verification** | HMAC-SHA256 signature validation on incoming GitHub and SVN webhooks |
| **Password hashing** | bcrypt for web dashboard passwords |
| **Session tokens** | HMAC-SHA256 signed with configurable secret |
| **Rate limiting** | Login endpoint rate limiting to prevent brute force |
| **Process isolation** | Runs as unprivileged `reposync` system user |
| **Systemd hardening** | `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome=true`, `PrivateTmp=true` |
| **Token redaction** | Tokens and passwords redacted in all log output |

---

## Technology Stack Summary

| Component | Technology | Version |
|-----------|-----------|---------|
| Language | Rust | 2021 edition (1.75+) |
| Async runtime | Tokio | 1.x (multi-threaded) |
| Web framework | Axum + Tower | 0.7 |
| Frontend | React + TypeScript + Vite | 18.3 |
| CSS | TailwindCSS | 3.4 |
| Database | SQLite (bundled) | WAL mode |
| Git library | git2-rs | 0.19 |
| SVN integration | CLI subprocess (`svn` command) | — |
| HTTP client | Reqwest | 0.12 |
| Serialization | Serde + TOML + JSON | — |
| Authentication | bcrypt, AES-GCM, HMAC-SHA256 | — |
| LDAP | ldap3 | — |
| Email | Lettre | 0.11 |

---

## Deployment Architecture

```
┌──────────────────────────┐
│   Developer Workstation   │
│                          │
│   svn commit / git push  │
└──────┬───────────┬───────┘
       │           │
       ▼           ▼
┌──────────┐  ┌────────────────┐
│ SVN      │  │ GitHub         │
│ Server   │  │ Enterprise     │
│          │  │                │
│ webhook ─┼──┼─▶ webhook      │
└──────┬───┘  └───────┬────────┘
       │              │
       ▼              ▼
┌──────────────────────────────┐
│  RepoSync Server (VM/Docker) │
│                              │
│  Port 8080: Web Dashboard    │
│  SQLite: /var/lib/reposync/  │
│  Config: /etc/reposync/      │
│  Logs: systemd journal       │
└──────────────────────────────┘
```

**Network requirements:**

- RepoSync → SVN server: HTTPS (port 443) or SVN protocol (port 3690)
- RepoSync → GitHub Enterprise: HTTPS (port 443) for API and Git operations
- GitHub Enterprise → RepoSync: HTTPS webhook delivery (port 8080, ideally behind a reverse proxy with TLS)
- SVN server → RepoSync: HTTP webhook delivery (port 8080) — if SVN post-commit hooks are configured
- Browser → RepoSync: HTTPS (port 443 via reverse proxy) for web dashboard access
