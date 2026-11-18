import {execFileSync} from "node:child_process";
import {mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {test, expect} from "@playwright/test";
import {Lab, archive199CanonicalXml, type Pair, type Snap} from "../../playwright/runtime.ts";
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

// #199 additive discrimination only; the legacy tests above remain unchanged.
import { archive199AssertPreserved, archive199AssertRestart, archive199AssertRefusal, archive199AssertUnauthorized } from "../../playwright/handlers.ts";

function archive199Evidence(side: "git" | "svn") {
  const row = {id:"archive-child",parent_id:"parent",enabled:1,sync_status:"idle",last_svn_rev:4,last_git_sha:pin,svn_branch:"branches/dev",git_branch:"dev"};
  const before:any = {registration:[row],maps:[{svn_rev:4,git_sha:pin}],records:[{id:"mapped"}],audit:[],archiveJournal:[],localCustody:{entries:3,sha256:"a".repeat(64)},git:{present:true,tip:pin,tree:"git-tree",history:"complete-git-history"},svn:{present:true,tree:"svn-tree",properties:"props",history:"complete-svn-history",branches:["dev"]}};
  const after=structuredClone(before);after.registration[0]={...row,enabled:0,sync_status:"archived"};after[side].present=false;
  after.audit=[{action:"branch_pair_archived",author:"admin",svn_rev:4,git_sha:pin,details:JSON.stringify({deleted_side:side,survivor:"untouched"})}];
  return {before,after};
}
for(const side of ["git","svn"] as const) for(const fault of ["none","missing-survivor","changed-history","lost-mapping","local-cleanup","reenabled","wrong-audit-side"]){
  test(`#199 ${side} archive oracle rejects ${fault}`,()=>{
    const {before,after}=archive199Evidence(side);const survivor=side==="git"?"svn":"git";
    if(fault==="missing-survivor")after[survivor].present=false;
    if(fault==="changed-history")after[survivor].history="truncated";
    if(fault==="lost-mapping")after.maps=[];
    if(fault==="local-cleanup")after.localCustody.entries=0;
    if(fault==="reenabled")after.registration[0].enabled=1;
    if(fault==="wrong-audit-side")after.audit[0].details=JSON.stringify({deleted_side:survivor,survivor:"untouched"});
    if(fault==="none")archive199AssertPreserved(before,after,side);
    else expect(()=>archive199AssertPreserved(before,after,side)).toThrow();
  });
}
for(const fault of ["none","no-restart","no-cycle","no-watermark","no-svn-cycle"]){
  test(`#199 restart oracle rejects ${fault}`,()=>{
    const witness={beforeStartedAt:"2026-01-01T00:00:00Z",afterStartedAt:"2026-01-01T00:01:00Z",before:{...baseline},after:{...baseline,git:"b".repeat(40),gitWatermark:"b".repeat(40),svn:"5"}};
    if(fault==="no-restart")witness.afterStartedAt=witness.beforeStartedAt;
    if(fault==="no-cycle")witness.after.git=witness.before.git;
    if(fault==="no-watermark")witness.after.gitWatermark=pin;
    if(fault==="no-svn-cycle")witness.after.svn=witness.before.svn;
    if(fault==="none")archive199AssertRestart(witness);else expect(()=>archive199AssertRestart(witness)).toThrow();
  });
}
for(const kind of ["busy","held"] as const) for(const fault of ["none","false-reason","stale-review","no-witness","foreign-journal","inactive-journal","disabled","new-intent"]){
  test(`#199 ${kind} refusal oracle rejects ${fault}`,()=>{
    const {before}=archive199Evidence("git");const evidence:any={kind,before,after:structuredClone(before),response:{status:409,json:{error:kind==="busy"?"writer is busy":"reconciliation held"}},witness:{owner:"owned",runId:"isolated-run",requestId:"request",method:"MERGE",target:"/svn/owned/!svn/me",phase:"before-forward",matched:true,reason:kind==="busy"?"writer running":"reconciliation held",producerJournal:{id:"writer",repo_id:"archive-child",state:kind==="busy"?"running":"reconciliation_required"}}};
    if(fault==="false-reason")evidence.response.json.error="unavailable remote";
    if(fault==="stale-review")evidence.response.json.error="busy but stale review";
    if(fault==="no-witness")evidence.witness.matched=false;
    if(fault==="foreign-journal")evidence.witness.producerJournal.repo_id="other";
    if(fault==="inactive-journal")evidence.witness.producerJournal.state="completed";
    if(fault==="disabled")evidence.after.registration[0].enabled=0;
    if(fault==="new-intent")evidence.after.archiveJournal.push({id:"new-archive"});
    if(fault==="none")archive199AssertRefusal(evidence,kind);else expect(()=>archive199AssertRefusal(evidence,kind)).toThrow();
  });
}
for(const fault of ["none","wrong-user","cached-admin","visible-control","wrong-refusal","mutation"]){
  test(`#199 LDAP archive oracle rejects ${fault}`,()=>{
    const {before}=archive199Evidence("git");const evidence:any={user:{username:"alice",role:"viewer"},uiControlsAbsent:true,review:{status:403,json:{error:"admin access required"}},approve:{status:403,json:{error:"admin access required"}},before,after:structuredClone(before)};
    if(fault==="wrong-user")evidence.user.username="bob";
    if(fault==="cached-admin")evidence.user.role="admin";
    if(fault==="visible-control")evidence.uiControlsAbsent=false;
    if(fault==="wrong-refusal")evidence.approve.status=401;
    if(fault==="mutation")evidence.after.maps=[];
    if(fault==="none")archive199AssertUnauthorized(evidence);else expect(()=>archive199AssertUnauthorized(evidence)).toThrow();
  });
}


test("#199 XML canonicalization ignores only lexical attribute order", () => {
  const a = '<log><logentry revision="2"><paths><path action="A" copyfrom-rev="1" copyfrom-path="/trunk">/branches/dev</path></paths><msg> keep whitespace </msg><!--history--></logentry></log>';
  const b = a.replace('action="A" copyfrom-rev="1" copyfrom-path="/trunk"', 'copyfrom-path="/trunk" action="A" copyfrom-rev="1"');
  const canonical = archive199CanonicalXml(a, "log");
  expect(archive199CanonicalXml(b, "log")).toBe(canonical);
  for (const changed of [a.replace('revision="2"', 'revision="3"'), a.replace('action="A"', 'action="D"'), a.replace('copyfrom-rev="1"', 'copyfrom-rev="0"'), a.replace('/trunk', '/other'), a.replace('/branches/dev', '/branches/other'), a.replace(' keep whitespace ', 'keep whitespace'), a.replace('<!--history-->', ''), a.replace('<paths>', '<paths>\n')]) {
    expect(archive199CanonicalXml(changed, "log")).not.toBe(canonical);
  }
});

test("#199 XML canonicalization retains element order, properties and complete history", () => {
  for (const [root, one, two] of [["log", '<logentry revision="2"><msg>new</msg></logentry>', '<logentry revision="1"><msg>old</msg></logentry>'], ["properties", '<target path="a"><property name="x">one</property></target>', '<target path="b"><property name="y">two</property></target>']] as const) {
    const canonical = archive199CanonicalXml(`<${root}>${one}${two}</${root}>`, root);
    expect(archive199CanonicalXml(`<${root}>${two}${one}</${root}>`, root)).not.toBe(canonical);
    expect(archive199CanonicalXml(`<${root}>${one}</${root}>`, root)).not.toBe(canonical);
    expect(archive199CanonicalXml(`<${root}>${one}${two.replace(root === "log" ? "old" : "two", "changed")}</${root}>`, root)).not.toBe(canonical);
  }
});

test("#199 XML canonicalization rejects malformed, entity-bearing and wrong-root receipts", () => {
  for (const xml of ['<log>', '<properties/>', '<!DOCTYPE log [<!ENTITY x "value">]><log>&x;</log>', '<log>' + 'x'.repeat(8_000_001) + '</log>']) {
    expect(() => archive199CanonicalXml(xml, "log")).toThrow();
  }
});


test("#199 parent imports disabled then admits global profile before enabling publication", async () => {
  const calls: any[] = [];
  const fake: any = {createPair: async (...args: any[]) => {calls.push(args); return parent;}, archive199AdmitPublicationScheduler:()=>calls.push("profile"), archive199ConfigureScheduling: async (...args: any[]) => calls.push(args)};
  expect(await Lab.prototype.archive199CreateParent.call(fake, "fixture")).toEqual(parent);
  expect(calls).toEqual([["fixture", {enable:false}], "profile", [parent, true, 3600]]);
});

for (const fault of ["none", "still-enabled", "busy", "held", "writer-window", "malformed-window", "running-journal", "missing-journal", "foreign-journal", "wrong-checkpoint"]) {
  test(`#199 quiet barrier refuses ${fault}`, async () => {
    const child = {...parent,id:"quiet-child",parentId:parent.id};
    const status: any = {observation:{repository_id:child.id,evidence_complete:true,registry_busy:fault === "busy",operations:fault === "held" ? [{state:"reconciliation_required",held:true}] : []}};
    const docs: any[] = fault === "missing-journal" ? [] : [{id:"producer",repo_id:fault === "foreign-journal" ? "other" : child.id,state:fault === "running-journal" ? "running" : "completed",source_git_sha:pin}];
    if (fault === "writer-window" || fault === "malformed-window") docs.push({id:"window",repo_id:child.id,...(fault === "writer-window" ? {phase:"preparing"} : {})});
    let observed = false;
    const fake: any = {api:async (method:string,path:string)=>{expect(method).toBe("GET");expect([`/api/status?repo_id=${child.id}`,`/api/repos/${child.id}`]).toContain(path);return {status:200,json:path===`/api/status?repo_id=${child.id}`?status:{enabled:fault === "still-enabled",poll_interval_secs:3600}};},
      sqlRows:async()=>docs.map(value=>({key:value.id==="window"?"pair_refresh_writer_window_v1:document:window":"git_to_svn_commit_v1:document:producer",value:JSON.stringify(value)})), snap:async()=>({...baseline,gitWatermark:fault === "wrong-checkpoint"?"b".repeat(40):pin}),
      assertSingleReceipt:async()=>{}, observation:(name:string)=>{if(name === "archive199-quiet-writer")observed=true;}, waitUntil:async(check:()=>Promise<boolean>)=>{if(!await check())throw new Error("not quiet");}};
    const pending = Lab.prototype.archive199WaitQuiet.call(fake,child,pin);
    if (fault === "none") {await pending; expect(observed).toBe(true);} else {await expect(pending).rejects.toThrow();expect(observed).toBe(false);}
  });
}

for (const primary of [true, false]) test(`#199 gate cleanup preserves primary=${primary}`, async () => {
  const original = new Error("primary producer failure"), cleanup = new Error("gate cleanup failure");
  const child: Pair = {id:"owned-child",parentId:"parent",name:"owned",gitRepo:"lab/owned",gitBranch:"archive199-busy",svnBranch:"branches/archive199-busy",svnUrl:"https://svn:8443/svn/owned"};
  let gate: any, gets=0;
  const annotations: any[]=[];
  const fake: any = {project:"rsacc-owned-run",info:{annotations},
    admin:async()=>{},archive199ConfigureProxyFraming:async()=>{},rememberSecret:()=>{},
    api:async(method:string,path:string,body:any)=>path.endsWith("/removal/review")?{status:200,json:{review_pin:"pin"}}:path.endsWith("/removal/approve")?{status:409,json:{error:"writer busy"}}:{status:200,json:{id:child.id,svn_url:body.svn_url}},
    archive199GateControl:async(body:any)=>{
      if(body){gate={...body,state:"blocked",matched_at:1,witnessed_at:1};return gate;}
      gets++;if(primary || gets===3)throw cleanup;return gate;
    },
    commitGit:async()=>{if(primary)throw original;return pin;},triggerGit:async()=>200,
    waitUntil:async(check:()=>Promise<boolean>)=>{expect(await check()).toBe(true);},
    sqlRows:async()=>[{value:JSON.stringify({id:"producer",repo_id:child.id,source_git_sha:pin,state:"running"})}],
    archive199Observe:async()=>({preserved:true}),observation:()=>{}
  };
  await expect(Lab.prototype.archive199WitnessRefusal.call(fake,child,"busy")).rejects.toBe(primary?original:cleanup);
  expect(annotations.map(x=>x.type)).toEqual(primary?["labCleanupFailure"]:[]);
});

import {readFileSync as readProxyFixture, symlinkSync as symlinkProxyFixture} from "node:fs";
for (const original of ["", "# original\n[groups]\n# empty\n\n[global]\nssl-authority-files = /certs/ca.crt\n", "[groups]", "[global]\r\nstore-passwords = no"]) {
  test("#199 proxy framing preserves file bytes " + JSON.stringify(original), () => {
    const output = Lab.prototype.archive199ProxyConfig(original);
    expect(Lab.prototype.archive199ProxyConfig(output)).toBe(output);
    const suffix = "\n# archive199 owned proxy framing\n[archive199-proxy]\nhttp-chunked-requests = no\n";
    let restored = output.slice(0, -suffix.length).replace("archive199-proxy = fault-proxy\n", "");
    if (!original.includes("[groups]")) restored = restored.slice(0, -"\n[groups]\n".length);
    else if (original === "[groups]") restored = restored.slice(0, -1);
    expect(restored).toBe(original);
  });
}
for (const bad of ["[groups]\nother = *\n", "[groups]\n[groups]\n", "[archive199-proxy]\n", "[ groups ]\nother=svn\n", "[groups] trailing\n"]) {
  test("#199 proxy framing rejects collision " + JSON.stringify(bad), () => {
    expect(() => Lab.prototype.archive199ProxyConfig(bad)).toThrow();
  });
}
test("#199 proxy framing rejects idempotence tampering", () => {
  const output = Lab.prototype.archive199ProxyConfig("[groups]\n# original\n");
  for (const bad of [output.replace("fault-proxy", "*"), output+"extra=true\n", output.replace("requests = no", "requests = yes"), output.replace("[groups]", "[groups]\nother=svn")]) {
    expect(() => Lab.prototype.archive199ProxyConfig(bad)).toThrow();
  }
});
for (const shape of ["new", "existing", "directory-symlink", "file-symlink"]) {
  test("#199 proxy file admission " + shape, () => {
    const dir = mkdtempSync(join(tmpdir(), "archive199-config-admission-"));
    try {
      const outside = join(dir, "outside"); mkdirSync(outside);
      writeFileSync(join(outside,"servers"),"untouched");
      if (shape === "directory-symlink") symlinkProxyFixture(outside,join(dir,".subversion"));
      else if (shape !== "new") {
        mkdirSync(join(dir,".subversion"));
        if (shape === "file-symlink") symlinkProxyFixture(join(outside,"servers"),join(dir,".subversion","servers"));
        else writeFileSync(join(dir,".subversion","servers"),"[groups]\n");
      }
      const source = readProxyFixture(new URL("../../playwright/runtime.ts",import.meta.url),"utf8");
      const guard = source.split("runuser -u reposync -- sh -s <<'ARCHIVE199_CONFIG'\n")[1].split("\n"+String.fromCharCode(96)+";")[0]
        .replace("cd /var/lib/reposync",'cd "$FIXTURE_HOME"');
      const adapter = process.platform === "darwin" ? 'stat() { case "$2" in %u) /usr/bin/stat -f %u "$3";; %h) /usr/bin/stat -f %l "$3";; %s) /usr/bin/stat -f %z "$3";; *) return 1;; esac; }\n' : "";
      const run = () => execFileSync("sh",["-c",adapter+guard],{env:{...process.env,FIXTURE_HOME:dir},timeout:5000,stdio:"pipe"});
      if (shape.includes("symlink")) expect(run).toThrow(); else run();
      expect(readProxyFixture(join(outside,"servers"),"utf8")).toBe("untouched");
    } finally {rmSync(dir,{recursive:true,force:true});}
  });
}




test("#199 both guard children publish before any parent or guard scheduling work", async () => {
  const calls: string[] = [];
  const fake: any = {archive199CreateParent:async()=>parent,
    archive199CreateChildUi:async(_p:Pair,name:string)=>{calls.push(`publish:${name}`);return {...parent,id:name,parentId:parent.id};},
    archive199ConfigureScheduling:async(pair:Pair)=>{calls.push(`schedule:${pair.id}`);},
    archive199WitnessRefusal:async()=>{throw new Error("reached real guard stage");}};
  await expect(runCase(fake,{id:"LAB_PAIR_DELETE_SIDE_REFUSED_DURING_SYNC",title:"guard",suite:"full"})).rejects.toThrow("reached real guard stage");
  expect(calls).toEqual(["publish:archive199-busy","publish:archive199-held","schedule:delete-parent","schedule:archive199-busy"]);
});

test("#199 happy path disables after real producer then verifies quiet before snapshot", async () => {
  const calls: string[] = [], child = {...parent,id:"child",parentId:parent.id};
  const fake: any = {archive199CreateParent:async()=>parent,archive199CreateChildUi:async()=>child,
    archive199ConfigureScheduling:async(_p:Pair,enabled:boolean,interval:number)=>calls.push(`enabled:${enabled}:interval:${interval}`),
    syncGitChange:async()=>{calls.push("receipt");return baseline;},
    archive199WaitQuiet:async(_p:Pair,source:string)=>{expect(source).toBe(pin);calls.push("quiet:disabled");},
    archive199Observe:async()=>{calls.push("snapshot");throw new Error("reached snapshot");}};
  await expect(runCase(fake,{id:"LAB_PAIR_DELETE_GIT_SIDE_ONLY",title:"archive",suite:"full"})).rejects.toThrow("reached snapshot");
  expect(calls).toEqual(["enabled:false:interval:3600","enabled:true:interval:30","receipt","enabled:false:interval:3600","quiet:disabled","snapshot"]);
});


test("#199 enabled publication requires the explicit exact generated global timer", () => {
  const config = "[daemon]\npoll_interval_secs = 3600\ndata_dir = \"/var/lib/reposync\"\n[svn]\nurl = \"https://svn:8443/svn/fixture\"\n";
  expect(Lab.archive199SchedulerInterval(config, "1")).toBe(3600);
  for (const changed of [config.replace("3600", "30"), config.replace("3600", "3601"), config + "[daemon]\npoll_interval_secs = 3600\n", config.replace("data_dir", "poll_interval_secs = 3600\ndata_dir"), config.replace("[daemon]", "[other]")]) expect(() => Lab.archive199SchedulerInterval(changed, "1")).toThrow();
  for (const flag of [undefined, "", "true", "default"]) expect(() => Lab.archive199SchedulerInterval(config, flag)).toThrow();
});

test("#199 inadmissible scheduler profile never enables publication parent", async () => {
  let enabled = false;
  const fake:any={createPair:async()=>parent,archive199AdmitPublicationScheduler:()=>{throw new Error("profile refused");},archive199ConfigureScheduling:async()=>{enabled=true;}};
  await expect(Lab.prototype.archive199CreateParent.call(fake,"fixture")).rejects.toThrow("profile refused");
  expect(enabled).toBe(false);
});


test("#199 enabled publication rejects parallel workers and expired/future daemon identity", () => {
  const now = Date.parse("2026-11-18T01:00:00Z");
  expect(Lab.archive199PublicationBudget(new Date(now-1000).toISOString(), "1", now)).toBe(3599000);
  for (const age of [-1, 1800000, 3600000]) expect(() => Lab.archive199PublicationBudget(new Date(now-age).toISOString(), "1", now)).toThrow();
  for (const workers of [undefined, "", "0", "2"]) expect(() => Lab.archive199PublicationBudget(new Date(now).toISOString(), workers, now)).toThrow();
  expect(() => Lab.archive199PublicationBudget("not-a-time", "1", now)).toThrow();
});


test("#199 launcher manual scheduler profile is explicit and leaves default unchanged", () => {
  const launcher = readFileSync(new URL("../../../../scripts/acceptance-lab.sh", import.meta.url), "utf8");
  const marker = `python3 - "$RUN_DIR/daemon.toml" "$LAB_UI_PORT" <<'PY'\n`;
  const rest = launcher.split(marker);
  expect(rest).toHaveLength(2);
  const source = rest[1].split("\nPY\n")[0];
  const template = '[daemon]\npoll_interval_secs = 30\n[web]\nadmin_password_env = "REPOSYNC_ADMIN_PASSWORD"\n';
  const root = mkdtempSync(join(tmpdir(), "archive199-render-"));
  try {
    const path = join(root, "daemon.toml");
    for (const flag of ["", "1", "invalid"]) {
      writeFileSync(path, template);
      const run = () => execFileSync("python3", ["-c", source, path, "51234"], {env: {...process.env, ACCEPTANCE_LAB_ARCHIVE199_MANUAL_POLL: flag}, timeout: 5000, stdio: "pipe"});
      if (flag === "invalid") { expect(run).toThrow(); expect(readFileSync(path, "utf8")).toBe(template); continue; }
      run();
      const rendered = readFileSync(path, "utf8");
      expect(rendered).toContain(`poll_interval_secs = ${flag === "1" ? 3600 : 30}\n`);
      expect(rendered).toContain('cors_origins = ["http://127.0.0.1:51234"]');
    }
    writeFileSync(path, template.replace("poll_interval_secs = 30", "poll_interval_secs = 60"));
    expect(() => execFileSync("python3", ["-c", source, path, "51234"], {env: {...process.env, ACCEPTANCE_LAB_ARCHIVE199_MANUAL_POLL: "1"}, timeout: 5000, stdio: "pipe"})).toThrow();
  } finally { rmSync(root, {recursive: true, force: true}); }
});

for (const fault of ["none", "busy", "window", "unscoped", "malformed", "running", "foreign", "incomplete", "identity"]) {
 test(`#199 post-refusal drain preserves holds and rejects unsafe evidence: ${fault}`, async () => {
  const child:Pair={...parent,id:"guard-child",parentId:parent.id,gitBranch:"archive199-held",svnBranch:"branches/archive199-held"};
  const row={id:child.id,parent_id:parent.id,git_branch:child.gitBranch,svn_branch:child.svnBranch,git_repo:child.gitRepo,svn_url:child.svnUrl.replace("https://svn:","https://fault-proxy:"),enabled:false};
  const calls:string[]=[];
  const fake:any={
   api:async(method:string,path:string)=>{expect(method).toBe("GET");return path.includes("/api/status")?{status:200,json:{observation:{repository_id:fault==="foreign"?"foreign":child.id,evidence_complete:fault!=="incomplete",registry_busy:fault==="busy",operations:[{state:fault==="running"?"running":"reconciliation_required",held:fault!=="running"}]}}}:{status:200,json:{...row,svn_url:fault==="identity"?"https://foreign.invalid":row.svn_url}};},
   archive199ConfigureScheduling:async(pair:Pair,enabled:boolean,interval:number)=>{expect(pair.svnUrl).toBe(row.svn_url);expect(enabled).toBe(false);expect(interval).toBe(3600);calls.push("disabled");},
   sqlRows:async(sql:string)=>{expect(sql).toMatch(/^SELECT /);return ["window","unscoped","malformed"].includes(fault)?[{key:"pair_refresh_writer_window_v1:document:w",value:JSON.stringify({id:"w",phase:fault==="malformed"?"unknown":"preparing",repo_id:fault==="unscoped"?null:child.id})}]:[];},
   waitUntil:async(fn:()=>Promise<boolean>,timeout:number)=>{expect(timeout).toBe(90000);if(!await fn())throw new Error("not drained");},
   observation:(name:string,evidence:any)=>{expect(name).toBe("archive199-post-refusal-drain");expect(evidence.durableHoldsPreserved).toBe(true);calls.push("observed");}
  };
  const result=Lab.prototype.archive199DrainGuard.call(fake,child);
  if(fault==="none"){await result;expect(calls).toEqual(["disabled","observed"]);}else{await expect(result).rejects.toThrow();expect(calls).not.toContain("observed");if(fault==="identity")expect(calls).toEqual([]);}
 });
}


for (const stage of ["status-admission", "disabled-config"]) test(`#199 quiet failure retains bounded diagnostic and original error: ${stage}`, async () => {
  const original = new Error("original timeout");
  const receipts: any[] = [];
  const status = {observation:{repository_id:"diag-child",evidence_complete:stage!=="status-admission",registry_busy:false,operations:[]}};
  const fake:any={api:async(_method:string,path:string)=>({status:200,json:path.includes("/api/status")?status:{id:"diag-child",enabled:true,password:"DO-NOT-CAPTURE"}}),sqlRows:async()=>[],
    waitUntil:async(fn:()=>Promise<boolean>)=>{expect(await fn()).toBe(false);throw original;},
    observation:(name:string,body:any)=>receipts.push({name,body})};
  await expect(Lab.prototype.archive199WaitQuiet.call(fake,{...parent,id:"diag-child"})).rejects.toBe(original);
  expect(receipts).toHaveLength(1);expect(receipts[0].name).toBe("archive199-quiet-failure");expect(receipts[0].body.stage).toBe(stage);expect(receipts[0].body.status).toEqual(status);
  expect(JSON.stringify(receipts)).not.toContain("DO-NOT-CAPTURE");
  if(stage==="disabled-config")expect(receipts[0].body.config).toEqual({httpStatus:200,id:"diag-child",enabled:true});
  fake.observation=()=>{throw new Error("diagnostic failed");};
  await expect(Lab.prototype.archive199WaitQuiet.call(fake,{...parent,id:"diag-child"})).rejects.toBe(original);
});

test("#199 response decoder accepts only complete bounded framing under fragmentation", () => {
  const proxy = new URL("../../docker/fault_proxy.py", import.meta.url).pathname;
  const script = String.raw`
import importlib.util, sys
spec=importlib.util.spec_from_file_location("proxy",sys.argv[1]); p=importlib.util.module_from_spec(spec); spec.loader.exec_module(p)
class Wire:
 def __init__(self,data,fragment=4096): self.data=data; self.fragment=fragment
 def recv(self,n):
  size=min(n,self.fragment); part=self.data[:size]; self.data=self.data[size:]; return part
chunked=b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/xml\r\n\r\n4\r\n<xml\r\n3\r\n/>!\r\n0\r\n\r\n"
assert p.read_http_message(Wire(chunked)).split(b"\r\n\r\n",1)[1]==b"", "predecessor reproduction must discard body"
for fragment in [1,2,3,7,4096]:
 result=p.read_http_response(Wire(chunked,fragment),b"REPORT")
 assert result.endswith(b"\r\n\r\n<xml/>!") and b"Content-Length: 7" in result and b"Transfer-Encoding" not in result
 assert p.archive199_framed(result,response=True,method="REPORT")
 result=p.read_http_response(Wire(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n<xml/>!",fragment),b"REPORT")
 assert result.endswith(b"<xml/>!") and p.archive199_framed(result,response=True,method="REPORT")
encoded=chunked.replace(b"Content-Type: text/xml",b"Content-Encoding: gzip")
result=p.read_http_response(Wire(encoded,1),b"REPORT")
assert b"Content-Encoding: gzip" in result and result.endswith(b"<xml/>!"), "entity encoding remains opaque"
for status,method,headers in [(204,b"DELETE",b""),(304,b"GET",b"Content-Length: 99\r\n"),(200,b"HEAD",b"Content-Length: 99\r\n")]:
 result=p.read_http_response(Wire(b"HTTP/1.1 "+str(status).encode()+b" OK\r\n"+headers+b"\r\n"),method)
 assert p.archive199_framed(result,response=True,method=method.decode())
bad=[
 b"HTTP/1.1 200 OK\r\n\r\nbody",
 b"HTTP/1.1 100 Continue\r\n\r\n",
 b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabc",
 b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
 b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\na",
 b"HTTP/1.1 200 OK\r\nContent-Length: 9000000\r\n\r\n",
 b"HTTP/1.1 200 OK\r\n folded: invalid\r\nContent-Length: 0\r\n\r\n",
 chunked.replace(b"Transfer-Encoding: chunked",b"Transfer-Encoding: chunked\r\nContent-Length: 7"),
 chunked.replace(b"Transfer-Encoding: chunked",b"Transfer-Encoding: gzip, chunked"),
 chunked.replace(b"Transfer-Encoding: chunked",b"Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked"),
 chunked.replace(b"4\r\n<xml",b"4;x=y\r\n<xml"),
 chunked.replace(b"4\r\n<xml",b"g\r\n<xml"),
 chunked.replace(b"4\r\n<xml",b"ffffffff\r\n<xml"),
 chunked.replace(b"<xml\r\n",b"<xmlXX"),
 chunked.replace(b"0\r\n\r\n",b"0\r\nX-Trailer: value\r\n\r\n"),
 chunked[:-1],chunked[:-5],chunked+b"extra",
]
for wire in bad:
 for fragment in [1,4096]:
  # Extra bytes in a later packet are not part of the one-response contract.
  if wire==chunked+b"extra" and fragment==1: continue
  try:p.read_http_response(Wire(wire,fragment),b"REPORT")
  except RuntimeError:pass
  else:raise AssertionError("accepted invalid framing")
print("complete framing regressions passed")
`;
  expect(execFileSync("python3", ["-B", "-c", script, proxy], {timeout: 10000, maxBuffer: 100000, encoding: "utf8"})).toContain("complete framing regressions passed");
});

test("#199 response declares one-request closure and failure receipts exclude transport secrets", () => {
  const proxy = new URL("../../docker/fault_proxy.py", import.meta.url).pathname;
  const script = String.raw`
import importlib.util,sys,io,json,contextlib
spec=importlib.util.spec_from_file_location("proxy",sys.argv[1]);p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)
class Wire:
 def __init__(self,data):self.data=data
 def recv(self,n):part=self.data[:n];self.data=self.data[n:];return part
for framing in [b"Content-Length: 3\r\n",b"Transfer-Encoding: chunked\r\n"]:
 for connection in [b"",b"Connection: keep-alive\r\nKeep-Alive: timeout=5\r\n",b"cOnNeCtIoN: keep-alive\r\nConnection: close\r\nkEeP-aLiVe: timeout=5\r\n"]:
  entity=b"abc" if framing.startswith(b"Content-Length") else b"3\r\nabc\r\n0\r\n\r\n"
  response=p.read_http_response(Wire(b"HTTP/1.1 200 OK\r\n"+framing+connection+b"\r\n"+entity),b"GET")
  head,body=response.split(b"\r\n\r\n",1)
  assert head.lower().count(b"connection:")==1 and b"Connection: close" in head
  assert b"keep-alive" not in head.lower() and body==b"abc"
  assert p.archive199_framed(response,response=True,method="GET")
for code,method in [(204,b"DELETE"),(304,b"GET"),(200,b"HEAD")]:
 response=p.read_http_response(Wire(b"HTTP/1.1 "+str(code).encode()+b" OK\r\n\r\n"),method)
 assert b"Connection: close" in response and p.archive199_framed(response,response=True,method=method.decode())
original=OSError("secret endpoint credential DO-NOT-LOG")
class Broken:
 def recv(self,n):raise original
out=io.StringIO()
with contextlib.redirect_stdout(out):
 try:p.read_http_response(Broken(),b"GET")
 except OSError as error:assert error is original
 else:raise AssertionError("transport failure accepted")
 assert json.loads(out.getvalue())=={"event":"upstream_response_failure","reason":"transport_error"}
assert "DO-NOT-LOG" not in out.getvalue()
out=io.StringIO()
with contextlib.redirect_stdout(out):
 try:p.read_http_response(Wire(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nx"),b"GET")
 except RuntimeError:pass
 else:raise AssertionError("truncation accepted")
assert json.loads(out.getvalue())=={"event":"upstream_response_failure","reason":"truncated_upstream_response"}
print("closure and sanitized failure regressions passed")
`;
  expect(execFileSync("python3", ["-B", "-c", script, proxy], {timeout: 10000, maxBuffer: 100000, encoding: "utf8"})).toContain("closure and sanitized failure regressions passed");
});

test("#199 handler failure stages are bounded metadata and never disclose exception or request secrets", () => {
 const proxy = new URL("../../docker/fault_proxy.py", import.meta.url).pathname;
 const script = String.raw`
import importlib.util,sys,io,json,contextlib
spec=importlib.util.spec_from_file_location("proxy",sys.argv[1]);p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)
secret="DO-NOT-LOG"
class Socket:
 def __init__(self):self.closed=0;self.sent=[]
 def close(self):self.closed+=1
 def settimeout(self,*args):pass
 def unwrap(self):return self
 def sendall(self,body):self.sent.append(body)
class Context:
 def wrap_socket(self,*args,**kwargs):return upstream
request=b"REPORT /"+secret.encode()+b" HTTP/1.1\r\nAuthorization: "+secret.encode()+b"\r\nContent-Length: 0\r\n\r\n"
response=b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
def fail(*args,**kwargs):raise ConnectionResetError(secret)
for expected in ["request-read","before-gate","tls-context","upstream-connect","upstream-tls","upstream-send","response-read","after-gate","client-send"]:
 client=Socket();upstream=Socket();context=Context()
 p.read_http_message=lambda sock:request
 p.archive199_before=lambda value:None
 p.ssl.create_default_context=lambda **kwargs:context
 p.socket.create_connection=lambda *args,**kwargs:upstream
 p.archive199_upstream_timeout=lambda *args:1
 p.archive199_bound_upstream=lambda *args:None
 p.read_http_response=lambda *args:response
 p.archive199_after=lambda *args:None
 p.take_drop=lambda:False
 if expected=="request-read":p.read_http_message=fail
 elif expected=="before-gate":p.archive199_before=fail
 elif expected=="tls-context":p.ssl.create_default_context=fail
 elif expected=="upstream-connect":p.socket.create_connection=fail
 elif expected=="upstream-tls":context.wrap_socket=fail
 elif expected=="upstream-send":upstream.sendall=fail
 elif expected=="response-read":p.read_http_response=fail
 elif expected=="after-gate":p.archive199_after=fail
 elif expected=="client-send":client.sendall=fail
 out=io.StringIO()
 with contextlib.redirect_stdout(out):p.handle_client(client)
 receipt=json.loads(out.getvalue());assert receipt["stage"]==expected and receipt["error_type"]=="ConnectionResetError"
 assert set(receipt)=={"event","stage","error_type","method","request_framing","request_bytes","response_bytes"}
 assert secret not in out.getvalue() and client.closed==1
 assert receipt["method"]==("unknown" if expected=="request-read" else "REPORT")
 assert receipt["request_framing"]==("unknown" if expected=="request-read" else "content-length")
# Diagnostic write failure must still execute the original close-and-return path.
p.read_http_message=fail
class BrokenLog:
 def write(self,*args):raise RuntimeError(secret)
 def flush(self):pass
client=Socket()
with contextlib.redirect_stdout(BrokenLog()):p.handle_client(client)
assert client.closed==1
print("handler diagnostic regressions passed")
`;
 expect(execFileSync("python3", ["-B", "-c", script, proxy], {timeout:10000,maxBuffer:100000,encoding:"utf8"})).toContain("handler diagnostic regressions passed");
});

test("#199 fixed diagnostic counters cover TLS acceptance, empty requests and successful send-close", () => {
 const proxy = new URL("../../docker/fault_proxy.py", import.meta.url).pathname;
 const script=String.raw`
import importlib.util,sys,contextlib,io
spec=importlib.util.spec_from_file_location("proxy",sys.argv[1]);p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)
class Socket:
 def __init__(self):self.closed=0
 def close(self):self.closed+=1
 def sendall(self,*args):pass
 def settimeout(self,*args):pass
 def unwrap(self):return self
client=Socket();upstream=Socket()
p.read_http_message=lambda sock:b""
p.handle_client(client)
assert client.closed==1 and p.state["handler_empty_request"]==1 and p.state["handler_close_completed"]==1
class Context:
 def wrap_socket(self,*args,**kwargs):return upstream
p.read_http_message=lambda sock:b"GET / HTTP/1.1\r\n\r\n"
p.archive199_before=lambda request:None
p.ssl.create_default_context=lambda **kwargs:Context()
p.socket.create_connection=lambda *args,**kwargs:upstream
p.archive199_upstream_timeout=lambda *args:1
p.archive199_bound_upstream=lambda *args:None
p.read_http_response=lambda *args:b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
p.archive199_after=lambda *args:None
p.take_drop=lambda:False
p.handle_client(client)
assert p.state["handler_entered"]==2 and p.state["handler_send_completed"]==1
assert p.state["handler_close_attempted"]==2 and p.state["handler_close_completed"]==2
# Stop a fake accept loop after one rejection and one acceptance; no sockets/TLS run.
class Stop(Exception):pass
class Listener:
 def setsockopt(self,*args):pass
 def bind(self,*args):pass
 def listen(self,*args):pass
 def accept(self):
  self.n=getattr(self,"n",0)+1
  if self.n>2:raise Stop()
  return client,None
class ServerContext:
 def load_cert_chain(self,*args):pass
 def wrap_socket(self,*args,**kwargs):
  self.n=getattr(self,"n",0)+1
  if self.n==1:raise p.ssl.SSLError("DO-NOT-LOG")
  return client
class Thread:
 def __init__(self,*args,**kwargs):pass
 def start(self):pass
p.socket.socket=lambda *args:Listener()
p.ssl.SSLContext=lambda *args:ServerContext()
p.threading.Thread=Thread
out=io.StringIO()
with contextlib.redirect_stdout(out):
 try:p.serve_tls()
 except Stop:pass
assert out.getvalue()=="" and p.state["tls_rejected_ssl"]==1 and p.state["tls_accepted"]==1
# A diagnostics-only counter failure does not escape.
p.state={}
p.diagnostic_count("handler_entered")
print("fixed counters passed")
`;
 expect(execFileSync("python3",["-B","-c",script,proxy],{timeout:10000,maxBuffer:100000,encoding:"utf8"})).toContain("fixed counters passed");
});

test("#199 graceful shutdown is bounded and exclusive to fully sent non-dropped responses", () => {
 const proxy=new URL("../../docker/fault_proxy.py",import.meta.url).pathname;
 const script=String.raw`
import importlib.util,sys,contextlib,io
spec=importlib.util.spec_from_file_location("proxy",sys.argv[1]);p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)
for fault in [None,TimeoutError("DO-NOT-LOG"),p.ssl.SSLError("DO-NOT-LOG"),OSError("DO-NOT-LOG")]:
 calls=[]
 class Raw:
  def close(self):calls.append("raw-close")
  def recv(self,*args):raise AssertionError("plaintext read")
  def sendall(self,*args):raise AssertionError("plaintext write")
 class Client:
  def settimeout(self,n):assert n==2.0;calls.append("timeout-2")
  def unwrap(self):
   calls.append("unwrap")
   if fault:raise fault
   return Raw()
  def close(self):calls.append("tls-close")
 before=dict(p.state);out=io.StringIO()
 with contextlib.redirect_stdout(out):p.close_successful_response(Client())
 assert calls==["timeout-2","unwrap","tls-close" if fault else "raw-close"]
 assert out.getvalue()==""
 assert p.state["successful_tls_shutdown_attempted"]==before["successful_tls_shutdown_attempted"]+1
 assert p.state["successful_tls_shutdown_completed"]==before["successful_tls_shutdown_completed"]+(fault is None)
 assert p.state["successful_tls_shutdown_fallback"]==before["successful_tls_shutdown_fallback"]+(fault is not None)
 assert p.state["handler_close_attempted"]==before["handler_close_attempted"]+1
 assert p.state["handler_close_completed"]==before["handler_close_completed"]+1
for path in ["empty","before-abort","after-abort","drop","send-error","success"]:
 calls=[]
 class Socket:
  def close(self):calls.append("close")
  def settimeout(self,*args):pass
  def sendall(self,*args):pass
 class Client(Socket):
  def sendall(self,*args):
   calls.append("send")
   if path=="send-error":raise OSError("DO-NOT-LOG")
 client=Client();upstream=Socket()
 class Context:
  def wrap_socket(self,*args,**kwargs):return upstream
 def before(*args):
  if path=="before-abort":raise p.Archive199Abort("DO-NOT-LOG")
 def after(*args):
  if path=="after-abort":raise p.Archive199Abort("DO-NOT-LOG")
 p.read_http_message=lambda sock:b"" if path=="empty" else b"GET / HTTP/1.1\r\n\r\n"
 p.archive199_before=before;p.archive199_after=after
 p.ssl.create_default_context=lambda **kwargs:Context()
 p.socket.create_connection=lambda *args,**kwargs:upstream
 p.archive199_upstream_timeout=lambda *args:1;p.archive199_bound_upstream=lambda *args:None
 p.read_http_response=lambda *args:b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
 p.take_drop=lambda:path=="drop"
 p.close_successful_response=lambda sock:calls.append("graceful")
 out=io.StringIO()
 with contextlib.redirect_stdout(out):p.handle_client(client)
 assert ("graceful" in calls)==(path=="success"),path
 if path=="success":assert calls[-2:]==["send","graceful"]
 assert "DO-NOT-LOG" not in out.getvalue()
print("successful-only graceful shutdown passed")
`;
 expect(execFileSync("python3",["-B","-c",script,proxy],{timeout:10000,maxBuffer:100000,encoding:"utf8"})).toContain("successful-only graceful shutdown passed");
});
