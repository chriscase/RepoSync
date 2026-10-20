import {execFile, execFileSync, spawn} from "node:child_process";
import {mkdtempSync, mkdirSync, readFileSync, rmSync} from "node:fs";
import {createServer as httpServer} from "node:http";
import {createServer as httpsServer} from "node:https";
import {createServer as tcpServer} from "node:net";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {promisify} from "node:util";
import {test, expect} from "@playwright/test";
import {Lab, type Pair, type Snap} from "../../playwright/runtime.ts";
import {runCase} from "../../playwright/handlers.ts";

const exec = promisify(execFile);
const pair: Pair = {id: "fixture", name: "fixture", svnUrl: "https://svn:8443/svn/fixture", gitRepo: "lab/fixture"};
const baseline: Snap = {svn: "1", git: "a".repeat(40), maps: "1", records: "1", watermark: "1", gitWatermark: "a".repeat(40), gitColumn: "a".repeat(40)};

for (const fault of ["none", "organization", "creation", "metadata", "existing-history", "operation", "import", "baseline", "receipt", "activation"]) {
  test(`fresh fixture imports while disabled and rejects setup failure: ${fault}`, async () => {
    const events: string[] = [];
    const fake: any = {
      repos: async () => [], admin: async () => "synthetic",
      sh: async () => {events.push("svn-seed"); return "";}, observer: async () => {events.push("empty-target"); return fault === "existing-history" ? `${baseline.git}\trefs/heads/main\n` : "";},
      giteaApi: async (method: string, path: string) => {
        if (path.endsWith("/orgs/lab")) return {status: fault === "organization" ? 404 : 200, json: {}};
        if (method === "POST") return {status: fault === "creation" ? 500 : 201, json: {}};
        return {status: 200, json: {full_name: fault === "metadata" ? "other/fixture" : pair.gitRepo, private: true, default_branch: "main"}};
      },
      api: async (method: string, path: string, body: any) => {
        if (path === "/api/repos") {expect(body.enabled).toBe(false); events.push("register-disabled"); return {status: 200, json: {id: pair.id}};}
        if (path.endsWith("/credentials")) {events.push("credentials"); return {status: 200};}
        if (path.endsWith("/import")) {events.push("import-once"); return {status: 200, json: {operation_id: fault === "operation" ? undefined : "operation"}};}
        expect([method, path, body]).toEqual(["PUT", `/api/repos/${pair.id}`, {enabled: true}]);
        expect(events.indexOf("confirmed-import")).toBeLessThan(events.indexOf("receipt"));
        events.push("enable"); return {status: fault === "activation" ? 500 : 200, json: {...pair, svn_url: pair.svnUrl, git_repo: pair.gitRepo, enabled: true}};
      },
      waitImport: async (id: string, operation: string) => {expect([id, operation]).toEqual([pair.id, "operation"]); if (fault === "import") throw new Error("terminal import failure"); events.push("confirmed-import");},
      snap: async () => fault === "baseline" ? {...baseline, maps: "0"} : baseline,
      assertSingleReceipt: async (_p: Pair, direction: string, rev: string) => {expect([direction, rev]).toEqual(["svn_to_git", "1"]); if (fault === "receipt") throw new Error("missing receipt"); events.push("receipt");},
      enableFixturePair: (p: Pair) => Lab.prototype.enableFixturePair.call(fake, p), observation: () => {},
    };
    const run = Lab.prototype.createPair.call(fake, pair.name);
    if (fault === "none") {expect(await run).toEqual(pair); expect(events).toEqual(["svn-seed", "empty-target", "register-disabled", "credentials", "import-once", "confirmed-import", "receipt", "enable"]);}
    else {await expect(run).rejects.toThrow(); if (fault !== "activation") expect(events).not.toContain("enable");}
    expect(events.filter(e => e === "import-once").length).toBeLessThanOrEqual(1);
  });
}

