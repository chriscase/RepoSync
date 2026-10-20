import {execFileSync} from "node:child_process";
import {mkdtempSync, mkdirSync, rmSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {test, expect} from "@playwright/test";
import {assertCompletedPublishRefusal, assertReplayEndpoints, Lab, loginError, type Pair, type Snap} from "../../playwright/runtime.ts";
import {labPairKey, requireRepeatIdle, runCase} from "../../playwright/handlers.ts";

const initial: Snap = {svn: "1", git: "a".repeat(40), maps: "1", records: "1", watermark: "1", gitWatermark: "a".repeat(40), gitColumn: "a".repeat(40)};
const pair: Pair = {id: "fixture", name: "fixture", svnUrl: "https://unused/svn/fixture", gitRepo: "lab/fixture"};

test("cycle endpoints allow a two-commit drain without substituting for per-commit receipts", () => {
  const commits = ["b".repeat(40), "c".repeat(40)];
  assertReplayEndpoints([commits[1]], commits);
  assertReplayEndpoints(commits, commits);
  assertReplayEndpoints([], []);
  for (const endpoints of [[], [commits[0]], ["d".repeat(40), commits[1]], [commits[0], commits[0], commits[1]], [commits[1], commits[0], commits[1]], null]) {
    expect(() => assertReplayEndpoints(endpoints, commits)).toThrow();
  }
});

for (const fault of ["none", "missing-first-receipt", "wrong-intermediate-tree", "baseline-read-cursor"]) {
  test(`actual publish accepts a per-cycle endpoint but still requires individual replay proof: ${fault}`, async () => {
    const commits = ["b".repeat(40), "c".repeat(40)], branch = "pairlatepublish";
    let seeded = false, writes = 0, published = false, disposition = "";
    const child = {id: "child", parent_id: pair.id, svn_url: pair.svnUrl, git_repo: pair.gitRepo, svn_branch: `branches/${branch}`, git_branch: branch};
    const journal = {state: "completed", parent_repo_id: pair.id, git_branch: branch, child_repo_id: "child", baseline_git_sha: initial.git,
      baseline_svn_rev: 1, svn_copy_source_rev: 1, pinned_git_tip: commits[1], replayed_git_shas: [commits[1]]};
    const plan = {parent_id: pair.id, git_tip: commits[1], verified_baseline: {git_sha: initial.git, svn_revision: 1}, pending_git: {count: 2},
      proposed_svn_copy_source_revision: 1, existing_svn_target: {exists: false}, scheduler_active: false, published: false};
    const fake: any = {
      createPair: async () => pair,
      snap: async (p: Pair) => p.id === "child" ? {...initial, svn: "4", git: commits[1], watermark: fault === "baseline-read-cursor" ? "2" : "4", gitWatermark: commits[1], gitColumn: commits[1]} : {...initial, svn: published ? "4" : "1"},
      commitGit: async () => {if (!seeded) {seeded = true; return initial.git;} return commits[writes++];},
      repos: async () => published ? [child] : [],
      sqlRows: async (sql: string) => sql.includes("late_pair_publish_v1") ? published ? [{value: JSON.stringify(journal)}] : [] : commits.map((git_sha, i) => ({git_sha, svn_rev: i + 3})),
      api: async (_m: string, _p: string, body: any) => {
        if (body.dry_run) return {status: 200, json: plan};
        if (published) return {status: 400, json: {error: `a branch pair with git_branch '${branch}' already exists`}};
        published = true; return {status: 200, json: {...plan, published: true, scheduler_active: true}};
      },
      observer: async (script: string) => script.includes("ls-remote") ? commits[1] : '<path copyfrom-rev="1" copyfrom-path="/trunk"/>',
      svnBytesOrMissing: async (_p: Pair, path: string, revision?: string) => fault === "wrong-intermediate-tree" && path === "child2.txt" && revision === "3" ? "child2\n" : null,
      svnBytes: async (_p: Pair, path: string) => path === "child1.txt" ? "child1\n" : "child2\n",
      svnBytesAtRevision: async () => "child1\n",
      sh: async (_service: string, sql: string) => fault === "missing-first-receipt" && sql.includes(commits[0]) ? "0" : "1",
      assertSingleReceipt: (p: Pair, d: "svn_to_git" | "git_to_svn", sha: string) => Lab.prototype.assertSingleReceipt.call(fake, p, d, sha),
      expectNoWrite: async (p: Pair, action: () => Promise<void>) => {const before = await fake.snap(p); await action(); expect(await fake.snap(p)).toEqual(before);},
      observation: () => {}, mark: (status: string) => {disposition = status;}, pass: () => {throw new Error("crash qualification must remain partial");},
    };
    const run = runCase(fake, {id: "PAIR-LATE-PUBLISH", title: "fixture", suite: "full"});
    if (fault === "none" || fault === "baseline-read-cursor") {await run; expect(disposition).toBe("PARTIAL");}
    else {await expect(run).rejects.toThrow(); expect(disposition).toBe("");}
  });
}

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

for (const fault of ["delayed", "duplicate"]) {
  test(`revert idle baseline waits for every outbound receipt: ${fault}`, async () => {
    const ancestor = "a".repeat(40);
    const commits = [1, 2, 3].map(n => n.toString(16).repeat(40));
    const tip = commits[2];
    const beforeSnap: Snap = {...initial, git: ancestor, gitWatermark: ancestor, gitColumn: ancestor};
    let phase = "before";
    let triggers = 0;
    let passed = false;
    const snap = (): Snap => {
      if (phase === "before") return {...beforeSnap};
      if (phase === "duplicated") return {svn: "9", git: tip, maps: "1", records: "9", watermark: "1", gitWatermark: tip, gitColumn: tip};
      if (phase === "first-svn") return {svn: "2", git: tip, maps: "1", records: "2", watermark: "1", gitWatermark: ancestor, gitColumn: ancestor};
      return {svn: "4", git: tip, maps: "1", records: "4", watermark: "1", gitWatermark: tip, gitColumn: tip};
    };
    const fake: any = {
      createPair: async () => pair,
      snap: async () => snap(),
      commitGit: async () => {phase = fault === "duplicate" ? "settled" : "first-svn"; return tip;},
      gitCommitsSince: async (_p: Pair, ancestorSha: string) => {expect(ancestorSha).toBe(ancestor); return commits;},
      triggerGit: async () => {triggers++; if (fault === "duplicate" && triggers > 1) phase = "duplicated"; return 200;},
      svnBytesOrMissing: async () => phase === "first-svn" ? "v2\n" : "v1\n",
      sh: async (_service: string, sql: string) => {
        const sha = commits.find(item => sql.includes(item));
        if (!sha) return "0";
        if (phase === "settled" || phase === "duplicated") return "1";
        if (phase === "tip-without-receipts") return sha === tip ? "1" : "0";
        return sha === commits[0] ? "1" : "0";
      },
      waitUntil: async (read: () => Promise<boolean>) => {
        const stages = fault === "duplicate" ? ["settled"] : ["first-svn", "tip-without-receipts", "settled"];
        for (const stage of stages) {
          phase = stage;
          expect(await read()).toBe(stage === "settled");
        }
      },
      expectNoWrite: async (_p: Pair, action: () => Promise<void>) => {
        expect(phase).toBe("settled");
        const baseline = snap();
        await action();
        expect(snap()).toEqual(baseline);
      },
      pass: () => {passed = true;},
    };
    fake.appliedReceiptCount = (...args: unknown[]) => Lab.prototype.appliedReceiptCount.call(fake, ...(args as [Pair, "git_to_svn", string]));
    fake.waitOutboundGitHistory = (...args: unknown[]) => Lab.prototype.waitOutboundGitHistory.call(fake, ...(args as [Pair, Snap, string[], string, string]));
    const run = runCase(fake, {id: "GIT-REVERT-NEW", title: "fixture", suite: "full"});
    if (fault === "delayed") {await run; expect(passed).toBe(true); expect(triggers).toBe(2);}
    else {await expect(run).rejects.toThrow(); expect(passed).toBe(false);}
  });
}

test("observed and unobserved rebase ids get different repositories", () => {
  const observed = labPairKey("GIT-PUBLISHED-REBASE-UNSYNCED-OBSERVED");
  const unobserved = labPairKey("GIT-PUBLISHED-REBASE-UNSYNCED-UNOBSERVED");
  expect(observed).not.toBe(unobserved);
  expect(observed).not.toBe("pgitpublishedrebaseunsync");
  expect(unobserved).not.toBe("pgitpublishedrebaseunsync");
  expect(labPairKey("GIT-REVERT-NEW")).toBe("pgitrevertnew");
});

for (const fault of ["none", "observed-tip", "late-b"]) {
  test(`unobserved rebase stays disabled until both tips and rejects ${fault}`, async () => {
    const ancestor = "a".repeat(40);
    const tipB = "b".repeat(40);
    const tipB2 = "d".repeat(40);
    const published = "c".repeat(40);
    const fresh: Snap = {...initial, git: ancestor, gitWatermark: ancestor, gitColumn: ancestor};
    let phase = "fresh";
    let passed = false;
    let enabled = false;
    let afterIdle = false;
    const snap = (): Snap => {
      if (phase === "remote-b") return {...fresh, git: tipB};
      if (phase === "remote-b2" || phase === "paused") return {...fresh, git: tipB2};
      if (phase === "already-published") return {svn: "5", git: published, maps: "1", records: "4", watermark: "5", gitWatermark: published, gitColumn: published};
      if (phase === "settled") return {svn: "2", git: tipB2, maps: "1", records: "2", watermark: "1", gitWatermark: tipB2, gitColumn: tipB2};
      return {...fresh};
    };
    const fake: any = {
      createPair: async (name: string, options?: {enable?: boolean}) => {
        expect(options).toEqual({enable: false});
        expect(enabled).toBe(false);
        return {...pair, name, gitRepo: `lab/${name}`, svnUrl: `https://svn:8443/svn/${name}`};
      },
      api: async (method: string) => method === "GET" ? {status: 200, json: {enabled: false}} : {status: 200, json: {}},
      observedInspectionTip: async () => fault === "observed-tip" && (phase === "remote-b" || phase === "remote-b2") ? tipB : "absent",
      enableFixturePair: async () => {expect(phase).toBe("remote-b2"); enabled = true;},
      snap: async () => snap(),
      commitGit: async (_p: Pair, script: string) => {expect(enabled).toBe(false); expect(script).not.toContain("--amend"); phase = "remote-b"; return tipB;},
      commitGitLocal: async (_p: Pair, script: string) => {expect(enabled).toBe(false); expect(script).toContain("--amend"); phase = "remote-b2"; return tipB2;},
      gitCommitsSince: async (_p: Pair, ancestorSha: string) => {expect(ancestorSha).toBe(ancestor); return [tipB2];},
      triggerGit: async () => {expect(enabled).toBe(true); return 200;},
      svnBytes: async () => phase === "settled" ? "B\n" : "other\n",
      svnBytesOrMissing: async () => phase === "settled" ? "B\n" : null,
      sh: async (_service: string, sql: string) => {
        if (sql.includes(tipB) && !sql.includes(tipB2)) return fault === "late-b" && afterIdle ? "1" : "0";
        return phase === "settled" && sql.includes(tipB2) ? "1" : "0";
      },
      waitUntil: async (read: () => Promise<boolean>) => {
        phase = "paused";
        expect(await read()).toBe(false);
        phase = "already-published";
        expect(await read()).toBe(false);
        phase = "settled";
        expect(await read()).toBe(true);
      },
      expectNoWrite: async (_p: Pair, action: () => Promise<void>) => {
        expect(phase).toBe("settled");
        await action();
        afterIdle = true;
      },
      pass: () => {passed = true;},
    };
    fake.appliedReceiptCount = (...args: unknown[]) => Lab.prototype.appliedReceiptCount.call(fake, ...(args as [Pair, "git_to_svn", string]));
    fake.waitOutboundGitHistory = (...args: unknown[]) => Lab.prototype.waitOutboundGitHistory.call(fake, ...(args as [Pair, Snap, string[], string, string]));
    const run = runCase(fake, {id: "GIT-PUBLISHED-REBASE-UNSYNCED-UNOBSERVED", title: "fixture", suite: "full"});
    if (fault === "none") {await run; expect(passed).toBe(true); expect(enabled).toBe(true);}
    else {await expect(run).rejects.toThrow(fault === "observed-tip" ? /inspection tip/ : /late receipt/); expect(passed).toBe(false);}
  });
}

test("cherry-pick publishes the descendant on the tracked branch", async () => {
  const ancestor = "a".repeat(40);
  const baseTip = "b".repeat(40);
  const later = "c".repeat(40);
  const descendant = "d".repeat(40);
  const fresh: Snap = {...initial, git: ancestor, gitWatermark: ancestor, gitColumn: ancestor};
  let phase = "fresh";
  let passed = false;
  const scripts: string[] = [];
  const snap = (): Snap => {
    if (phase === "base-synced") return {svn: "2", git: baseTip, maps: "1", records: "2", watermark: "1", gitWatermark: baseTip, gitColumn: baseTip};
    if (phase === "descendant-synced") return {svn: "4", git: descendant, maps: "1", records: "4", watermark: "1", gitWatermark: descendant, gitColumn: descendant};
    return {...fresh};
  };
  const fake: any = {
    createPair: async () => pair,
    snap: async () => snap(),
    commitGit: async (_p: Pair, script: string) => {
      scripts.push(script);
      if (script.includes("cherry-pick")) {
        expect(script).not.toContain("checkout -b pick");
        phase = "descendant-remote";
        return descendant;
      }
      phase = "base-remote";
      return baseTip;
    },
    gitCommitsSince: async (_p: Pair, ancestorSha: string) => ancestorSha === ancestor ? [baseTip] : [later, descendant],
    triggerGit: async () => 200,
    svnBytesOrMissing: async () => "picked\n",
    sh: async (_service: string, sql: string) => (phase === "base-synced" && sql.includes(baseTip)) || (phase === "descendant-synced" && (sql.includes(later) || sql.includes(descendant))) ? "1" : "0",
    waitUntil: async (read: () => Promise<boolean>) => {
      if (phase === "base-remote") {
        phase = "unchanged-main";
        expect(await read()).toBe(false);
        phase = "base-synced";
      } else {
        phase = "unchanged-main";
        expect(await read()).toBe(false);
        phase = "descendant-synced";
      }
      expect(await read()).toBe(true);
    },
    expectNoWrite: async (_p: Pair, action: () => Promise<void>) => {await action();},
    pass: () => {passed = true;},
  };
  fake.appliedReceiptCount = (...args: unknown[]) => Lab.prototype.appliedReceiptCount.call(fake, ...(args as [Pair, "git_to_svn", string]));
  fake.waitOutboundGitHistory = (...args: unknown[]) => Lab.prototype.waitOutboundGitHistory.call(fake, ...(args as [Pair, Snap, string[], string, string]));
  await runCase(fake, {id: "GIT-CHERRY-PICK-NEW", title: "fixture", suite: "full"});
  expect(passed).toBe(true);
  expect(scripts.some(script => script.includes("cherry-pick") && script.includes("HEAD~1"))).toBe(true);
});

test("an unchanged tracked branch is not a cherry-pick publication", async () => {
  const ancestor = "a".repeat(40);
  const baseTip = "b".repeat(40);
  const fresh: Snap = {...initial, git: ancestor, gitWatermark: ancestor, gitColumn: ancestor};
  let passed = false;
  let calls = 0;
  const fake: any = {
    createPair: async () => pair,
    snap: async () => calls < 2 ? {...fresh, svn: "2", git: baseTip, records: "2", gitWatermark: baseTip, gitColumn: baseTip} : {...fresh, git: baseTip, svn: "2", records: "2", gitWatermark: baseTip, gitColumn: baseTip},
    commitGit: async () => {calls++; return baseTip;},
    gitCommitsSince: async () => [baseTip],
    triggerGit: async () => 200,
    svnBytesOrMissing: async () => "picked\n",
    sh: async () => "1",
    waitUntil: async (read: () => Promise<boolean>) => {expect(await read()).toBe(true);},
    pass: () => {passed = true;},
  };
  fake.appliedReceiptCount = (...args: unknown[]) => Lab.prototype.appliedReceiptCount.call(fake, ...(args as [Pair, "git_to_svn", string]));
  fake.waitOutboundGitHistory = async () => fake.snap();
  await expect(runCase(fake, {id: "GIT-CHERRY-PICK-NEW", title: "fixture", suite: "full"})).rejects.toThrow(/did not advance/);
  expect(passed).toBe(false);
});

test("rejected repeat webhook is not an idle result", async () => {
  await expect(requireRepeatIdle({triggerGit: async () => 500, expectNoWrite: async (_p: Pair, action: () => Promise<void>) => {await action();}} as unknown as Lab, pair)).rejects.toThrow(/repeat webhook 500/);
  let status = 0;
  await requireRepeatIdle({triggerGit: async () => {status = 200; return 200;}, expectNoWrite: async (_p: Pair, action: () => Promise<void>) => {await action();}} as unknown as Lab, pair);
  expect(status).toBe(200);
});

test("an already-published pair cannot start the unobserved rebase", async () => {
  const published = "c".repeat(40);
  let waited = false;
  let passed = false;
  const fake: any = {
    createPair: async (name: string) => ({...pair, name}),
    snap: async () => ({svn: "5", git: published, maps: "1", records: "4", watermark: "5", gitWatermark: published, gitColumn: published}),
    commitGit: async () => {throw new Error("commit on a reused pair");},
    waitUntil: async () => {waited = true;},
    pass: () => {passed = true;},
  };
  await expect(runCase(fake, {id: "GIT-PUBLISHED-REBASE-UNSYNCED-UNOBSERVED", title: "fixture", suite: "full"})).rejects.toThrow(/fresh import/);
  expect(waited).toBe(false);
  expect(passed).toBe(false);
});

for (const id of ["PAIR-CANCEL-IMPORT", "PAIR-CANCEL-RESTART-GENERATION"]) {
  for (const fault of ["none", "missing-operation", "mismatch", "terminal", "retained-message", "http", "restart-started", "restart-unexpected-success", "restart-server-error", "restart-baseline", "restart-missing-hold", "restart-changed-operation", "restart-completed", "restart-hold-moved"]) {
    test(`${id} cancels only its started operation: ${fault}`, async () => {
      const calls: string[] = [];
      let passed = "";
      let marked = "";
      const fake: any = {
        createPair: async (name: string, options: unknown) => {
          expect(name).toBe(labPairKey(id));
          expect(options).toEqual({enable: false, settle: false});
          return fault === "missing-operation" ? {...pair, name} : {...pair, name, importOperationId: "op-live"};
        },
        api: async (method: string, path: string) => {
          calls.push(`${method} ${path}`);
          if (path === `/api/repos/${pair.id}/import/op-live/cancel`) {
            if (fault === "mismatch") return {status: 200, text: "", json: {ok: true, operation_id: "op-other", lifecycle: "cancel_requested", message: "cancellation durably requested; worker still stopping"}};
            if (fault === "terminal") return {status: 200, text: "", json: {ok: true, operation_id: "op-live", lifecycle: "completed", message: "terminal outcome retained"}};
            if (fault === "retained-message") return {status: 200, text: "", json: {ok: true, operation_id: "op-live", lifecycle: "cancel_requested", message: "terminal outcome retained"}};
            if (fault === "http") return {status: 400, text: "exact operation ID required", json: {error: "exact operation ID required"}};
            return {status: 200, text: "", json: {ok: true, operation_id: "op-live", lifecycle: "cancel_requested", message: "cancellation durably requested; worker still stopping"}};
          }
          if (method === "GET" && path === `/api/repos/${pair.id}/import/status`) {
            const afterRestart = calls.some(call => call === `POST /api/repos/${pair.id}/import`);
            if (!afterRestart && fault === "restart-missing-hold") return {status: 200, text: "", json: {lifecycle: "cancel_requested"}};
            if (!afterRestart && fault === "restart-changed-operation") return {status: 200, text: "", json: {operation_id: "op-other", lifecycle: "cancel_requested"}};
            if (!afterRestart && fault === "restart-completed") return {status: 200, text: "", json: {operation_id: "op-live", lifecycle: "completed"}};
            if (afterRestart && fault === "restart-hold-moved") return {status: 200, text: "", json: {operation_id: "op-other", lifecycle: "cancel_requested"}};
            return {status: 200, text: "", json: {operation_id: "op-live", lifecycle: "cancel_requested"}};
          }
          if (path === `/api/repos/${pair.id}/import`) {
            if (fault === "restart-started") return {status: 200, text: "", json: {operation_id: "op-new", lifecycle: "queued"}};
            if (fault === "restart-unexpected-success") return {status: 200, text: "repository import is active or held for reconciliation", json: {error: "repository import is active or held for reconciliation"}};
            if (fault === "restart-server-error") return {status: 500, text: "repository import is active or held for reconciliation", json: {error: "repository import is active or held for reconciliation"}};
            if (fault === "restart-baseline") return {status: 400, text: "repository import baseline is not pending", json: {error: "repository import baseline is not pending"}};
            return {status: 400, text: "repository import is active or held for reconciliation", json: {error: "repository import is active or held for reconciliation"}};
          }
          return {status: 400, text: "exact operation ID required", json: {error: "exact operation ID required"}};
        },
        pass: (reason: string) => {passed = reason;},
        mark: (status: string, reason: string) => {marked = `${status} ${reason}`;},
      };
      const run = runCase(fake, {id, title: "fixture", suite: "full"});
      if (fault === "missing-operation") {
        await expect(run).rejects.toThrow(/no import operation id/);
        expect(calls).toEqual([]);
        expect(passed).toBe("");
        expect(marked).toBe("");
      } else if (fault === "none" && id === "PAIR-CANCEL-IMPORT") {
        await run;
        expect(calls).toEqual([`POST /api/repos/${pair.id}/import/op-live/cancel`]);
        expect(passed).toBe("cancelled import op-live");
        expect(marked).toBe("");
      } else if (fault === "none" && id === "PAIR-CANCEL-RESTART-GENERATION") {
        await run;
        expect(calls).toEqual([
          `POST /api/repos/${pair.id}/import/op-live/cancel`,
          `GET /api/repos/${pair.id}/import/status`,
          `POST /api/repos/${pair.id}/import`,
          `GET /api/repos/${pair.id}/import/status`,
        ]);
        expect(passed).toContain("op-live");
        expect(passed).toContain("refused");
        expect(passed).toContain("400");
        expect(marked).toBe("");
      } else if (id === "PAIR-CANCEL-IMPORT" && fault.startsWith("restart-")) {
        await run;
        expect(calls).toEqual([`POST /api/repos/${pair.id}/import/op-live/cancel`]);
        expect(passed).toBe("cancelled import op-live");
      } else {
        await expect(run).rejects.toThrow();
        expect(passed).toBe("");
        expect(marked).toBe("");
        expect(calls[0]).toBe(`POST /api/repos/${pair.id}/import/op-live/cancel`);
      }
      expect(calls.some(call => /\/import\/cancel$/.test(call))).toBe(false);
    });
  }
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
