# RepoSync Adoption: Decision & Planning Document

## Document Information

| Field | Value |
|-------|-------|
| **Status** | Draft |
| **Author** | [Your Name] |
| **Date** | April 2026 |
| **Stakeholders** | Engineering Team |

---

## Problem Statement

Our team currently uses SVN for version control, but we also have access to GitHub Enterprise (GHE). This creates several pain points:

- **Workflow friction**: Some team members prefer Git-based workflows (branching, pull requests, code review) while others work primarily in SVN.
- **No unified view**: Changes made in SVN are invisible in GHE, and vice versa. There is no single source of truth.
- **Manual syncing is error-prone**: Developers who try to bridge the gap manually risk losing commits, overwriting work, or introducing merge conflicts.
- **Modern tooling gap**: CI/CD pipelines, code review tools, and IDE integrations increasingly assume Git. Staying SVN-only limits our ability to adopt these tools.

We need a solution that lets both SVN and Git coexist seamlessly during our transition period, without requiring an abrupt cutover.

---

## Alternatives Evaluated

| Tool | Type | Cost | Bidirectional | Daemon Mode | Conflict Resolution | Verdict |
|------|------|------|---------------|-------------|---------------------|---------|
| **git-svn** | CLI bridge | Free | Partial (manual) | No | None | Single-developer only; corrupts merge history; no automation |
| **SubGit** | Server plugin | Commercial license | Yes | Yes | Limited | Requires disabling writes to one side; license cost |
| **svn2git** | Migration script | Free | No (one-way) | No | N/A | One-time migration only; no ongoing sync |
| **Manual workflow** | Process-based | Free | Manual | No | Manual | High risk of human error; doesn't scale |
| **RepoSync** | Sync daemon | Free (MIT) | Yes | Yes | Automatic + manual | Open-source; production-grade; web UI for monitoring |

### Why Not git-svn?

`git-svn` is designed for a single developer to interact with SVN from Git. It does not support team-wide synchronization, has no daemon mode, and is known to corrupt merge history over time. It also requires each developer to manage their own bridge, which doesn't scale for a team.

### Why Not SubGit?

SubGit is a commercial product that installs as an SVN server-side plugin. While it provides bidirectional sync, it requires a commercial license and typically requires disabling direct writes to one of the two repositories. This doesn't fit our need for both repositories to remain fully writable during the transition.

---

## Recommended Solution: RepoSync

**RepoSync** is a free, open-source (MIT-licensed) bidirectional SVN-Git synchronization bridge written in Rust. It is actively maintained and designed specifically for the enterprise coexistence problem we face.

### Key Reasons for Selection

1. **True bidirectional sync**: Commits made in SVN appear in Git, and commits/PRs merged in Git appear in SVN. Both repositories remain writable.
2. **Team Mode with web dashboard**: A centralized server daemon monitors both repositories and syncs automatically. The React-based web UI provides visibility into sync status, conflicts, and audit history.
3. **Automatic conflict resolution**: Non-overlapping changes are merged automatically via 3-way merge. Only true conflicts require human intervention.
4. **Identity mapping**: SVN usernames are transparently mapped to Git name+email pairs (and back), preserving authorship across both systems.
5. **Notifications**: Slack and email alerts when conflicts need attention.
6. **Crash recovery**: SQLite-based transaction log ensures no data loss on daemon restart.
7. **GitHub Enterprise support**: First-class support for GHE API URLs, webhooks, and OAuth.
8. **Open source**: MIT license, no vendor lock-in, full access to source code for customization if needed.

---

## Chosen Mode: Team Mode

RepoSync offers two modes. We are adopting **Team Mode**:

| Aspect | Team Mode | Personal Branch Mode |
|--------|-----------|---------------------|
| **Deployment** | Centralized server daemon on a VM | Each developer runs their own daemon |
| **Web Dashboard** | Yes (React UI on port 8080) | No |
| **Identity Mapping** | Whole-team mapping (file or LDAP) | Single user only |
| **Conflict Resolution** | Web UI + CLI + notifications | CLI only |
| **Setup Complexity** | Server provisioning + config | `reposync personal init` wizard |
| **Best For** | Teams needing unified sync | Individual developers |

