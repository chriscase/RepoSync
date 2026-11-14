import {execFileSync} from "node:child_process";
import {mkdtempSync, mkdirSync, writeFileSync, rmSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {test, expect} from "@playwright/test";
import {Lab, type Pair, type Snap} from "../../playwright/runtime.ts";
import {runCase} from "../../playwright/handlers.ts";

const parent: Pair = {id: "delete-parent", name: "deletefixture", svnUrl: "https://svn:8443/svn/fixture", gitRepo: "lab/fixture"};
const pin = "a".repeat(40);
const baseline: Snap = {svn: "4", git: pin, maps: "1", records: "1", watermark: "1", gitWatermark: pin, gitColumn: pin};
const faults = ["none", "root", "missing-git", "missing-svn", "opt-out-ref-delete", "opt-out-path-delete", "refusal", "warnings", "response-flags", "git-retained", "svn-retained", "parent-ref-changed", "sibling-ref-deleted", "parent-checkpoint-changed", "sibling-byte-changed", "registration-retained", "extra-svn-revision", "wrong-svn-delete"];

for (const fault of faults) {
  test(`actual remote-delete oracle rejects false success and collateral changes: ${fault}`, async () => {
    const children: Pair[] = []; let stage = 0, disposition = "";
    const calls: Array<{id: string; requested: boolean}> = []; const observations: any[] = [];
    const fake: any = {
      createPair: async () => parent,
      createChildPair: async (_parent: Pair, name: string) => {
        const child = {...parent, id: `child-${children.length}`, parentId: fault === "root" ? undefined : parent.id, gitBranch: name, svnBranch: `branches/${name}`}; children.push(child); return child;
      },
      gitRemoteHeads: async () => {
        const refs: Record<string, string> = {"refs/heads/main": stage === 2 && fault === "parent-ref-changed" ? "b".repeat(40) : pin};
        children.forEach((c, i) => {
          if (i === 1 && (fault === "missing-git" || stage === 2 && fault !== "git-retained")) return;
          if (i === 0 && stage > 0 && fault === "opt-out-ref-delete" || i === 2 && stage === 2 && fault === "sibling-ref-deleted") return;
          refs[`refs/heads/${c.gitBranch}`] = pin;
        }); return refs;
      },
      svnBranchNames: async () => children.filter((_c, i) => !(i === 1 && (fault === "missing-svn" || stage === 2 && fault !== "svn-retained") || i === 0 && stage > 0 && fault === "opt-out-path-delete")).map(c => c.gitBranch).sort(),
      snap: async () => ({...baseline, svn: stage === 2 ? fault === "extra-svn-revision" ? "6" : "5" : "4", watermark: stage === 2 && fault === "parent-checkpoint-changed" ? "5" : "1"}),
      svnBytes: async (p: Pair) => stage === 2 && fault === "sibling-byte-changed" && p.id === children[2].id ? "changed\n" : "seed\n",
      gitBytes: async () => "seed\n",
      repos: async () => [parent, ...children.filter((_c, i) => !(i === 0 && stage > 0 || i === 1 && stage === 2 && fault !== "registration-retained"))].map(p => ({id: p.id, parent_id: p.parentId, git_repo: p.gitRepo, svn_url: p.svnUrl, git_branch: p.gitBranch, svn_branch: p.svnBranch})),
      api: async (method: string, path: string) => {
        expect(method).toBe("DELETE"); const url = new URL(path, "http://lab.invalid");
        expect(url.searchParams.get("explicit_remote_deletion_opts")).toBe("true");
        const requested = url.searchParams.get("delete_git") === "true";
        expect(url.searchParams.get("delete_svn")).toBe(String(requested));
        const child = children[requested ? 1 : 0]; expect(url.pathname).toBe(`/api/repos/${child.id}/branch-pair`);
        calls.push({id: child.id, requested}); stage = requested ? 2 : 1;
        return {status: requested && fault === "refusal" ? 400 : 200, json: {ok: true, registration_listed: false,
          warnings: requested && fault === "warnings" ? ["failed to delete SVN"] : [], remote_deletion: {
            explicit_opts: true, delete_git_requested: requested && fault !== "response-flags", delete_svn_requested: requested,
            git_ref: requested ? child.gitBranch : "", svn_path: requested ? child.svnBranch : "",
          }}};
      },
      svnRevisionLog: async (_parent: Pair, revision: string) => {expect(revision).toBe("5"); return `<log><logentry revision="5"><paths><path action="D" kind="dir">/${fault === "wrong-svn-delete" ? children[2].svnBranch : children[1].svnBranch}</path></paths></logentry></log>`;},
      observation: (_name: string, value: any) => observations.push(value), pass: () => disposition = "PASS",
    };
    const run = runCase(fake, {id: "PAIR-REMOTE-DELETE-EXPLICIT", title: "delete", suite: "full"});
    if (fault === "none") {await run; expect(disposition).toBe("PASS"); expect(calls).toEqual([{id: "child-0", requested: false}, {id: "child-1", requested: true}]); expect(observations).toHaveLength(4);}
    else {await expect(run).rejects.toThrow(); expect(disposition).toBe("");}
    if (["root", "missing-git", "missing-svn"].includes(fault)) expect(calls).toHaveLength(0);
  });
}

for (const fault of ["transport", "bad-sha", "duplicate", "bad-entry", "unsafe-git", "unsafe-svn"]) {
  test(`remote inventory readers fail closed: ${fault}`, async () => {
    const fake: any = {observer: async () => {
      if (fault === "transport") throw new Error("provider transport or authentication failed");
      if (fault === "bad-sha") return `invalid\trefs/heads/main\n`;
      if (fault === "duplicate") return `${pin}\trefs/heads/main\n${pin}\trefs/heads/main\n`;
      return "unexpected-file\n";
    }};
    const run = ["bad-entry", "unsafe-svn"].includes(fault)
      ? Lab.prototype.svnBranchNames.call(fake, fault === "unsafe-svn" ? {...parent, svnUrl: "https://other.invalid/repo"} : parent)
      : Lab.prototype.gitRemoteHeads.call(fake, fault === "unsafe-git" ? {...parent, gitRepo: "lab/fixture;bad"} : parent);
    await expect(run).rejects.toThrow();
  });
}

test("actual Git and SVN inventory readers prove only selected ref/path deletion and keep errors fatal", () => {
  const root = mkdtempSync(join(tmpdir(), "lab-delete-inventory-"));
  const svn = join(root, "svn"), bare = join(root, "remote.git"), work = join(root, "git"), seed = join(root, "seed");
  const run = (cmd: string, args: string[], cwd?: string) => execFileSync(cmd, args, {cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"]});
  try {
    run("svnadmin", ["create", svn]); mkdirSync(join(seed, "trunk"), {recursive: true}); mkdirSync(join(seed, "branches")); writeFileSync(join(seed, "trunk/README.txt"), "seed\n");
    run("svn", ["import", seed, `file://${svn}`, "-m", "seed"]);
    for (const name of ["keep", "drop", "peer"]) run("svn", ["copy", `file://${svn}/trunk@1`, `file://${svn}/branches/${name}`, "-m", "copy"]);
    run("git", ["init", "--bare", "--object-format=sha1", bare]); run("git", ["init", "--initial-branch=main", "--object-format=sha1", work]); writeFileSync(join(work, "README.txt"), "seed\n");
    run("git", ["add", "README.txt"], work); run("git", ["-c", "user.name=Lab", "-c", "user.email=lab@lab.invalid", "commit", "-m", "seed"], work);
    run("git", ["push", bare, ...["main", "keep", "drop", "peer"].map(name => `main:refs/heads/${name}`)], work);
    const q = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;
    const fake: any = {observer: async (script: string) => execFileSync("bash", ["-c", script.replace("https://gitea:3000/lab/fixture.git", q(bare)).replace("https://svn:8443/svn/fixture", `file://${svn}`).replace('"$HOME/.svn"', q(join(root, "config")))], {encoding: "utf8", env: {...process.env, GIT_USER: "synthetic", GIT_TOKEN: "synthetic", SVN_PASSWORD: "synthetic"}, stdio: ["ignore", "pipe", "pipe"]})};
    return (async () => {
      const refs = await Lab.prototype.gitRemoteHeads.call(fake, parent); expect(Object.keys(refs).sort()).toEqual(["refs/heads/drop", "refs/heads/keep", "refs/heads/main", "refs/heads/peer"]);
      expect(await Lab.prototype.svnBranchNames.call(fake, parent)).toEqual(["drop", "keep", "peer"]);
      run("git", ["push", bare, ":refs/heads/drop"], work); run("svn", ["delete", `file://${svn}/branches/drop`, "-m", "delete"]);
      const expected = {...refs}; delete expected["refs/heads/drop"]; expect(await Lab.prototype.gitRemoteHeads.call(fake, parent)).toEqual(expected);
      expect(await Lab.prototype.svnBranchNames.call(fake, parent)).toEqual(["keep", "peer"]);
      const log = run("svn", ["log", "--xml", "-v", "-r", "5", `file://${svn}`]);
      expect([...log.matchAll(/<path\b([^>]*)>([^<]+)<\/path>/g)].map(m => ({action: m[1].match(/\baction="([A-Z])"/)?.[1], path: m[2]}))).toEqual([{action: "D", path: "/branches/drop"}]);
      rmSync(bare, {recursive: true}); await expect(Lab.prototype.gitRemoteHeads.call(fake, parent)).rejects.toThrow();
      run("svn", ["delete", `file://${svn}/branches`, "-m", "remove directory"]); await expect(Lab.prototype.svnBranchNames.call(fake, parent)).rejects.toThrow();
    })().finally(() => rmSync(root, {recursive: true, force: true}));
  } catch (error) {rmSync(root, {recursive: true, force: true}); throw error;}
});
