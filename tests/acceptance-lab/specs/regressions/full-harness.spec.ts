import {execFileSync} from "node:child_process";
import {mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {test, expect} from "@playwright/test";
import {runCase} from "../../playwright/handlers.ts";
import {Lab, reserveLoginAttempt, safeRelativePath, type Pair} from "../../playwright/runtime.ts";

test("login budget survives a failed worker and paces without retrying authentication", async () => {
  const root = mkdtempSync(join(tmpdir(), "lab-login-"));
  let time = 100_000;
  const waits: number[] = [];
  try {
    for (let i = 0; i < 8; i++) await reserveLoginAttempt(root, "run-one", () => time);
    // A new invocation reads the previous worker's file rather than a cache.
    await reserveLoginAttempt(root, "run-one", () => time, async ms => {waits.push(ms); time += ms;});
    expect(waits).toEqual([60_000, 1000]);
    expect(JSON.parse(readFileSync(join(root, "login-budget.json"), "utf8")).attempts).toEqual([161_000]);
    await expect(reserveLoginAttempt(root, "foreign-run", () => time)).rejects.toThrow("foreign");
    await expect(reserveLoginAttempt(root, "run-one", () => time - 1)).rejects.toThrow("backwards");
  } finally {rmSync(root, {recursive: true, force: true});}
});

test("dotfile reader paths allow hidden components and reject traversal or shell syntax", () => {
  for (const path of [".notes", "nested/.notes", "dir/a..b", ".config/name"]) expect(safeRelativePath(path)).toBe(path);
  for (const path of ["", ".", "..", "../escape", "dir/../escape", "/absolute", "dir//file", "name;id", "$(id)", "-option", "dir/./file"]) {
    expect(() => safeRelativePath(path)).toThrow("unsafe path");
  }
});

for (const failure of ["none", "healthy", "configuration", "throttle", "server", "observation", "restore", "recovery"]) {
  test(`LDAP bind proof ${failure === "none" ? "requires healthy controls, 401 and observed bind refusal" : `rejects ${failure} failure`}`, async () => {
    let passed = false, controls = 0, enabled = 0, observed = false;
    const good = {status: 200, json: {token: "synthetic-token", user: {username: "alice"}}};
    const fake = {
      alicePassword: "synthetic-alice", bindPassword: "synthetic-bind",
      enableLdap: async () => {enabled++; if (enabled === 2 && failure === "restore") throw new Error("restore failed");},
      login: async () => {
        controls++;
        if (controls === 1) return failure === "healthy" ? {status: 401, json: {}} : good;
        if (controls === 2) return {status: failure === "throttle" ? 400 : failure === "server" ? 500 : 401, json: {error: "fixture refusal"}};
        return failure === "recovery" ? {status: 401, json: {}} : good;
      },
      api: async (_method: string, _path: string, body: any) => {
        expect(body.url).toBe("ldaps://ldap:636");
        expect(body.bind_password).toBe("wrong-bind-password");
        return {status: failure === "configuration" ? 422 : 200, json: {}};
      },
      expectLdapBindRefusal: async () => {observed = true; if (failure === "observation") throw new Error("no bind refusal");},
      pass: () => {passed = true;},
    };
    const run = runCase(fake as unknown as Lab, {id: "LDAP-SERVICE-BIND-SEARCH", title: "fixture", suite: "full"});
    if (failure === "none") {await run; expect(passed).toBe(true); expect(controls).toBe(3); expect(observed).toBe(true);}
    else {await expect(run).rejects.toThrow(); expect(passed).toBe(false);}
    if (failure !== "healthy") expect(enabled).toBe(2);
  });
}

function vcsFixture() {
  const root = mkdtempSync(join(tmpdir(), "lab-vcs-"));
  const svn = join(root, "svn"), bare = join(root, "bare"), source = join(root, "source");
  mkdirSync(join(root, "work"));
  const command = (tool: string, args: string[], cwd = root) => execFileSync(tool, args, {cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"]});
  command("svnadmin", ["create", svn]);
  command("svn", ["mkdir", `file://${svn}/trunk`, "-m", "trunk"]);
  command("git", ["init", "--bare", "--initial-branch=main", bare]);
  command("git", ["clone", bare, source]);
  command("git", ["config", "user.email", "fixture@lab.invalid"], source);
  command("git", ["config", "user.name", "Fixture"], source);
  writeFileSync(join(source, "README.txt"), "seed\n");
  command("git", ["add", "."], source); command("git", ["commit", "-m", "seed"], source); command("git", ["push", "origin", "main"], source);
  const pair: Pair = {id: "fixture", name: "fixture", gitRepo: "lab/fixture", svnUrl: `file://${svn}`};
  const fake = {
    observer: async (script: string) => command("sh", ["-c", script.replaceAll("https://gitea:3000/lab/fixture.git", `file://${bare}`).replaceAll("https://gitea:3000/missing/repo.git", `file://${root}/no-git`).replaceAll("/work/", `${root}/work/`)], root),
    svnBytesOrMissing: (p: Pair, path: string) => Lab.prototype.svnBytesOrMissing.call(fake as unknown as Lab, p, path),
    gitBytesOrMissing: (p: Pair, path: string) => Lab.prototype.gitBytesOrMissing.call(fake as unknown as Lab, p, path),
    commitGit: (p: Pair, script: string, ref?: string, publish?: boolean): Promise<string> => Lab.prototype.commitGit.call(fake as unknown as Lab, p, script, ref, publish),
  };
  // The actual observer scripts use these variables; no server is contacted.
  const observer = fake.observer;
  fake.observer = script => observer(`export HOME='${root}' SVN_PASSWORD=synthetic GIT_USER=fixture GIT_TOKEN=synthetic\n${script}`);
  return {root, svn, bare, source, pair, fake, command};
}

test("real SVN/Git observers distinguish delayed paths, bytes and broken remotes", async () => {
  const f = vcsFixture();
  try {
    expect(await f.fake.svnBytesOrMissing(f.pair, ".notes")).toBeNull();
    expect(await f.fake.gitBytesOrMissing(f.pair, ".notes")).toBeNull();
    await expect(Lab.prototype.svnBytes.call(f.fake as unknown as Lab, f.pair, ".notes")).rejects.toThrow("absent");
    const work = join(f.root, "svn-source");
    f.command("svn", ["checkout", `${f.pair.svnUrl}/trunk`, work]);
    writeFileSync(join(work, ".notes"), "SVN dot bytes\n");
    f.command("svn", ["add", ".notes"], work); f.command("svn", ["commit", "-m", "dot"], work);
    writeFileSync(join(f.source, ".notes"), "Git dot bytes\n");
    f.command("git", ["add", ".notes"], f.source); f.command("git", ["commit", "-m", "dot"], f.source); f.command("git", ["push", "origin", "main"], f.source);
    expect(await f.fake.svnBytesOrMissing(f.pair, ".notes")).toBe("SVN dot bytes\n");
    expect(await f.fake.gitBytesOrMissing(f.pair, ".notes")).toBe("Git dot bytes\n");
    await expect(f.fake.svnBytesOrMissing({...f.pair, svnUrl: `file://${f.root}/broken`}, ".notes")).rejects.toThrow();
    await expect(f.fake.gitBytesOrMissing({...f.pair, id: "broken", gitRepo: "missing/repo"}, ".notes")).rejects.toThrow();
  } finally {rmSync(f.root, {recursive: true, force: true});}
});

test("historical SVN reads distinguish a path before birth from its later bytes", async () => {
  const f = vcsFixture();
  try {
    const revision = await Lab.prototype.commitSvn.call(f.fake as unknown as Lab, f.pair,
      "printf 'first\\n' > first.txt\nsvn add first.txt\n");
    const later = await Lab.prototype.commitSvn.call(f.fake as unknown as Lab, f.pair,
      "printf 'second\\n' > second.txt\nsvn add second.txt\n");
    expect(await Lab.prototype.svnBytesOrMissing.call(f.fake as unknown as Lab, f.pair, "second.txt", revision)).toBeNull();
    expect(await Lab.prototype.svnBytesOrMissing.call(f.fake as unknown as Lab, f.pair, "first.txt", revision)).toBe("first\n");
    expect(await Lab.prototype.svnBytesAtRevision.call(f.fake as unknown as Lab, f.pair, "first.txt", revision)).toBe("first\n");
    expect(await Lab.prototype.svnBytesAtRevision.call(f.fake as unknown as Lab, f.pair, "second.txt", later)).toBe("second\n");
    await expect(Lab.prototype.svnBytesAtRevision.call(f.fake as unknown as Lab, f.pair, "second.txt", revision)).rejects.toThrow();
  } finally {rmSync(f.root, {recursive: true, force: true});}
});

test("historical SVN reads survive deletion and recreation at HEAD", async () => {
  const f = vcsFixture();
  try {
    const old = await Lab.prototype.commitSvn.call(f.fake as unknown as Lab, f.pair,
      "printf 'original\\n' > replaced.txt\nsvn add replaced.txt\n");
    await Lab.prototype.commitSvn.call(f.fake as unknown as Lab, f.pair, "svn rm replaced.txt\n");
    const replacement = await Lab.prototype.commitSvn.call(f.fake as unknown as Lab, f.pair,
      "printf 'replacement\\n' > replaced.txt\nsvn add replaced.txt\n");
    expect(await f.fake.svnBytesOrMissing(f.pair, "replaced.txt")).toBe("replacement\n");
    expect(await Lab.prototype.svnBytesOrMissing.call(f.fake as unknown as Lab, f.pair, "replaced.txt", old)).toBe("original\n");
    expect(await Lab.prototype.svnBytesAtRevision.call(f.fake as unknown as Lab, f.pair, "replaced.txt", old)).toBe("original\n");
    expect(await Lab.prototype.svnBytesAtRevision.call(f.fake as unknown as Lab, f.pair, "replaced.txt", replacement)).toBe("replacement\n");
  } finally {rmSync(f.root, {recursive: true, force: true});}
});

test("historical SVN reads keep broken remotes and invalid revisions fatal", async () => {
  const f = vcsFixture();
  try {
    const broken = {...f.pair, svnUrl: `file://${f.root}/broken`};
    await expect(Lab.prototype.svnBytesOrMissing.call(f.fake as unknown as Lab, broken, "first.txt", "1")).rejects.toThrow();
    await expect(Lab.prototype.svnBytesAtRevision.call(f.fake as unknown as Lab, broken, "first.txt", "1")).rejects.toThrow();
    for (const revision of ["HEAD", "-1", "1;id"]) {
      await expect(Lab.prototype.svnBytesOrMissing.call(f.fake as unknown as Lab, f.pair, "first.txt", revision)).rejects.toThrow("invalid SVN read revision");
      await expect(Lab.prototype.svnBytesAtRevision.call(f.fake as unknown as Lab, f.pair, "first.txt", revision)).rejects.toThrow("invalid SVN read revision");
    }
  } finally {rmSync(f.root, {recursive: true, force: true});}
});

test("local reset and rebase leave the actual published Git tip unchanged", async () => {
  const f = vcsFixture();
  try {
    await f.fake.commitGit(f.pair, "printf 'base\\n' > base.txt\ngit add base.txt\ngit commit -m base\n");
    const tip = f.command("git", ["rev-parse", "main"], f.bare).trim();
    for (const script of ["git reset --mixed HEAD~1\n", "git checkout -b local-only HEAD~1\nprintf 'local\\n' > local.txt\ngit add local.txt\ngit commit -m local\ngit rebase main\n"]) {
      const local = await Lab.prototype.commitGitLocal.call(f.fake as unknown as Lab, f.pair, script);
      expect(local).not.toBe(tip);
      expect(f.command("git", ["rev-parse", "main"], f.bare).trim()).toBe(tip);
    }
  } finally {rmSync(f.root, {recursive: true, force: true});}
});

test("SVN symlink fixture commits a real svn:special revision", async () => {
  const f = vcsFixture();
  try {
    const revision = await Lab.prototype.commitSvn.call(f.fake as unknown as Lab, f.pair,
      "printf 'target\\n' > README.txt\nsvn add README.txt\nln -s README.txt link.txt\ntest -L link.txt\nsvn add link.txt\nsvn propget svn:special link.txt | grep -F '*'\n");
    expect(Number(revision)).toBeGreaterThan(1);
    expect(f.command("svn", ["propget", "svn:special", `${f.pair.svnUrl}/trunk/link.txt`]).trim()).toBe("*");
    expect(f.command("svn", ["cat", `${f.pair.svnUrl}/trunk/link.txt`])).toBe("link README.txt");
  } finally {rmSync(f.root, {recursive: true, force: true});}
});

for (const fault of ["none", "admission", "root-row", "wrong-remotes", "missing-pins"]) {
  test(`child fixture ${fault === "none" ? "establishes parent and branch pins" : `rejects ${fault}`}`, async () => {
    const parent: Pair = {id: "parent", name: "fixture", svnUrl: "https://svn:8443/svn/fixture", gitRepo: "lab/fixture"};
    const row = {id: "child", parent_id: fault === "root-row" ? undefined : "parent", svn_url: parent.svnUrl,
      svn_branch: fault === "wrong-remotes" ? "trunk" : "branches/child", git_repo: parent.gitRepo, git_branch: "child"};
    let published = false;
    const fake = {
      commitGit: async (_p: Pair, script: string) => {expect(script).toContain("git push origin refs/heads/child:refs/heads/child");},
      api: async (_m: string, path: string, body: any) => {
        expect(path).toBe("/api/repos/parent/branches"); expect(body.dry_run).toBe(false); expect(body.auto_create_git_branch).toBe(false);
        published = true; return {status: fault === "admission" ? 400 : 200, text: "fixture refusal"};
      },
      repos: async () => [row], snap: async () => ({git: fault === "missing-pins" ? "" : "a".repeat(40), svn: "2"}), info: {annotations: []},
    };
    const run = Lab.prototype.createChildPair.call(fake as unknown as Lab, parent, "child");
    if (fault === "none") expect(await run).toMatchObject({id: "child", parentId: "parent", svnBranch: "branches/child", gitBranch: "child"});
    else await expect(run).rejects.toThrow();
    expect(published).toBe(true);
  });
}
