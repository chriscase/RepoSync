import { execFile } from "node:child_process";
import { createHmac, randomBytes } from "node:crypto";
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { promisify } from "node:util";
import { expect, type Page, type TestInfo } from "@playwright/test";

const exec = promisify(execFile);

export type LabOp = {
  id: string;
  title: string;
  suite: string;
  contract?: string;
  supports_workflow?: boolean;
  r_criteria?: string[];
};

export type Pair = {
  id: string;
  name: string;
  svnUrl: string;
  gitRepo: string;
  svnBranch?: string;
  gitBranch?: string;
  parentId?: string;
};

export type Snap = { svn: string; git: string; maps: string; records: string; watermark: string; gitWatermark: string; gitColumn: string };
export type FaultStats = {drop_responses_remaining: number; client_connections: number; upstream_connections: number; dropped_responses: number; forwarded_responses: number};

export function readFaultStats(value: unknown): FaultStats {
  const stats = value as FaultStats | null;
  for (const key of ["drop_responses_remaining", "client_connections", "upstream_connections", "dropped_responses", "forwarded_responses"] as const) {
    if (!stats || !Number.isSafeInteger(stats[key]) || stats[key] < 0) throw new Error("invalid fault control stats");
  }
  return stats!;
}

export function assertSvnRefusalState(before: Snap, now: Snap, revision: string): void {
  // The operator owns the new SVN revision. Only the bridge's destination,
  // mappings and directional checkpoints must remain at the pre-source pins.
  if (now.svn !== revision) throw new Error("SVN source moved beyond the intentional fixture revision");
  for (const field of ["git", "maps", "watermark", "gitWatermark", "gitColumn"] as const) {
    if (now[field] !== before[field]) throw new Error(`SVN refusal contract changed ${field}: ${before[field]} -> ${now[field]}`);
  }
}

export function loginError(page: Page) {
  // Login.tsx renders its error span in the red border inside the form. The
  // LDAP domain badge lives outside it and may legitimately contain "invalid".
  return page.locator("form .border-red-700 span");
}

export function assertCompletedPublishRefusal(result: {status: number; json: unknown}, branch: string): void {
  if (result.status !== 400 || (result.json as {error?: string} | null)?.error !== `a branch pair with git_branch '${branch}' already exists`) {
    throw new Error(`repeat publish did not return the completed-child refusal (${result.status})`);
  }
}

export function assertReplayEndpoints(endpoints: unknown, commits: string[]): void {
  // The journal records handled_after once per cycle. One cycle can drain
  // several commits; individual applied sync_records prove each publication.
  if (!Array.isArray(endpoints) || (commits.length === 0 ? endpoints.length !== 0 : endpoints.at(-1) !== commits.at(-1))) {
    throw new Error("publish journal does not end at the selected Git tip");
  }
  let previous = -1;
  for (const endpoint of endpoints) {
    const index = typeof endpoint === "string" ? commits.indexOf(endpoint) : -1;
    if (index <= previous) throw new Error("publish journal contains an unknown, repeated or unordered cycle endpoint");
    previous = index;
  }
}

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`acceptance lab env ${name} is not set`);
  return value;
}

function safe(value: string, label: string): string {
  if (!/^[A-Za-z0-9][A-Za-z0-9._/-]{0,80}$/.test(value)) {
    throw new Error(`unsafe ${label}`);
  }
  return value;
}

export function safeRelativePath(value: string): string {
  if (!value || value.length > 200 || value.split("/").some(part =>
    !/^[A-Za-z0-9_.][A-Za-z0-9._-]*$/.test(part) || part === "." || part === "..")) {
    throw new Error("unsafe path");
  }
  return value;
}

// One worker runs the catalog. Keep its login budget on disk so a failed
// Playwright worker cannot forget requests made by its predecessor.
export async function reserveLoginAttempt(root: string, identity: string,
  now = Date.now, sleep = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms))): Promise<void> {
  const file = `${root}/login-budget.json`;
  const stored = existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : {identity, attempts: []};
  if (stored.identity !== identity || !Array.isArray(stored.attempts)
    || stored.attempts.some((x: unknown) => typeof x !== "number" || !Number.isFinite(x))) {
    throw new Error("invalid or foreign login budget");
  }
  let attempts: number[] = stored.attempts;
  for (;;) {
    const time = now();
    attempts = attempts.filter(t => time - t < 61_000);
    if (attempts.some(t => t > time)) throw new Error("login budget clock moved backwards");
    if (attempts.length < 8) {
      writeFileSync(file, JSON.stringify({identity, attempts: [...attempts, time]}), {mode: 0o600});
      return;
    }
    await sleep(Math.min(60_000, Math.max(1, attempts[0] + 61_000 - time)));
  }
}

function svnTarget(pair: Pair): string { return `${pair.svnUrl}/${pair.svnBranch || "trunk"}`; }

function readResult(output: string): string | null {
  if (output === "LAB_READ_ABSENT\n") return null;
  if (!output.startsWith("LAB_READ_PRESENT\n")) throw new Error("invalid published-file observation");
  return output.slice("LAB_READ_PRESENT\n".length);
}

export function assertLdapUser(result: {status: number; json: unknown}, username: string): void {
  const body = result.json as {token?: string; user?: {username?: string}} | null;
  if (result.status !== 200 || !body?.token || body.user?.username !== username) {
    throw new Error(`healthy LDAP control failed (${result.status})`);
  }
}

export function assertAuthRefusal(result: {status: number; json: unknown}): void {
  if (result.status !== 401 || (result.json as {token?: string} | null)?.token) {
    throw new Error(`expected authentication refusal 401, received ${result.status}; throttle/setup errors are not LDAP evidence`);
  }
}

export function isCredentialRefusal(line: string, pair: Pair, addresses: string[], protocol: "git" | "svn"): boolean {
  if (protocol === "git") {
    return line.includes(`/${pair.gitRepo}.git/`) && addresses.some(ip => line.includes(`for ${ip}:`)) && /, (403|404) /.test(line);
  }
  const repository = line.includes(`/svn/${pair.name}/`) || line.includes(`/svn/${pair.name} `);
  return repository && addresses.some(ip => line.includes(`${ip} - alice `)) && /" 403 /.test(line);
}