for (const fault of ["none", "failed", "held", "cancelled", "wrong-operation", "http", "missing-phase"]) {
  test(`actual import waiter preserves terminal status without replay: ${fault}`, async () => {
    let reads = 0; const observations: any[] = [];
    const fake: any = {api: async () => {reads++; return {status: fault === "http" ? 500 : 200, json: {
      operation_id: fault === "wrong-operation" ? "other" : "operation",
      phase: fault === "failed" ? "failed" : fault === "held" ? "failed" : fault === "cancelled" ? "cancelled" : fault === "missing-phase" ? undefined : "completed",
      lifecycle: fault === "held" ? "ReconciliationRequired" : fault === "missing-phase" ? undefined : "Completed",
      outcome_detail: "retained-terminal-reason",
    }};}, observation: (_name: string, data: any) => observations.push(data)};
    const run = Lab.prototype.waitImport.call(fake, pair.id, "operation");
    if (fault === "none") {await run; expect(observations[0].setup_failed).toBeUndefined();}
    else {await expect(run).rejects.toThrow(); expect(observations[0].setup_failed).toBe(true);}
    expect(reads).toBe(1); expect(observations).toHaveLength(1); expect(observations[0].result.outcome_detail).toBe("retained-terminal-reason");
  });
}

test("import waiter does not activate on in-memory completion before durable completion", async () => {
  let reads = 0, observed = false;
  const fake: any = {api: async () => ({status: 200, json: {operation_id: "operation", phase: "completed", lifecycle: ++reads === 1 ? "Running" : "Completed"}}), observation: () => {observed = true;}};
  await Lab.prototype.waitImport.call(fake, pair.id, "operation");
  expect(reads).toBe(2); expect(observed).toBe(true);
});

for (const fault of ["none", "missing-lifecycle", "missing-operation", "changed-operation"]) {
  test(`reused disabled fixture pins its import and waits for durable completion: ${fault}`, async () => {
    let reads = 0, enabled = false;
    const observations: any[] = [];
    const row = {id: pair.id, name: pair.name, git_repo: pair.gitRepo, svn_url: pair.svnUrl, enabled: false};
    const fake: any = {
      repos: async () => [row], observation: (_name: string, data: any) => observations.push(data),
      waitImport: (id: string) => Lab.prototype.waitImport.call(fake, id),
      enableFixturePair: (p: Pair) => Lab.prototype.enableFixturePair.call(fake, p),
      api: async (method: string) => {
        if (method === "GET") {reads++; return {status: 200, json: {
          operation_id: fault === "missing-operation" ? undefined : fault === "changed-operation" && reads > 1 ? "other" : "operation",
          phase: "completed", lifecycle: fault === "missing-lifecycle" ? undefined : reads === 1 ? "Running" : "Completed",
        }};}
        expect(reads).toBe(2); expect(observations[0].result.lifecycle).toBe("Completed"); enabled = true;
        return {status: 200, json: {...row, enabled: true}};
      },
    };
    const run = Lab.prototype.createPair.call(fake, pair.name);
    if (fault === "none") {expect(await run).toEqual(pair); expect(enabled).toBe(true); expect(observations[0].operation_id).toBe("operation");}
    else {await expect(run).rejects.toThrow(); expect(enabled).toBe(false); expect(observations[0].setup_failed).toBe(true);}
    expect(reads).toBe(fault === "missing-lifecycle" || fault === "missing-operation" ? 1 : 2);
  });
}

test("unsettled fixture returns the started operation and does not wait or enable", async () => {
  const events: string[] = [];
  const fake: any = {
    repos: async () => [], admin: async () => "synthetic", sh: async () => {events.push("svn-seed"); return "";}, observer: async () => "",
    giteaApi: async (method: string, path: string) => {
      if (path.endsWith("/orgs/lab")) return {status: 200, json: {}};
      if (method === "POST") return {status: 201, json: {}};
      return {status: 200, json: {full_name: pair.gitRepo, private: true, default_branch: "main"}};
    },
    api: async (_method: string, path: string, body: any) => {
      if (path === "/api/repos") {expect(body.enabled).toBe(false); events.push("register"); return {status: 200, json: {id: pair.id}};}
      if (path.endsWith("/credentials")) {events.push("credentials"); return {status: 200};}
      if (path.endsWith("/import")) {events.push("import"); return {status: 200, json: {operation_id: "op-live"}, text: ""};}
      events.push(`unexpected ${path}`); return {status: 500, text: "", json: {}};
    },
    waitImport: async () => {events.push("wait");}, snap: async () => {events.push("snap"); return baseline;},
    assertSingleReceipt: async () => {events.push("receipt");}, enableFixturePair: async () => {events.push("enable");}, observation: () => {events.push("observed");},
  };
  expect(await Lab.prototype.createPair.call(fake, pair.name, {enable: false, settle: false})).toEqual({...pair, importOperationId: "op-live"});
  expect(events).toEqual(["svn-seed", "register", "credentials", "import", "observed"]);
});