**Rationale**: With a small team (2-5 people) of mixed SVN/Git skill levels, a centralized daemon eliminates the burden of each team member managing their own sync. The web dashboard provides visibility without requiring CLI comfort. Centralized conflict resolution and notifications ensure nothing falls through the cracks.

---

## Rollout Plan

### Phase 1: Pilot (Week 1-2)

- **Scope**: One non-critical repository
- **Goal**: Validate sync behavior, identity mapping, and conflict resolution
- **Tasks**:
  - Provision a VM or container for the RepoSync daemon
  - Configure `config.toml` with SVN and GHE connection details
  - Set up `authors.toml` with team member identity mappings
  - Configure GitHub Enterprise webhooks for the pilot repository
  - Run initial import of SVN history into GHE
  - Monitor sync cycles for 1-2 weeks; verify commits appear correctly on both sides
  - Test conflict detection and resolution via the web UI
- **Success Criteria**: Commits sync bidirectionally within 30 seconds; identity mapping is correct; conflicts are detected and resolvable

### Phase 2: Team Onboarding (Week 3)

- **Scope**: Same repository, all team members
- **Goal**: Ensure the team understands the new workflow and can resolve conflicts
- **Tasks**:
  - Distribute team onboarding guide (see companion document)
  - Walk team through the web dashboard
  - Have each team member make a test commit in their preferred VCS and verify it syncs
  - Set up Slack notifications for the team channel
  - Identify a sync admin (point of contact for conflict resolution and troubleshooting)
- **Success Criteria**: All team members can verify their commits sync; at least one conflict is resolved end-to-end

### Phase 3: Expand (Week 4+)

- **Scope**: Additional repositories as needed
- **Goal**: Roll out RepoSync to remaining repositories
- **Tasks**:
  - Add repositories to RepoSync via the web dashboard or config
  - Configure per-repository identity mappings if needed
  - Set up webhooks for each new repository
  - Monitor and tune poll intervals based on commit frequency
- **Success Criteria**: All target repositories syncing reliably

### Phase 4: Steady State

- **Scope**: Ongoing operations
- **Goal**: Maintain reliable sync with minimal manual intervention
- **Tasks**:
  - Regular backup of SQLite database
  - Monitor health endpoint and Slack alerts
  - Update RepoSync when new versions are released
  - Periodically review audit logs for anomalies

---

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| **Sync conflicts block development** | Medium | High | Auto-merge handles most cases; Slack alerts ensure prompt resolution; designate a sync admin |
| **Daemon downtime** | Low | Medium | Systemd auto-restart; health check monitoring; commits queue and sync on recovery |
| **Identity mapping gaps** | Medium | Low | Fallback to `username@company.com`; add mappings to `authors.toml` as discovered |
| **GitHub Enterprise API changes** | Low | Medium | RepoSync uses standard GHE v3 API; pin to known-good RepoSync version |
| **Team resistance to new workflow** | Medium | Medium | Emphasize "nothing changes for you" — commit to SVN or Git as before; sync is transparent |
| **Data loss during sync** | Very Low | High | SQLite WAL mode + crash recovery; backup database regularly; test with pilot repo first |

---

## Success Criteria

1. **Sync reliability**: 99%+ of commits sync successfully without manual intervention within 60 seconds
2. **Team adoption**: All team members can verify their commits appear on both sides
3. **Conflict resolution**: Conflicts are detected, notified, and resolved within 1 business day
4. **Operational stability**: Daemon uptime > 99.5% over a 30-day period
5. **Identity accuracy**: All commits attributed to the correct author on both SVN and Git

---

## Decision

**We will adopt RepoSync in Team Mode** to bridge our SVN and GitHub Enterprise repositories. The pilot will begin with [repository name] and expand based on results.

| Decision | Details |
|----------|---------|
| **Tool** | RepoSync (open-source, MIT license) |
| **Mode** | Team Mode (centralized daemon) |
| **Deployment** | [Docker / systemd on VM] — to be decided during pilot |
| **Sync Admin** | [Name] — responsible for conflict resolution and monitoring |
| **Pilot Repository** | [Repository name] |
| **Target Start Date** | [Date] |
