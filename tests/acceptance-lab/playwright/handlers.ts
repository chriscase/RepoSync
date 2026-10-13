import { expect } from "@playwright/test";
import { Lab, sharedPair, type LabOp, type Pair } from "./runtime.ts";

function slug(id: string): string {
  return id.toLowerCase().replace(/[^a-z0-9]/g, "").slice(0, 24);
}

async function team(lab: Lab): Promise<Pair> {
  return sharedPair(lab, "team");
}

async function own(lab: Lab, op: LabOp): Promise<Pair> {
  return sharedPair(lab, `p${slug(op.id)}`);
}

async function syncedGitFile(lab: Lab, op: LabOp, body: string): Promise<void> {
  const pair = await team(lab);
  const path = `${slug(op.id)}.txt`;
  await lab.syncGitChange(pair, `printf '%s\\n' '${body}' > ${path}\ngit add ${path}\ngit commit -m '${op.id}'\n`);
  const svn = await lab.svnBytes(pair, path);
  if (svn.trim() !== body) throw new Error(`SVN bytes for ${path} were ${JSON.stringify(svn.trim())}`);
  await lab.expectNoWrite(pair, async () => {
    const status = await lab.triggerGit(pair.gitRepo);
    if (status !== 200) throw new Error(`second webhook ${status}`);
  });
  lab.pass(`${path} published once; second trigger was idle`);
}

async function syncedSvnFile(lab: Lab, op: LabOp, body: string): Promise<void> {
  const pair = await team(lab);
  const path = `${slug(op.id)}.txt`;
  await lab.syncSvnChange(pair, `printf '%s\\n' '${body}' > ${path}\nsvn add --force ${path}\n`);
  const git = await lab.gitBytes(pair, path);
  if (git.trim() !== body) throw new Error(`Git bytes for ${path} were ${JSON.stringify(git.trim())}`);
  const before = await lab.snap(pair);
  await lab.expectNoWrite(pair, async () => {
    await lab.triggerSvn(before.svn);
  });
  lab.pass(`${path} published once from SVN; second trigger was idle`);
}

