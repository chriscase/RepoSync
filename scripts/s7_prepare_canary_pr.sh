#!/usr/bin/env bash
# S5/S6 of the single-member PR canary. The harness supplies its API, logging
# and scenario helpers. S7 consumes only the identities established here.
prepare_canary_pr() {
    local branch_created=false
    local MAIN_REF_RESP MAIN_REF_CODE MAIN_SHA CREATE_REF_RESP CREATE_REF_CODE
    local COMMIT_RESP COMMIT_CODE PR_CREATE_RESP PR_CREATE_CODE MERGE_RESP MERGE_CODE

    # Reset evidence even when an operator reuses a cycle artifact directory.
    GIT_SHA=""
    PR_NUMBER=""
    PR_MERGED=false
    PR_MERGE_SHA=""
    # ---- Scenario 5: Git→ create branch + commit via GHE API ----
    # RepoSync only syncs *merged PR* commits from Git→SVN. Direct pushes to
    # main are NOT replayed. So we must: create branch → commit → open PR →
    # merge PR → sync → verify in SVN.
    log "▶ S5: Git create branch + commit file via PR"
    PR_BRANCH="validation/${CANARY}"
    GIT_FILE="git_${CANARY}.txt"
    GIT_CONTENT="git-pr-add-${CANARY}"
    GIT_CONTENT_B64=$(echo -n "$GIT_CONTENT" | base64)

    # A revision that existed before this PR cannot prove a new replay.
    SVN_AUTH=(--username "$SVN_USERNAME" --password "$SVN_PASSWORD" --non-interactive --no-auth-cache)
    S5_SVN_BASELINE=$(svn info "$SVN_URL" --show-item revision "${SVN_AUTH[@]}" 2>/dev/null) || S5_SVN_BASELINE=""
    printf '%s\n' "$S5_SVN_BASELINE" > "$CYCLE_DIR/s5-svn-baseline.txt"

    if [[ ! "$S5_SVN_BASELINE" =~ ^(0|[1-9][0-9]*)$ ]]; then
        record_scenario "s5-git-branch-commit" "fail" "missing or malformed pre-PR SVN revision"
        record_scenario "s6-git-pr-merge" "skip" "S5 did not produce a commit"
        CYCLE_OK=false
        return
    fi

    # Get main branch SHA for creating the new branch.
    MAIN_REF_RESP=$(gh_api GET "/repos/${GHE_OWNER}/${GHE_REPO}/git/ref/heads/main" "")
    MAIN_REF_CODE=$(gh_status "$MAIN_REF_RESP")
    MAIN_SHA=$(gh_body "$MAIN_REF_RESP" | jq -r '.object.sha // ""' 2>/dev/null || echo "")

    if [[ "$MAIN_REF_CODE" == "200" && -n "$MAIN_SHA" ]]; then
        # Create feature branch.
        CREATE_REF_RESP=$(gh_api POST "/repos/${GHE_OWNER}/${GHE_REPO}/git/refs" \
            "{\"ref\":\"refs/heads/${PR_BRANCH}\",\"sha\":\"${MAIN_SHA}\"}")
        CREATE_REF_CODE=$(gh_status "$CREATE_REF_RESP")
        if [[ "$CREATE_REF_CODE" == "201" ]]; then
            branch_created=true
            # Commit file on the feature branch.
            COMMIT_RESP=$(gh_api PUT "/repos/${GHE_OWNER}/${GHE_REPO}/contents/${GIT_FILE}" \
                "{\"message\":\"Validation: add $GIT_FILE via PR\",\"content\":\"${GIT_CONTENT_B64}\",\"branch\":\"${PR_BRANCH}\"}")
            COMMIT_CODE=$(gh_status "$COMMIT_RESP")
            GIT_SHA=$(gh_body "$COMMIT_RESP" | jq -er \
                '.commit.sha | strings | select(test("^[0-9a-fA-F]{40}$"))' 2>/dev/null) || GIT_SHA=""
            if [[ "$COMMIT_CODE" == "201" && -n "$GIT_SHA" ]]; then
                echo "$GIT_SHA" > "$CYCLE_DIR/s5-git-sha.txt"
                record_scenario "s5-git-branch-commit" "pass" "sha=${GIT_SHA:0:8} on ${PR_BRANCH}"
            else
                GIT_SHA=""
                record_scenario "s5-git-branch-commit" "fail" "commit unconfirmed or missing SHA (HTTP $COMMIT_CODE)"
                gh_body "$COMMIT_RESP" > "$CYCLE_DIR/s5-error.json" 2>/dev/null || true
                CYCLE_OK=false
            fi
        else
            record_scenario "s5-git-branch-commit" "fail" "create-ref HTTP $CREATE_REF_CODE"
            CYCLE_OK=false
        fi
    else
        record_scenario "s5-git-branch-commit" "fail" "could not get main SHA (HTTP $MAIN_REF_CODE)"
        CYCLE_OK=false
    fi

    # ---- Scenario 6: Open + merge PR via GHE API ----
    log "▶ S6: Open and merge PR"
    if [[ -n "$GIT_SHA" ]]; then
        # Open PR.
        PR_CREATE_RESP=$(gh_api POST "/repos/${GHE_OWNER}/${GHE_REPO}/pulls" \
            "{\"title\":\"Validation: ${CANARY}\",\"head\":\"${PR_BRANCH}\",\"base\":\"main\",\"body\":\"Automated RepoSync validation PR\"}")
        PR_CREATE_CODE=$(gh_status "$PR_CREATE_RESP")
        PR_NUMBER=$(gh_body "$PR_CREATE_RESP" | jq -r '.number // ""' 2>/dev/null || echo "")
        if [[ "$PR_CREATE_CODE" == "201" && "$PR_NUMBER" =~ ^[1-9][0-9]*$ ]]; then
            # Merge the PR (squash merge to keep it clean).
            sleep 1  # Give GitHub a moment for merge-ability check.
            MERGE_RESP=$(gh_api PUT "/repos/${GHE_OWNER}/${GHE_REPO}/pulls/${PR_NUMBER}/merge" \
                "{\"merge_method\":\"squash\",\"sha\":\"${GIT_SHA}\",\"commit_title\":\"Validation: merge ${CANARY}\"}")
            MERGE_CODE=$(gh_status "$MERGE_RESP")
            PR_MERGE_SHA=$(gh_body "$MERGE_RESP" | jq -er \
                'select(.merged == true) | .sha | strings | select(test("^[0-9a-fA-F]{40}$"))' 2>/dev/null) || PR_MERGE_SHA=""
            if [[ "$MERGE_CODE" == "200" && -n "$PR_MERGE_SHA" ]]; then
                PR_MERGED=true
                echo "$PR_NUMBER" > "$CYCLE_DIR/s6-pr-number.txt"
                echo "$PR_MERGE_SHA" > "$CYCLE_DIR/s6-merge-sha.txt"
                record_scenario "s6-git-pr-merge" "pass" "PR #${PR_NUMBER} merged (sha=${PR_MERGE_SHA:0:8})"
            else
                record_scenario "s6-git-pr-merge" "fail" "merge unconfirmed or missing commit SHA (HTTP $MERGE_CODE)"
                gh_body "$MERGE_RESP" > "$CYCLE_DIR/s6-merge-error.json" 2>/dev/null || true
                CYCLE_OK=false
            fi
        else
            record_scenario "s6-git-pr-merge" "fail" "create-PR HTTP $PR_CREATE_CODE"
            gh_body "$PR_CREATE_RESP" > "$CYCLE_DIR/s6-pr-error.json" 2>/dev/null || true
            CYCLE_OK=false
        fi
    else
        record_scenario "s6-git-pr-merge" "skip" "S5 did not produce a commit"
    fi

    # A name collision or rejected create does not give this cycle a ref to delete.
    if "$branch_created"; then
        gh_api DELETE "/repos/${GHE_OWNER}/${GHE_REPO}/git/refs/heads/${PR_BRANCH}" "" > /dev/null 2>&1 || true
    fi
}