export class Lab {
  readonly page: Page;
  readonly info: TestInfo;
  readonly ui = required("LAB_UI_URL");
  readonly project = required("LAB_COMPOSE_PROJECT");
  readonly composeFile = required("LAB_COMPOSE_FILE");
  readonly svnPassword = required("SVN_SYNC_PASSWORD");
  readonly gitToken = required("GITEA_ADMIN_TOKEN");
  readonly gitUser = process.env.GITEA_ADMIN_USER || "labadmin";
  readonly adminUser = process.env.LAB_ADMIN_USER || "admin";
  readonly adminPassword = required("LAB_ADMIN_PASSWORD");
  readonly alicePassword = required("ALICE_PASSWORD");
  readonly bobPassword = required("BOB_PASSWORD");
  readonly bindPassword = required("LDAP_BIND_PASSWORD");
  readonly webhookSecret = required("LAB_WEBHOOK_SECRET");
  readonly svnWebhookSecret = required("LAB_SVN_WEBHOOK_SECRET");
  readonly localPassword = required("LAB_LOCAL_PASSWORD");
  private token = "";
  private minted: string[] = [];

  constructor(page: Page, info: TestInfo) {
    this.page = page;
    this.info = info;
  }

  private rememberSecret(value: string): void {
    if (value.length < 8) return;
    this.minted.push(value);
    const root = process.env.LAB_RUN_DIR;
    if (!root) throw new Error("LAB_RUN_DIR is not set; refusing to mint a credential");
    appendFileSync(`${root}/secrets.env`, `SCOPED_${this.minted.length}=${value}\n`);
  }

  mark(status: "PASS" | "FAIL" | "PARTIAL" | "NOT_RUN", reason: string, qualification = ""): void {
    this.info.annotations.push({ type: "labStatus", description: status });
    this.info.annotations.push({
      type: "labQualification",
      description: qualification || (status === "PASS" ? "executed" : "UNQUALIFIED"),
    });
    if (reason) this.info.annotations.push({ type: "labReason", description: reason });
  }

  pass(reason = ""): void {
    this.mark("PASS", reason, "executed");
  }

  gap(reason: string): never {
    this.mark("FAIL", reason, "UNQUALIFIED");
    throw new Error(reason);
  }

  observation(name: string, data: unknown): void {
    let text = JSON.stringify(data);
    for (const secret of [this.svnPassword, this.gitToken, this.adminPassword, this.alicePassword,
      this.bobPassword, this.bindPassword, this.localPassword, this.webhookSecret, this.svnWebhookSecret, ...this.minted]) {
      if (secret) text = text.replaceAll(secret, "[redacted]");
    }
    this.info.annotations.push({type: "labObservation", description: `${name}: ${text}`});
    const root = `${required("LAB_RUN_DIR")}/evidence/observations`;
    mkdirSync(root, {recursive: true});
    writeFileSync(`${root}/${safe(name, "observation")}-${randomBytes(4).toString("hex")}.json`, text + "\n");
  }

  async sqlRows(sql: string): Promise<Array<Record<string, any>>> {
    // SQL is supplied only by harness code; observations always open readonly.
    const encoded = Buffer.from(sql, "utf8").toString("base64");
    const result = await this.sh("reposync", `set -eu\nprintf %s '${encoded}' | base64 -d | sqlite3 -readonly -json /var/lib/reposync/reposync.db`, "root");
    return result.trim() ? JSON.parse(result) : [];
  }

  async expectSvnRefusal(pair: Pair, before: Snap, revision: string): Promise<void> {
    if (!/^[0-9]+$/.test(revision)) throw new Error("invalid refusal source revision");
    const postSource = await this.snap(pair);
    this.observation(`${pair.name}-refusal-source`, {before, postSource, revision});
    assertSvnRefusalState(before, postSource, revision);
    await this.triggerSvn(revision);
    const start = Date.now();
    let receipt: Array<Record<string, any>> = [];
    await this.waitUntil(async () => {
      const now = await this.snap(pair);
      assertSvnRefusalState(before, now, revision);
      receipt = await this.sqlRows(`SELECT action,svn_rev,details,success,repo_id FROM audit_log WHERE repo_id='${safe(pair.id, "repo id")}' AND svn_rev=${revision} AND direction='svn_to_git' AND action='svn_to_git_apply_failed' AND success=0;`);
      return Date.now() - start >= 12_000 && receipt.length > 0;
    }, 25_000);
    this.observation(`${pair.name}-refusal-receipt`, {revision, receipt, after: await this.snap(pair)});
  }

  async sh(service: string, script: string, user = ""): Promise<string> {
    const payload = Buffer.from(script, "utf8").toString("base64");
    const args = ["compose", "-p", this.project, "-f", this.composeFile, "exec", "-T"];
    if (user) args.push("-u", user);
    if (user === "observer") args.push("-e", "HOME=/home/observer");
    args.push(
      "-e", "SVN_PASSWORD",
      "-e", "GIT_TOKEN",
      "-e", "GIT_USER",
      "-e", "ALICE_PASSWORD",
      "-e", "BOB_PASSWORD",
      "-e", "LDAP_BIND_PASSWORD",
      "-e", "WEBHOOK_SECRET",
      "-e", "SVN_WEBHOOK_SECRET",
      "-e", "SCRIPT_B64",
      service,
      "sh", "-c", "printf %s \"$SCRIPT_B64\" | base64 -d | sh",
    );
    const env = {
      ...process.env,
      SVN_PASSWORD: this.svnPassword,
      GIT_TOKEN: this.gitToken,
      GIT_USER: this.gitUser,
      ALICE_PASSWORD: this.alicePassword,
      BOB_PASSWORD: this.bobPassword,
      LDAP_BIND_PASSWORD: this.bindPassword,
      WEBHOOK_SECRET: this.webhookSecret,
      SVN_WEBHOOK_SECRET: this.svnWebhookSecret,
      SCRIPT_B64: payload,
    };
    try {
      const result = await exec("docker", args, { env, maxBuffer: 8_000_000, timeout: 240_000 });
      return result.stdout;
    } catch (error) {
      const failed = error as { stdout?: string; stderr?: string; message?: string };
      let stderr = `${failed.stderr || ""}\n${failed.stdout || ""}`;
      for (const secret of [
        this.svnPassword,
        this.gitToken,
        this.adminPassword,
        this.alicePassword,
        this.bobPassword,
        this.bindPassword,
        this.localPassword,
        this.webhookSecret,
        this.svnWebhookSecret,
        ...this.minted,
      ]) {
        if (secret) stderr = stderr.replaceAll(secret, "[redacted]");
      }
      if (stderr.length > 2000) stderr = stderr.slice(-2000);
      throw new Error(`docker exec ${service} failed: ${stderr || failed.message}`);
    }
  }

  observer(script: string): Promise<string> {
    return this.sh("observer", script, "observer");
  }