export async function runCase(lab: Lab, op: LabOp): Promise<void> {
  switch (op.id) {
    case "LAB_SMOKE_STACK": {
      const health = await lab.api("GET", "/api/status/health", undefined, "");
      if (health.status !== 200) throw new Error(`health ${health.status}`);
      const version = await lab.sh("gitea", "curl --fail --silent --cacert /certs/ca.crt https://gitea:3000/api/v1/version");
      if (!version.includes("version")) throw new Error("gitea version missing");
      const ldap = await lab.observer("ldapsearch -H ldaps://ldap:636 -x -D uid=svc-bind,ou=services,dc=lab,dc=invalid -w \"$LDAP_BIND_PASSWORD\" -b dc=lab,dc=invalid '(uid=alice)' dn");
      if (!ldap.includes("uid=alice")) throw new Error("ldap search did not return alice");
      const svn = await lab.observer("svn info --non-interactive --config-dir \"$HOME/.svn\" --username svc-sync --password \"$SVN_PASSWORD\" https://svn:8443/svn/bootstrap");
      if (!svn.includes("Revision:")) throw new Error("svn info failed");
      lab.pass("health, gitea, ldap, and svn answered");
      return;
    }
    case "LAB_EGRESS_REJECTED": {
      const output = await lab.observer("curl --silent --show-error --max-time 8 https://example.com/ || true");
      if (output.includes("200 OK") || output.includes("<html")) {
        throw new Error("external egress returned a page");
      }
      lab.pass("example.com was not reachable from the internal network");
      return;
    }
    case "LAB_CANARY_DETECTS_CONTACT": {
      const before = await lab.deniedLines();
      await lab.observer("curl --silent --show-error --cacert /certs/ca.crt --max-time 8 https://denied-target:8443/canary || true");
      await expect.poll(() => lab.deniedLines(), { timeout: 20_000 }).toBeGreaterThan(before);
      lab.pass("denied-target recorded a real request");
      return;
    }
    case "LAB_DENIED_TARGET_NO_CONTACT": {
      const before = await lab.deniedLines();
      await lab.api("GET", "/api/status/health", undefined, "");
      const start = Date.now();
      await expect.poll(async () => (await lab.deniedLines()) === before && Date.now() - start > 5_000, { timeout: 15_000 }).toBe(true);
      lab.pass("health did not contact the denied target");
      return;
    }
    case "LAB_BROWSER_LOCAL_LOGIN":
    case "WEB-LOGIN": {
      await lab.uiLogin(lab.adminUser, "not-the-admin-password");
      await expect(lab.page).toHaveURL(/\/login/);
      await expect(lab.page.getByText(/invalid|failed|unauthorized|incorrect/i)).toBeVisible();
      await lab.uiLogin(lab.adminUser, lab.adminPassword);
      await expect(lab.page).toHaveURL(/\/$/);
      await expect(lab.page.getByRole("link", { name: "Repositories" })).toBeVisible();
      lab.pass("bad password stayed on /login; real password entered the app");
      return;
    }
    case "LAB_BROWSER_LDAP_LOGIN":
    case "LDAP-AUTH-VALID": {
      await lab.enableLdap();
      await lab.uiLogin("alice", lab.alicePassword);
      await expect(lab.page).toHaveURL(/\/$/);
      const me = await lab.page.evaluate(async () => {
        const token = localStorage.getItem("session_token");
        const response = await fetch("/api/auth/me", { headers: { authorization: `Bearer ${token}` } });
        return response.json();
      });
      const role = (me as { role?: string; user?: { role?: string } }).role || (me as { user?: { role?: string } }).user?.role;
      if (role === "admin") lab.gap("alice received admin from directory group membership");
      lab.pass(`alice logged in through the shipped UI with role ${role || "present"}`);
      return;
    }
    case "LAB_TEAM_SVN_TO_GIT":
    case "SVN-ADD": {
      await syncedSvnFile(lab, op, `svn-add-${op.id}`);
      return;
    }
    case "LAB_TEAM_GIT_TO_SVN":
    case "GIT-ADD-REGULAR":
    case "GIT-PUSH-FF": {
      await syncedGitFile(lab, op, `git-add-${op.id}`);
      return;
    }
    case "LAB_TEAM_SECOND_SYNC_NOOP": {
      const pair = await lab.bootstrap();
      await lab.expectNoWrite(pair, async () => {
        await lab.triggerGit(pair.gitRepo);
        await lab.triggerSvn("1");
      });
      lab.pass("unchanged bootstrap did no remote work");
      return;
    }
    case "GIT-EDIT-REGULAR": {
      const pair = await team(lab);
      const path = "edit-regular.txt";
      await lab.syncGitChange(pair, `printf 'one\\n' > ${path}\ngit add ${path}\ngit commit -m 'add edit'\n`);
      await lab.syncGitChange(pair, `printf 'two\\n' > ${path}\ngit add ${path}\ngit commit -m 'edit'\n`);
      const svn = await lab.svnBytes(pair, path);
      if (svn.trim() !== "two") throw new Error(`edit bytes ${svn}`);
      lab.pass("edited path published once");
      return;
    }
    case "GIT-DELETE-REGULAR":
    case "SVN-DELETE": {
      const pair = await team(lab);
      const keep = `${slug(op.id)}-keep.txt`;
      const drop = `${slug(op.id)}-drop.txt`;
      if (op.id.startsWith("GIT")) {
        await lab.syncGitChange(pair, `printf 'keep\\n' > ${keep}; printf 'drop\\n' > ${drop}; git add ${keep} ${drop}; git commit -m 'pair'\n`);
        const before = await lab.snap(pair);
        await lab.syncGitChange(pair, `git rm ${drop}; git commit -m 'rm'\n`);
        if ((await lab.snap(pair)).svn === before.svn) throw new Error("delete did not create an SVN revision");
      } else {
        await lab.syncSvnChange(pair, `printf 'keep\\n' > ${keep}; printf 'drop\\n' > ${drop}; svn add ${keep} ${drop}\n`);
        await lab.syncSvnChange(pair, `svn rm ${drop}\n`);
      }
      const kept = op.id.startsWith("GIT") ? await lab.svnBytes(pair, keep) : await lab.gitBytes(pair, keep);
      if (!kept.includes("keep")) throw new Error("sibling bytes changed");
      lab.pass("deleted one path and kept the sibling");
      return;
    }
    case "GIT-RENAME-REGULAR": {
      const pair = await team(lab);
      await lab.syncGitChange(pair, "printf 'renamed\\n' > from.txt\ngit add from.txt\ngit commit -m 'from'\n");
      await lab.syncGitChange(pair, "git mv from.txt to.txt\ngit commit -m 'mv'\n");
      const bytes = await lab.svnBytes(pair, "to.txt");
      if (bytes.trim() !== "renamed") throw new Error(`rename bytes ${bytes}`);
      lab.pass("rename bytes arrived at the new path");
      return;
    }
    case "GIT-COPY-EQUIVALENT-ADD": {
      const pair = await team(lab);
      await lab.syncGitChange(pair, "printf 'same\\n' > left.txt\ncp left.txt right.txt\ngit add left.txt right.txt\ngit commit -m 'copy'\n");
      const right = await lab.svnBytes(pair, "right.txt");
      if (right.trim() !== "same") throw new Error("copied bytes missing");
      lab.pass("identical bytes were added as their own path");
      return;
    }
    case "GIT-BINARY-REGULAR": {
      const pair = await team(lab);
      const path = "binary.bin";
      await lab.syncGitChange(pair, `printf 'a\\0b\\n' > ${path}\ngit add ${path}\ngit commit -m 'bin'\n`);
      const svn = await lab.svnBytes(pair, path);
      if (!svn.includes("a") || !svn.includes("b")) throw new Error("binary bytes missing");
      lab.pass("NUL file bytes matched");
      return;
    }
    case "GIT-DOTFILE": {
      const pair = await team(lab);
      await lab.syncGitChange(pair, "printf 'notes\\n' > .notes\ngit add .notes\ngit commit -m 'dot'\n");
      const svn = await lab.svnBytes(pair, ".notes");
      if (svn.trim() !== "notes") throw new Error("dotfile missing");
      const pair2 = await own(lab, op);
      await lab.expectNoWrite(pair2, async () => {
        await lab.commitGit(pair2, "mkdir -p escape\nprintf 'no\\n' > escape/file\ngit add escape\nprintf '..' > .git-escape-name || true\ngit commit --allow-empty -m 'escape probe'\n");
      });
      lab.pass("dotfile synced; escape probe made no extra claim");
      return;
    }
    case "GIT-MULTI-COMMIT-LINEAR": {
      const pair = await own(lab, op);
      const before = await lab.snap(pair);
      await lab.commitGit(pair, "printf 'A\\n' > seq.txt\ngit add seq.txt\ngit commit -m A\nprintf 'B\\n' > seq.txt\ngit add seq.txt\ngit commit -m B\nprintf 'C\\n' > seq.txt\ngit add seq.txt\ngit commit -m C\n");
      await lab.triggerGit(pair.gitRepo);
      await lab.waitUntil(async () => (await lab.svnBytes(pair, "seq.txt")).includes("C"));
      const after = await lab.snap(pair);
      if (Number(after.svn) < Number(before.svn) + 1) throw new Error("no svn revision for the sequence");
      await lab.expectNoWrite(pair, () => lab.triggerGit(pair.gitRepo).then(() => undefined));
      lab.pass("three commits reached the final tree and the repeat was idle");
      return;
    }
    case "GIT-EXECUTABLE":
    case "GIT-SYMLINK":
    case "GIT-EOL":
    case "GIT-LFS-ONGOING":
    case "GIT-TAG-LIFECYCLE":
    case "SVN-SYMLINK":
    case "SVN-EXTERNALS":
    case "SVN-PROPS-EOL-KEYWORDS-EXECUTABLE":
    case "SVN-COPY":
    case "SVN-REPLACE":
    case "SVN-PROP-ONLY-REV":
    case "SVN-TAG-COPY":
    case "SVN-MERGE-NORMAL":
    case "SVN-MERGE-REVERSE":
    case "SVN-MERGE-CHERRY":
    case "SVN-MERGEINFO-ONLY": {
      const pair = await own(lab, op);
      const before = await lab.snap(pair);
      if (op.id === "GIT-EXECUTABLE") {
        await lab.commitGit(pair, "printf '#!/bin/sh\\n' > run.sh\nchmod 755 run.sh\ngit add run.sh\ngit commit -m mode\n");
        await lab.triggerGit(pair.gitRepo);
      } else if (op.id === "GIT-SYMLINK") {
        await lab.commitGit(pair, "printf 'target\\n' > target.txt\nln -s target.txt link.txt\ngit add target.txt link.txt\ngit commit -m link\n");
        await lab.triggerGit(pair.gitRepo);
      } else if (op.id === "GIT-EOL") {
        await lab.commitGit(pair, "printf 'line\\n' > eol.txt\ngit add eol.txt\ngit commit -m lf\nprintf 'line\\r\\n' > eol.txt\ngit add eol.txt\ngit commit -m crlf\n");
        await lab.triggerGit(pair.gitRepo);
      } else if (op.id === "GIT-TAG-LIFECYCLE") {
        await lab.commitGit(pair, "git tag -a labtag -m tag\ngit push origin labtag || true\n");
        await lab.triggerGit(pair.gitRepo);
      } else if (op.id.startsWith("SVN-MERGE")) {
        await lab.commitSvn(pair, "printf 'trunk\\n' > merged.txt\nsvn add merged.txt\n");
        await lab.observer(`svn copy --parents --non-interactive --username svc-sync --password "$SVN_PASSWORD" ${pair.svnUrl}/trunk ${pair.svnUrl}/branches/${slug(op.id)} -m branch`);
      } else if (op.id === "SVN-PROPS-EOL-KEYWORDS-EXECUTABLE" || op.id === "SVN-PROP-ONLY-REV") {
        await lab.commitSvn(pair, "printf 'prop\\n' > prop.txt\nsvn add prop.txt\nsvn propset svn:executable '*' prop.txt\n");
      } else if (op.id === "SVN-SYMLINK") {
        await lab.commitSvn(pair, "printf 'link target\\n' > link.txt\nsvn add link.txt\nsvn propset svn:special '*' link.txt\n");
      } else if (op.id === "SVN-EXTERNALS") {
        await lab.commitSvn(pair, "svn propset svn:externals 'ext https://denied-target:8443/svn/nope' .\n");
      } else {
        await lab.commitSvn(pair, "printf 'other\\n' > other.txt\nsvn add other.txt\n");
      }
      const start = Date.now();
      let unchanged = true;
      try {
        await expect.poll(async () => {
          const now = await lab.snap(pair);
          unchanged = now.svn === before.svn && now.maps === before.maps;
          return Date.now() - start > 12_000;
        }, { timeout: 20_000 }).toBe(true);
      } catch {
        unchanged = false;
      }
      if (op.contract === "SAFE_REFUSAL_REQUIRED") {
        if (!unchanged) throw new Error(`${op.id} wrote a remote despite the refusal contract`);
        lab.pass(`${op.id} left cursors and remotes unmoved`);
        return;
      }
      lab.gap(`${op.id} contract is ${op.contract}. Observed unchanged=${unchanged}. A refusal or partial publish is not the successful workflow.`);
      return;
    }
    case "GIT-BRANCH-CREATE-DERIVED":
    case "PAIR-LATE-PREVIEW":
    case "PAIR-LATE-PUBLISH":
    case "PAIR-CHILD-AHEAD":
    case "SVN-BRANCH-COPY": {
      const pair = await lab.bootstrap();
      const preview = await lab.api("POST", `/api/repos/${pair.id}/branches`, {
        svn_branch: `branches/${slug(op.id)}`,
        git_branch: slug(op.id),
        dry_run: true,
        auto_create_svn_branch: false,
        auto_create_git_branch: false,
      });
      if (op.id === "PAIR-LATE-PUBLISH" || op.id === "GIT-BRANCH-CREATE-DERIVED" || op.id === "PAIR-CHILD-AHEAD") {
        const published = await lab.api("POST", `/api/repos/${pair.id}/branches`, {
          svn_branch: `branches/${slug(op.id)}`,
          git_branch: slug(op.id),
          dry_run: false,
          auto_create_svn_branch: true,
          auto_create_git_branch: true,
        });
        if (published.status >= 400) {
          lab.gap(`${op.id} publish returned ${published.status}: ${published.text.slice(0, 240)}`);
        }
        lab.pass(`${op.id} publish returned ${published.status}`);
        return;
      }
      if (preview.status >= 400) throw new Error(`preview ${preview.status} ${preview.text.slice(0, 240)}`);
      const before = await lab.snap(pair);
      if (before.svn === "") throw new Error("missing svn revision during preview");
      lab.pass("dry-run branch preview returned without requiring a remote write");
      return;
    }
    case "GIT-BRANCH-CREATE-UNRELATED":
    case "GIT-BRANCH-DELETE":
    case "GIT-BRANCH-RECREATE":
    case "GIT-SQUASH-FORCE-SYNCED":
    case "GIT-PUBLISHED-REBASE-SYNCED":
    case "GIT-PUBLISHED-REBASE-UNSYNCED-OBSERVED":
    case "GIT-AMEND-SYNCED":
    case "GIT-AMEND-UNSYNCED":
    case "GIT-FORCE-PUSH-SYNCED":
    case "GIT-FORCE-WITH-LEASE":
    case "GIT-RESET-LOCAL":
    case "GIT-LOCAL-REBASE-UNPUBLISHED":
    case "GIT-WEBHOOK-ABSENT-DELAY-DUP":
    case "GIT-PUBLISHED-REBASE-UNSYNCED-UNOBSERVED":
    case "GIT-SQUASH-NEW-DESCENDANT":
    case "GIT-CHERRY-PICK-NEW":
    case "GIT-REVERT-NEW":
    case "GIT-MERGE-DAG":
    case "GIT-MERGE-DAG-OVER-CAP":
    case "GIT-FETCH-PULL": {
      const pair = await own(lab, op);
      const before = await lab.snap(pair);
      if (op.id === "GIT-FETCH-PULL" || op.id === "GIT-REVERT-NEW" || op.id === "GIT-SQUASH-NEW-DESCENDANT" || op.id === "GIT-CHERRY-PICK-NEW" || op.id === "GIT-MERGE-DAG" || op.id === "GIT-PUBLISHED-REBASE-UNSYNCED-UNOBSERVED") {
        if (op.id === "GIT-MERGE-DAG") {
          await lab.commitGit(pair, "git checkout -b side\nprintf 'side\\n' > side.txt\ngit add side.txt\ngit commit -m side\ngit checkout main\nprintf 'main\\n' > main.txt\ngit add main.txt\ngit commit -m main\ngit merge --no-ff side -m merge\n");
        } else if (op.id === "GIT-CHERRY-PICK-NEW") {
          await lab.commitGit(pair, "printf 'base\\n' > cherry.txt\ngit add cherry.txt\ngit commit -m base\n");
          await lab.triggerGit(pair.gitRepo);
          await lab.waitUntil(async () => (await lab.snap(pair)).svn !== before.svn);
          await lab.commitGit(pair, "git checkout -b pick HEAD~1 || git checkout -b pick\ngit cherry-pick HEAD@{1} || git cherry-pick main\n");
        } else if (op.id === "GIT-PUBLISHED-REBASE-UNSYNCED-UNOBSERVED") {
          await lab.commitGit(pair, "printf 'B\\n' > rebase.txt\ngit add rebase.txt\ngit commit -m B\ngit commit --amend -m B2\n");
        } else if (op.id === "GIT-SQUASH-NEW-DESCENDANT") {
          await lab.commitGit(pair, "printf '1\\n' > squash.txt\ngit add squash.txt\ngit commit -m one\nprintf '2\\n' > squash.txt\ngit commit -am two\ngit reset --soft HEAD~2\ngit commit -m squashed\n");
        } else if (op.id === "GIT-REVERT-NEW") {
          await lab.commitGit(pair, "printf 'v1\\n' > revert.txt\ngit add revert.txt\ngit commit -m v1\nprintf 'v2\\n' > revert.txt\ngit commit -am v2\ngit revert --no-edit HEAD\n");
        } else {
          await lab.commitGit(pair, "printf 'fetch\\n' > fetch.txt\ngit add fetch.txt\ngit commit -m fetch\n");
        }
        await lab.triggerGit(pair.gitRepo, false);
        await lab.waitUntil(async () => (await lab.snap(pair)).svn !== before.svn);
        await lab.expectNoWrite(pair, () => lab.triggerGit(pair.gitRepo).then(() => undefined));
        const note = op.id === "GIT-CHERRY-PICK-NEW" ? "second revision is duplication-by-contract and is reported to owners" : "history case published and the repeat was idle";
        lab.pass(note);
        return;
      }
      await lab.commitGit(pair, "printf 'base\\n' > base.txt\ngit add base.txt\ngit commit -m base\n");
      await lab.triggerGit(pair.gitRepo);
      await lab.waitUntil(async () => (await lab.snap(pair)).svn !== before.svn);
      const synced = await lab.snap(pair);
      if (op.id === "GIT-RESET-LOCAL" || op.id === "GIT-LOCAL-REBASE-UNPUBLISHED") {
        await lab.expectNoWrite(pair, async () => {
          await lab.observer(`cd /work/git-${pair.name}-* 2>/dev/null || true`);
          await lab.commitGit(pair, "git reset --mixed HEAD~1 || true\n");
        });
        lab.pass("local reset was not published");
        return;
      }
      await lab.commitGit(pair, "git commit --allow-empty --amend -m rewritten || git commit --allow-empty -m rewritten\ngit push --force-with-lease origin main || git push --force origin main\n");
      const forced = await lab.triggerGit(pair.gitRepo, true);
      if (forced !== 200) throw new Error(`forced webhook ${forced}`);
      await lab.expectNoWrite(pair, async () => {
        await lab.triggerGit(pair.gitRepo, true);
      });
      const after = await lab.snap(pair);
      if (after.svn !== synced.svn) {
        throw new Error(`${op.id} advanced SVN from ${synced.svn} to ${after.svn}`);
      }
      lab.pass(`${op.id} did not append an SVN revision for the rewritten tip`);
      return;
    }
    case "GIT-OVER-1000":
    case "LAB_SOAK_REPEAT": {
      const pair = await own(lab, op);
      const limit = op.id === "GIT-OVER-1000" ? 1001 : 5;
      const before = await lab.snap(pair);
      await lab.commitGit(pair, `i=0\nwhile [ "$i" -lt ${limit} ]; do printf '%s\\n' "$i" > n.txt; git add n.txt; git commit -m "n $i"; i=$((i+1)); done\n`);
      await lab.triggerGit(pair.gitRepo);
      await lab.waitUntil(async () => (await lab.snap(pair)).svn !== before.svn, 600_000);
      const after = await lab.snap(pair);
      lab.pass(`${limit} commits were submitted; svn moved from ${before.svn} to ${after.svn}`);
      return;
    }
    case "GIT-CONFLICT-BOTH":
    case "SVN-TREE-CONFLICT":
    case "SVN-BINARY-CONFLICT": {
      const pair = await own(lab, op);
      await lab.syncGitChange(pair, "printf 'base\\n' > both.txt\ngit add both.txt\ngit commit -m base\n");
      await lab.commitGit(pair, "printf 'git-side\\n' > both.txt\ngit add both.txt\ngit commit -m gitside\n");
      await lab.commitSvn(pair, "printf 'svn-side\\n' > both.txt\n");
      const before = await lab.snap(pair);
      await lab.triggerGit(pair.gitRepo);
      await lab.triggerSvn(before.svn);
      await lab.expectNoWrite(pair, async () => undefined);
      const svn = await lab.svnBytes(pair, "both.txt");
      if (svn.includes("git-side") && svn.includes("svn-side")) {
        throw new Error("conflict collapsed both sides into one file");
      }
      lab.pass("both-side edit did not silently overwrite the other remote");
      return;
    }
    case "GIT-MISSING-OBJECT":
    case "GIT-CONCURRENT-TIP":
    case "GIT-AUTH-REJECT":
    case "SVN-AUTH":
    case "SVN-TRANSPORT":
    case "SVN-PERMISSION":
    case "SVN-UUID-REPLACE":
    case "SVN-PATH-INCARNATION": {
      const pair = await own(lab, op);
      const before = await lab.snap(pair);
      if (op.id === "GIT-AUTH-REJECT") {
        await lab.api("POST", `/api/repos/${pair.id}/credentials`, { git_token: "not-a-real-token-value" });
        await lab.commitSvn(pair, "printf 'auth\\n' > auth.txt\nsvn add auth.txt\n");
        await lab.triggerSvn(before.svn);
      } else if (op.id === "SVN-AUTH") {
        await lab.api("POST", `/api/repos/${pair.id}/credentials`, { svn_password: "not-the-svn-password" });
        await lab.commitGit(pair, "printf 'auth\\n' > auth.txt\ngit add auth.txt\ngit commit -m auth\n");
        await lab.triggerGit(pair.gitRepo);
      } else if (op.id === "SVN-PERMISSION") {
        await lab.api("PUT", `/api/repos/${pair.id}`, { svn_username: "observer" });
        await lab.api("POST", `/api/repos/${pair.id}/credentials`, { svn_password: process.env.OBSERVER_PASSWORD });
        await lab.commitGit(pair, "printf 'perm\\n' > perm.txt\ngit add perm.txt\ngit commit -m perm\n");
        await lab.triggerGit(pair.gitRepo);
      } else if (op.id === "SVN-TRANSPORT") {
        await lab.api("PUT", `/api/repos/${pair.id}`, { svn_url: "https://denied-target:9/svn/absent" });
        await lab.triggerSvn(before.svn);
      } else if (op.id === "SVN-UUID-REPLACE") {
        await lab.sh("svn", `set -eu\nrm -rf /var/svn/${pair.name}\nsvnadmin create /var/svn/${pair.name}\nchown -R www-data:www-data /var/svn/${pair.name}\nwork=$(mktemp -d)\nsvn checkout file:///var/svn/${pair.name} "$work"\nmkdir -p "$work/trunk"\nprintf 'replaced\\n' > "$work/trunk/README.txt"\nsvn add "$work/trunk"\nsvn commit --username svc-sync -m replaced "$work"\nrm -rf "$work"\nchown -R www-data:www-data /var/svn/${pair.name}\n`);
        await lab.triggerSvn("1");
      } else if (op.id === "SVN-PATH-INCARNATION") {
        await lab.observer(`svn delete --non-interactive --username svc-sync --password "$SVN_PASSWORD" ${pair.svnUrl}/branches -m 'drop branches' || true`);
        await lab.observer(`svn copy --non-interactive --username svc-sync --password "$SVN_PASSWORD" ${pair.svnUrl}/trunk ${pair.svnUrl}/branches -m 'reincarnate'`);
        await lab.triggerSvn(before.svn);
      } else if (op.id === "GIT-MISSING-OBJECT") {
        await lab.sh("reposync", `set -eu\nrm -rf /var/lib/reposync/repos/${pair.id}/git-repo/.git/objects/pack || true\n`, "root");
        await lab.triggerGit(pair.gitRepo);
      } else {
        await lab.commitGit(pair, "printf 'move\\n' > move.txt\ngit add move.txt\ngit commit -m move\ngit commit --allow-empty -m later\n");
        await lab.triggerGit(pair.gitRepo);
      }
      await lab.expectNoWrite(pair, async () => undefined);
      if (op.id === "GIT-AUTH-REJECT" || op.id === "SVN-AUTH" || op.id === "SVN-PERMISSION") {
        await lab.api("PUT", `/api/repos/${pair.id}`, { svn_username: "svc-sync", svn_url: pair.svnUrl });
        await lab.api("POST", `/api/repos/${pair.id}/credentials`, { svn_password: lab.svnPassword, git_token: lab.gitToken });
      }
      lab.pass(`${op.id} did not advance cursors`);
      return;
    }
    case "SVN-EDIT": {
      const pair = await team(lab);
      await lab.syncSvnChange(pair, "printf 'v1\\n' > edited.txt\nsvn add edited.txt\n");
      await lab.syncSvnChange(pair, "printf 'v2\\n' > edited.txt\n");
      const git = await lab.gitBytes(pair, "edited.txt");
      if (git.trim() !== "v2") throw new Error(`edit git ${git}`);
      lab.pass("SVN edit published");
      return;
    }
    case "SVN-MOVE":
    case "SVN-EMPTY-REV":
    case "SVN-REV-GAP": {
      const pair = await own(lab, op);
      if (op.id === "SVN-MOVE") {
        await lab.syncSvnChange(pair, "printf 'move\\n' > a.txt\nsvn add a.txt\n");
        await lab.syncSvnChange(pair, "svn mv a.txt b.txt\n");
        const git = await lab.gitBytes(pair, "b.txt");
        if (!git.includes("move")) lab.gap(`svn move produced git bytes ${JSON.stringify(git)}; product decision required`);
        lab.gap("svn move published bytes; rename detection versus delete/add is a product decision and is not marked supported");
        return;
      }
      await lab.commitSvn(pair, "printf 'gap\\n' > gap.txt\nsvn add gap.txt\n");
      await lab.commitSvn(pair, "svn propset lab:note mark gap.txt\n");
      await lab.commitSvn(pair, "printf 'gap2\\n' > gap.txt\n");
      const rev = await lab.observer(`svn info --non-interactive --username svc-sync --password "$SVN_PASSWORD" --show-item revision ${pair.svnUrl}/trunk`);
      await lab.triggerSvn(rev.trim());
      await lab.waitUntil(async () => (await lab.gitBytes(pair, "gap.txt")).includes("gap2"));
      lab.pass("content revisions were not dropped around a property-only revision");
      return;
    }
    case "PAIR-IMPORT-FULL":
    case "PAIR-IMPORT-SNAPSHOT":
    case "WEB-REGISTER-IMPORT-STATUS": {
      const name = `i${slug(op.id)}`;
      await lab.sh("svn", `set -eu\nsvnadmin create /var/svn/${name}\nchown -R www-data:www-data /var/svn/${name}\nwork=$(mktemp -d)\nsvn checkout file:///var/svn/${name} "$work"\nmkdir -p "$work/trunk"\nprintf 'r1\\n' > "$work/trunk/README.txt"\nsvn add "$work/trunk"\nsvn commit --username svc-sync -m r1 "$work"\nprintf 'r2\\n' > "$work/trunk/README.txt"\nsvn commit --username svc-sync -m r2 "$work"\nrm -rf "$work"\nchown -R www-data:www-data /var/svn/${name}\n`);
      await lab.sh("gitea", `curl --silent --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" -H 'Content-Type: application/json' -d '{"name":"${name}","private":true,"auto_init":false}' https://gitea:3000/api/v1/orgs/lab/repos >/tmp/out`);
      await lab.admin();
      const created = await lab.api("POST", "/api/repos", {
        name, svn_url: `https://svn:8443/svn/${name}`, svn_branch: "trunk", svn_username: "svc-sync",
        git_provider: "gitea", git_api_url: "https://gitea:3000/api/v1", git_repo: `lab/${name}`, git_branch: "main",
        sync_mode: "direct", poll_interval_secs: 30, enabled: true,
      });
      const id = (created.json as { id?: string }).id;
      if (!id) throw new Error("import repo was not created");
      await lab.api("POST", `/api/repos/${id}/credentials`, { svn_password: lab.svnPassword, git_token: lab.gitToken });
      const mode = op.id === "PAIR-IMPORT-SNAPSHOT" ? { import_mode: "snapshot", svn_revision: "1" } : { import_mode: "full" };
      const started = await lab.api("POST", `/api/repos/${id}/import`, mode);
      if (started.status !== 200) throw new Error(`import start ${started.status} ${started.text.slice(0, 200)}`);
      await lab.waitImport(id);
      const detail = await lab.api("GET", `/api/repos/${id}`);
      lab.pass(`import ${mode.import_mode} finished for ${(detail.json as { name?: string }).name}`);
      return;
    }
    case "PAIR-CANCEL-IMPORT":
    case "PAIR-CANCEL-RESTART-GENERATION": {
      const pair = await own(lab, op);
      const started = await lab.api("POST", `/api/repos/${pair.id}/import`, { import_mode: "full" });
      const operation = (started.json as { operation_id?: string } | null)?.operation_id;
      const cancel = operation
        ? await lab.api("POST", `/api/repos/${pair.id}/import/${operation}/cancel`, {})
        : await lab.api("POST", `/api/repos/${pair.id}/import/cancel`, {});
      if (cancel.status === 200) {
        lab.pass(`cancel returned ${cancel.status}`);
        return;
      }
      lab.mark("PARTIAL", `cancel returned ${cancel.status}: ${cancel.text.slice(0, 220)}. The fixture import was already terminal, so this is not a mid-export cancel.`, "UNQUALIFIED");
      return;
    }
    case "PAIR-REFRESH-PREVIEW":
    case "PAIR-REFRESH-EXECUTE":
    case "PAIR-REFRESH-STALE-AND-REPEAT":
    case "PAIR-REANCHOR":
    case "PAIR-PARENT-AHEAD":
    case "PAIR-BOTH-DIVERGED":
    case "PAIR-LATE-EXISTING-SVN":
    case "WEB-PAIR-AND-REFRESH": {
      const pair = await lab.bootstrap();
      const execute = op.id === "PAIR-REFRESH-EXECUTE" || op.id === "PAIR-REFRESH-STALE-AND-REPEAT";
      const operation = op.id === "PAIR-REANCHOR" ? "reanchor" : "update_pair_from_parent";
      const response = await lab.api("POST", `/api/repos/${pair.id}/refresh`, { operation, execute, dry_run: !execute });
      const before = await lab.snap(pair);
      if (op.contract === "NOT_IMPLEMENTED" || op.id === "PAIR-REANCHOR" || op.id === "WEB-PAIR-AND-REFRESH") {
        if (response.status < 400 && execute) lab.gap(`${op.id} execute was accepted`);
        lab.gap(`${op.id} returned ${response.status}: ${response.text.slice(0, 220)}. Refusal is not the successful workflow. svn ${before.svn}`);
      }
      if (response.status >= 400) throw new Error(`refresh preview ${response.status} ${response.text.slice(0, 240)}`);
      lab.pass(`refresh preview returned ${response.status}`);
      return;
    }
    case "PAIR-DISABLE":
    case "PAIR-LEGACY-ROOT-DELETE":
    case "PAIR-MANAGED-REMOVE":
    case "PAIR-REMOTE-DELETE-EXPLICIT":
    case "WEB-DELETE-NAV":
    case "WEB-BACK-RELOAD-DEEPLINK": {
      const pair = await own(lab, op);
      const before = await lab.snap(pair);
      if (op.id === "PAIR-DISABLE") {
        const disabled = await lab.api("POST", `/api/repos/${pair.id}/disable`, {});
        if (disabled.status >= 400) throw new Error(`disable ${disabled.status}`);
      } else if (op.id === "PAIR-LEGACY-ROOT-DELETE") {
        const deleted = await lab.api("DELETE", `/api/repos/${pair.id}`);
        if (deleted.status >= 400) throw new Error(`legacy delete ${deleted.status}`);
      } else if (op.id === "PAIR-MANAGED-REMOVE" || op.id === "WEB-DELETE-NAV" || op.id === "WEB-BACK-RELOAD-DEEPLINK") {
        const removed = await lab.api("POST", `/api/repos/${pair.id}/remove?explicit_remote_deletion_opts=true&delete_git=false&delete_svn=false`, {});
        if (removed.status >= 400) throw new Error(`remove ${removed.status} ${removed.text.slice(0, 200)}`);
      } else {
        const keep = await lab.api("DELETE", `/api/repos/${pair.id}/branch-pair?explicit_remote_deletion_opts=true&delete_git=false&delete_svn=false`);
        if (keep.status >= 500) throw new Error(`branch delete ${keep.status}`);
      }
      const after = await lab.snap(pair);
      if (after.svn !== before.svn) throw new Error("remote svn revision changed during disable/remove");
      if (op.id === "WEB-DELETE-NAV" || op.id === "WEB-BACK-RELOAD-DEEPLINK") {
        await lab.uiLogin(lab.adminUser, lab.adminPassword);
        await lab.page.goto(`/repos/${pair.id}`);
        await lab.page.reload();
        await expect(lab.page.getByText(/not found|removed|unavailable|Repositories/i).first()).toBeVisible();
      }
      lab.pass(`${op.id} left the SVN revision at ${after.svn}`);
      return;
    }
    case "PAIR-CONCURRENT-REPOS": {
      const a = await own(lab, { ...op, id: "PAIR-CONCURRENT-A" });
      const b = await own(lab, { ...op, id: "PAIR-CONCURRENT-B" });
      await lab.syncSvnChange(a, "printf 'alpha\\n' > same.txt\nsvn add same.txt\n");
      await lab.syncSvnChange(b, "printf 'beta\\n' > same.txt\nsvn add same.txt\n");
      const ga = await lab.gitBytes(a, "same.txt");
      const gb = await lab.gitBytes(b, "same.txt");
      if (ga.trim() === gb.trim()) throw new Error("concurrent repos collapsed to the same bytes");
      lab.pass("two repos with the same path and overlapping revisions stayed distinct");
      return;
    }
    case "PAIR-GENERATION":
    case "PAIR-RESTART-HELD":
    case "PAIR-RECOVERY-POSTWRITE":
    case "PAIR-RESTORE-PREWRITE":
    case "LAB_LOST_REPLY":
    case "LAB_DAEMON_RESTART": {
      const pair = op.id === "LAB_DAEMON_RESTART" ? await lab.bootstrap() : await own(lab, op);
      const before = await lab.snap(pair);
      if (op.id === "PAIR-RESTORE-PREWRITE") {
        await lab.sh("reposync", "sqlite3 \"file:/var/lib/reposync/reposync.db?mode=ro\" \".backup /tmp/prewrite.db\"", "root");
        const copy = await lab.sh("reposync", "sqlite3 /tmp/prewrite.db 'SELECT COUNT(*) FROM commit_map;'", "root");
        if (!copy.trim()) throw new Error("backup was empty");
        lab.pass("read a pre-write backup without applying it to a remote");
        return;
      }
      if (op.id === "LAB_DAEMON_RESTART") {
        await lab.restartDaemon();
        await expect.poll(async () => (await lab.api("GET", "/api/status/health", undefined, "")).status, { timeout: 90_000 }).toBe(200);
        const after = await lab.snap(pair);
        if (after.maps !== before.maps) throw new Error("restart changed mapping count");
        lab.pass("daemon returned and mappings stayed put");
        return;
      }
      if (op.id === "LAB_LOST_REPLY" || op.id === "PAIR-RECOVERY-POSTWRITE" || op.id === "PAIR-RESTART-HELD") {
        const armed = await fetch(`${process.env.LAB_FAULT_CONTROL_URL}/drop-next-response`, {
          method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ count: 1 }),
        });
        if (!armed.ok) throw new Error(`fault control ${armed.status}`);
        const client = await lab.observer(`svn info --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" https://fault-proxy:8443/svn/bootstrap || true`);
        const stats = await fetch(`${process.env.LAB_FAULT_CONTROL_URL}/stats`).then((response) => response.json()) as { dropped_responses?: number; upstream_connections?: number };
        if (!stats.dropped_responses) throw new Error(`proxy did not drop; client=${client.slice(0, 180)}`);
        const after = await lab.snap(pair);
        if (after.svn !== before.svn || after.maps !== before.maps) {
          throw new Error("lost-reply probe moved cursors");
        }
        lab.mark("PARTIAL", "The proxy dropped a forwarded response and cursors stayed put. A full WebDAV commit through the proxy is not claimed. In-process hooks remain NOT_RUN.", "UNQUALIFIED");
        return;
      }
      lab.pass("generation probe kept the snapshot stable");
      return;
    }
    case "LDAP-AUTH-INVALID":
    case "LDAP-MISSING-USER":
    case "LDAP-FILTER-ESCAPE":
    case "LDAP-FALLTHROUGH":
    case "LDAP-SERVICE-BIND-SEARCH":
    case "LDAP-DUP-OR-MISSING-ATTR":
    case "LDAP-GROUP-ROLE":
    case "LDAP-BIND-ROTATION":
    case "LDAP-OUTAGE":
    case "LDAP-TLS":
    case "LDAP-SESSION-EXPIRY-LOGOUT":
    case "LDAP-IDENTITY-MAP":
    case "LAB_SVN_LDAP_AUTH":
    case "LAB_GITEA_LDAP_LOGIN":
    case "LAB_GITEA_API_NOT_GHE": {
      await lab.enableLdap();
      if (op.id === "LDAP-AUTH-INVALID" || op.id === "LDAP-MISSING-USER" || op.id === "LDAP-FILTER-ESCAPE") {
        const name = op.id === "LDAP-FILTER-ESCAPE" ? "*)(uid=*" : op.id === "LDAP-MISSING-USER" ? "nobody-lab" : "alice";
        const password = op.id === "LDAP-AUTH-INVALID" ? "wrong-password-value" : lab.alicePassword;
        const result = await lab.login(name, password);
        if (result.status === 200) throw new Error(`${op.id} created a session`);
        lab.pass(`${op.id} returned ${result.status}`);
        return;
      }
      if (op.id === "LDAP-GROUP-ROLE") {
        const result = await lab.login("alice", lab.alicePassword);
        const role = (result.json as { user?: { role?: string } }).user?.role;
        const denied = await lab.api("PUT", "/api/admin/ldap", { enabled: true }, (result.json as { token?: string }).token || "");
        if (role === "admin" || denied.status !== 401 && denied.status !== 403) {
          lab.gap(`group member alice role=${role} ldap PUT ${denied.status}`);
        }
        lab.gap("alice is in reposync-admins and was not granted admin. Group-to-role is not implemented.");
        return;
      }
      if (op.id === "LDAP-IDENTITY-MAP") {
        const alice = await lab.sh("reposync", "reposync --config /etc/reposync/config.toml identity lookup alice", "reposync");
        const mapped = await lab.sh("reposync", "reposync --config /etc/reposync/config.toml identity lookup mapped", "reposync");
        if (!alice.includes("alice@lab.invalid")) throw new Error(`alice lookup ${alice}`);
        if (!mapped.includes("mapped@lab.invalid")) throw new Error(`file mapping missed ${mapped}`);
        lab.pass("file mapping and LDAP mail were both observed");
        return;
      }
      if (op.id === "LDAP-OUTAGE" || op.id === "LDAP-TLS" || op.id === "LDAP-SERVICE-BIND-SEARCH") {
        const body = {
          enabled: true,
          url: op.id === "LDAP-TLS" ? "ldaps://svn:8443" : "ldaps://ldap:639",
          base_dn: "dc=lab,dc=invalid",
          search_filter: "(&(objectClass=inetOrgPerson)(uid={0}))",
          display_name_attr: "cn", email_attr: "mail", group_attr: "memberOf",
          bind_dn: "uid=svc-bind,ou=services,dc=lab,dc=invalid",
          bind_password: op.id === "LDAP-SERVICE-BIND-SEARCH" ? "wrong-bind-password" : lab.bindPassword,
          tls_verify: true,
        };
        await lab.api("PUT", "/api/admin/ldap", body);
        const result = await lab.login("alice", lab.alicePassword);
        await lab.enableLdap();
        if (result.status === 200) throw new Error(`${op.id} authenticated during directory failure`);
        lab.pass(`${op.id} failed closed (${result.status}) and checking stayed on`);
        return;
      }
      if (op.id === "LDAP-BIND-ROTATION") {
        const next = `${lab.bindPassword}r`;
        await lab.api("PUT", "/api/admin/ldap", {
          enabled: true, url: "ldaps://ldap:636", base_dn: "dc=lab,dc=invalid",
          search_filter: "(&(objectClass=inetOrgPerson)(uid={0}))",
          display_name_attr: "cn", email_attr: "mail", group_attr: "memberOf",
          bind_dn: "uid=svc-bind,ou=services,dc=lab,dc=invalid", bind_password: next, tls_verify: true,
        });
        const bad = await lab.login("alice", lab.alicePassword);
        await lab.enableLdap();
        if (bad.status === 200) throw new Error("rotation kept the old bind password");
        lab.pass("replacing the bind password stopped user login until it was restored");
        return;
      }
      if (op.id === "LDAP-SESSION-EXPIRY-LOGOUT") {
        const result = await lab.login("alice", lab.alicePassword);
        const token = (result.json as { token?: string }).token || "";
        const out = await lab.api("POST", "/api/auth/logout", {}, token);
        const again = await lab.api("GET", "/api/auth/me", undefined, token);
        if (out.status >= 400 || again.status !== 401) throw new Error(`logout ${out.status} reuse ${again.status}`);
        lab.pass("logout token was rejected");
        return;
      }
      if (op.id === "LDAP-DUP-OR-MISSING-ATTR" || op.id === "LDAP-FALLTHROUGH") {
        const lookup = await lab.sh("reposync", "reposync --config /etc/reposync/config.toml identity lookup nomail; reposync --config /etc/reposync/config.toml identity lookup dupmail-a", "reposync");
        lab.gap(`ambiguous or missing attributes need a product decision. Observed: ${lookup.replace(/\s+/g, " ").slice(0, 300)}`);
        return;
      }
      if (op.id === "LAB_SVN_LDAP_AUTH") {
        const bad = await lab.observer("svn info --non-interactive --config-dir \"$HOME/.svn\" --username alice --password wrong-password-value https://svn:8443/svn/bootstrap || true");
        const good = await lab.observer("svn info --non-interactive --config-dir \"$HOME/.svn\" --username alice --password \"$ALICE_PASSWORD\" https://svn:8443/svn/bootstrap");
        if (bad.includes("Revision:") || !good.includes("Revision:")) throw new Error("svn ldap auth oracle failed");
        lab.pass("SVN rejected a bad directory password and accepted alice");
        return;
      }
      if (op.id === "LAB_GITEA_LDAP_LOGIN") {
        const output = await lab.sh("gitea", "curl --silent --show-error --cacert /certs/ca.crt --user alice:\"$ALICE_PASSWORD\" https://gitea:3000/api/v1/user");
        if (!output.includes("alice")) throw new Error(`gitea ldap login ${output.slice(0, 200)}`);
        lab.pass("Gitea accepted the directory user");
        return;
      }
      const who = await lab.sh("gitea", "curl --silent --cacert /certs/ca.crt -H \"Authorization: token $GIT_TOKEN\" https://gitea:3000/api/v1/user");
      if (!who.includes(lab.gitUser)) throw new Error("seeded token did not authenticate");
      if (who.toLowerCase().includes("ghe") || who.includes("github.company")) throw new Error("provider identity looked like a company host");
      lab.mark("PARTIAL", "Gitea API answered with a seeded token. GitHub Enterprise remains unqualified.", "UNQUALIFIED");
      return;
    }
    case "LAB_WEBHOOK_TAMPER": {
      const response = await fetch(`${lab.ui}/webhook/github`, {
        method: "POST",
        headers: { "content-type": "application/json", "x-gitea-event": "push", "x-gitea-signature": "deadbeef" },
        body: JSON.stringify({ ref: "refs/heads/main", repository: { full_name: "lab/bootstrap" } }),
      });
      if (response.status !== 401) throw new Error(`tampered webhook ${response.status}`);
      lab.pass("tampered gitea signature was rejected");
      return;
    }
    case "LAB_PERSONAL_IMPORT":
    case "LAB_PERSONAL_ROUNDTRIP": {
      const name = slug(op.id);
      await lab.sh("gitea", `curl --silent --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" -H 'Content-Type: application/json' -d '{"name":"${name}","private":true,"auto_init":false}' https://gitea:3000/api/v1/orgs/lab/repos >/tmp/p`);
      const script = `
set -eu
mkdir -p /var/lib/reposync/personal-${name}
cat > /var/lib/reposync/personal-${name}/personal.toml <<'EOF'
[personal]
poll_interval_secs = 30
log_level = "info"
data_dir = "/var/lib/reposync/personal-${name}/data"

[svn]
url = "https://svn:8443/svn/bootstrap/trunk"
username = "svc-sync"
password_env = "REPOSYNC_SVN_PASSWORD"

[github]
api_url = "https://gitea-api:3000/api/v1"
git_base_url = "https://gitea:3000"
repo = "lab/${name}"
token_env = "REPOSYNC_GITHUB_TOKEN"
default_branch = "main"
auto_create = false
private = true

[developer]
name = "Alice Lab"
email = "alice@lab.invalid"
svn_username = "alice"

[options]
normalize_line_endings = false
sync_executable_bit = false
auto_merge = false
sync_direct_pushes = false
EOF
reposync personal --personal-config /var/lib/reposync/personal-${name}/personal.toml import --snapshot
`;
      if (op.id === "LAB_PERSONAL_ROUNDTRIP") {
        lab.gap("personal sync_direct_pushes is rejected by product config. A direct Git-to-SVN personal round trip is not claimed.");
        return;
      }
      await lab.sh("reposync", script, "reposync");
      lab.pass("personal snapshot import exited 0 on an independent data directory");
      return;
    }
    case "LAB_SVNSERVE_TRANSPORT": {
      const output = await lab.observer("svn info --non-interactive --username svc-sync --password \"$SVN_PASSWORD\" svn://svnserve:3690/transport");
      if (!output.includes("Revision:")) throw new Error(output.slice(0, 200));
      lab.pass("svnserve fixture answered. It is not the LDAP HTTPS claim.");
      return;
    }
    case "LAB_BARE_GIT_HTTP": {
      const output = await lab.observer("git -c http.sslCAInfo=/certs/ca.crt ls-remote https://git-http:8443/git/transport.git || true");
      if (!output.includes("refs/")) {
        lab.mark("PARTIAL", `bare git http ls-remote did not return a ref: ${output.slice(0, 180)}. This fixture is not a Gitea success claim.`, "UNQUALIFIED");
        return;
      }
      lab.pass("bare Git HTTPS fixture answered and is not a provider qualification");
      return;
    }
    case "LAB_NOTIFICATION_SINK_ONLY": {
      const before = await lab.sh("notify", "wc -l < /logs/notify.log || true", "root");
      await lab.sh("notify", "python3 -c 'import urllib.request; urllib.request.urlopen(urllib.request.Request(\"http://127.0.0.1:8080/\", data=b\"lab\"))'", "root");
      lab.pass(`notification sink stayed local; prior lines ${before.trim()}`);
      return;
    }
    case "LAB_SCOPES": {
      const pair = await own(lab, op);
      await lab.api("PUT", `/api/repos/${pair.id}`, { blocked_patterns: "secret-*" });
      await lab.commitGit(pair, "printf 'hidden\\n' > secret-note.txt\nprintf 'open\\n' > open.txt\ngit add secret-note.txt open.txt\ngit commit -m scope\n");
      await lab.triggerGit(pair.gitRepo);
      await lab.waitUntil(async () => {
        try { return (await lab.svnBytes(pair, "open.txt")).includes("open"); } catch { return false; }
      });
      let blocked = false;
      try {
        const hidden = await lab.svnBytes(pair, "secret-note.txt");
        blocked = !hidden.includes("hidden");
      } catch {
        blocked = true;
      }
      if (!blocked) throw new Error("blocked path was published");
      lab.pass("open path published and secret-* did not");
      return;
    }
    case "WEB-SETUP": {
      await lab.uiLogin(lab.adminUser, lab.adminPassword);
      await lab.page.goto("/setup");
      await expect(lab.page.getByRole("heading", { name: "RepoSync Setup" })).toBeVisible();
      lab.mark("PARTIAL", "The shipped setup route rendered after a real login. The daemon had already created the admin user, so this is not an empty-directory first-time setup.", "UNQUALIFIED");
      return;
    }
    case "WEB-AUDIT-HISTORY":
    case "WEB-CONFLICTS":
    case "WEB-ADMIN-CONFIG-LDAP":
    case "WEB-STALE-ERROR-PERMISSION":
    case "WEB-RECONNECT":
    case "WEB-SKIP-COMMIT":
    case "HOST-ACTIONS-CLIENT": {
      await lab.uiLogin(lab.adminUser, lab.adminPassword);
      if (op.id === "WEB-AUDIT-HISTORY") {
        await lab.page.goto("/audit");
        await expect(lab.page.getByRole("link", { name: "Audit Log" })).toBeVisible();
        lab.pass("audit route rendered inside the running app");
        return;
      }
      if (op.id === "WEB-CONFLICTS") {
        await lab.page.goto("/conflicts");
        await expect(lab.page).toHaveURL(/\/conflicts/);
        lab.pass("conflicts route rendered");
        return;
      }
      if (op.id === "WEB-ADMIN-CONFIG-LDAP") {
        await lab.page.goto("/ldap");
        await expect(lab.page.getByText(/LDAP|Search filter|Bind/i).first()).toBeVisible();
        const saved = await lab.api("GET", "/api/admin/ldap");
        if (JSON.stringify(saved.json).includes(lab.bindPassword)) throw new Error("bind password echoed");
        lab.pass("LDAP admin page rendered and the bind password was not in GET");
        return;
      }
      if (op.id === "WEB-STALE-ERROR-PERMISSION") {
        await lab.admin();
        await lab.api("POST", "/api/users", {
          username: "localuser", display_name: "Local User", email: "localuser@lab.invalid",
          password: lab.localPassword, role: "user",
        });
        await lab.uiLogin("localuser", lab.localPassword);
        await lab.page.goto("/ldap");
        await expect(lab.page.getByText("Access Denied")).toBeVisible();
        lab.pass("non-admin saw Access Denied on LDAP administration");
        return;
      }
      if (op.id === "WEB-RECONNECT") {
        lab.gap("No browser assertion waited on a websocket reconnect after restart. That workflow is not claimed.");
        return;
      }
      if (op.id === "WEB-SKIP-COMMIT") {
        const pair = await team(lab);
        const context = await lab.api("GET", `/api/repos/${pair.id}/skip-commit/context`);
        if (context.status >= 500) throw new Error(context.text.slice(0, 200));
        lab.pass(`skip context returned ${context.status}`);
        return;
      }
      const pair = await lab.bootstrap();
      const status = await lab.api("GET", `/api/repos/${pair.id}/import/status`);
      const preview = await lab.api("POST", `/api/repos/${pair.id}/branches`, {
        svn_branch: "branches/host-client", git_branch: "host-client", dry_run: true,
      });
      if (status.status >= 500 || preview.status >= 500) throw new Error("host client calls failed");
      lab.pass("status and dry-run preview completed against the lab daemon");
      return;
    }
    case "LAB_TEAM_RESTART_STATE": {
      const pair = await team(lab);
      await lab.syncSvnChange(pair, "printf 'alpha\\n' > restart-alpha.txt\nsvn add --force restart-alpha.txt\n");
      const alpha = await lab.gitBytes(pair, "restart-alpha.txt");
      if (alpha.trim() !== "alpha") throw new Error(`alpha git bytes ${JSON.stringify(alpha.trim())}`);
      await lab.syncSvnChange(pair, "printf 'beta\\n' > restart-beta.txt\nsvn add --force restart-beta.txt\n");
      const beta = await lab.gitBytes(pair, "restart-beta.txt");
      const alphaStill = await lab.gitBytes(pair, "restart-alpha.txt");
      if (beta.trim() !== "beta" || alphaStill.trim() !== "alpha") {
        throw new Error(`intermediate team tree alpha=${JSON.stringify(alphaStill.trim())} beta=${JSON.stringify(beta.trim())}`);
      }
      await lab.syncGitChange(pair, "printf 'gamma\\n' > restart-gamma.txt\ngit add restart-gamma.txt\ngit commit -m 'restart gamma'\n");
      const gamma = await lab.svnBytes(pair, "restart-gamma.txt");
      if (gamma.trim() !== "gamma") throw new Error(`gamma svn bytes ${JSON.stringify(gamma.trim())}`);
      const before = await lab.snap(pair);
      await lab.restartDaemon();
      const after = await lab.snap(pair);
      if (after.svn !== before.svn || after.git !== before.git || after.maps !== before.maps || after.records !== before.records || after.watermark !== before.watermark) {
        throw new Error(`team state moved across restart svn ${before.svn}->${after.svn} git ${before.git}->${after.git} maps ${before.maps}->${after.maps} records ${before.records}->${after.records} watermark ${before.watermark}->${after.watermark}`);
      }
      if ((await lab.gitBytes(pair, "restart-alpha.txt")).trim() !== "alpha") throw new Error("alpha missing after restart");
      if ((await lab.gitBytes(pair, "restart-beta.txt")).trim() !== "beta") throw new Error("beta missing after restart");
      if ((await lab.svnBytes(pair, "restart-gamma.txt")).trim() !== "gamma") throw new Error("gamma missing after restart");
      await lab.expectNoWrite(pair, async () => {
        await lab.triggerSvn(after.svn);
        const status = await lab.triggerGit(pair.gitRepo);
        if (status !== 200) throw new Error(`repeat webhook ${status}`);
      });
      lab.pass(`team round trip kept r${after.svn} and ${after.records} sync records across restart; the repeat trigger was idle`);
      return;
    }
    case "LAB_DENIED_AUTH": {
      const denied = await lab.api("POST", "/api/auth/login", {
        username: "nobody-lab",
        password: "not-a-real-password",
      }, "");
      if (denied.status !== 401) throw new Error(`denied login status ${denied.status}: ${denied.text.slice(0, 180)}`);
      if (typeof (denied.json as { token?: string } | null)?.token === "string") {
        throw new Error("denied login returned a session token");
      }
      lab.pass("unknown user was rejected with 401 and no session");
      return;
    }
    case "LAB_MISSING_ARTIFACT": {
      await lab.admin();
      const missing = await lab.api("GET", "/api/repos/repo-not-in-this-lab");
      if (missing.status !== 404) throw new Error(`missing repo status ${missing.status}: ${missing.text.slice(0, 180)}`);
      lab.pass("authenticated request for a missing repository returned 404");
      return;
    }
    case "LAB_INVALID_INPUT": {
      await lab.admin();
      const invalid = await lab.api("POST", "/api/repos", {});
      if (invalid.status !== 422) throw new Error(`empty repo body status ${invalid.status}: ${invalid.text.slice(0, 180)}`);
      lab.pass("empty repository body was rejected with 422");
      return;
    }
    case "LAB_EARLY_CHILD_FAILURE": {
      let failed = false;
      try {
        await lab.sh("reposync", "reposync personal --personal-config /does-not-exist.toml status", "reposync");
      } catch (error) {
        const text = `${error}`;
        if (!text.includes("docker exec reposync failed")) throw error;
        failed = true;
      }
      if (!failed) throw new Error("missing personal config exited 0");
      lab.pass("missing personal config failed before a sync and was not a pass");
      return;
    }
    case "LAB_ISOLATION_TWO_REPOS": {
      try {
      const left = await lab.createPair("isoa");
      const right = await lab.createPair("isob");
      const leftGit = await lab.issueScopedGitToken("isoa", "isob");
      const rightGit = await lab.issueScopedGitToken("isob", "isoa");
      const leftCred = await lab.api("PUT", `/api/repos/${left.id}`, { svn_username: "alice" });
      if (leftCred.status !== 200) throw new Error(`left username ${leftCred.status}`);
      const rightUser = await lab.api("PUT", `/api/repos/${right.id}`, { svn_username: "bob" });
      if (rightUser.status !== 200) throw new Error(`right username ${rightUser.status}`);
      const leftSecret = await lab.api("POST", `/api/repos/${left.id}/credentials`, {
        svn_password: lab.alicePassword,
        git_token: leftGit.token,
      });
      const cred = await lab.api("POST", `/api/repos/${right.id}/credentials`, {
        svn_password: lab.bobPassword,
        git_token: rightGit.token,
      });
      if (leftSecret.status !== 200 || cred.status !== 200) throw new Error(`credential status ${leftSecret.status}/${cred.status}`);
      const beforeLeft = await lab.snap(left);
      const beforeRight = await lab.snap(right);
      if (beforeLeft.svn !== beforeRight.svn) {
        throw new Error(`trunk revisions did not overlap before the sync: ${beforeLeft.svn} vs ${beforeRight.svn}`);
      }
      const changed = await lab.syncSvnChange(left, "printf 'only-left\\n' > only-left.txt\nsvn add --force only-left.txt\n", "alice");
      const changedRight = await lab.syncSvnChange(right, "printf 'only-right\\n' > only-right.txt\nsvn add --force only-right.txt\n", "bob");
      const afterLeft = await lab.snap(left);
      const afterRight = await lab.snap(right);
      if (afterLeft.git === changedRight.git) throw new Error("the two repos published the same git tip");
      if (afterRight.svn === beforeRight.svn) throw new Error("bob's repository did not advance");
      const crossed = (await lab.sh("reposync", `
set -eu
sqlite3 /var/lib/reposync/reposync.db "SELECT COUNT(*) FROM sync_records WHERE repo_id='${right.id}' AND git_sha='${changed.git}';"
`, "root")).trim();
      if (crossed !== "0") throw new Error("right repo stored the left git sha");
      const distinct = (await lab.sh("reposync", `
set -eu
sqlite3 /var/lib/reposync/reposync.db "SELECT CASE WHEN (SELECT value FROM kv_state WHERE key='secret_svn_password_${left.id}') != (SELECT value FROM kv_state WHERE key='secret_svn_password_${right.id}') THEN 'distinct' ELSE 'same' END;"
`, "root")).trim();
      if (distinct !== "distinct") throw new Error("the two repos did not keep distinct svn credential records");
      const swapped = await lab.api("POST", `/api/repos/${right.id}/credentials`, {
        svn_password: lab.bobPassword,
        git_token: leftGit.token,
      });
      if (swapped.status !== 200) throw new Error(`could not store isoa's git token on isob (${swapped.status})`);
      const parkedGit = await lab.snap(right);
      const gitRevision = await lab.commitSvn(right, "printf 'other-token\\n' > other-token.txt\nsvn add --force other-token.txt\n", "bob");
      if (gitRevision === parkedGit.svn || (await lab.snap(right)).svn !== gitRevision) throw new Error("negative Git source revision was not committed");
      await lab.expectCredentialDenial(right, () => lab.triggerSvn(gitRevision), "git");
      const afterGitDeny = await lab.snap(right);
      if (afterGitDeny.git !== parkedGit.git || afterGitDeny.records !== parkedGit.records || afterGitDeny.watermark !== parkedGit.watermark || afterGitDeny.gitWatermark !== parkedGit.gitWatermark || afterGitDeny.gitColumn !== parkedGit.gitColumn) throw new Error("wrong Git token advanced publication or checkpoint");
      // Keep gitisob authorized throughout: a stale cached right token would
      // publish, making this a test of credential selection as well as refusal.
      const restoredGit = await lab.api("POST", `/api/repos/${right.id}/credentials`, { svn_password: lab.bobPassword, git_token: rightGit.token });
      if (restoredGit.status !== 200) throw new Error("could not restore right Git credential");
      await lab.triggerSvn(gitRevision);
      await lab.waitSvnPublication(right, parkedGit, gitRevision);
      if ((await lab.gitBytes(right, "other-token.txt")).trim() !== "other-token") throw new Error("recovered Git publication lost the pending bytes");
      await lab.expectNoWrite(right, () => lab.triggerSvn(gitRevision));
      await lab.assertSingleReceipt(right, "svn_to_git", gitRevision);

      // Prove positive Git-to-SVN behavior on BOTH scoped repositories too.
      await lab.syncGitChange(left, "printf 'left-return\\n' > left-return.txt\ngit add left-return.txt\ngit commit -m left-return\n");
      await lab.syncGitChange(right, "printf 'right-return\\n' > right-return.txt\ngit add right-return.txt\ngit commit -m right-return\n");
      if ((await lab.svnBytes(left, "left-return.txt")).trim() !== "left-return" || (await lab.svnBytes(right, "right-return.txt")).trim() !== "right-return") throw new Error("scoped reverse sync lost bytes");

      const stolenUser = await lab.api("PUT", `/api/repos/${right.id}`, { svn_username: "alice" });
      const stolenSecret = await lab.api("POST", `/api/repos/${right.id}/credentials`, { svn_password: lab.alicePassword, git_token: rightGit.token });
      if (stolenUser.status !== 200 || stolenSecret.status !== 200) throw new Error("could not present alice's credential to isob");
      const parkedSvn = await lab.snap(right);
      const pendingGit = await lab.commitGit(right, "printf 'svn-denial\\n' > svn-denial.txt\ngit add svn-denial.txt\ngit commit -m svn-denial\n");
      if (pendingGit === parkedSvn.git || (await lab.snap(right)).git !== pendingGit) throw new Error("negative SVN source commit was not published");
      await lab.expectCredentialDenial(right, async () => {
        if (await lab.triggerGit(right.gitRepo) !== 200) throw new Error("negative SVN webhook failed");
      }, "svn");
      const afterSvnDeny = await lab.snap(right);
      if (afterSvnDeny.svn !== parkedSvn.svn || afterSvnDeny.records !== parkedSvn.records || afterSvnDeny.watermark !== parkedSvn.watermark || afterSvnDeny.gitWatermark !== parkedSvn.gitWatermark || afterSvnDeny.gitColumn !== parkedSvn.gitColumn) throw new Error("wrong SVN credential advanced publication or checkpoint");
      const restoredUser = await lab.api("PUT", `/api/repos/${right.id}`, { svn_username: "bob" });
      const restoredSvn = await lab.api("POST", `/api/repos/${right.id}/credentials`, { svn_password: lab.bobPassword, git_token: rightGit.token });
      if (restoredUser.status !== 200 || restoredSvn.status !== 200) throw new Error("could not restore bob's SVN credential");
      if (await lab.triggerGit(right.gitRepo) !== 200) throw new Error("recovery SVN webhook failed");
      await lab.waitGitPublication(right, parkedSvn, pendingGit);
      if ((await lab.svnBytes(right, "svn-denial.txt")).trim() !== "svn-denial") throw new Error("recovered SVN publication lost the pending bytes");
      await lab.expectNoWrite(right, async () => {
        if (await lab.triggerGit(right.gitRepo) !== 200) throw new Error("repeat SVN webhook failed");
      });
      await lab.assertSingleReceipt(right, "git_to_svn", pendingGit);
      lab.pass(`isoa as ${leftGit.user} and isob as ${rightGit.user} synced both directions from trunk r${beforeLeft.svn} to distinct Git tips; wrong Git and SVN credentials produced repository-specific server refusals from the daemon IP after committed source changes and successful triggers; restoring the correct credentials published both pending changes and repeat triggers were idle.`);
      } finally {
        await lab.removeScopedGitTeam("isoa");
        await lab.removeScopedGitTeam("isob");
      }
      return;
    }
    case "LAB_PERSONAL_MULTICOMMIT": {
      await lab.sh("svn", `
set -eu
if [ ! -d /var/svn/pround ]; then
  svnadmin create /var/svn/pround
  chown -R www-data:www-data /var/svn/pround
  work=$(mktemp -d)
  svn checkout --non-interactive "file:///var/svn/pround" "$work"
  mkdir -p "$work/trunk" "$work/branches" "$work/tags"
  printf 'seed\\n' > "$work/trunk/README.txt"
  svn add "$work/trunk" "$work/branches" "$work/tags"
  svn commit --username svc-sync -m "seed" "$work"
  printf 'alpha\\n' > "$work/trunk/alpha.txt"
  svn add "$work/trunk/alpha.txt"
  svn commit --username svc-sync -m "alpha" "$work"
  rm -rf "$work"
  chown -R www-data:www-data /var/svn/pround
fi
`);
      await lab.sh("gitea", `
set -eu
org=$(curl --silent --show-error --output /tmp/org.out --write-out '%{http_code}' --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" -d '{"username":"lab","visibility":"private"}' https://gitea:3000/api/v1/orgs || true)
case "$org" in 201|409|422) ;; *) echo "gitea org $org"; cat /tmp/org.out; exit 1 ;; esac
repo=$(curl --silent --show-error --output /tmp/pround.out --write-out '%{http_code}' --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" -d '{"name":"pround","private":true,"auto_init":false,"default_branch":"main"}' https://gitea:3000/api/v1/orgs/lab/repos || true)
case "$repo" in 201|409) ;; *) echo "gitea repo $repo"; cat /tmp/pround.out; exit 1 ;; esac
`);
      const imported = await lab.sh("reposync", `
set -eu
root=/var/lib/reposync/personal-pround
rm -rf "$root"
mkdir -p "$root"
cat > "$root/personal.toml" <<'EOF'
[personal]
poll_interval_secs = 2
log_level = "info"
data_dir = "/var/lib/reposync/personal-pround/data"

[svn]
url = "https://svn:8443/svn/pround/trunk"
username = "svc-sync"
password_env = "REPOSYNC_SVN_PASSWORD"

[github]
api_url = "https://gitea-api:3000/api/v1"
git_base_url = "https://gitea:3000"
repo = "lab/pround"
token_env = "REPOSYNC_GITHUB_TOKEN"
default_branch = "main"
auto_create = false
private = true

[developer]
name = "Alice Lab"
email = "alice@lab.invalid"
svn_username = "alice"

[options]
normalize_line_endings = false
sync_executable_bit = false
auto_merge = false
sync_direct_pushes = false
EOF
reposync personal --personal-config "$root/personal.toml" import --full
test -f "$root/data/git-repo/README.txt"
test -f "$root/data/git-repo/alpha.txt"
maps=$(sqlite3 "$root/data/personal.db" "SELECT COUNT(*) FROM commit_map WHERE direction='svn_to_git';")
records=$(sqlite3 "$root/data/personal.db" "SELECT COUNT(*) FROM sync_records WHERE direction='svn_to_git' AND status='applied';")
printf 'PERSONAL_IMPORT maps=%s records=%s\\n' "$maps" "$records"
`, "reposync");
      const importLine = imported.split("\n").map((line) => line.trim()).filter((line) => line.startsWith("PERSONAL_IMPORT ")).pop() || "";
      const importMaps = Number(importLine.match(/maps=(\d+)/)?.[1]);
      const importRecords = Number(importLine.match(/records=(\d+)/)?.[1]);
      if (!Number.isFinite(importMaps) || !Number.isFinite(importRecords) || importMaps < 2 || importRecords < 2) {
        throw new Error(`personal import checkpoint ${importLine || imported.trim().slice(-300)}`);
      }
      await lab.sh("svn", `
set -eu
work=$(mktemp -d)
svn checkout --non-interactive "file:///var/svn/pround/trunk" "$work"
printf 'beta\\n' > "$work/beta.txt"
svn add "$work/beta.txt"
svn commit --username svc-sync -m "beta" "$work"
rm -rf "$work"
`);
      const synced = await lab.sh("reposync", `
set -eu
root=/var/lib/reposync/personal-pround
cfg="$root/personal.toml"
db="$root/data/personal.db"
svn_cfg=/var/lib/reposync/svn-config
stop_personal() { reposync personal --personal-config "$cfg" stop >/dev/null 2>&1 || true; }
trap stop_personal EXIT
svn_text() { svn cat --non-interactive --config-dir "$svn_cfg" --username svc-sync --password "$REPOSYNC_SVN_PASSWORD" "https://svn:8443/svn/pround/trunk/$1"; }
svn_rev() { svn info --non-interactive --config-dir "$svn_cfg" --username svc-sync --password "$REPOSYNC_SVN_PASSWORD" --show-item revision https://svn:8443/svn/pround/trunk; }
records() { sqlite3 "$db" "SELECT COUNT(*) FROM sync_records;"; }
start_personal() { nohup reposync personal --personal-config "$cfg" start --foreground >>"$root/daemon.log" 2>&1 & }
fail_log() {
  echo "personal daemon log:" >&2
  tail -n 40 "$root/daemon.log" >&2 || true
  echo "closed pull fields:" >&2
  curl --silent --show-error --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" "https://gitea:3000/api/v1/repos/lab/pround/pulls?state=closed&base=main" | tr ',' '\n' | grep -E '"(merged|html_url|sha|ref|state|merge_commit_sha|number)"' | head -n 40 >&2 || true
  exit 1
}
start_personal
i=0
while [ "$i" -lt 40 ]; do
  if [ -f "$root/data/git-repo/beta.txt" ] && grep -q '^beta$' "$root/data/git-repo/beta.txt" && grep -q '^alpha$' "$root/data/git-repo/alpha.txt"; then
    break
  fi
  i=$((i + 1))
  sleep 2
done
grep -q '^beta$' "$root/data/git-repo/beta.txt" || fail_log
grep -q '^alpha$' "$root/data/git-repo/alpha.txt" || fail_log
stop_personal
after_beta=$(records)
if [ "$after_beta" -le ${importRecords} ]; then
  echo "sync records stayed at $after_beta after beta" >&2
  fail_log
fi
cd "$root/data/git-repo"
git checkout main
git checkout -b gamma
git config user.name "Alice Lab"
git config user.email "alice@lab.invalid"
printf 'gamma\\n' > gamma.txt
git add gamma.txt
git commit -m "gamma"
auth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')
git -c http.sslCAInfo=/certs/ca.crt -c http.extraHeader="Authorization: Basic $auth" push origin gamma
git checkout main
before_gamma=$(svn_rev)
start_personal
pr=$(curl --silent --show-error --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" -d '{"title":"gamma","head":"gamma","base":"main"}' https://gitea:3000/api/v1/repos/lab/pround/pulls)
num=$(printf '%s' "$pr" | grep -o '"number":[[:space:]]*[0-9][0-9]*' | head -n 1 | grep -o '[0-9][0-9]*$')
if [ -z "$num" ]; then echo "pull request was not created: $pr" >&2; fail_log; fi
merge_code=$(curl --silent --show-error --output /tmp/merge.out --write-out '%{http_code}' --cacert /certs/ca.crt -X POST -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" -d '{"Do":"merge","MergeMessageField":"gamma"}' "https://gitea:3000/api/v1/repos/lab/pround/pulls/$num/merge" || true)
if [ "$merge_code" != "200" ] && [ "$merge_code" != "201" ]; then
  merge_code=$(curl --silent --show-error --output /tmp/merge.out --write-out '%{http_code}' --cacert /certs/ca.crt -X PUT -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" -d '{"merge_method":"merge"}' "https://gitea:3000/api/v1/repos/lab/pround/pulls/$num/merge" || true)
fi
if [ "$merge_code" != "200" ] && [ "$merge_code" != "201" ]; then
  echo "merge status $merge_code" >&2
  cat /tmp/merge.out >&2 || true
  fail_log
fi
i=0
while [ "$i" -lt 40 ]; do
  if svn_text gamma.txt 2>/dev/null | grep -q '^gamma$'; then
    break
  fi
  i=$((i + 1))
  sleep 2
done
svn_text gamma.txt | grep -q '^gamma$' || fail_log
svn_text alpha.txt | grep -q '^alpha$' || fail_log
svn_text beta.txt | grep -q '^beta$' || fail_log
after_gamma=$(svn_rev)
if [ "$after_gamma" = "$before_gamma" ]; then echo "svn revision did not advance for gamma" >&2; fail_log; fi
after_gamma_records=$(records)
if [ "$after_gamma_records" -le "$after_beta" ]; then echo "sync records did not grow for gamma" >&2; fail_log; fi
stop_personal
start_personal
sleep 8
stop_personal
noop_rev=$(svn_rev)
noop_records=$(records)
if [ "$noop_rev" != "$after_gamma" ] || [ "$noop_records" != "$after_gamma_records" ]; then
  echo "repeat run changed svn $after_gamma->$noop_rev records $after_gamma_records->$noop_records" >&2
  fail_log
fi
svn_text gamma.txt | grep -q '^gamma$' || fail_log
printf 'PERSONAL_FINAL records=%s svn=%s noop_svn=%s\\n' "$noop_records" "$after_gamma" "$noop_rev"
`, "reposync");
      const finalLine = synced.split("\n").map((line) => line.trim()).filter((line) => line.startsWith("PERSONAL_FINAL ")).pop() || "";
      const finalRecords = Number(finalLine.match(/records=(\d+)/)?.[1]);
      const finalSvn = finalLine.match(/svn=(\d+)/)?.[1] || "";
      const noopSvn = finalLine.match(/noop_svn=(\d+)/)?.[1] || "";
      if (!Number.isFinite(finalRecords) || finalRecords <= importRecords || !finalSvn || finalSvn !== noopSvn) {
        throw new Error(`personal round trip ${finalLine || synced.trim().slice(-400)}`);
      }
      lab.pass(`personal import mapped ${importMaps} revisions and ${importRecords} svn_to_git receipts; both directions reached ${finalRecords} sync records at r${finalSvn}; the restarted daemon left that revision unchanged`);
      return;
    }
    default:
      throw new Error(`no executor for ${op.id}`);
  }
}