test("unsettled fixture does not continue after a start with no operation id", async () => {
  const events: string[] = [];
  const fake: any = {
    repos: async () => [], admin: async () => "synthetic", sh: async () => "", observer: async () => "",
    giteaApi: async (method: string, path: string) => path.endsWith("/orgs/lab") ? {status: 200, json: {}} : method === "POST" ? {status: 201, json: {}} : {status: 200, json: {full_name: pair.gitRepo, private: true, default_branch: "main"}},
    api: async (_method: string, path: string) => {
      events.push(path);
      if (path === "/api/repos") return {status: 200, json: {id: pair.id}};
      if (path.endsWith("/credentials")) return {status: 200};
      return {status: 200, json: {}, text: ""};
    },
    waitImport: async () => {events.push("wait");}, enableFixturePair: async () => {events.push("enable");}, observation: () => {},
  };
  await expect(Lab.prototype.createPair.call(fake, pair.name, {enable: false, settle: false})).rejects.toThrow(/no operation id/);
  expect(events).not.toContain("wait");
  expect(events).not.toContain("enable");
  expect(events.some(path => path.endsWith("/import/cancel"))).toBe(false);
});

test("unsettled import cannot be enabled before it is cancelled", async () => {
  const fake: any = {repos: async () => {throw new Error("must not look up repos");}};
  await expect(Lab.prototype.createPair.call(fake, pair.name, {settle: false})).rejects.toThrow(/cannot be enabled/);
});

for (const fault of ["running", "missing-operation", "completed", "scheduled"]) {
  test(`unsettled reuse keeps only an in-flight operation id: ${fault}`, async () => {
    const calls: string[] = [];
    const row = {id: pair.id, name: pair.name, git_repo: pair.gitRepo, svn_url: pair.svnUrl, enabled: fault === "scheduled" ? true : false};
    const fake: any = {
      repos: async () => [row], waitImport: async () => {calls.push("wait");}, enableFixturePair: async () => {calls.push("enable");},
      api: async (method: string, path: string) => {
        calls.push(`${method} ${path}`);
        return {status: 200, json: {operation_id: fault === "missing-operation" ? undefined : "op-live", lifecycle: fault === "completed" ? "completed" : "running"}};
      },
    };
    const run = Lab.prototype.createPair.call(fake, pair.name, {enable: false, settle: false});
    if (fault === "running") {
      expect(await run).toEqual({...pair, importOperationId: "op-live"});
      expect(calls).toEqual([`GET /api/repos/${pair.id}/import/status`]);
    } else {
      await expect(run).rejects.toThrow();
      expect(calls).not.toContain("wait");
      expect(calls).not.toContain("enable");
      expect(calls.some(call => /\/import\/cancel$/.test(call))).toBe(false);
    }
  });
}

for (const fault of ["none", "control", "stale", "healthy", "arm", "no-current-drop", "no-forward", "moved"]) {
  test(`actual fault handler requires healthy forwarding and a new drop, remains partial: ${fault}`, async () => {
    let stage = 0, marked = "";
    const counters = (upstream: number, forwarded: number, dropped: number, remaining = 0) => ({client_connections: upstream, upstream_connections: upstream, forwarded_responses: forwarded, dropped_responses: dropped, drop_responses_remaining: remaining});
    const fake: any = {createPair: async () => pair, snap: async () => stage === 4 && fault === "moved" ? {...baseline, git: "b".repeat(40)} : baseline,
      faultRequest: async (path: string, body?: any) => {
        if (fault === "control") throw new Error("control refused");
        if (path === "drop-next-response") {expect(body).toEqual({count: 1}); stage = 2; return {ok: true, armed: fault === "arm" ? 0 : 1};}
        if (stage === 0) return counters(0, 0, 5, fault === "stale" ? 1 : 0);
        if (stage === 1) return counters(fault === "healthy" ? 0 : 1, 1, 5);
        if (stage === 2) return counters(1, 1, 5, 1);
        stage = 4; return counters(fault === "no-forward" ? 1 : 2, 1, fault === "no-current-drop" ? 5 : 6);
      }, observer: async (script: string) => {expect(script).toContain("https://fault-proxy:8443/svn/fixture/trunk"); if (script.includes("-X OPTIONS")) {stage = 1; return "";} stage = 3; return "LAB_FAULT_CLIENT_EXIT:1\nupstream response was dropped\n";},
      observation: () => {}, mark: (status: string) => {marked = status;}, pass: () => {throw new Error("read fault cannot qualify write recovery");},
    };
    const run = runCase(fake, {id: "LAB_LOST_REPLY", title: "fixture", suite: "full"});
    if (fault === "none") {await run; expect(marked).toBe("PARTIAL");}
    else {await expect(run).rejects.toThrow(); expect(marked).toBe("");}
  });
}