  async api(method: string, path: string, body?: unknown, token = this.token): Promise<{ status: number; json: unknown; text: string }> {
    if (method === "POST" && path === "/api/auth/login") await this.reserveLogin();
    const headers: Record<string, string> = {};
    if (token) headers.authorization = `Bearer ${token}`;
    if (body !== undefined) headers["content-type"] = "application/json";
    const response = await fetch(`${this.ui}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await response.text();
    let json: unknown = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = null;
    }
    if (path === "/api/auth/login" && response.status === 200) {
      const minted = (json as {token?: string} | null)?.token;
      if (minted) this.rememberSecret(minted);
    }
    return { status: response.status, json, text };
  }

  async reserveLogin(): Promise<void> {
    await reserveLoginAttempt(required("LAB_RUN_DIR"), `${this.project}|${this.ui}`);
  }

  async admin(): Promise<string> {
    if (this.token) return this.token;
    const result = await this.api("POST", "/api/auth/login", {
      username: this.adminUser,
      password: this.adminPassword,
    }, "");
    const token = (result.json as { token?: string } | null)?.token;
    if (result.status !== 200 || !token) {
      const reason = (result.json as {error?: string} | null)?.error || "no server reason";
      throw new Error(`admin login failed (${result.status}): ${reason.replaceAll(this.adminPassword, "[redacted]").slice(0, 200)}`);
    }
    this.token = token;
    return token;
  }

  async login(username: string, password: string): Promise<{ status: number; json: unknown }> {
    const result = await this.api("POST", "/api/auth/login", { username, password }, "");
    return result;
  }

  async repos(): Promise<Array<{ id: string; git_repo: string; name: string; svn_url: string; git_branch?: string; svn_branch?: string; parent_id?: string; enabled?: boolean }>> {
    await this.admin();
    const result = await this.api("GET", "/api/repos");
    if (result.status !== 200 || !Array.isArray(result.json)) throw new Error(`list repos failed (${result.status})`);
    return result.json as Array<{ id: string; git_repo: string; name: string; svn_url: string }>;
  }

  async bootstrap(): Promise<Pair> {
    const rows = await this.repos();
    const row = rows.find((item) => item.git_repo === "lab/bootstrap");
    if (!row) throw new Error("daemon did not register lab/bootstrap from the lab config");
    await this.api("POST", `/api/repos/${row.id}/credentials`, {
      svn_password: this.svnPassword,
      git_token: this.gitToken,
    });
    const detail = await this.api("GET", `/api/repos/${row.id}`);
    const initializing = Boolean((detail.json as { initializing?: boolean } | null)?.initializing);
    if (initializing) {
      const started = await this.api("POST", `/api/repos/${row.id}/import`, { import_mode: "full" });
      if (started.status !== 200) throw new Error(`bootstrap import failed (${started.status}) ${started.text.slice(0, 300)}`);
      await this.waitImport(row.id);
    }
    return { id: row.id, name: "bootstrap", svnUrl: "https://svn:8443/svn/bootstrap", gitRepo: "lab/bootstrap" };
  }

  async waitImport(id: string, operation?: string): Promise<void> {
    let last: unknown = null;
    let pinnedOperation = operation;
    try {
      await expect.poll(async () => {
        const status = await this.api("GET", `/api/repos/${id}/import/status`);
        last = status.json;
        const body = status.json as {phase?: string; lifecycle?: string; operation_id?: string; error?: string; outcome_detail?: string} | null;
        if (status.status !== 200 || !body || !body.operation_id || (pinnedOperation && body.operation_id !== pinnedOperation)) {
          throw new Error(`import ${id} status is invalid (${status.status}) or belongs to another operation`);
        }
        pinnedOperation ??= body.operation_id;
        const phase = (body.phase || body.lifecycle || "").toLowerCase();
        if (!phase) throw new Error(`import ${id} returned no phase`);
        if (!body.lifecycle) throw new Error(`import ${id} returned no durable lifecycle`);
        if (/failed|cancelled|reconciliation.?required/i.test(`${phase} ${body.lifecycle || ""}`)) {
          throw new Error(`import ${id} terminated as ${phase}: ${body.error || body.outcome_detail || "no server detail"}`);
        }
        // The in-memory progress can finish before durable finalization.
        return phase === "completed" ? body.lifecycle.toLowerCase() : phase;
      }, {timeout: 180_000, intervals: [1000, 2000, 3000]}).toBe("completed");
      this.observation(`import-${safe(id, "repo id")}`, {repo_id: id, operation_id: pinnedOperation, result: last});
    } catch (error) {
      this.observation(`import-${safe(id, "repo id")}`, {repo_id: id, operation_id: pinnedOperation, result: last, setup_failed: true});
      throw error;
    }
  }

  async giteaApi(method: "GET" | "POST", path: string, body?: unknown): Promise<{status: number; json: any}> {
    if (!/^\/api\/v1\/[A-Za-z0-9_./-]+$/.test(path)) throw new Error("unsafe Gitea fixture API path");
    const payload = Buffer.from(JSON.stringify(body ?? {}), "utf8").toString("base64");
    const output = await this.sh("gitea", `
set -eu
data=$(mktemp)
trap 'rm -f "$data"' EXIT
printf %s '${payload}' | base64 -d > "$data"
curl --silent --show-error --connect-timeout 5 --max-time 30 --cacert /certs/ca.crt \
  -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" -X ${method} \
  ${method === "POST" ? '--data-binary "@$data"' : ""} \
  --write-out '\\nLAB_HTTP_STATUS:%{http_code}\\n' https://gitea:3000${path}
`);
    const status = output.match(/\nLAB_HTTP_STATUS:([0-9]{3})\n$/);
    if (!status) throw new Error("Gitea fixture API returned no HTTP status");
    let json: unknown = null;
    try {json = JSON.parse(output.slice(0, status.index));} catch { /* Status still identifies the failed provider request. */ }
    return {status: Number(status[1]), json};
  }

  async enableFixturePair(pair: Pair): Promise<void> {
    const enabled = await this.api("PUT", `/api/repos/${pair.id}`, {enabled: true});
    const body = enabled.json as {id?: string; enabled?: boolean; git_repo?: string; svn_url?: string} | null;
    if (enabled.status !== 200 || body?.id !== pair.id || body.enabled !== true || body.git_repo !== pair.gitRepo || body.svn_url !== pair.svnUrl) {
      throw new Error(`fixture ${pair.name} activation failed (${enabled.status})`);
    }
  }

  async faultRequest(path: "stats" | "drop-next-response", body?: {count: number}): Promise<unknown> {
    const root = new URL(required("LAB_FAULT_CONTROL_URL"));
    if (root.protocol !== "http:" || root.hostname !== "127.0.0.1" || !root.port || root.pathname !== "/" || root.username || root.password || root.search || root.hash) {
      throw new Error("fault control must use the run's published loopback port");
    }
    const response = await fetch(new URL(path, root), {method: body ? "POST" : "GET", headers: {"content-type": "application/json"},
      body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(5000), redirect: "error"});
    if (response.status !== 200) throw new Error(`fault control ${path} failed (${response.status})`);
    return response.json();
  }

  async createPair(name: string): Promise<Pair> {
    const repo = safe(name, "repo");
    // Playwright restarts its worker after a failed case. Find the persisted
    // pair instead of registering a duplicate when the in-memory cache is gone.
    const existing = (await this.repos()).filter(row => row.git_repo === `lab/${repo}` || row.name === repo);
    if (existing.length > 1) throw new Error(`ambiguous existing lab pair ${repo}`);
    if (existing.length === 1) {
      const row = existing[0];
      if (row.name !== repo || row.git_repo !== `lab/${repo}` || row.svn_url !== `https://svn:8443/svn/${repo}`) throw new Error(`existing lab pair ${repo} has different remotes`);
      await this.waitImport(row.id);
      const pair = {id: row.id, name: repo, svnUrl: row.svn_url, gitRepo: row.git_repo};
      if (row.enabled === false) await this.enableFixturePair(pair);
      return pair;
    }
    await this.sh("svn", `
set -eu
if [ ! -d /var/svn/${repo} ]; then
  svnadmin create /var/svn/${repo}
  chown -R www-data:www-data /var/svn/${repo}
  work=$(mktemp -d)
  svn checkout --non-interactive "file:///var/svn/${repo}" "$work"
  mkdir -p "$work/trunk" "$work/branches" "$work/tags"
  printf 'seed %s\\n' ${repo} > "$work/trunk/README.txt"
  svn add "$work/trunk" "$work/branches" "$work/tags"
  svn commit --username svc-sync -m "lab seed ${repo}" "$work"
  rm -rf "$work"
  chown -R www-data:www-data /var/svn/${repo}
fi
`);
    const organization = await this.giteaApi("GET", "/api/v1/orgs/lab");
    if (organization.status !== 200) throw new Error(`fixture organization unavailable (${organization.status})`);
    const provider = await this.giteaApi("POST", "/api/v1/orgs/lab/repos", {name: repo, private: true, auto_init: false, default_branch: "main"});
    if (![201, 409, 422].includes(provider.status)) throw new Error(`Gitea fixture ${repo} creation failed (${provider.status})`);
    const remote = await this.giteaApi("GET", `/api/v1/repos/lab/${repo}`);
    if (remote.status !== 200 || remote.json?.full_name !== `lab/${repo}` || remote.json.private !== true || remote.json.default_branch !== "main") {
      throw new Error(`Gitea fixture ${repo} has missing or different remote metadata (${remote.status})`);
    }
    const refs = await this.observer(`set -eu\nauth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')\ngit -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt ls-remote https://gitea:3000/lab/${repo}.git\n`);
    if (refs.trim()) throw new Error(`Gitea fixture ${repo} is not empty; refusing to replay into an existing target`);
    await this.admin();
    const created = await this.api("POST", "/api/repos", {
      name: repo,
      svn_url: `https://svn:8443/svn/${repo}`,
      svn_branch: "trunk",
      svn_username: "svc-sync",
      git_provider: "gitea",
      git_api_url: "https://gitea:3000/api/v1",
      git_repo: `lab/${repo}`,
      git_branch: "main",
      sync_mode: "direct",
      poll_interval_secs: 30,
      // Keep the ordinary poller out of the initial-import working tree.
      enabled: false,
    });
    const id = (created.json as { id?: string } | null)?.id;
    if (created.status !== 200 || !id) throw new Error(`create repo ${repo} failed (${created.status})`);
    const creds = await this.api("POST", `/api/repos/${id}/credentials`, {
      svn_password: this.svnPassword,
      git_token: this.gitToken,
    });
    if (creds.status !== 200) throw new Error(`credentials for ${repo} failed`);
    const started = await this.api("POST", `/api/repos/${id}/import`, { import_mode: "full" });
    if (started.status !== 200) throw new Error(`import ${repo} failed (${started.status}) ${started.text.slice(0, 400)}`);
    const operation = (started.json as {operation_id?: string} | null)?.operation_id;
    if (!operation) throw new Error(`import ${repo} returned no operation id`);
    await this.waitImport(id, operation);
    const pair = {id, name: repo, svnUrl: `https://svn:8443/svn/${repo}`, gitRepo: `lab/${repo}`};
    const baseline = await this.snap(pair);
    if (baseline.svn !== "1" || baseline.watermark !== "1" || baseline.maps !== "1" || baseline.records !== "1"
      || !/^[0-9a-f]{40}$/.test(baseline.git) || baseline.gitWatermark !== baseline.git || baseline.gitColumn !== baseline.git) {
      this.observation(`${repo}-import-baseline`, {pair, operation, baseline, setup_failed: true});
      throw new Error(`fixture ${repo} import did not establish the exact r1 baseline`);
    }
    await this.assertSingleReceipt(pair, "svn_to_git", "1");
    await this.enableFixturePair(pair);
    this.observation(`${repo}-import-ready`, {pair, operation, provider_status: provider.status, baseline, scheduler_enabled_after_import: true});
    return pair;
  }

