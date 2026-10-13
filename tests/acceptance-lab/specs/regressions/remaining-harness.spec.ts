import {execFileSync} from "node:child_process";
import {mkdtempSync, mkdirSync, rmSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {test, expect} from "@playwright/test";
import {assertCompletedPublishRefusal, Lab, loginError, type Pair, type Snap} from "../../playwright/runtime.ts";
import {runCase} from "../../playwright/handlers.ts";

const initial: Snap = {svn: "1", git: "a".repeat(40), maps: "1", records: "1", watermark: "1", gitWatermark: "a".repeat(40), gitColumn: "a".repeat(40)};
const pair: Pair = {id: "fixture", name: "fixture", svnUrl: "https://unused/svn/fixture", gitRepo: "lab/fixture"};

test("completed-publish repeat accepts only the exact existing-child refusal", () => {
  const error = "a branch pair with git_branch 'feature' already exists";
  assertCompletedPublishRefusal({status: 400, json: {error}}, "feature");
  for (const result of [
    {status: 200, json: {}}, {status: 422, json: {error}}, {status: 500, json: {error}},
    {status: 400, json: {error: "missing_git_ref"}}, {status: 400, json: {error: "existing_svn_target_blocks_publish"}},
    {status: 400, json: null}, {status: 400, json: {error: error.replace("feature", "other")}},
  ]) expect(() => assertCompletedPublishRefusal(result, "feature")).toThrow("completed-child refusal");
});

for (const fault of ["none", "trunk-fixture", "outside-published", "missing-no-target", "cursor-overrun", "duplicate"]) {
  test(`actual empty-to-project handler requires an outside revision and explicit outcome: ${fault}`, async () => {
    let state = {...initial}, contents = 0, passed = false, outsideCommitted = false;
    const fake: any = {
      createPair: async () => pair, snap: async () => ({...state}),
      syncSvnChange: async () => {contents++; state = {...state, svn: contents === 1 ? "2" : "4", git: (contents === 1 ? "b" : "c").repeat(40), watermark: contents === 1 ? "2" : "4", records: String(contents + 1)}; return {...state};},
      commitSvn: async (p: Pair, script: string) => {expect(p.svnBranch).toBe("branches"); expect(script).toContain("outside.txt"); outsideCommitted = true; state.svn = "3"; return "3";},
      svnRevisionLog: async () => fault === "trunk-fixture" ? "/trunk/outside.txt" : "/branches/outside.txt",
      triggerSvn: async (revision: string) => {if (revision === "3") state.watermark = fault === "cursor-overrun" ? "4" : "3";},
      gitBytes: async (_p: Pair, path: string) => path === "first.txt" ? "first\n" : "second\n",
      gitBytesOrMissing: async () => fault === "outside-published" ? "outside\n" : null,
      assertSingleReceipt: async () => {if (fault === "duplicate") throw new Error("duplicate receipt");},
      sqlRows: async () => fault === "missing-no-target" ? [] : [{action: "svn_to_git_no_target", svn_rev: 3}],
      expectNoWrite: async (_p: Pair, action: () => Promise<void>) => {const before = {...state}; await action(); expect(state).toEqual(before);},
      observation: () => {}, pass: () => {passed = true;}, gap: (message: string) => {throw new Error(message);},
    };
    const clock = Date.now; let time = 100_000;
    Date.now = () => time += 13_000;
    try {
      const run = runCase(fake, {id: "SVN-EMPTY-REV", title: "fixture", suite: "full"});
      if (fault === "none") {await run; expect(passed).toBe(true); expect(outsideCommitted).toBe(true); expect(contents).toBe(2);}
      else {await expect(run).rejects.toThrow(); expect(passed).toBe(false);}
    } finally {Date.now = clock;}
  });
}

for (const fault of ["none", "missing-ref", "wrong-tip", "wrong-baseline", "pending", "target", "child-row", "journal", "cursor"]) {
  test(`actual late-pair preview requires derived pins and leaves no writes: ${fault}`, async () => {
    let seeded = false, count = 0, previewed = false, passed = false;
    const fake: any = {
      createPair: async () => pair,
      snap: async () => ({...initial, watermark: previewed && fault === "cursor" ? "2" : "1"}),
      commitGit: async (_p: Pair, script: string) => {
        if (!seeded) {expect(script).toContain("git push origin refs/heads/pairlatepreview:refs/heads/pairlatepreview"); seeded = true; return initial.git;}
        count++; return (count === 1 ? "b" : "c").repeat(40);
      },
      repos: async () => previewed && fault === "child-row" ? [{id: "premature-child"}] : [],
      sqlRows: async () => previewed && fault === "journal" ? [{value: JSON.stringify({parent_repo_id: pair.id, git_branch: "pairlatepreview"})}] : [],
      api: async (_m: string, _path: string, body: any) => {
        expect(seeded).toBe(true); expect(count).toBe(2); expect(body.dry_run).toBe(true); expect(body.auto_create_git_branch).toBe(false);
        previewed = true;
        return {status: fault === "missing-ref" ? 400 : 200, text: "missing_git_ref", json: {
          parent_id: pair.id, git_tip: (fault === "wrong-tip" ? "a" : "c").repeat(40),
          verified_baseline: {git_sha: initial.git, svn_revision: fault === "wrong-baseline" ? 2 : 1},
          pending_git: {count: fault === "pending" ? 0 : 2}, proposed_svn_copy_source_revision: 1,
          existing_svn_target: {exists: fault === "target"}, scheduler_active: false, published: false,
        }};
      },
      expectNoWrite: async (_p: Pair, action: () => Promise<void>) => {
        const before = await fake.snap(); await action(); expect(await fake.snap()).toEqual(before);
      },
      observer: async () => "c".repeat(40), svnBytesOrMissing: async () => null,
      observation: () => {}, pass: () => {passed = true;},
    };
    const run = runCase(fake, {id: "PAIR-LATE-PREVIEW", title: "fixture", suite: "full"});
    if (fault === "none") {await run; expect(passed).toBe(true);}
    else {await expect(run).rejects.toThrow(); expect(passed).toBe(false);}
  });
}

for (const fault of ["none", "git", "maps", "watermark", "gitWatermark", "gitColumn", "source", "missing-receipt", "observer", "trigger"]) {
  test(`actual SVN refusal oracle permits only the intentional source commit: ${fault}`, async () => {
    let observations = 0, triggered = false;
    const now: Snap = {...initial, svn: "2"};
    if (["git", "maps", "watermark", "gitWatermark", "gitColumn"].includes(fault)) (now as any)[fault] = "changed";
    if (fault === "source") now.svn = "3";
    const fake: any = {
      snap: async () => {if (fault === "observer") throw new Error("observer unavailable"); return now;},
      observation: () => {observations++;},
      triggerSvn: async () => {triggered = true; if (fault === "trigger") throw new Error("webhook unavailable");},
      sqlRows: async () => fault === "missing-receipt" ? [] : [{action: "svn_to_git_apply_failed", svn_rev: 2, repo_id: pair.id, success: 0}],
      waitUntil: async (read: () => Promise<boolean>) => {if (!await read()) throw new Error("no daemon refusal receipt");},
    };
    const clock = Date.now; let time = 100_000;
    Date.now = () => time += 13_000;
    try {
      const result = Lab.prototype.expectSvnRefusal.call(fake, pair, initial, "2");
      if (fault === "none") {await result; expect(triggered).toBe(true); expect(observations).toBe(2);}
      else await expect(result).rejects.toThrow();
    } finally {Date.now = clock;}
  });
}

test("actual SVN fixtures distinguish sibling-only revision, scoped content and pure property change", async () => {
  const root = mkdtempSync(join(tmpdir(), "lab-gap-"));
  const repository = join(root, "svn");
  mkdirSync(join(root, "work"));
  const command = (tool: string, args: string[]) => execFileSync(tool, args, {cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"]});
  try {
    command("svnadmin", ["create", repository]);
    command("svn", ["mkdir", `file://${repository}/trunk`, `file://${repository}/branches`, "-m", "layout"]);
    const p = {...pair, svnUrl: `file://${repository}`};
    const fake: any = {observer: async (script: string) => command("sh", ["-c", `export HOME='${root}' SVN_PASSWORD=synthetic\n${script.replaceAll("/work/", `${root}/work/`)}`])};
    const commit = (p: Pair, script: string) => Lab.prototype.commitSvn.call(fake, p, script);
    const first = await commit(p, "printf 'first\\n' > README.txt\nsvn add README.txt\n");
    const outside = await commit({...p, svnBranch: "branches"}, "printf 'outside\\n' > outside.txt\nsvn add outside.txt\n");
    const log = await Lab.prototype.svnRevisionLog.call(fake, p, outside);
    expect(log).toContain("/branches/outside.txt"); expect(log).not.toContain("/trunk/");
    const scopedLog = command("svn", ["log", "--xml", `${p.svnUrl}/trunk`]);
    expect(scopedLog).not.toContain(`revision="${outside}"`);
    const second = await commit(p, "printf 'second\\n' > second.txt\nsvn add second.txt\n");
    expect(Number(second)).toBe(Number(first) + 2);
    const property = await commit(p, "svn propset svn:eol-style LF README.txt\n");
    expect(command("svn", ["cat", "-r", property, `${p.svnUrl}/trunk/README.txt`])).toBe("first\n");
    const diff = command("svn", ["diff", "--ignore-properties", "-c", property, `${p.svnUrl}/trunk`]);
    expect(diff.trim()).toBe("");
    expect(command("svn", ["propget", "svn:eol-style", `${p.svnUrl}/trunk/README.txt`]).trim()).toBe("LF");
  } finally {rmSync(root, {recursive: true, force: true});}
});

for (const fault of ["none", "schema", "auth", "role", "login"]) {
  test(`group-role uses an admin-accepted schema and remains unqualified: ${fault}`, async () => {
    let disposition = "", enabled = false;
    const config = {enabled: true, url: "ldaps://ldap:636", base_dn: "dc=lab,dc=invalid", search_filter: "(uid={0})",
      display_name_attr: "cn", email_attr: "mail", group_attr: "memberOf", bind_dn: "synthetic", bind_password: "synthetic", tls_verify: true};
    const fake: any = {
      alicePassword: "synthetic", enableLdap: async () => {enabled = true;}, ldapConfig: () => config,
      login: async () => ({status: fault === "login" ? 401 : 200, json: {token: "synthetic-token", user: {username: "alice", role: fault === "role" ? "admin" : "user"}}}),
      api: async (method: string, path: string, body: unknown, token: string) => {
        expect(enabled).toBe(true); expect([method, path, token]).toEqual(["PUT", "/api/admin/ldap", "synthetic-token"]); expect(body).toEqual(config);
        return {status: fault === "schema" ? 422 : fault === "auth" ? 200 : 401};
      }, observation: () => {}, mark: (status: string) => {disposition = status;}, pass: () => {throw new Error("group mapping must not PASS");},
    };
    const run = runCase(fake, {id: "LDAP-GROUP-ROLE", title: "fixture", suite: "full"});
    if (fault === "none") {await run; expect(disposition).toBe("PARTIAL");}
    else {await expect(run).rejects.toThrow(); expect(disposition).toBe("");}
  });
}

for (const fault of ["none", "login", "fresh", "logout", "reuse"]) {
  test(`logout sends the issued token and proves revocation without qualifying expiry: ${fault}`, async () => {
    let disposition = "", reads = 0;
    const fake: any = {
      alicePassword: "synthetic", enableLdap: async () => {},
      login: async () => ({status: fault === "login" ? 401 : 200, json: {token: "synthetic-token", user: {username: "alice"}}}),
      api: async (method: string, path: string, body: unknown, token: string) => {
        expect(token).toBe("synthetic-token");
        if (method === "POST") {expect(path).toBe("/api/auth/logout"); expect(body).toEqual({token}); return {status: fault === "logout" ? 422 : 200, json: {ok: true}};}
        expect(path).toBe("/api/auth/me"); reads++; return {status: reads === 1 ? fault === "fresh" ? 401 : 200 : fault === "reuse" ? 200 : 401};
      }, observation: () => {}, mark: (status: string) => {disposition = status;}, pass: () => {throw new Error("expiry must not PASS");},
    };
    const run = runCase(fake, {id: "LDAP-SESSION-EXPIRY-LOGOUT", title: "fixture", suite: "full"});
    if (fault === "none") {await run; expect(disposition).toBe("PARTIAL"); expect(reads).toBe(2);}
    else {await expect(run).rejects.toThrow(); expect(disposition).toBe("");}
  });
}

test("login error locator excludes the LDAP domain and requires an actual form error", async ({page}) => {
  await page.setContent(`<div><span>(lab.invalid)</span></div><form><div class="border-red-700"><div><svg></svg><span>invalid username or password</span></div></div><input name="username"><input type="password"><button>Sign in</button></form>`);
  await expect(page.getByText(/invalid|failed|unauthorized|incorrect/i)).toHaveCount(2);
  await expect(loginError(page)).toHaveCount(1);
  await expect(loginError(page)).toHaveText("invalid username or password");
  await page.locator("form .border-red-700").evaluate(node => node.remove());
  await expect(loginError(page)).toHaveCount(0);
  await expect(page.getByText("(lab.invalid)", {exact: true})).toBeVisible();
});
