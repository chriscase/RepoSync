#!/bin/bash
# Stress test: many commits across all repos, both directions, verify everything
set -e

SANDBOX_ID="f4ee7f21-4b7d-47e3-9aac-85cc379a521a"
LARGE_ID="6e4805ec-6f26-431d-9e82-3853f8ec654c"
BRANCH_ID="e46c9f82-2f79-4de3-9de3-1a0a236d8c06"
SANDBOX_SVN="svn://localhost:3691/sandbox-repo"
LARGE_SVN="svn://localhost:3692/large-repo"
GITEA_TOKEN=$(cat /opt/reposync/gitea-sandbox-token.txt)
REPOSYNC="http://localhost:8080"
PASS=0
FAIL=0

check() {
    local TEST="$1" EXPECTED="$2" ACTUAL="$3"
    if [ "$EXPECTED" = "$ACTUAL" ]; then
        echo "  ✅ $TEST"
        PASS=$((PASS+1))
    else
        echo "  ❌ $TEST (expected=$EXPECTED actual=$ACTUAL)"
        FAIL=$((FAIL+1))
    fi
}

TOKEN=$(curl -s -X POST "$REPOSYNC/api/auth/login" \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"changeme"}' | python3 -c "import sys,json; print(json.load(sys.stdin)['token'])")

echo "=========================================="
echo "  STRESS TEST — $(date)"
echo "=========================================="