  async createChildPair(parent: Pair, branchName: string): Promise<Pair> {
    const branch = safe(branchName, "branch");
    // Admission requires a real Git ref with SVN-origin ancestry. Seed the
    // ref from the imported parent; the product owns SVN copy and registration.
    await this.commitGit(parent, `git branch ${branch}\ngit push origin refs/heads/${branch}:refs/heads/${branch}\n`);
    const published = await this.api("POST", `/api/repos/${parent.id}/branches`, {
      svn_branch: `branches/${branch}`, git_branch: branch, dry_run: false,
      auto_create_svn_branch: true, auto_create_git_branch: false,
    });
    if (published.status !== 200) throw new Error(`child fixture admission failed (${published.status}): ${published.text.slice(0, 350)}`);
    const rows = (await this.repos()).filter(row => row.parent_id === parent.id && row.git_branch === branch);
    if (rows.length !== 1 || rows[0].svn_url !== parent.svnUrl || rows[0].svn_branch !== `branches/${branch}` || rows[0].git_repo !== parent.gitRepo) {
      throw new Error("child fixture did not establish the expected parent and remotes");
    }
    const child: Pair = {id: rows[0].id, name: parent.name, svnUrl: parent.svnUrl, gitRepo: parent.gitRepo,
      svnBranch: rows[0].svn_branch, gitBranch: branch, parentId: parent.id};
    const head = await this.snap(child);
    if (!/^[0-9a-f]{40}$/.test(head.git) || !/^[0-9]+$/.test(head.svn)) throw new Error("child fixture remote pins are missing");
    this.info.annotations.push({type: "labObservation", description: `child ${child.id} parent ${parent.id} Git ${branch}@${head.git} SVN ${child.svnBranch}@${head.svn}`});
    return child;
  }

