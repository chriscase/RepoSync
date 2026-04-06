#!/bin/bash
# Final E2E: commits both ways on all sandbox repos, verify everything
set -e

SANDBOX_ID="f4ee7f21-4b7d-47e3-9aac-85cc379a521a"
LARGE_ID="6e4805ec-6f26-431d-9e82-3853f8ec654c"
BRANCH_ID="e46c9f82-2f79-4de3-9de3-1a0a236d8c06"
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
echo "  FINAL E2E TEST — $(date)"
echo "=========================================="

# Snapshot before
SANDBOX_GIT_B=$(cd /opt/reposync/repos/$SANDBOX_ID/git-repo && git log --oneline | wc -l)
LARGE_GIT_B=$(cd /opt/reposync/repos/$LARGE_ID/git-repo && git log --oneline | wc -l)
SANDBOX_SVN_B=$(svn info svn://localhost:3691/sandbox-repo --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')
LARGE_SVN_B=$(svn info svn://localhost:3692/large-repo --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')

echo "Before: Sandbox(Git=$SANDBOX_GIT_B SVN=r$SANDBOX_SVN_B) Large(Git=$LARGE_GIT_B SVN=r$LARGE_SVN_B)"

# ==========================================
echo ""
echo "=== 1. SVN commits: 5 per repo ==="
# ==========================================
SWC="/tmp/sandbox-svn-wc"
LWC="/tmp/large-svn-wc"
svn update "$SWC" --username alice --password alice123 --non-interactive 2>/dev/null
svn update "$LWC" --username alice --password alice123 --non-interactive 2>/dev/null

for i in 1 2 3 4 5; do
    U_S=("alice" "bob" "charlie" "alice" "bob")
    P_S=("alice123" "bob123" "charlie123" "alice123" "bob123")
    U_L=("dave" "eve" "frank" "dave" "eve")
    P_L=("dave123" "eve123" "frank123" "dave123" "eve123")

    echo "// Final-E2E-$i $(date +%s)" >> "$SWC/src/main.c"
    svn commit "$SWC" -m "Final-E2E-$i: ${U_S[$((i-1))]} on sandbox" --username "${U_S[$((i-1))]}" --password "${P_S[$((i-1))]}" --non-interactive 2>/dev/null

    echo "// Final-E2E-$i $(date +%s)" >> "$LWC/src/core/main.c"
    svn commit "$LWC" -m "Final-E2E-$i: ${U_L[$((i-1))]} on large" --username "${U_L[$((i-1))]}" --password "${P_L[$((i-1))]}" --non-interactive 2>/dev/null
done
echo "  10 SVN commits done (5 per repo)"

# ==========================================
echo ""
echo "=== 2. Git commits: 3 per repo ==="
# ==========================================
for REPO_NAME in "sandbox-sync-test" "large-sync-test"; do
    CLONE="/tmp/final-e2e-$REPO_NAME"
    rm -rf "$CLONE"
    git clone "http://x-access-token:$GITEA_TOKEN@localhost:3001/admin/$REPO_NAME.git" "$CLONE" 2>/dev/null
    cd "$CLONE"
    FILE="src/main.c"
    [ "$REPO_NAME" = "large-sync-test" ] && FILE="src/core/main.c"
    for i in 1 2 3; do
        echo "// Final-Git-$i $(date +%s)" >> "$FILE"
        git add -A && git commit -m "Final-Git-$i: GitDev on $REPO_NAME" --author="GitDev$i <gitdev$i@test.com>" 2>/dev/null
    done
    git push origin main 2>/dev/null
done
echo "  6 Git commits done (3 per repo)"

# ==========================================
echo ""
echo "=== 3. Branch pair: 1 SVN + 1 Git ==="
# ==========================================
svn update "$SWC" --username alice --password alice123 --non-interactive 2>/dev/null
echo "// Branch-Final $(date +%s)" >> "$SWC/src/main.c"
svn commit "$SWC" -m "Final-Branch-SVN: alice on trunk" --username alice --password alice123 --non-interactive 2>/dev/null

BCLONE="/tmp/final-e2e-branch"
rm -rf "$BCLONE"
git clone -b test-branch "http://x-access-token:$GITEA_TOKEN@localhost:3001/admin/sandbox-sync-test.git" "$BCLONE" 2>/dev/null || {
    git clone "http://x-access-token:$GITEA_TOKEN@localhost:3001/admin/sandbox-sync-test.git" "$BCLONE" 2>/dev/null
    cd "$BCLONE" && git checkout -b test-branch 2>/dev/null
}
cd "$BCLONE"
echo "// Branch-Git-Final $(date +%s)" >> src/main.c 2>/dev/null || echo "// branch" > src/final_branch.c
git add -A && git commit -m "Final-Branch-Git: BranchDev" --author="BranchDev <branch@test.com>" 2>/dev/null
git push origin test-branch 2>/dev/null
echo "  2 branch pair commits done"

# ==========================================
echo ""
echo "=== 4. Wait 90s for sync ==="
# ==========================================
sleep 90

# ==========================================
echo ""
echo "=== 5. Verify SVN→Git ==="
# ==========================================
SANDBOX_GIT_A=$(cd /opt/reposync/repos/$SANDBOX_ID/git-repo && git log --oneline | wc -l)
LARGE_GIT_A=$(cd /opt/reposync/repos/$LARGE_ID/git-repo && git log --oneline | wc -l)
SG_NEW=$((SANDBOX_GIT_A - SANDBOX_GIT_B))
LG_NEW=$((LARGE_GIT_A - LARGE_GIT_B))
check "Sandbox SVN→Git (≥5)" "YES" "$([ $SG_NEW -ge 5 ] && echo YES || echo NO)"
check "Large SVN→Git (≥5)" "YES" "$([ $LG_NEW -ge 5 ] && echo YES || echo NO)"
echo "  Sandbox: +$SG_NEW git commits, Large: +$LG_NEW git commits"

# ==========================================
echo ""
echo "=== 6. Verify Git→SVN ==="
# ==========================================
SANDBOX_SVN_A=$(svn info svn://localhost:3691/sandbox-repo --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')
LARGE_SVN_A=$(svn info svn://localhost:3692/large-repo --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')
SS_NEW=$((SANDBOX_SVN_A - SANDBOX_SVN_B))
LS_NEW=$((LARGE_SVN_A - LARGE_SVN_B))
check "Sandbox Git→SVN (≥8: 5svn+3git)" "YES" "$([ $SS_NEW -ge 8 ] && echo YES || echo NO)"
check "Large Git→SVN (≥8: 5svn+3git)" "YES" "$([ $LS_NEW -ge 8 ] && echo YES || echo NO)"
echo "  Sandbox: +$SS_NEW svn revs, Large: +$LS_NEW svn revs"

# ==========================================
echo ""
echo "=== 7. Verify sync records have repo_id ==="
# ==========================================
SR_WITH=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT COUNT(*) FROM sync_records WHERE repo_id IS NOT NULL AND repo_id != ''")
SR_WITHOUT=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT COUNT(*) FROM sync_records WHERE repo_id IS NULL OR repo_id = ''")
check "Sync records have repo_id" "0" "$SR_WITHOUT"
echo "  With: $SR_WITH, Without: $SR_WITHOUT"

# ==========================================
echo ""
echo "=== 8. Verify audit log has repo_id + authors ==="
# ==========================================
AUDIT_WITH=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT COUNT(*) FROM audit_log WHERE repo_id IS NOT NULL AND repo_id != ''")
AUDIT_AUTHORS=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT COUNT(DISTINCT author) FROM audit_log WHERE author IS NOT NULL AND author != ''")
check "Audit entries have repo_id (≥1)" "YES" "$([ $AUDIT_WITH -ge 1 ] && echo YES || echo NO)"
check "Multiple authors in audit (≥3)" "YES" "$([ $AUDIT_AUTHORS -ge 3 ] && echo YES || echo NO)"
echo "  Audit with repo_id: $AUDIT_WITH, Distinct authors: $AUDIT_AUTHORS"

# ==========================================
echo ""
echo "=== 9. Watermarks valid ==="
# ==========================================
sqlite3 /opt/reposync/gitsvnsync.db "SELECT name, last_svn_rev FROM repositories" | while IFS='|' read -r N R; do
    check "Watermark $N > 0" "YES" "$([ "$R" -gt 0 ] 2>/dev/null && echo YES || echo NO)"
done

# ==========================================
echo ""
echo "=== 10. No echo loop ==="
# ==========================================
PRE=$(svn info svn://localhost:3691/sandbox-repo --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')
sleep 75
POST=$(svn info svn://localhost:3691/sandbox-repo --username alice --password alice123 --non-interactive 2>&1 | grep "^Revision:" | awk '{print $2}')
DIFF=$((POST - PRE))
check "No echo (diff=$DIFF ≤ 1)" "YES" "$([ $DIFF -le 1 ] && echo YES || echo NO)"

# ==========================================
echo ""
echo "=== 11. Server stability ==="
# ==========================================
S=0
for i in $(seq 1 10); do
    H=$(curl -s -m 3 -o /dev/null -w "%{http_code}" "$REPOSYNC/api/status/health")
    R=$(curl -s -m 3 -o /dev/null -w "%{http_code}" -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/repos")
    [ "$H" = "200" ] && [ "$R" = "200" ] && S=$((S+1))
    sleep 2
done
check "Stable (10/10)" "10" "$S"

# ==========================================
echo ""
echo "=== 12. Credentials persist ==="
# ==========================================
for RID in $SANDBOX_ID $LARGE_ID $BRANCH_ID; do
    N=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT name FROM repositories WHERE id = '$RID'")
    HAS=$(sqlite3 /opt/reposync/gitsvnsync.db "SELECT CASE WHEN length(value) > 0 THEN 'YES' ELSE 'NO' END FROM kv_state WHERE key = 'secret_svn_password_$RID'" 2>/dev/null)
    check "Creds $N" "YES" "${HAS:-NO}"
done

# ==========================================
echo ""
echo "=== 13. API endpoints work ==="
# ==========================================
check "GET /api/repos" "200" "$(curl -s -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/repos")"
check "GET /api/status" "200" "$(curl -s -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/status")"
check "GET /api/audit" "200" "$(curl -s -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/audit")"
check "GET /api/sync-records" "200" "$(curl -s -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/sync-records")"
check "GET /api/commit-map" "200" "$(curl -s -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/commit-map")"
check "GET /api/status/system" "200" "$(curl -s -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/status/system")"
check "GET /api/repos/$SANDBOX_ID" "200" "$(curl -s -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/repos/$SANDBOX_ID")"
check "GET /api/repos/$SANDBOX_ID/branches" "200" "$(curl -s -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/repos/$SANDBOX_ID/branches")"
check "POST /api/repos/$SANDBOX_ID/test-svn" "200" "$(curl -s -m 15 -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/repos/$SANDBOX_ID/test-svn")"
check "POST /api/repos/$SANDBOX_ID/test-git" "200" "$(curl -s -m 15 -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $TOKEN" "$REPOSYNC/api/repos/$SANDBOX_ID/test-git")"
check "GET /api/auth/info" "200" "$(curl -s -m 5 -o /dev/null -w '%{http_code}' "$REPOSYNC/api/auth/info")"

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