async function ports(count: number): Promise<number[]> {
  const servers = Array.from({length: count}, () => tcpServer());
  try {
    await Promise.all(servers.map(server => new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve))));
    return servers.map(server => (server.address() as {port: number}).port);
  } finally {await Promise.all(servers.map(server => new Promise<void>(resolve => server.close(() => resolve()))));}
}

test("native TLS fault proxy forwards a healthy scoped response then drops the next one", async () => {
  const root = mkdtempSync(join(tmpdir(), "lab-fault-tls-"));
  const cert = join(root, "cert.pem"), key = join(root, "key.pem");
  execFileSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", cert, "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"], {stdio: "ignore"});
  const requests: string[] = [];
  const upstream = httpsServer({key: readFileSync(key), cert: readFileSync(cert)}, (req, res) => {
    requests.push(req.url!); const body = Buffer.from(JSON.stringify({path: req.url})); res.writeHead(200, {"content-length": body.length}); res.end(body);
  });
  await new Promise<void>(resolve => upstream.listen(0, "127.0.0.1", resolve));
  const [tlsPort, controlPort] = await ports(2);
  const proxy = spawn("python3", [join(process.cwd(), "tests/acceptance-lab/docker/fault_proxy.py")], {env: {...process.env, UPSTREAM_HOST: "localhost", UPSTREAM_PORT: String((upstream.address() as {port: number}).port), LISTEN_PORT: String(tlsPort), CONTROL_PORT: String(controlPort), UPSTREAM_CA: cert, PROXY_CERT: cert, PROXY_KEY: key}, stdio: "pipe"});
  const closed = new Promise<void>(resolve => proxy.once("close", () => resolve()));
  let stderr = ""; proxy.stderr.on("data", data => stderr += data);
  const control = `http://127.0.0.1:${controlPort}`;
  const previousControl = process.env.LAB_FAULT_CONTROL_URL;
  process.env.LAB_FAULT_CONTROL_URL = control;
  try {
    await expect.poll(async () => {if (proxy.exitCode !== null) throw new Error(stderr); try {return (await fetch(control + "/stats")).status;} catch {return 0;}}, {timeout: 5000}).toBe(200);
    const curl = () => exec("curl", ["--fail", "--silent", "--show-error", "--max-time", "5", "--cacert", cert, `https://localhost:${tlsPort}/svn/fixture/trunk`]);
    expect(JSON.parse((await curl()).stdout).path).toBe("/svn/fixture/trunk");
    const before: any = await Lab.prototype.faultRequest.call({} as Lab, "stats");
    expect(await Lab.prototype.faultRequest.call({} as Lab, "drop-next-response", {count: 1})).toMatchObject({ok: true, armed: 1});
    await expect(curl()).rejects.toThrow();
    const after: any = await Lab.prototype.faultRequest.call({} as Lab, "stats");
    expect(after.dropped_responses - before.dropped_responses).toBe(1); expect(after.upstream_connections - before.upstream_connections).toBe(1); expect(after.drop_responses_remaining).toBe(0);
    expect(requests).toEqual(["/svn/fixture/trunk", "/svn/fixture/trunk"]);
  } finally {
    if (previousControl === undefined) delete process.env.LAB_FAULT_CONTROL_URL; else process.env.LAB_FAULT_CONTROL_URL = previousControl;
    proxy.kill("SIGTERM"); await closed;
    await new Promise<void>(resolve => upstream.close(() => resolve())); rmSync(root, {recursive: true, force: true});
  }
});

test("actual Gitea fixture requests retain provider HTTP failures over verified TLS", async () => {
  const root = mkdtempSync(join(tmpdir(), "lab-provider-tls-"));
  const cert = join(root, "cert.pem"), key = join(root, "key.pem");
  execFileSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", cert, "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"], {stdio: "ignore"});
  const server = httpsServer({key: readFileSync(key), cert: readFileSync(cert)}, (req, res) => {expect(req.headers.authorization).toBe("token synthetic-token"); res.writeHead(req.url!.endsWith("failure") ? 500 : 201, {"content-type": "application/json"}); res.end(JSON.stringify({ok: true}));});
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const fake: any = {sh: (_service: string, script: string) => exec("sh", ["-c", script.replaceAll("/certs/ca.crt", cert).replaceAll("https://gitea:3000", `https://localhost:${(server.address() as {port: number}).port}`)], {env: {...process.env, GIT_TOKEN: "synthetic-token"}}).then(r => r.stdout)};
  try {
    expect(await Lab.prototype.giteaApi.call(fake, "POST", "/api/v1/success", {name: "fixture"})).toEqual({status: 201, json: {ok: true}});
    expect(await Lab.prototype.giteaApi.call(fake, "GET", "/api/v1/failure")).toEqual({status: 500, json: {ok: true}});
  } finally {await new Promise<void>(resolve => server.close(() => resolve())); rmSync(root, {recursive: true, force: true});}
});