  async gitRemoteHeads(pair: Pair): Promise<Record<string, string>> {
    if (!/^lab\/[A-Za-z0-9_.-]+$/.test(pair.gitRepo)) throw new Error("unsafe fixture Git repository");
    const output = await this.observer(`set -eu\nauth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')\ngit -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt ls-remote --heads https://gitea:3000/${pair.gitRepo}.git\n`);
    const heads: Record<string, string> = {};
    for (const line of output.trim().split("\n").filter(Boolean)) {
      const match = line.match(/^([0-9a-f]{40})\t(refs\/heads\/[A-Za-z0-9_.\/-]+)$/);
      if (!match || heads[match[2]]) throw new Error("invalid or duplicate remote Git head");
      heads[match[2]] = match[1];
    }
    return heads;
  }

  async svnBranchNames(parent: Pair): Promise<string[]> {
    if (!/^https:\/\/svn:8443\/svn\/[A-Za-z0-9_.-]+$/.test(parent.svnUrl)) throw new Error("unsafe fixture SVN repository");
    // Read the existing branches directory successfully. A transport/auth error
    // or missing repository is never converted into target-path absence.
    const output = await this.observer(`set -eu\nexport LC_ALL=C\nsvn list --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" ${parent.svnUrl}/branches\n`);
    const names = output.trim().split("\n").filter(Boolean).map(line => {
      if (!/^[A-Za-z0-9_.-]+\/$/.test(line)) throw new Error("unexpected SVN branches entry");
      return line.slice(0, -1);
    });
    if (new Set(names).size !== names.length) throw new Error("duplicate SVN branch entry");
    return names.sort();
  }

  // A Gitea user who can write only this repository. The token is not the lab admin token.
  async issueScopedGitToken(repoName: string, otherRepo: string): Promise<{ token: string; user: string; tokenName: string }> {
    const repo = safe(repoName, "repo");
    const other = safe(otherRepo, "repo");
    const user = safe(`git${repo}`, "user");
    const tokenName = safe(`lab${repo}`, "token");
    const team = safe(`write${repo}`, "team");
    const password = `${randomBytes(18).toString("hex")}Aa1!`;
    this.rememberSecret(password);
    const output = await this.sh("gitea", `
set -eu
fail_step() {
  label="$1"
  file="$2"
  msg=$(grep -o '"message":"[^"]*"' "$file" 2>/dev/null | head -n 1 | cut -d'"' -f4 || true)
  echo "$label $code $msg"
  exit 1
}
code=$(curl --silent --show-error --output /tmp/cu.out --write-out '%{http_code}' --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" -d '{"username":"${user}","email":"${user}@lab.invalid","password":"${password}","must_change_password":false,"visibility":"private","source_id":0}' https://gitea:3000/api/v1/admin/users || true)
case "$code" in
  201|409) ;;
  *) fail_step CREATE_USER /tmp/cu.out ;;
esac
code=$(curl --silent --show-error --output /tmp/team.out --write-out '%{http_code}' --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" -d '{"name":"${team}","permission":"write","can_create_org_repo":false,"includes_all_repositories":false,"units":["repo.code","repo.pulls"]}' https://gitea:3000/api/v1/orgs/lab/teams || true)
case "$code" in
  201) ;;
  *) fail_step CREATE_TEAM /tmp/team.out ;;
esac
team_id=$(grep -o '"id":[0-9]*' /tmp/team.out | head -n 1 | cut -d: -f2)
if [ -z "$team_id" ]; then echo "TEAM_ID"; exit 1; fi
code=$(curl --silent --show-error --output /tmp/tr.out --write-out '%{http_code}' --cacert /certs/ca.crt -X PUT -H "Authorization: token $GIT_TOKEN" "https://gitea:3000/api/v1/teams/$team_id/repos/lab/${repo}" || true)
case "$code" in
  204|201) ;;
  *) fail_step TEAM_REPO /tmp/tr.out ;;
esac
code=$(curl --silent --show-error --output /tmp/tm.out --write-out '%{http_code}' --cacert /certs/ca.crt -X PUT -H "Authorization: token $GIT_TOKEN" "https://gitea:3000/api/v1/teams/$team_id/members/${user}" || true)
case "$code" in
  204|201) ;;
  *) fail_step TEAM_MEMBER /tmp/tm.out ;;
esac
token_cli="gitea admin user generate-access-token --config /data/gitea/conf/app.ini --username ${user} --token-name ${tokenName} --scopes all --raw"
if ! sh -c "$token_cli" > /tmp/ct.out 2>/tmp/ct.err; then
  if ! su -s /bin/sh git -c "$token_cli" > /tmp/ct.out 2>/tmp/ct.err; then
    echo "TOKEN_CLI $(tr '\n' ' ' < /tmp/ct.err | cut -c1-180)"
    rm -f /tmp/ct.out /tmp/ct.err
    exit 1
  fi
fi
value=$(awk 'NF { line=$NF } END { print line }' /tmp/ct.out)
rm -f /tmp/cu.out /tmp/team.out /tmp/tr.out /tmp/tm.out /tmp/ct.out /tmp/ct.err
if ! printf '%s\n' "$value" | grep -Eq '^[0-9a-fA-F]{20,}$'; then
  echo "TOKEN_PARSE"
  exit 1
fi
own=$(curl --silent --output /dev/null --write-out '%{http_code}' --cacert /certs/ca.crt -H "Authorization: token $value" "https://gitea:3000/api/v1/repos/lab/${repo}")
other_code=$(curl --silent --output /dev/null --write-out '%{http_code}' --cacert /certs/ca.crt -H "Authorization: token $value" "https://gitea:3000/api/v1/repos/lab/${other}")
case "$own" in
  200) ;;
  *) echo "OWN $own"; exit 1 ;;
esac
case "$other_code" in
  403|404) ;;
  *) echo "OTHER $other_code"; exit 1 ;;
esac
printf 'SCOPED own=%s other=%s\n' "$own" "$other_code"
printf '%s\n' "$value"
`);
    const lines = output.trim().split("\n").map((line) => line.trim()).filter(Boolean);
    const marker = lines.find((line) => line.startsWith("SCOPED "));
    const token = lines[lines.length - 1] || "";
    if (!marker || !/^[0-9a-fA-F]{20,}$/.test(token)) {
      throw new Error(`scoped git token for ${repo} was not issued (${marker || "no marker"})`);
    }
    this.rememberSecret(token);
    return { token, user, tokenName };
  }