# Record starting state
SANDBOX_GIT_BEFORE=$(cd /opt/reposync/repos/$SANDBOX_ID/git-repo && git log --oneline | wc -l)
LARGE_GIT_BEFORE=$(cd /opt/reposync/repos/$LARGE_ID/git-repo && git log --oneline | wc -l)
SANDBOX_SVN_BEFORE=$(svn info "$SANDBOX_SVN" --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')
LARGE_SVN_BEFORE=$(svn info "$LARGE_SVN" --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')

echo "Before: Sandbox Git=$SANDBOX_GIT_BEFORE SVN=r$SANDBOX_SVN_BEFORE | Large Git=$LARGE_GIT_BEFORE SVN=r$LARGE_SVN_BEFORE"

# ==========================================
echo ""
echo "=== PHASE 1: Rapid SVN commits (10 per repo) ==="
# ==========================================

SWC="/tmp/sandbox-svn-wc"
LWC="/tmp/large-svn-wc"
svn update "$SWC" --username alice --password alice123 --non-interactive 2>/dev/null
svn update "$LWC" --username alice --password alice123 --non-interactive 2>/dev/null

USERS_S=(alice bob charlie alice bob charlie alice bob charlie alice)
PW_S=(alice123 bob123 charlie123 alice123 bob123 charlie123 alice123 bob123 charlie123 alice123)
USERS_L=(dave eve frank dave eve frank dave eve frank dave)
PW_L=(dave123 eve123 frank123 dave123 eve123 frank123 dave123 eve123 frank123 dave123)

for i in $(seq 1 10); do
    UI=$((i-1))
    echo "// Stress SVN $i by ${USERS_S[$UI]} $(date +%s)" >> "$SWC/src/main.c"
    svn commit "$SWC" -m "Stress-SVN-$i: ${USERS_S[$UI]} commit on sandbox" --username "${USERS_S[$UI]}" --password "${PW_S[$UI]}" --non-interactive 2>/dev/null

    echo "// Stress SVN $i by ${USERS_L[$UI]} $(date +%s)" >> "$LWC/src/core/main.c"
    svn commit "$LWC" -m "Stress-SVN-$i: ${USERS_L[$UI]} commit on large" --username "${USERS_L[$UI]}" --password "${PW_L[$UI]}" --non-interactive 2>/dev/null
done
echo "  20 SVN commits created (10 per repo)"

# ==========================================
echo ""
echo "=== PHASE 2: Rapid Git commits (5 per repo) ==="
# ==========================================

SCLONE="/tmp/stress-sandbox-clone"
LCLONE="/tmp/stress-large-clone"
rm -rf "$SCLONE" "$LCLONE"
git clone "http://x-access-token:$GITEA_TOKEN@localhost:3001/admin/sandbox-sync-test.git" "$SCLONE" 2>/dev/null
git clone "http://x-access-token:$GITEA_TOKEN@localhost:3001/admin/large-sync-test.git" "$LCLONE" 2>/dev/null

GIT_AUTHORS_S=("Alice Dev:alice@dev.com" "Bob Dev:bob@dev.com" "Charlie Dev:charlie@dev.com" "Dana Dev:dana@dev.com" "Eve Dev:eve@dev.com")
GIT_AUTHORS_L=("Frank Dev:frank@dev.com" "Grace Dev:grace@dev.com" "Hank Dev:hank@dev.com" "Ivy Dev:ivy@dev.com" "Jack Dev:jack@dev.com")

cd "$SCLONE"
for i in $(seq 1 5); do
    AI=$((i-1))
    AUTHOR="${GIT_AUTHORS_S[$AI]}"
    NAME="${AUTHOR%%:*}"
    EMAIL="${AUTHOR##*:}"
    echo "// Stress Git $i by $NAME $(date +%s)" >> src/main.c
    git add -A && git commit -m "Stress-Git-$i: $NAME on sandbox" --author="$NAME <$EMAIL>" 2>/dev/null
done
git push origin main 2>/dev/null
echo "  5 Git commits pushed to Sandbox"

cd "$LCLONE"
for i in $(seq 1 5); do
    AI=$((i-1))
    AUTHOR="${GIT_AUTHORS_L[$AI]}"
    NAME="${AUTHOR%%:*}"
    EMAIL="${AUTHOR##*:}"
    echo "// Stress Git $i by $NAME $(date +%s)" >> src/core/main.c
    git add -A && git commit -m "Stress-Git-$i: $NAME on large" --author="$NAME <$EMAIL>" 2>/dev/null
done
git push origin main 2>/dev/null
echo "  5 Git commits pushed to Large"

# ==========================================
echo ""
echo "=== PHASE 3: Branch pair commits ==="
# ==========================================

svn update "$SWC" --username alice --password alice123 --non-interactive 2>/dev/null
echo "// Branch stress by alice $(date +%s)" >> "$SWC/src/main.c"
svn commit "$SWC" -m "Stress-Branch-SVN: Alice on trunk (for test-branch)" --username alice --password alice123 --non-interactive 2>/dev/null

BCLONE="/tmp/stress-branch-clone"
rm -rf "$BCLONE"
git clone -b test-branch "http://x-access-token:$GITEA_TOKEN@localhost:3001/admin/sandbox-sync-test.git" "$BCLONE" 2>/dev/null || {
    git clone "http://x-access-token:$GITEA_TOKEN@localhost:3001/admin/sandbox-sync-test.git" "$BCLONE" 2>/dev/null
    cd "$BCLONE" && git checkout -b test-branch 2>/dev/null
}
cd "$BCLONE"
echo "// Branch stress by BranchDev $(date +%s)" >> src/main.c 2>/dev/null || echo "// branch" > src/branch_stress.c
git add -A && git commit -m "Stress-Branch-Git: BranchDev on test-branch" --author="Branch Dev <branch@dev.com>" 2>/dev/null
git push origin test-branch 2>/dev/null
echo "  2 branch pair commits (1 SVN + 1 Git)"

# ==========================================
echo ""
echo "=== PHASE 4: Wait for sync (2 cycles = 120s) ==="
# ==========================================

echo "  Waiting..."
sleep 120

# ==========================================
echo ""
echo "=== PHASE 5: Verify SVN→Git sync ==="
# ==========================================

SANDBOX_GIT_AFTER=$(cd /opt/reposync/repos/$SANDBOX_ID/git-repo && git log --oneline | wc -l)
LARGE_GIT_AFTER=$(cd /opt/reposync/repos/$LARGE_ID/git-repo && git log --oneline | wc -l)
SANDBOX_GIT_NEW=$((SANDBOX_GIT_AFTER - SANDBOX_GIT_BEFORE))
LARGE_GIT_NEW=$((LARGE_GIT_AFTER - LARGE_GIT_BEFORE))

echo "  Sandbox: $SANDBOX_GIT_NEW new Git commits (expected ≥10)"
echo "  Large: $LARGE_GIT_NEW new Git commits (expected ≥10)"
check "Sandbox SVN→Git (≥10)" "YES" "$([ $SANDBOX_GIT_NEW -ge 10 ] && echo YES || echo NO)"
check "Large SVN→Git (≥10)" "YES" "$([ $LARGE_GIT_NEW -ge 10 ] && echo YES || echo NO)"

# ==========================================
echo ""
echo "=== PHASE 6: Verify Git→SVN sync ==="
# ==========================================

SANDBOX_SVN_AFTER=$(svn info "$SANDBOX_SVN" --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')
LARGE_SVN_AFTER=$(svn info "$LARGE_SVN" --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')
SANDBOX_SVN_NEW=$((SANDBOX_SVN_AFTER - SANDBOX_SVN_BEFORE))
LARGE_SVN_NEW=$((LARGE_SVN_AFTER - LARGE_SVN_BEFORE))

echo "  Sandbox: $SANDBOX_SVN_NEW new SVN revisions (expected ≥15: 10+5 git)"
echo "  Large: $LARGE_SVN_NEW new SVN revisions (expected ≥15: 10+5 git)"
check "Sandbox Git→SVN (≥5 from git)" "YES" "$([ $SANDBOX_SVN_NEW -ge 15 ] && echo YES || echo NO)"
check "Large Git→SVN (≥5 from git)" "YES" "$([ $LARGE_SVN_NEW -ge 15 ] && echo YES || echo NO)"

# ==========================================
echo ""
echo "=== PHASE 7: Verify repo_id on new records ==="
# ==========================================

SR_WITH_REPO=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT COUNT(*) FROM sync_records WHERE repo_id IS NOT NULL AND repo_id != ''")
SR_WITHOUT_REPO=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT COUNT(*) FROM sync_records WHERE repo_id IS NULL OR repo_id = ''")
check "New sync records have repo_id" "YES" "$([ $SR_WITH_REPO -gt 0 ] && echo YES || echo NO)"
check "No sync records without repo_id" "0" "$SR_WITHOUT_REPO"
echo "  With repo_id: $SR_WITH_REPO, Without: $SR_WITHOUT_REPO"

AUDIT_WITH_REPO=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT COUNT(*) FROM audit_log WHERE repo_id IS NOT NULL AND repo_id != ''")
echo "  Audit entries with repo_id: $AUDIT_WITH_REPO"

# ==========================================
echo ""
echo "=== PHASE 8: Verify watermarks ==="
# ==========================================

sqlite3 /opt/reposync/gitsvnsync.db "SELECT name, last_svn_rev, sync_status FROM repositories" | while IFS='|' read -r NAME REV STATUS; do
    check "Watermark $NAME > 0" "YES" "$([ "$REV" -gt 0 ] 2>/dev/null && echo YES || echo NO)"
done

# ==========================================
echo ""
echo "=== PHASE 9: Verify echo prevention ==="
# ==========================================

SANDBOX_SVN_PRE_ECHO=$SANDBOX_SVN_AFTER
echo "  Waiting 75s for echo check..."
sleep 75
SANDBOX_SVN_POST_ECHO=$(svn info "$SANDBOX_SVN" --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')
ECHO_DIFF=$((SANDBOX_SVN_POST_ECHO - SANDBOX_SVN_PRE_ECHO))
check "No echo loop (diff=$ECHO_DIFF ≤ 2)" "YES" "$([ $ECHO_DIFF -le 2 ] && echo YES || echo NO)"

# ==========================================
echo ""
echo "=== PHASE 10: Server stability ==="
# ==========================================

STABLE=0
for i in $(seq 1 10); do
    H=$(curl -s -m 3 -o /dev/null -w "%{http_code}" "$REPOSYNC/api/status/health")
    R=$(curl -s -m 3 -o /dev/null -w "%{http_code}" -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/repos")
    if [ "$H" = "200" ] && [ "$R" = "200" ]; then
        STABLE=$((STABLE+1))
    fi
    sleep 2
done
check "Server stable (10/10)" "10" "$STABLE"

# ==========================================
echo ""
echo "=== PHASE 11: Credentials persist ==="
# ==========================================

for RID in $SANDBOX_ID $LARGE_ID $BRANCH_ID; do
    RNAME=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT name FROM repositories WHERE id = '$RID'")
    HAS_PW=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT CASE WHEN value != '' THEN 'YES' ELSE 'NO' END FROM kv_state WHERE key = 'secret_svn_password_$RID'" 2>/dev/null)
    check "Credentials for $RNAME" "YES" "${HAS_PW:-NO}"
done

# ==========================================
echo ""
echo "=== PHASE 12: Authors in sync records ==="
# ==========================================

AUTHORS=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT DISTINCT author FROM sync_records WHERE author != '' ORDER BY author LIMIT 10")
AUTHOR_COUNT=$(echo "$AUTHORS" | wc -l)
check "Multiple authors in sync records (≥3)" "YES" "$([ $AUTHOR_COUNT -ge 3 ] && echo YES || echo NO)"
echo "  Authors found: $AUTHORS" | tr '\n' ', '
echo ""

# ==========================================
echo ""
echo "=== PHASE 13: Scheduler running all repos ==="
# ==========================================

for RNAME in "Sandbox Test" "Large Test" "test-branch"; do
    CYCLES=$(grep "per-repo sync cycle completed.*$RNAME" /tmp/gitsvnsync.log | wc -l)
    check "Scheduler ran $RNAME (≥1 cycle)" "YES" "$([ $CYCLES -ge 1 ] && echo YES || echo NO)"
done

# ==========================================
echo ""
echo "=========================================="
echo "  RESULTS"
echo "=========================================="
echo "  ✅ PASSED: $PASS"
echo "  ❌ FAILED: $FAIL"
echo "  TOTAL:    $((PASS+FAIL))"
TOTAL=$((PASS+FAIL))
if [ "$FAIL" -eq 0 ]; then
    echo "  🎉 ALL TESTS PASSED!"
else
    PCT=$((PASS * 100 / TOTAL))
    echo "  Pass rate: ${PCT}%"
fi
echo "=========================================="