test("rendered Compose routes only fault control through a loopback forwarder", () => {
  const file = join(process.cwd(), "tests/acceptance-lab/compose/docker-compose.yml");
  const env = {...process.env};
  for (const variable of new Set([...readFileSync(file, "utf8").matchAll(/\$\{([A-Z0-9_]+)\}/g)].map(match => match[1]))) {
    env[variable] = variable.endsWith("IMAGE") ? "fixture:local" : variable === "LAB_RUN_DIR" ? tmpdir() : variable.endsWith("PORT") ? "45000" : "synthetic";
  }
  env.RUN_ID = "rsfixture"; env.COMPOSE_PROJECT_NAME = "rsaccrsfixture";
  // Config rendering must not reach any user's Docker daemon.
  env.DOCKER_HOST = "unix:/tmp/acceptance-lab-no-daemon.sock";
  const rendered = JSON.parse(execFileSync("docker", ["compose", "-f", file, "config", "--format", "json"], {env, encoding: "utf8", timeout: 10_000}));
  const fault = rendered.services.fault, forwarder = rendered.services["fault-control-forwarder"];
  expect(Object.keys(fault.networks)).toEqual(["lab"]); expect(fault.ports.every((port: any) => port.target !== 8090)).toBe(true);
  expect(Object.keys(forwarder.networks)).toEqual(["lab", "publish"]);
  expect(forwarder.ports).toHaveLength(1); expect(forwarder.ports[0]).toMatchObject({host_ip: "127.0.0.1", target: 8090});
  expect(forwarder.command).toEqual(["TCP-LISTEN:8090,bind=0.0.0.0,fork,reuseaddr", "TCP:fault:8090"]);
  expect(forwarder.depends_on.fault.condition).toBe("service_healthy");
  expect(forwarder.labels).toMatchObject({"acceptance-lab.run-id": "rsfixture", "acceptance-lab.disposable": "true"});
  expect(rendered.networks.lab.internal).toBe(true); expect(rendered.networks.publish.driver_opts["com.docker.network.bridge.enable_ip_masquerade"]).toBe("false");
});

for (const fault of ["none", "http", "schema", "no-forwarder"]) {
  test(`shipped startup probe validates published controller before readiness: ${fault}`, async () => {
    const root = mkdtempSync(join(tmpdir(), "lab-control-ready-")); mkdirSync(join(root, "evidence"));
    const stats = {drop_responses_remaining: 0, client_connections: 0, upstream_connections: 0, dropped_responses: 0, forwarded_responses: fault === "schema" ? true : 0};
    const server = httpServer((_req, res) => {res.writeHead(fault === "http" ? 503 : 200, {"content-type": "application/json"}); res.end(JSON.stringify(stats));});
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const script = `set -eu\nexport ACCEPTANCE_LAB_SOURCE_ONLY=1\nsource scripts/acceptance-lab.sh\nRUN_ID=rsfixture\nEVIDENCE='${root}/evidence'\nLAB_FAULT_CONTROL_PORT=${(server.address() as {port: number}).port}\ncompose() { printf '%s\\n' '${fault === "no-forwarder" ? "" : "forwarder-fixture"}'; }\nprobe_fault_control\n`;
    try {
      const run = exec("bash", ["-c", script]);
      if (fault === "none") {await run; expect(JSON.parse(readFileSync(join(root, "evidence/fault-control.json"), "utf8"))).toMatchObject({run_id: "rsfixture", host_health: "ok", forwarder: "forwarder-fixture", stats});}
      else {await expect(run).rejects.toThrow(); expect(() => readFileSync(join(root, "evidence/fault-control.json"))).toThrow();}
    } finally {await new Promise<void>(resolve => server.close(() => resolve())); rmSync(root, {recursive: true, force: true});}
  });
}