  async revokeScopedGitToken(repoName: string): Promise<void> {
    const repo = safe(repoName, "repo");
    const user = safe(`git${repo}`, "user");
    const team = safe(`write${repo}`, "team");
    const output = await this.sh("gitea", `
set -eu
json=$(curl --silent --show-error --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" https://gitea:3000/api/v1/orgs/lab/teams)
team_id=$(printf '%s' "$json" | tr '{' '\n' | grep '"name":"${team}"' | grep -o '"id":[0-9]*' | head -n 1 | cut -d: -f2)
if [ -z "$team_id" ]; then echo "REVOKE_TEAM"; exit 1; fi
code=$(curl --silent --output /tmp/rv.out --write-out '%{http_code}' --cacert /certs/ca.crt -X DELETE -H "Authorization: token $GIT_TOKEN" "https://gitea:3000/api/v1/teams/$team_id/members/${user}" || true)
case "$code" in
  204|200) ;;
  *) echo "REVOKE $code"; exit 1 ;;
esac
rm -f /tmp/rv.out
printf 'REVOKED\n'
`);
    if (!output.includes("REVOKED")) throw new Error(`scoped git write for ${repo} was not revoked`);
  }

  async removeScopedGitTeam(repoName: string): Promise<void> {
    const repo = safe(repoName, "repo");
    const team = safe(`write${repo}`, "team");
    try {
      await this.sh("gitea", `
set -eu
json=$(curl --silent --show-error --cacert /certs/ca.crt -H "Authorization: token $GIT_TOKEN" https://gitea:3000/api/v1/orgs/lab/teams)
team_id=$(printf '%s' "$json" | tr '{' '\n' | grep '"name":"${team}"' | grep -o '"id":[0-9]*' | head -n 1 | cut -d: -f2)
if [ -z "$team_id" ]; then printf 'ABSENT\n'; exit 0; fi
code=$(curl --silent --output /dev/null --write-out '%{http_code}' --cacert /certs/ca.crt -X DELETE -H "Authorization: token $GIT_TOKEN" "https://gitea:3000/api/v1/teams/$team_id" || true)
case "$code" in
  204|200|404) ;;
  *) echo "DELETE_TEAM $code"; exit 1 ;;
esac
printf 'REMOVED\n'
`);
    } catch {
      // A leftover team must not hide the isolation result, and must not stay for the next test.
    }
  }

  async snap(pair: Pair): Promise<Snap> {
    const svn = (await this.observer(`
set -eu
svn info --non-interactive \
  --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" \
  --show-item revision ${svnTarget(pair)}
`)).trim();
    const git = (await this.observer(`
set -eu
auth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')
git -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt \
  ls-remote https://gitea:3000/${pair.gitRepo}.git refs/heads/${safe(pair.gitBranch || "main", "ref")} | awk '{print $1}'
`)).trim();
    const counts = (await this.sh("reposync", `
set -eu
db=/var/lib/reposync/reposync.db
scope_column=$(sqlite3 -readonly "$db" "SELECT COUNT(*) FROM pragma_table_info('repositories') WHERE name='scope_uuid';")
case "$scope_column" in
  0) checkpoint_scope='${safe(pair.id, "repo id")}' ;;
  1)
    checkpoint_scope=$(sqlite3 -readonly "$db" "SELECT scope_uuid FROM repositories WHERE id='${safe(pair.id, "repo id")}';")
    printf '%s\\n' "$checkpoint_scope" | grep -Eq '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$' || { echo 'missing or invalid repository scope UUID' >&2; exit 1; }
    ;;
  *) echo 'cannot identify repository checkpoint schema' >&2; exit 1 ;;
esac
# New schemas use the durable scope UUID. Never borrow a legacy mirror when
# that scoped checkpoint is absent; absence must fail recovery qualification.
sqlite3 -readonly "$db" "SELECT COUNT(*) FROM commit_map WHERE repo_id='${pair.id}'; SELECT COUNT(*) FROM sync_records WHERE repo_id='${pair.id}'; SELECT COALESCE((SELECT value FROM kv_state WHERE key='last_svn_rev_${pair.id}'), 'none'); SELECT COALESCE((SELECT value FROM kv_state WHERE key='last_git_sha_$checkpoint_scope'), 'none'); SELECT COALESCE(NULLIF(last_git_sha, ''), 'none') FROM repositories WHERE id='${pair.id}';"
`, "root")).trim().split(/\s+/);
    return {
      svn,
      git,
      maps: counts[0] || "0",
      records: counts[1] || "0",
      watermark: counts[2] || "none",
      gitWatermark: counts[3] || "none",
      gitColumn: counts[4] || "none",
    };
  }

  async expectCredentialDenial(pair: Pair, trigger: () => Promise<void>, protocol: "git" | "svn"): Promise<void> {
    // Server-side refusal must come from this daemon, on this repository, after
    // the committed source change. Observer reads and normal 401 challenges do
    // not prove that the daemon used the wrong scoped credential.
    const prefix = ["compose", "-p", this.project, "-f", this.composeFile];
    const ids = (await exec("docker", [...prefix, "ps", "-q", "reposync"], { timeout: 10_000 })).stdout.trim().split(/\s+/);
    if (ids.length !== 1 || !/^[0-9a-f]{12,64}$/.test(ids[0])) throw new Error("cannot identify the daemon for authentication observation");
    const addresses = (await exec("docker", ["inspect", "--format", "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}", ids[0]], { timeout: 10_000 })).stdout.trim().split(/\s+/);
    if (!addresses.length || addresses.some(ip => !/^(?:[0-9]{1,3}\.){3}[0-9]{1,3}$/.test(ip))) throw new Error("cannot identify daemon network addresses");
    const since = new Date().toISOString();
    await trigger(); // An HTTP/setup error is a failure, never a denial receipt.
    let refusal = "";
    await this.waitUntil(async () => {
      const result = await exec("docker", [...prefix, "logs", "--no-color", "--since", since, protocol === "git" ? "gitea" : "svn"], { timeout: 10_000, maxBuffer: 8_000_000 });
      const lines = `${result.stdout}\n${result.stderr}`.split("\n");
      refusal = lines.find(line => isCredentialRefusal(line, pair, addresses, protocol)) || "";
      return Boolean(refusal);
    }, 90_000);
    if (!refusal) throw new Error("credential refusal from the daemon was not observed");
    this.info.annotations.push({ type: "labObservation", description: `${pair.name}: ${protocol} server refused daemon access after committed source change: ${refusal}` });
  }

