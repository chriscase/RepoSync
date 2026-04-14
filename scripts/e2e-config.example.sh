#!/usr/bin/env bash
# =============================================================================
# e2e-config.sh — Configuration for RepoSync E2E tests
# =============================================================================
#
# Copy this file to e2e-config.sh and fill in your values:
#   cp e2e-config.example.sh e2e-config.sh
#
# The test script (e2e-test.sh) sources this file automatically.
# Do NOT commit e2e-config.sh — it contains credentials.
# =============================================================================

# ---- SVN checkouts ----------------------------------------------------------
# Full paths to local SVN working copies for each branch level.
# Create these with:
#   svn checkout <SVN_URL>/branches/<branch_name> <path>

SVN_SANDBOX="/path/to/svn-checkout/sandbox"
SVN_DEV="/path/to/svn-checkout/developer"
SVN_FEATURE="/path/to/svn-checkout/feature"

# ---- Git clone --------------------------------------------------------------
# Path to a single git clone that has all 3 branches.
# Create with:
#   git clone <GIT_REPO_URL> <path>
#   cd <path> && git checkout <each branch>

GIT_REPO="/path/to/git-clone"

# ---- Git branch names -------------------------------------------------------
# Must match the branch pair names configured in the RepoSync dashboard.

GIT_BRANCH_SANDBOX="dev/sandbox"
GIT_BRANCH_DEV="dev/developer1"
GIT_BRANCH_FEATURE="dev/developer1-feature1"

# ---- GitHub / GitHub Enterprise API -----------------------------------------
# Used for creating and merging pull requests during the merge-chain tests.
# If not set, PR tests are skipped and only direct SVN/Git sync is tested.

GHE_API="https://github.example.com/api/v3"    # or https://api.github.com for github.com
GHE_REPO="org/repo-name"
GHE_TOKEN=""  # GitHub personal access token (e.g. ghp_xxx)

# ---- Test parameters --------------------------------------------------------

# Number of full test rounds to run. Each round exercises:
#   - SVN add/modify on all 3 branches
#   - Git add/modify/subdir on all 3 branches
#   - Bidirectional sync verification
#   - PR merge chain: Feature -> Developer -> Sandbox
#   - Deletion test (every other round)
ROUNDS=3

# Seconds to wait for RepoSync to complete a sync cycle.
# Should be at least 1.5x the daemon's poll_interval_secs.
# Default daemon interval is 60s, so 90s gives a safe margin.
PAUSE=90

# Subdirectory inside the repo where test files are created.
# Must be under an allowed_paths prefix (e.g. source/).
TEST_DIR="source/tests/e2e"

# ---- SSH access to RepoSync server (optional) -------------------------------
# If set, the test script can check daemon logs for errors after each round.
# Uses the same SSH alias as deploy.sh.

REPOSYNC_SSH_HOST=""         # e.g. "myserver" or "user@host"
REPOSYNC_SSH_OPTS=""         # e.g. "-o ControlMaster=no -o ControlPath=none"
