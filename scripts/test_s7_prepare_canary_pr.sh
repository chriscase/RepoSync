#!/usr/bin/env bash
# Run the real S5/S6 state transitions against owned command fakes.
set -euo pipefail
source "$(dirname "$0")/s7_prepare_canary_pr.sh"

fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
CYCLE_DIR="$fixture"
SVN_URL=fixture
SVN_USERNAME=fixture
SVN_PASSWORD=fixture
GHE_OWNER=fixture
GHE_REPO=fixture
CANARY=fixture
member=$(printf '%040d' 1)
merge=$(printf '%040d' 2)
log() { :; }
sleep() { :; }
record_scenario() { printf '%s %s\n' "$1" "$2" >> "$fixture/scenarios"; }
gh_status() { tail -1 <<< "$1"; }
gh_body() { sed '$d' <<< "$1"; }
svn() {
    case "$mode" in
        missing) return 1 ;;
        malformed) echo '92?' ;;
        *) echo 92 ;;
    esac
}
gh_api() {
    local method=$1 endpoint=$2 body=$3
    printf '%s %s\n' "$method" "$endpoint" >> "$fixture/requests"
    case "$method:$endpoint" in
        GET:*/git/ref/heads/main)
            printf '{"object":{"sha":"%s"}}\n200\n' "$member" ;;
        POST:*/git/refs)
            if [[ "$mode" == collision ]]; then printf '{}\n422\n'; else printf '{}\n201\n'; fi ;;
        PUT:*/contents/*)
            if [[ "$mode" == failed_commit ]]; then
                # Even a well-shaped body cannot supply a member after a non-success HTTP result.
                printf '{"commit":{"sha":"%s"}}\n500\n' "$member"
            else
                printf '{"commit":{"sha":"%s"}}\n201\n' "$member"
            fi ;;
        POST:*/pulls) printf '{"number":42}\n201\n' ;;
        PUT:*/pulls/42/merge)
            jq -e --arg expected "$member" '.sha == $expected' <<< "$body" >/dev/null
            printf '{"merged":true,"sha":"%s"}\n200\n' "$merge" ;;
        DELETE:*/git/refs/heads/*) printf '{}\n204\n' ;;
        *) return 70 ;;
    esac
}

for mode in missing malformed collision failed_commit positive; do
    : > "$fixture/requests"
    : > "$fixture/scenarios"
    # Simulate the previous cycle or a reused --artifacts-dir.
    GIT_SHA=$member
    PR_MERGE_SHA=$merge
    PR_MERGED=true
    PR_NUMBER=42
    CYCLE_OK=true
    printf '%s\n' "$member" > "$CYCLE_DIR/s5-git-sha.txt"
    printf '%s\n' "$merge" > "$CYCLE_DIR/s6-merge-sha.txt"
    echo 42 > "$CYCLE_DIR/s6-pr-number.txt"
    prepare_canary_pr

    if [[ "$mode" == positive ]]; then
        [[ "$PR_MERGED" == true && "$GIT_SHA" == "$member" && "$PR_MERGE_SHA" == "$merge" ]]
        grep -Fxq 's6-git-pr-merge pass' "$fixture/scenarios"
        [[ $(cut -d ' ' -f 1 "$fixture/requests" | paste -sd, -) == GET,POST,PUT,POST,PUT,DELETE ]]
    else
        [[ "$CYCLE_OK" == false && "$PR_MERGED" == false && -z "$GIT_SHA" && -z "$PR_MERGE_SHA" ]]
        grep -Fxq 's5-git-branch-commit fail' "$fixture/scenarios"
        grep -Fxq 's6-git-pr-merge skip' "$fixture/scenarios"
        case "$mode" in
            missing|malformed) [[ ! -s "$fixture/requests" ]] ;;
            collision) [[ $(cut -d ' ' -f 1 "$fixture/requests" | paste -sd, -) == GET,POST ]] ;;
            failed_commit) [[ $(cut -d ' ' -f 1 "$fixture/requests" | paste -sd, -) == GET,POST,PUT,DELETE ]] ;;
        esac
    fi
done
echo 'S5/S6 preflight controls passed (5 cases).'