  async assertSingleReceipt(pair: Pair, direction: "svn_to_git" | "git_to_svn", source: string): Promise<void> {
    if (direction === "svn_to_git" ? !/^[0-9]+$/.test(source) : !/^[0-9a-f]{40}$/.test(source)) throw new Error("invalid receipt source");
    const column = direction === "svn_to_git" ? "svn_rev" : "git_sha";
    const count = (await this.sh("reposync", `sqlite3 /var/lib/reposync/reposync.db "SELECT COUNT(*) FROM sync_records WHERE repo_id='${safe(pair.id, "repo id")}' AND direction='${direction}' AND status='applied' AND ${column}='${source}';"`, "root")).trim();
    if (count !== "1") throw new Error(`${pair.name} ${direction} source has ${count} receipts instead of one`);
  }

  async waitSvnPublication(pair: Pair, before: Snap, revision: string): Promise<Snap> {
    await this.waitUntil(async () => {
      const now = await this.snap(pair);
      return now.svn === revision && now.git !== before.git && now.records !== before.records && now.watermark === revision;
    });
    return this.snap(pair);
  }

  async waitGitPublication(pair: Pair, before: Snap, source: string): Promise<Snap> {
    // Git-to-SVN confirmation advances both Git checkpoints, not the SVN-read
    // cursor (db/svn_commit_operations.rs::finalize_tx).
    try {
      await this.waitUntil(async () => {
        const now = await this.snap(pair);
        return now.svn !== before.svn && now.records !== before.records
          && now.gitWatermark === source && now.gitColumn === source;
      });
    } catch (error) {
      throw new Error(`Git recovery checkpoint not observed: before=${JSON.stringify(before)} after=${JSON.stringify(await this.snap(pair))}; ${error}`);
    }
    return this.snap(pair);
  }

  async triggerSvn(revision: string): Promise<void> {
    const response = await fetch(`${this.ui}/webhook/svn`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-webhook-secret": this.svnWebhookSecret,
      },
      body: JSON.stringify({ revision: Number(revision), author: "svc-sync", message: "lab" }),
    });
    if (response.status !== 200) throw new Error(`svn webhook ${response.status}`);
  }

  async triggerGit(repo: string, forced = false): Promise<number> {
    const body = JSON.stringify({
      ref: "refs/heads/main",
      forced,
      repository: { full_name: repo },
    });
    const signature = createHmac("sha256", this.webhookSecret).update(body).digest("hex");
    const response = await fetch(`${this.ui}/webhook/github`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-gitea-event": "push",
        "x-gitea-signature": signature,
      },
      body,
    });
    return response.status;
  }

  async waitUntil(read: () => Promise<boolean>, timeout = 90_000): Promise<void> {
    await expect.poll(read, { timeout, intervals: [1000, 2000] }).toBe(true);
  }

  async svnBytes(pair: Pair, path: string): Promise<string> {
    const bytes = await this.svnBytesOrMissing(pair, path);
    if (bytes === null) throw new Error(`published SVN path is absent: ${path}`);
    return bytes;
  }

  async svnBytesOrMissing(pair: Pair, path: string, revision?: string): Promise<string | null> {
    const rel = safeRelativePath(path);
    if (revision !== undefined && !/^[0-9]+$/.test(revision)) throw new Error("invalid SVN read revision");
    return readResult(await this.observer(`
set -eu
export LC_ALL=C
data=$(mktemp); err=$(mktemp)
trap 'rm -f "$data" "$err"' EXIT
if svn cat ${revision === undefined ? "" : `-r ${revision}`} --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" \
  ${svnTarget(pair)}/${rel}${revision === undefined ? "" : `@${revision}`} >"$data" 2>"$err"; then
  printf 'LAB_READ_PRESENT\\n'; cat "$data"
elif grep -q "W160013:.*path not found" "$err" && ! grep -E '(E|W)[0-9]{6}' "$err" | grep -Ev '(W160013|E200009)' >/dev/null; then
  printf 'LAB_READ_ABSENT\\n'
else
  cat "$err" >&2; exit 1
fi
`));
  }

  async gitBytes(pair: Pair, path: string): Promise<string> {
    const bytes = await this.gitBytesOrMissing(pair, path);
    if (bytes === null) throw new Error(`published Git path is absent: ${path}`);
    return bytes;
  }

  async gitBytesOrMissing(pair: Pair, path: string): Promise<string | null> {
    const rel = safeRelativePath(path);
    const branch = safe(pair.gitBranch || "main", "ref");
    const dir = `/work/read-${safe(pair.id, "repo id")}`;
    return readResult(await this.observer(`
set -eu
auth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')
if [ ! -d ${dir}/.git ]; then
  git -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt \
    clone --branch ${branch} https://gitea:3000/${pair.gitRepo}.git ${dir}
fi
git -C ${dir} -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt fetch origin ${branch}
entry=$(git -C ${dir} ls-tree FETCH_HEAD -- ${rel})
if [ -z "$entry" ]; then printf 'LAB_READ_ABSENT\\n'; else
  printf 'LAB_READ_PRESENT\\n'; git -C ${dir} show FETCH_HEAD:${rel}
fi
`));
  }

  async commitSvn(pair: Pair, script: string, username = "svc-sync"): Promise<string> {
    const dir = `/work/svn-${pair.name}-${randomBytes(3).toString("hex")}`;
    const user = safe(username, "svn user");
    const password = user === "alice" ? "$ALICE_PASSWORD" : user === "bob" ? "$BOB_PASSWORD" : "$SVN_PASSWORD";
    const output = await this.observer(`
set -eu
export LC_ALL=C
svn checkout --non-interactive --config-dir "$HOME/.svn" --username ${user} --password "${password}" \
  ${svnTarget(pair)} ${dir}
cd ${dir}
${script}
svn commit --non-interactive --username ${user} --password "${password}" -m "lab ${pair.name}" .
`);
    const committed = output.match(/^Committed revision ([0-9]+)\.$/m);
    if (!committed) throw new Error("SVN commit did not return a committed revision");
    return committed[1];
  }

  async svnRevisionLog(pair: Pair, revision: string): Promise<string> {
    if (!/^[0-9]+$/.test(revision)) throw new Error("invalid SVN log revision");
    return this.observer(`set -eu\nsvn log --xml -v -r ${revision} --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" ${pair.svnUrl}\n`);
  }

  async svnBytesAtRevision(pair: Pair, path: string, revision: string): Promise<string> {
    if (!/^[0-9]+$/.test(revision)) throw new Error("invalid SVN read revision");
    return this.observer(`set -eu\nsvn cat -r ${revision} --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" ${svnTarget(pair)}/${safeRelativePath(path)}@${revision}\n`);
  }

  async commitGit(pair: Pair, script: string, ref = pair.gitBranch || "main", publish = true): Promise<string> {
    const dir = `/work/git-${pair.name}-${randomBytes(3).toString("hex")}`;
    const branch = safe(ref, "ref");
    const output = await this.observer(`
set -eu
auth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')
export GIT_CONFIG_COUNT=1
export GIT_CONFIG_KEY_0=http.extraHeader
export GIT_CONFIG_VALUE_0="Authorization: Basic $auth"
rm -rf ${dir}
git -c http.sslCAInfo=/certs/ca.crt clone --branch ${branch} https://gitea:3000/${pair.gitRepo}.git ${dir}
cd ${dir}
git config user.name "Alice Lab"
git config user.email "alice@lab.invalid"
${script}
${publish ? `git push origin ${branch}` : "# Local-only history edit; no push or webhook."}
git rev-parse HEAD
`);
    const lines = output.trim().split("\n");
    return lines[lines.length - 1].trim();
  }

  async commitGitLocal(pair: Pair, script: string): Promise<string> {
    return this.commitGit(pair, script, pair.gitBranch || "main", false);
  }

  async syncSvnChange(pair: Pair, script: string, username = "svc-sync"): Promise<Snap> {
    const before = await this.snap(pair);
    const rev = await this.commitSvn(pair, script, username);
    await this.triggerSvn(rev);
    // Team confirm writes sync_records and last_svn_rev_<repo>. It does not
    // insert commit_map. Run 38199181879 moved the Git tip while that map
    // count stayed put. The receipt and the watermark are the checkpoint.
    // Issue #196 remains the unstripped trunk-path warning.
    try {
      await this.waitUntil(async () => {
        const now = await this.snap(pair);
        return now.svn !== before.svn
          && /^[0-9a-f]{40}$/.test(now.git)
          && now.git !== before.git
          && now.records !== before.records
          && now.watermark === now.svn;
      });
    } catch (error) {
      const now = await this.snap(pair);
      throw new Error(`svn checkpoint not observed: svn ${before.svn}->${now.svn} git ${before.git}->${now.git} maps ${before.maps}->${now.maps} records ${before.records}->${now.records} watermark ${before.watermark}->${now.watermark}; ${error}`);
    }
    return this.snap(pair);
  }

  async syncGitChange(pair: Pair, script: string): Promise<Snap> {
    const before = await this.snap(pair);
    await this.commitGit(pair, script);
    const status = await this.triggerGit(pair.gitRepo, false);
    if (status !== 200) throw new Error(`git webhook ${status}`);
    try {
      await this.waitUntil(async () => {
        const now = await this.snap(pair);
        return now.svn !== before.svn && now.records !== before.records;
      });
    } catch (error) {
      const now = await this.snap(pair);
      throw new Error(`git checkpoint not observed: svn ${before.svn}->${now.svn} git ${before.git}->${now.git} maps ${before.maps}->${now.maps} records ${before.records}->${now.records} watermark ${before.watermark}->${now.watermark}; ${error}`);
    }
    return this.snap(pair);
  }

  async expectNoWrite(pair: Pair, action: () => Promise<void>): Promise<void> {
    const before = await this.snap(pair);
    await action();
    const start = Date.now();
    await expect.poll(async () => {
      const now = await this.snap(pair);
      const same = now.svn === before.svn
        && now.git === before.git
        && now.maps === before.maps
        && now.records === before.records
        && now.watermark === before.watermark
        && now.gitWatermark === before.gitWatermark
        && now.gitColumn === before.gitColumn;
      return same && Date.now() - start > 12_000;
    }, { timeout: 25_000, intervals: [1000] }).toBe(true);
  }

  ldapConfig() {
    return {
      enabled: true,
      url: "ldaps://ldap:636",
      base_dn: "dc=lab,dc=invalid",
      search_filter: "(&(objectClass=inetOrgPerson)(uid={0}))",
      display_name_attr: "cn",
      email_attr: "mail",
      group_attr: "memberOf",
      bind_dn: "uid=svc-bind,ou=services,dc=lab,dc=invalid",
      bind_password: this.bindPassword,
      tls_verify: true,
    };
  }

  async enableLdap(): Promise<void> {
    await this.admin();
    const saved = await this.api("PUT", "/api/admin/ldap", this.ldapConfig());
    if (saved.status !== 200) throw new Error(`save ldap failed (${saved.status})`);
  }

  async expectLdapBindRefusal(since: string): Promise<void> {
    const logs = await exec("docker", ["compose", "-p", this.project, "-f", this.composeFile,
      "logs", "--no-color", "--since", since, "reposync"], {timeout: 10_000, maxBuffer: 8_000_000});
    const observed = `${logs.stdout}\n${logs.stderr}`.split("\n").find(line =>
      line.includes("LDAP auth failed for 'alice'") && line.includes("service account bind rejected"));
    if (!observed) throw new Error("LDAP server service-bind refusal was not observed");
    this.info.annotations.push({type: "labObservation", description: "Daemon observed the LDAP server rejecting the configured service-account bind after the accepted configuration change"});
  }

  async uiLogin(username: string, password: string): Promise<{status: number; json: unknown}> {
    await this.page.goto("/login");
    await this.page.evaluate(() => localStorage.clear());
    await this.page.goto("/login");
    await this.page.locator('input[name="username"]').fill(username);
    await this.page.locator('input[type="password"]').fill(password);
    await this.reserveLogin();
    const reply = this.page.waitForResponse(response => response.url().endsWith("/api/auth/login") && response.request().method() === "POST");
    await this.page.getByRole("button", { name: "Sign in" }).click();
    const response = await reply;
    const json = await response.json();
    if (response.status() === 200) {
      const token = (json as {token?: string}).token;
      if (token) this.rememberSecret(token);
    }
    return {status: response.status(), json};
  }

  async restartDaemon(): Promise<void> {
    await exec("docker", ["compose", "-p", this.project, "-f", this.composeFile, "--profile", "app", "restart", "reposync"], { timeout: 120_000 });
    // The forwarder closes the socket while reposync is restarting.
    // expect.poll does not retry a thrown fetch, so treat that as not ready.
    await this.waitUntil(async () => {
      try {
        const health = await this.api("GET", "/api/status/health", undefined, "");
        return health.status === 200;
      } catch {
        return false;
      }
    }, 120_000);
  }

  async deniedLines(): Promise<number> {
    const text = await this.sh("denied", "wc -l < /logs/access.log || true", "root");
    return Number(text.trim() || "0");
  }
}

const pairs = new Map<string, Pair>();

export async function sharedPair(lab: Lab, key: string): Promise<Pair> {
  const existing = pairs.get(key);
  if (existing) return existing;
  const created = key === "bootstrap" ? await lab.bootstrap() : await lab.createPair(key);
  pairs.set(key, created);
  return created;
}
