import { execFile, spawnSync } from "node:child_process";
import { createHash, createHmac, randomBytes } from "node:crypto";
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { promisify } from "node:util";
import { expect, type Page, type TestInfo } from "@playwright/test";

const exec = promisify(execFile);

/** Canonical XML preserves complete history semantics, not serializer attribute order. */
export function archive199CanonicalXml(xml: string, root: "log" | "properties"): string {
  if (Buffer.byteLength(xml, "utf8") > 8_000_000 || /<!\s*(?:DOCTYPE|ENTITY)\b/i.test(xml)) throw new Error("unsafe or oversized archive XML");
  const script = `import sys,xml.etree.ElementTree as ET
xml=sys.stdin.read()
if ET.fromstring(xml).tag != sys.argv[1]: raise ValueError("unexpected archive XML root")
sys.stdout.write(ET.canonicalize(xml, with_comments=True, strip_text=False, rewrite_prefixes=False))
`;
  const result = spawnSync("python3", ["-c", script, root], {input: xml, encoding: "utf8", timeout: 5000, maxBuffer: 8_000_000});
  if (result.error || result.status !== 0 || !result.stdout) throw new Error("archive XML canonicalization failed");
  return result.stdout;
}

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
  importOperationId?: string;
};

export function importOperationId(value: unknown): string | undefined {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(value)) return undefined;
  return value;
}

export type Snap = { svn: string; git: string; maps: string; records: string; watermark: string; gitWatermark: string; gitColumn: string };
// RS199-HARNESS-ASTRA-20261117: additive, source-only until qualified execution.
export type Archive199Side = "git" | "svn";
export type Archive199Observation = {
  pairId: string;
  registration: Array<Record<string, any>>;
  maps: Array<Record<string, any>>;
  records: Array<Record<string, any>>;
  audit: Array<Record<string, any>>;
  archiveJournal: Array<Record<string, any>>;
  localCustody: {sha256: string; entries: number};
  git: {present: boolean; tip: string | null; tree: string | null; history: string | null; heads: Record<string, string>};
  svn: {present: boolean; revision: string | null; tree: string | null; properties: string | null; history: string | null; branches: string[]};
};
export type Archive199Gate = {
  state: string; run_id?: string; request_id?: string; method?: string; target?: string;
  scope_path?: string; phase?: string; matched_at?: number; witnessed_at?: number;
  upstream_response_complete?: boolean; response_status?: number; error?: string;
};

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

  async observedInspectionTip(pair: Pair): Promise<string> {
    // history_inspect fetches the remote tip into this ref before publication.
    // Publication checkpoints can stay put while this ref already names B.
    const id = safe(pair.id, "repo id");
    const output = await this.sh("reposync", `
set -eu
repo=/var/lib/reposync/repos/${id}/git-repo
if [ ! -d "$repo/.git" ] && [ ! -d "$repo" ]; then
  printf 'absent\\n'
  exit 0
fi
tip=$(git -C "$repo" rev-parse --verify --end-of-options 'refs/reposync/inspection/incoming^{commit}' 2>/dev/null || true)
if [ -z "$tip" ]; then printf 'absent\\n'; else printf '%s\\n' "$tip"; fi
`, "reposync");
    const tip = output.trim().split("\n").filter(line => line.length > 0).pop() || "absent";
    if (tip !== "absent" && !/^[0-9a-f]{40}$/.test(tip)) throw new Error(`invalid inspection tip ${tip}`);
    return tip;
  }

  async createPair(name: string, options?: {enable?: boolean; settle?: boolean}): Promise<Pair> {
    const enable = options?.enable !== false;
    const settle = options?.settle !== false;
    if (!settle && enable) throw new Error("unsettled import cannot be enabled");
    const repo = safe(name, "repo");
    // Playwright restarts its worker after a failed case. Find the persisted
    // pair instead of registering a duplicate when the in-memory cache is gone.
    const existing = (await this.repos()).filter(row => row.git_repo === `lab/${repo}` || row.name === repo);
    if (existing.length > 1) throw new Error(`ambiguous existing lab pair ${repo}`);
    if (existing.length === 1) {
      const row = existing[0];
      if (row.name !== repo || row.git_repo !== `lab/${repo}` || row.svn_url !== `https://svn:8443/svn/${repo}`) throw new Error(`existing lab pair ${repo} has different remotes`);
      if (!settle) {
        if (row.enabled !== false) throw new Error("scheduled pair cannot prove an in-flight import cancel");
        const status = await this.api("GET", `/api/repos/${row.id}/import/status`);
        const body = status.json as {operation_id?: unknown; lifecycle?: unknown} | null;
        const operation = importOperationId(body?.operation_id);
        const lifecycle = typeof body?.lifecycle === "string" ? body.lifecycle : "";
        if (status.status !== 200 || !operation || !/^(queued|running|cancel_requested|cancelling)$/.test(lifecycle)) {
          throw new Error(`existing lab pair ${repo} has no in-flight import operation to cancel`);
        }
        return {id: row.id, name: repo, svnUrl: row.svn_url, gitRepo: row.git_repo, importOperationId: operation};
      }
      await this.waitImport(row.id);
      const pair = {id: row.id, name: repo, svnUrl: row.svn_url, gitRepo: row.git_repo};
      if (row.enabled === true && !enable) throw new Error("scheduled pair cannot prove an unobserved interval");
      if (row.enabled === false && enable) await this.enableFixturePair(pair);
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
    const operation = importOperationId((started.json as {operation_id?: unknown} | null)?.operation_id);
    if (!operation) throw new Error(`import ${repo} returned no operation id`);
    const pair = {id, name: repo, svnUrl: `https://svn:8443/svn/${repo}`, gitRepo: `lab/${repo}`};
    if (!settle) {
      const startedPair = {...pair, importOperationId: operation};
      this.observation(`${repo}-import-started`, {pair: startedPair, operation, scheduler_enabled_after_import: false});
      return startedPair;
    }
    await this.waitImport(id, operation);
    const baseline = await this.snap(pair);
    if (baseline.svn !== "1" || baseline.watermark !== "1" || baseline.maps !== "1" || baseline.records !== "1"
      || !/^[0-9a-f]{40}$/.test(baseline.git) || baseline.gitWatermark !== baseline.git || baseline.gitColumn !== baseline.git) {
      this.observation(`${repo}-import-baseline`, {pair, operation, baseline, setup_failed: true});
      throw new Error(`fixture ${repo} import did not establish the exact r1 baseline`);
    }
    await this.assertSingleReceipt(pair, "svn_to_git", "1");
    if (enable) await this.enableFixturePair(pair);
    this.observation(`${repo}-import-ready`, {pair, operation, provider_status: provider.status, baseline, scheduler_enabled_after_import: enable});
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

  async appliedReceiptCount(pair: Pair, direction: "svn_to_git" | "git_to_svn", source: string): Promise<string> {
    if (direction === "svn_to_git" ? !/^[0-9]+$/.test(source) : !/^[0-9a-f]{40}$/.test(source)) throw new Error("invalid receipt source");
    const column = direction === "svn_to_git" ? "svn_rev" : "git_sha";
    const count = (await this.sh("reposync", `sqlite3 /var/lib/reposync/reposync.db "SELECT COUNT(*) FROM sync_records WHERE repo_id='${safe(pair.id, "repo id")}' AND direction='${direction}' AND status='applied' AND ${column}='${source}';"`, "root")).trim();
    if (!/^[0-9]+$/.test(count)) throw new Error(`${pair.name} ${direction} receipt count is invalid`);
    return count;
  }

  async assertSingleReceipt(pair: Pair, direction: "svn_to_git" | "git_to_svn", source: string): Promise<void> {
    // Call through the prototype so a harness fake that only supplies sh still works.
    const count = await Lab.prototype.appliedReceiptCount.call(this, pair, direction, source);
    if (count !== "1") throw new Error(`${pair.name} ${direction} source has ${count} receipts instead of one`);
  }

  async gitCommitsSince(pair: Pair, ancestor: string): Promise<string[]> {
    if (!/^[0-9a-f]{40}$/.test(ancestor)) throw new Error("invalid ancestor");
    const branch = safe(pair.gitBranch || "main", "ref");
    const dir = `/work/read-${safe(pair.id, "repo id")}`;
    const output = await this.observer(`
set -eu
auth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')
if [ ! -d ${dir}/.git ]; then
  git -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt \
    clone --branch ${branch} https://gitea:3000/${pair.gitRepo}.git ${dir}
fi
git -C ${dir} -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt fetch origin ${branch}
git -C ${dir} rev-list --reverse ${ancestor}..FETCH_HEAD
`);
    const commits = output.trim().split("\n").filter(line => line.length > 0);
    if (commits.some(sha => !/^[0-9a-f]{40}$/.test(sha))) throw new Error("invalid commit list");
    return commits;
  }

  async waitOutboundGitHistory(pair: Pair, before: Snap, commits: string[], path: string, bytes: string): Promise<Snap> {
    // Finish every intended Git-to-SVN receipt, the selected tip, its bytes,
    // and both outbound Git checkpoints before any repeat-idleness baseline.
    // An earlier SVN revision is still the original publication. watermark is
    // the inbound SVN-read cursor and is not one of those checkpoints.
    if (commits.length === 0 || new Set(commits).size !== commits.length || commits.some(sha => !/^[0-9a-f]{40}$/.test(sha))) {
      throw new Error("intended Git commits are missing");
    }
    const tip = commits[commits.length - 1];
    const published = async () => {
      const now = await this.snap(pair);
      if (now.git !== tip || now.gitWatermark !== tip || now.gitColumn !== tip) return false;
      if (now.svn === before.svn || now.records === before.records) return false;
      if (await this.svnBytesOrMissing(pair, path) !== bytes) return false;
      for (const sha of commits) {
        if (await Lab.prototype.appliedReceiptCount.call(this, pair, "git_to_svn", sha) !== "1") return false;
      }
      return true;
    };
    try {
      await this.waitUntil(published);
    } catch (error) {
      throw new Error(`outbound Git history not settled before the repeat baseline: tip=${tip} before=${JSON.stringify(before)} after=${JSON.stringify(await this.snap(pair))}; ${error}`);
    }
    return this.snap(pair);
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

  async svnInventory(pair: Pair): Promise<string[]> {
    const output = await this.observer(`set -eu\nexport LC_ALL=C\nsvn list -R --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" ${svnTarget(pair)}\n`);
    return output.split("\n").map(line => line.trim()).filter(line => line.length > 0 && !line.endsWith("/")).sort();
  }

  async gitInventory(pair: Pair): Promise<string[]> {
    const branch = safe(pair.gitBranch || "main", "ref");
    const dir = `/work/read-${safe(pair.id, "repo id")}`;
    const output = await this.observer(`
set -eu
auth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')
if [ ! -d ${dir}/.git ]; then
  git -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt \
    clone --branch ${branch} https://gitea:3000/${pair.gitRepo}.git ${dir}
fi
git -C ${dir} -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt fetch origin ${branch}
git -C ${dir} ls-tree -r --name-only FETCH_HEAD
`);
    return output.split("\n").map(line => line.trim()).filter(line => line.length > 0).sort();
  }

  async svnSha256(pair: Pair, path: string): Promise<string | null> {
    const rel = safeRelativePath(path);
    const output = (await this.observer(`
set -eu
export LC_ALL=C
data=$(mktemp); err=$(mktemp)
trap 'rm -f "$data" "$err"' EXIT
if svn cat --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" \
  ${svnTarget(pair)}/${rel} >"$data" 2>"$err"; then
  sha256sum "$data" | awk '{print $1}'
elif grep -q "W160013:.*path not found" "$err" && ! grep -E '(E|W)[0-9]{6}' "$err" | grep -Ev '(W160013|E200009)' >/dev/null; then
  printf 'LAB_SHA_ABSENT\\n'
else
  cat "$err" >&2; exit 1
fi
`)).trim();
    if (output === "LAB_SHA_ABSENT") return null;
    if (!/^[0-9a-f]{64}$/.test(output)) throw new Error(`svn sha256 for ${path} was not a hash`);
    return output;
  }

  async appliedGitToSvn(pair: Pair): Promise<Array<{git_sha: string; svn_rev: string}>> {
    const rows = await this.sqlRows(`SELECT git_sha, svn_rev FROM sync_records WHERE repo_id='${safe(pair.id, "repo id")}' AND direction='git_to_svn' AND status='applied' ORDER BY CAST(svn_rev AS INTEGER), git_sha;`);
    return rows.map(row => {
      const gitSha = String(row.git_sha || "");
      const svnRev = String(row.svn_rev || "");
      if (!/^[0-9a-f]{40}$/.test(gitSha) || !/^[0-9]+$/.test(svnRev)) throw new Error("invalid git-to-svn receipt");
      return {git_sha: gitSha, svn_rev: svnRev};
    });
  }

  async svnBytesForRevisions(pair: Pair, path: string, revisions: string[]): Promise<Array<string | null>> {
    if (revisions.some(revision => !/^[0-9]+$/.test(revision))) throw new Error("invalid SVN read revision");
    const rel = safeRelativePath(path);
    const output = await this.observer(`
set -eu
export LC_ALL=C
for rev in ${revisions.join(" ")}; do
  data=$(mktemp); err=$(mktemp)
  if svn cat -r "$rev" --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" \
    ${svnTarget(pair)}/${rel}@"$rev" >"$data" 2>"$err"; then
    printf 'LAB_REV %s %s\\n' "$rev" "$(wc -c < "$data" | tr -d ' ')"
    cat "$data"
  elif grep -q "W160013:.*path not found" "$err" && ! grep -E '(E|W)[0-9]{6}' "$err" | grep -Ev '(W160013|E200009)' >/dev/null; then
    printf 'LAB_REV %s ABSENT\\n' "$rev"
  else
    cat "$err" >&2; exit 1
  fi
  rm -f "$data" "$err"
done
`);
    const values: Array<string | null> = [];
    let rest = output;
    for (const revision of revisions) {
      const newline = rest.indexOf("\n");
      if (newline < 0) throw new Error("truncated revision read");
      const header = rest.slice(0, newline);
      rest = rest.slice(newline + 1);
      const absent = header.match(/^LAB_REV ([0-9]+) ABSENT$/);
      if (absent) {
        if (absent[1] !== revision) throw new Error("revision read order changed");
        values.push(null);
        continue;
      }
      const present = header.match(/^LAB_REV ([0-9]+) ([0-9]+)$/);
      if (!present || present[1] !== revision) throw new Error("revision read order changed");
      const size = Number(present[2]);
      values.push(rest.slice(0, size));
      rest = rest.slice(size);
    }
    if (rest.length) throw new Error("revision read had trailing bytes");
    return values;
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

  async archive199ConfigureScheduling(pair: Pair, enabled: boolean, interval: 30 | 3600): Promise<void> {
    const saved = await this.api("PUT", `/api/repos/${pair.id}`, {enabled, poll_interval_secs: interval});
    const readback = await this.api("GET", `/api/repos/${pair.id}`);
    const row = readback.json as any;
    if (saved.status !== 200 || readback.status !== 200 || row?.id !== pair.id || row.enabled !== enabled || row.poll_interval_secs !== interval
      || row.svn_url !== pair.svnUrl || row.git_repo !== pair.gitRepo) throw new Error("archive199 fixture scheduling readback mismatch");
  }

  static archive199SchedulerInterval(config: string, profile: string | undefined): number {
    if (profile !== "1" || config.length > 65536) throw new Error("archive199 explicit scheduler profile is required");
    const sections = config.split(/^\s*\[daemon\]\s*$/m);
    if (sections.length !== 2) throw new Error("archive199 generated daemon section is ambiguous");
    const daemon = sections[1].split(/^\s*\[/m)[0];
    const values = daemon.split("\n").map(line => line.replace(/#.*$/, "").trim()).filter(line => /^poll_interval_secs\s*=/.test(line));
    if (values.length !== 1 || !/^poll_interval_secs\s*=\s*3600\s*$/.test(values[0])) throw new Error("archive199 global daemon interval must be exactly 3600");
    return 3600;
  }

  static archive199PublicationBudget(startedAt: string, workers: string | undefined, now = Date.now()): number {
    const started = Date.parse(startedAt), age = now - started;
    if (workers !== "1" || !Number.isFinite(started) || age < 0 || age >= 1_800_000) throw new Error("archive199 requires one worker and a fresh daemon publication budget");
    return 3_600_000 - age;
  }

  async archive199AdmitPublicationScheduler(): Promise<void> {
    const root = required("LAB_RUN_DIR");
    const config = readFileSync(`${root}/daemon.toml`, "utf8");
    const interval = Lab.archive199SchedulerInterval(config, process.env.ACCEPTANCE_LAB_ARCHIVE199_MANUAL_POLL);
    const manifest = JSON.parse(readFileSync(`${root}/run.json`, "utf8"));
    if (manifest.project !== this.project || manifest.run_id !== required("RUN_ID")) throw new Error("archive199 scheduler profile belongs to another run");
    const processIdentity = await this.archive199ProcessIdentity();
    const remainingMs = Lab.archive199PublicationBudget(processIdentity.startedAt, process.env.LAB_WORKERS);
    const configSha256 = createHash("sha256").update(config).digest("hex");
    const mountedSha256 = (await this.sh("reposync", "sha256sum /etc/reposync/config.toml | awk '{print $1}'", "root")).trim();
    if (mountedSha256 !== configSha256) throw new Error("archive199 mounted daemon config differs from generated profile");
    this.observation("archive199-publication-scheduler", {profile: "explicit-webhook-3600", interval, project: this.project,
      runId: manifest.run_id, configSha256, mountedSha256, processIdentity, remainingMs, workers: 1});
  }

  async archive199CreateParent(name: string): Promise<Pair> {
    const parent = await this.createPair(name, {enable: false});
    await this.archive199AdmitPublicationScheduler();
    await this.archive199ConfigureScheduling(parent, true, 3600);
    return parent;
  }

  async archive199WaitQuiet(pair: Pair, sourceSha?: string): Promise<void> {
    if (sourceSha !== undefined && !/^[0-9a-f]{40}$/.test(sourceSha)) throw new Error("invalid archive199 source witness");
    const id = safe(pair.id, "repo id");
    let last: Record<string, unknown> = {stage: "status-request"};
    try {
      await this.waitUntil(async () => {
        last = {stage: "status-request"};
        const response = await this.api("GET", `/api/status?repo_id=${encodeURIComponent(id)}`);
        const status = response.json as any;
        const observed = status?.observation;
        // Keep only the most recent DTO, bounded; never capture repository secrets.
        last = {stage: "status-admission", httpStatus: response.status,
          status: JSON.stringify(status)?.length <= 65536 ? status : {omitted: "status DTO exceeds diagnostic bound"}};
        if (response.status !== 200 || observed?.repository_id !== id || observed.evidence_complete !== true || observed.registry_busy !== false
          || !Array.isArray(observed.operations) || observed.operations.some((op: any) => op.held || !["completed", "cancelled"].includes(op.state))) return false;
        last.stage = "writer-windows-and-journals";
        const rows = await this.sqlRows(`SELECT key,value FROM kv_state WHERE key LIKE 'pair_refresh_writer_window_%' OR key LIKE 'git_to_svn_commit_v1:document:%' OR key LIKE 'svn_to_git_push_v1:document:%';`);
        const producers: Array<Record<string, any>> = [];
        for (const row of rows) {
          const doc = JSON.parse(row.value);
          if (typeof row.key !== "string" || !doc || typeof doc.id !== "string" || !doc.id) throw new Error("malformed archive199 writer evidence");
          if (row.key.startsWith("pair_refresh_writer_window_")) {
            if (!row.key.startsWith("pair_refresh_writer_window_v1:document:") || !["preparing", "unjournalled_publication_possible"].includes(doc.phase)
              || !(doc.repo_id === null || typeof doc.repo_id === "string" && doc.repo_id.length > 0)) throw new Error("malformed archive199 writer window");
            if (doc.repo_id === null || doc.repo_id === id) return false;
          } else if (row.key.startsWith("git_to_svn_commit_v1:document:") || row.key.startsWith("svn_to_git_push_v1:document:")) {
            if (typeof doc.repo_id !== "string" || !doc.repo_id || typeof doc.state !== "string") throw new Error("malformed archive199 producer journal");
            if (doc.repo_id === id) producers.push(doc);
          } else throw new Error("unknown archive199 writer evidence");
        }
        last.stage = "producer-completion";
        if (producers.some(doc => doc.state !== "completed")) return false;
        last.stage = "source-producer-witness";
        if (sourceSha && !producers.some(doc => doc.id && doc.source_git_sha === sourceSha && doc.state === "completed")) return false;
        last.stage = "disabled-config";
        const config = await this.api("GET", `/api/repos/${id}`);
        last.config = {httpStatus: config.status, id: (config.json as any)?.id, enabled: (config.json as any)?.enabled};
        if (config.status !== 200 || (config.json as any)?.enabled !== false) return false;
        if (sourceSha) {
          last.stage = "source-checkpoint";
          const snap = await this.snap(pair);
          if (snap.git !== sourceSha || snap.gitWatermark !== sourceSha) return false;
          last.stage = "single-receipt";
          await this.assertSingleReceipt(pair, "git_to_svn", sourceSha);
        }
        this.observation("archive199-quiet-writer", {pairId: id, sourceSha, status, producers, enabled: false});
        return true;
      }, 90_000);
    } catch (error) {
      try { this.observation("archive199-quiet-failure", {pairId: id, sourceSha, ...last}); }
      catch { /* Diagnostic failure must not replace the original failure. */ }
      throw error;
    }
  }

  // #199 browser publication must use P's real preview/publish UI, never the
  // predecessor createChildPair's single POST contract.
  async archive199CreateChildUi(parent: Pair, branchName: string): Promise<Pair> {
    const branch = safeRelativePath(branchName);
    if (branch.includes("/")) throw new Error("archive199 fixtures use one-level branch names");
    if ((await this.repos()).some(row => row.parent_id === parent.id && row.git_branch === branch)) {
      this.gap("BLOCKED: archive199 child already exists; no fresh browser creation witness");
    }
    await this.archive199AdmitPublicationScheduler();
    const scheduling = await this.api("GET", `/api/repos/${parent.id}`);
    if (scheduling.status !== 200 || (scheduling.json as any)?.enabled !== true || (scheduling.json as any)?.poll_interval_secs !== 3600) throw new Error("archive199 child creation requires its admitted enabled publication parent");
    const parentBefore = await this.snap(parent);
    await this.commitGit(parent, `git branch ${branch}\ngit push origin refs/heads/${branch}:refs/heads/${branch}\n`);
    const heads = await this.gitRemoteHeads(parent);
    if (heads[`refs/heads/${branch}`] !== parentBefore.git) throw new Error("child Git seed is not the observed SVN-derived parent tip");
    const login = await this.uiLogin(this.adminUser, this.adminPassword);
    if (login.status !== 200) throw new Error("archive199 administrator browser login failed");
    await expect(this.page).toHaveURL(/\/$/);
    await this.page.goto(`/repos/${parent.id}?section=branches`);
    await this.page.getByRole("button", {name: "Add Branch Pair", exact: true}).click();
    const dialog = this.page.getByRole("dialog", {name: "Review Branch Pair", exact: true});
    await dialog.getByLabel(/Branch Name/).fill(branch);
    await dialog.getByLabel(/SVN Branch Path/).fill(`branches/${branch}`);
    const endpoint = `/api/repos/${parent.id}/branches`;
    const reply = (action: string) => this.page.waitForResponse(response => {
      if (new URL(response.url()).pathname !== endpoint || response.request().method() !== "POST") return false;
      return response.request().postDataJSON()?.action === action;
    });
    const previewResponse = reply("preview");
    await dialog.getByRole("button", {name: "Preview Pair", exact: true}).click();
    const preview = await previewResponse; const plan = await preview.json();
    if (preview.status() !== 200 || !plan.admitted || !plan.review_pin || plan.conflicts?.length || plan.git_branch !== branch || plan.svn_branch !== `branches/${branch}`) {
      this.observation("archive199-child-preview-refused", {status: preview.status(), plan});
      this.gap("BLOCKED: real browser child-pair preview did not admit this exact SVN-origin branch");
    }
    const publishResponse = reply("publish");
    await dialog.getByRole("button", {name: "Publish reviewed result", exact: true}).click();
    const published = await publishResponse; let result = await published.json();
    if (published.status() !== 200 || result.review_pin !== plan.review_pin || !result.operation_id) {
      throw new Error("browser publish did not retain the reviewed operation identity");
    }
    // Only inspect the admitted operation. No implicit resume or replacement.
    await this.waitUntil(async () => {
      if (result.published === true && result.scheduler_active === true) return true;
      if (["failed", "reconciliation_required", "cancelled"].includes(result.pair_state)) this.gap("BLOCKED: child publication is held or failed");
      const response = reply("status");
      await dialog.getByRole("button", {name: "Inspect operation", exact: true}).click();
      const observed = await response;
      if (observed.status() !== 200) throw new Error("child operation inspection failed");
      const next = await observed.json();
      if (next.operation_id !== result.operation_id) throw new Error("child operation identity changed");
      result = next; return result.published === true && result.scheduler_active === true;
    }, 120_000);
    const rows = (await this.repos()).filter(row => row.parent_id === parent.id && row.git_branch === branch);
    if (rows.length !== 1 || rows[0].svn_url !== parent.svnUrl || rows[0].svn_branch !== `branches/${branch}` || rows[0].git_repo !== parent.gitRepo) throw new Error("published child identity/remotes differ from reviewed request");
    const pair: Pair = {id: rows[0].id, name: parent.name, svnUrl: parent.svnUrl, gitRepo: parent.gitRepo, parentId: parent.id, gitBranch: branch, svnBranch: `branches/${branch}`};
    const childConfig = await this.api("GET", `/api/repos/${pair.id}`);
    if (childConfig.status !== 200 || (childConfig.json as any)?.poll_interval_secs !== 3600 || (childConfig.json as any)?.enabled !== true) throw new Error("archive199 child did not inherit admitted long scheduling interval");
    // Publication admits the child enabled. Disable only this owned fixture;
    // an already-started writer must genuinely exit before further setup.
    await this.archive199ConfigureScheduling(pair, false, 3600);
    await this.archive199WaitQuiet(pair);
    this.observation("archive199-created-child", {parentBefore, seedTip: heads[`refs/heads/${branch}`], plan, published: result, pair});
    return pair;
  }

  async archive199DeleteUi(pair: Pair, side: Archive199Side): Promise<{status: number; json: any}> {
    if (!pair.parentId || !pair.gitBranch || !pair.svnBranch) throw new Error("archive199 requires an observed child pair");
    const login = await this.uiLogin(this.adminUser, this.adminPassword);
    if (login.status !== 200) throw new Error("archive199 administrator browser login failed");
    await expect(this.page).toHaveURL(/\/$/);
    await this.page.goto(`/repos/${pair.id}?section=settings`);
    await this.page.getByTestId("delete-viewed-branch-pair").click();
    await this.page.getByTestId(`delete-${side}-opt`).check();
    await this.page.getByTestId("delete-branch-confirm-input").fill(side === "git" ? pair.gitBranch : pair.svnBranch);
    const reply = this.page.waitForResponse(response => new URL(response.url()).pathname === `/api/repos/${pair.id}/removal/approve` && response.request().method() === "POST");
    await this.page.getByTestId("confirm-delete-branch-pair").click();
    const response = await reply; const request = response.request().postDataJSON(); const result = await response.json();
    if (request.archive_pair !== true || request.delete_git !== (side === "git") || request.delete_svn !== (side === "svn") || !request.review_pin) throw new Error("browser sent a different archive disposition");
    this.observation("archive199-browser-approval", {pairId: pair.id, side, request, status: response.status(), result});
    if (response.status() !== 200 || !result.ok || result.state !== "completed" || result.archive_pair !== true || result.registration_listed !== true || result.restore_supported !== false) throw new Error("archive completion was not confirmed by the real API");
    await expect(this.page.getByTestId("archived-pair-notice")).toBeVisible();
    if (new URL(this.page.url()).pathname !== `/repos/${pair.id}`) throw new Error("archive removed the retained detail route");
    await expect(this.page.getByTestId("header-sync-switch")).toBeDisabled();
    await expect(this.page.getByTestId("delete-viewed-branch-pair")).toHaveCount(0);
    return {status: response.status(), json: result};
  }

  async archive199Observe(pair: Pair, allowedAbsentSide?: Archive199Side, baseline?: Archive199Observation): Promise<Archive199Observation> {
    const id = safe(pair.id, "repo id"), branch = safeRelativePath(pair.gitBranch || "");
    if (!pair.parentId || !/^branches\/[A-Za-z0-9_.-]+$/.test(pair.svnBranch || "") || !/^lab\/[A-Za-z0-9_.-]+$/.test(pair.gitRepo) || !/^https:\/\/(svn|fault-proxy):8443\/svn\/[A-Za-z0-9_.-]+$/.test(pair.svnUrl)) throw new Error("archive199 observation refuses non-fixture or non-child targets");
    // Observation bypasses only our fault injection, never certificate/auth checks.
    const direct = {...pair, svnUrl: pair.svnUrl.replace("https://fault-proxy:8443/", "https://svn:8443/")};
    const heads = await this.gitRemoteHeads(direct), branches = await this.svnBranchNames(direct);
    const gitPresent = Object.prototype.hasOwnProperty.call(heads, `refs/heads/${branch}`);
    const svnPresent = branches.includes(pair.svnBranch!.slice("branches/".length));
    if ((!gitPresent && allowedAbsentSide !== "git") || (!svnPresent && allowedAbsentSide !== "svn")) throw new Error("unexpected survivor absence; healthy parent inventories were read successfully");
    let gitTree: string | null = null, gitHistory: string | null = null;
    if (gitPresent) {
      const dir = `/work/archive199-read-${id}-${randomBytes(4).toString("hex")}`;
      const output = await this.observer(`set -eu\nmkdir -p ${dir}\ngit -C ${dir} init -q\nauth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\n')\ngit -C ${dir} -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt fetch --no-tags https://gitea:3000/${pair.gitRepo}.git refs/heads/${branch}\ntest "$(git -C ${dir} rev-parse FETCH_HEAD)" = '${heads[`refs/heads/${branch}`]}'\ngit -C ${dir} ls-tree -r -t --full-tree FETCH_HEAD\nprintf '\nARCHIVE199_HISTORY\n'\ngit -C ${dir} rev-list --parents --topo-order FETCH_HEAD\n`);
      const parts = output.split("\nARCHIVE199_HISTORY\n");
      if (parts.length !== 2) throw new Error("invalid complete Git tree/history observation");
      [gitTree, gitHistory] = parts;
    }
    let revision: string | null = null, svnTree: string | null = null, properties: string | null = null, history: string | null = null;
    if (svnPresent) {
      const latest = await this.observer(`set -eu\nsvn log --xml -l 1 --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" ${svnTarget(direct)}\n`);
      revision = latest.match(/<logentry\s+revision="([0-9]+)"/)?.[1] ?? null;
      if (!revision) throw new Error("SVN branch history revision was not observed");
    }
    else if (baseline) {
      if (baseline.pairId !== pair.id || !baseline.svn.present) throw new Error("foreign or incomplete SVN baseline");
      revision = baseline.svn.revision;
    }
    if (revision !== null) {
      if (!/^[0-9]+$/.test(revision)) throw new Error("invalid observed SVN revision");
      const target = `${svnTarget(direct)}@${revision}`;
      const common = `--non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD"`;
      const listing = await this.observer(`set -eu\nsvn list -R ${common} -r ${revision} ${target}\n`);
      const entries = listing.split("\n").filter(Boolean).sort();
      const hashes: Array<{path: string; sha256: string | null}> = [];
      for (const path of entries) {
        if (path.endsWith("/")) { safeRelativePath(path.slice(0, -1)); hashes.push({path, sha256: null}); continue; }
        safeRelativePath(path);
        const hash = (await this.observer(`set -eu\ntmp=$(mktemp)\ntrap 'rm -f "$tmp"' EXIT\nsvn cat ${common} -r ${revision} ${svnTarget(direct)}/${path}@${revision} > "$tmp"\nsha256sum "$tmp" | awk '{print $1}'\n`)).trim();
        if (!/^[0-9a-f]{64}$/.test(hash)) throw new Error("invalid SVN file digest"); hashes.push({path, sha256: hash});
      }
      svnTree = JSON.stringify(hashes);
      const rawProperties = await this.observer(`set -eu\nsvn proplist -R --verbose --xml ${common} -r ${revision} ${target}\n`);
      const rawHistory = await this.observer(`set -eu\nsvn log --xml --verbose ${common} -r ${revision}:1 ${target}\n`);
      this.observation("archive199-svn-raw-xml", {pairId: pair.id, revision, properties: rawProperties, history: rawHistory});
      properties = archive199CanonicalXml(rawProperties, "properties");
      history = archive199CanonicalXml(rawHistory, "log");
    }
    const custody = (await this.sh("reposync", `set -eu\nroot=/var/lib/reposync/repos/${id}\ntest -d "$root" && test ! -L "$root"\ncd "$root"\ntmp=$(mktemp)\ntrap 'rm -f "$tmp"' EXIT\ntar --sort=name --format=gnu -cf "$tmp" .\nsha256sum "$tmp" | awk '{print $1}'\nfind . -printf '%p\\0' | tr -cd '\\000' | wc -c\n`, "root")).trim().split(/\s+/);
    if (custody.length !== 2 || !/^[0-9a-f]{64}$/.test(custody[0]) || !/^[0-9]+$/.test(custody[1])) throw new Error("complete local custody observation unavailable");
    return {pairId: id,
      registration: await this.sqlRows(`SELECT * FROM repositories WHERE id='${id}' ORDER BY id;`),
      maps: await this.sqlRows(`SELECT * FROM commit_map WHERE repo_id='${id}' ORDER BY rowid;`),
      records: await this.sqlRows(`SELECT * FROM sync_records WHERE repo_id='${id}' ORDER BY rowid;`),
      audit: await this.sqlRows(`SELECT * FROM audit_log WHERE repo_id='${id}' ORDER BY rowid;`),
      archiveJournal: await this.sqlRows(`SELECT key,json_extract(value,'$.id') AS operation_id,json_extract(value,'$.operation_id') AS tombstone_operation_id,json_extract(value,'$.state') AS state,json_extract(value,'$.retention') AS retention,json_extract(value,'$.restore_supported') AS restore_supported FROM kv_state WHERE json_valid(value) AND (key LIKE 'managed_remove_v1:%:${id}' OR key IN (SELECT 'managed_remove_v1:document:'||value FROM kv_state WHERE key='managed_remove_v1:active:${id}')) ORDER BY key;`),
      localCustody: {sha256: custody[0], entries: Number(custody[1])},
      git: {present: gitPresent, tip: heads[`refs/heads/${branch}`] ?? null, tree: gitTree, history: gitHistory, heads},
      svn: {present: svnPresent, revision, tree: svnTree, properties, history, branches}};
  }

  private async archive199ProcessIdentity(): Promise<{container: string; startedAt: string; pid: number}> {
    const result = await exec("docker", ["compose", "-p", this.project, "-f", this.composeFile, "ps", "-q", "reposync"], {timeout: 10_000});
    const container = result.stdout.trim(); if (!/^[0-9a-f]{12,64}$/.test(container)) throw new Error("ambiguous owned daemon container");
    const inspected = await exec("docker", ["inspect", "--format", "{{json .}}", container], {timeout: 10_000});
    const item = JSON.parse(inspected.stdout);
    if (item.Config?.Labels?.["com.docker.compose.project"] !== this.project || item.Config?.Labels?.["com.docker.compose.service"] !== "reposync" || !item.State?.Running || !Number.isSafeInteger(item.State.Pid) || item.State.Pid <= 0 || !item.State.StartedAt) throw new Error("owned daemon process identity is unavailable");
    return {container, startedAt: item.State.StartedAt, pid: item.State.Pid};
  }

  async archive199RestartAndWitnessCycle(controlPair: Pair) {
    const beforeProcess = await this.archive199ProcessIdentity();
    const before = await this.snap(controlPair);
    await this.restartDaemon();
    const afterProcess = await this.archive199ProcessIdentity();
    if (afterProcess.container !== beforeProcess.container || afterProcess.startedAt === beforeProcess.startedAt) throw new Error("no real same-owned-container restart witnessed");
    const file = `archive199-cycle-${randomBytes(5).toString("hex")}.txt`;
    const bytes = `archive199 witnessed restart ${randomBytes(8).toString("hex")}`;
    await this.syncGitChange(controlPair, `printf '%s' '${bytes}' > ${file}\ngit add ${file}\ngit commit -m 'archive199 restart cycle'\n`);
    await this.waitUntil(async () => { const now = await this.snap(controlPair); return now.gitWatermark === now.git && now.git !== before.git && Number(now.svn) > Number(before.svn); });
    const after = await this.snap(controlPair);
    await this.assertSingleReceipt(controlPair, "git_to_svn", after.git);
    if (await this.svnBytes(controlPair, file) !== bytes) throw new Error("post-restart cycle did not publish exact control bytes");
    const evidence = {beforeStartedAt: beforeProcess.startedAt, afterStartedAt: afterProcess.startedAt,
      beforeProcess, afterProcess, before, after, controlPairId: controlPair.id, file, bytes};
    this.observation("archive199-restart-cycle", evidence);
    return evidence;
  }

  async archive199NonAdminDenial(pair: Pair) {
    await this.admin(); await this.enableLdap();
    const review = await this.api("POST", `/api/repos/${pair.id}/removal/review`, {archive_pair: true, delete_git: true, delete_svn: false});
    const plan = review.json as {review_pin?: string} | null;
    if (review.status !== 200 || !plan?.review_pin) this.gap("BLOCKED: administrator control cannot obtain current archive review");
    const before = await this.archive199Observe(pair);
    const login = await this.uiLogin("alice", this.alicePassword);
    assertLdapUser(login, "alice");
    const identity = (login.json as {token: string; user: {id: string; username: string; role: string}});
    if (!identity.user.id || identity.user.role === "admin" || !["user", "viewer"].includes(identity.user.role)) throw new Error("LDAP identity is not a positively verified non-administrator");
    // The login response precedes client-session persistence and React navigation.
    await expect(this.page).toHaveURL(/\/$/);
    await this.page.goto(`/repos/${pair.id}?section=settings`);
    await expect(this.page.getByTestId("repo-detail-heading")).toBeVisible();
    await expect(this.page.getByTestId("delete-viewed-branch-pair")).toHaveCount(0);
    const refused = [];
    for (const action of ["review", "approve"] as const) {
      const response = await this.api("POST", `/api/repos/${pair.id}/removal/${action}`, {archive_pair: true, delete_git: true, delete_svn: false, ...(action === "approve" ? {review_pin: plan!.review_pin, request_id: `archive199-denied-${randomBytes(6).toString("hex")}`} : {})}, identity.token);
      if (response.status !== 403 || (response.json as {operation_id?: string})?.operation_id) throw new Error("non-admin archive did not receive actual server authorization denial");
      refused.push({action, status: response.status, json: response.json});
    }
    const after = await this.archive199Observe(pair);
    expect(after).toEqual(before);
    const evidence = {user: identity.user, uiControlsAbsent: true, review: refused[0], approve: refused[1], before, after};
    this.observation("archive199-nonadmin-denial", evidence);
    return evidence;
  }

  /** Exact byte-preserving insertion; never change another host's options. */
  archive199ProxyConfig(contents: string): string {
    if (contents.includes("\0")) throw new Error("invalid SVN config bytes");
    const suffix = "\n# archive199 owned proxy framing\n[archive199-proxy]\nhttp-chunked-requests = no\n";
    const mapping = "archive199-proxy = fault-proxy\n";
    const lines = contents.match(/[^\n]*\n|[^\n]+$/g) || [];
    let section = "", offset = 0, groupEnd = -1, groups = 0, proxies = 0;
    const groupLines: string[] = [], proxyLines: string[] = [];
    for (const line of lines) {
      const trimmed = line.trim(), header = trimmed.match(/^\[([^\]]+)\]$/);
      if (trimmed.startsWith("[") && !header) throw new Error("ambiguous SVN config section");
      if (header) {
        section = header[1].trim().toLowerCase();
        if (section === "groups") { groups++; groupEnd = offset + line.length; }
        if (section === "archive199-proxy") proxies++;
      } else if (trimmed && !/^[#;]/.test(trimmed)) {
        if (section === "groups") groupLines.push(trimmed);
        if (section === "archive199-proxy") proxyLines.push(trimmed);
      }
      offset += line.length;
    }
    if (groups > 1 || proxies > 1) throw new Error("duplicate proxy configuration sections");
    if (proxies || contents.includes("# archive199 owned proxy framing")) {
      if (groups !== 1 || proxies !== 1 || !contents.endsWith(suffix) ||
          groupLines.length !== 1 || groupLines[0] !== mapping.trim() ||
          proxyLines.length !== 1 || proxyLines[0] !== "http-chunked-requests = no" ||
          contents.split("# archive199 owned proxy framing").length !== 2) throw new Error("unowned or changed proxy configuration");
      return contents;
    }
    if (groupLines.length) throw new Error("existing nonempty SVN host groups");
    if (groups) {
      const separator = contents.slice(0, groupEnd).endsWith("\n") ? "" : "\n";
      return contents.slice(0, groupEnd) + separator + mapping + contents.slice(groupEnd) + suffix;
    }
    return contents + "\n[groups]\n" + mapping + suffix;
  }

  /** Per-host request framing in the disposable daemon's default SVN config. */
  async archive199ConfigureProxyFraming(): Promise<void> {
    if (!/^[A-Za-z0-9_-]{8,80}$/.test(this.project)) throw new Error("invalid owned proxy configuration identity");
    const process = await this.archive199ProcessIdentity();
    const inspected = await exec("docker", ["inspect", "--format", "{{json .Config.Labels}}", process.container], {timeout: 10_000});
    const labels = JSON.parse(inspected.stdout);
    if (labels["acceptance-lab.disposable"] !== "true" || labels["acceptance-lab.run-id"] !== required("RUN_ID")) throw new Error("proxy framing requires the exact disposable run");
    // Ordinary SvnClient uses $HOME/.subversion, not SVN_CONFIG_DIR.
    const admission = String.raw`set -eu
runuser -u reposync -- sh -s <<'ARCHIVE199_PROCESS'
set -eu
count=0
for proc in /proc/[0-9]*; do
  exe=$(readlink "$proc/exe" 2>/dev/null || true)
  if [ "$exe" = /usr/local/bin/reposync-daemon ]; then
    count=$((count + 1))
    test "$(tr '\000' '\n' < "$proc/environ" | grep '^HOME=')" = HOME=/var/lib/reposync
    test "$(awk '/^Uid:/ {print $2}' "$proc/status")" = "$(id -u reposync)"
  fi
done
test "$count" = 1
ARCHIVE199_PROCESS
for path in /var /var/lib /var/lib/reposync; do test -d "$path"; test ! -L "$path"; done
test "$(stat -c %u /var/lib/reposync)" = "$(id -u reposync)"
runuser -u reposync -- sh -s <<'ARCHIVE199_CONFIG'
set -eu
cd /var/lib/reposync
test ! -L .subversion
if [ ! -e .subversion ]; then mkdir .subversion; fi
test -d .subversion
test "$(stat -c %u .subversion)" = "$(id -u)"
file=.subversion/servers
test ! -L "$file"
if [ -e "$file" ]; then
  test -f "$file"
  test "$(stat -c %u "$file")" = "$(id -u)"
  test "$(stat -c %h "$file")" = 1
  test "$(stat -c %s "$file")" -le 1048576
else
  (set -C; : > "$file")
fi
`;
    const encoded = (await this.sh("reposync", admission + 'base64 "$file"\nARCHIVE199_CONFIG\n', "root")).replace(/\s/g, "");
    if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)) throw new Error("invalid config readback");
    const bytes = Buffer.from(encoded, "base64"), contents = bytes.toString("utf8");
    if (!Buffer.from(contents).equals(bytes)) throw new Error("invalid config UTF8");
    const next = this.archive199ProxyConfig(contents);
    const before = createHash("sha256").update(bytes).digest("hex");
    const after = createHash("sha256").update(next).digest("hex");
    const replacement = Buffer.from(next).toString("base64");
    const receipt = await this.sh("reposync", admission + `test "$(sha256sum "$file" | cut -d ' ' -f 1)" = '${before}'
printf '%s' '${replacement}' | base64 -d > "$file"
test "$(sha256sum "$file" | cut -d ' ' -f 1)" = '${after}'
printf '%s\\n' '${after}'
ARCHIVE199_CONFIG
`, "root");
    if (receipt.trim() !== after) throw new Error("missing proxy framing option readback");
    this.observation("archive199-proxy-framing", {path:"/var/lib/reposync/.subversion/servers", host:"fault-proxy", httpChunkedRequests:"no", before, after});
  }


  /** Only the run-owned proxy control namespace; never changes TLS/auth policy. */
  async archive199GateControl(body?: Record<string, unknown>): Promise<Archive199Gate> {
    const root = new URL(required("LAB_FAULT_CONTROL_URL"));
    if (root.protocol !== "http:" || root.hostname !== "127.0.0.1" || !root.port || root.pathname !== "/" || root.username || root.password || root.search || root.hash) throw new Error("archive199 gate requires the owned published loopback port");
    const response = await fetch(new URL("archive199/gate", root), {method: body ? "POST" : "GET",
      headers: {"content-type": "application/json"}, body: body ? JSON.stringify(body) : undefined,
      redirect: "error", signal: AbortSignal.timeout(5000)});
    const result = await response.json() as {ok?: boolean; gate?: Archive199Gate};
    if (response.status !== 200 || result.ok !== true || !result.gate?.state) throw new Error(`archive199 gate control refused (${response.status})`);
    return result.gate;
  }

  /** Post-oracle drain only: preserve durable holds, never resume or repair them. */
  async archive199DrainGuard(original: Pair): Promise<void> {
    if (!original.parentId || !/^archive199-(busy|held)/.test(original.gitBranch || "")
      || !/^https:\/\/svn:8443\/svn\/[A-Za-z0-9_.-]+$/.test(original.svnUrl)) throw new Error("guard drain requires its dedicated synthetic child");
    const id = safe(original.id, "repo id");
    const pair = {...original, svnUrl: original.svnUrl.replace("https://svn:8443/", "https://fault-proxy:8443/")};
    const initial = await this.api("GET", `/api/repos/${id}`);
    const row = initial.json as any;
    if (initial.status !== 200 || row?.id !== id || row.parent_id !== pair.parentId || row.git_branch !== pair.gitBranch
      || row.svn_branch !== pair.svnBranch || row.svn_url !== pair.svnUrl || row.git_repo !== pair.gitRepo) throw new Error("guard drain refuses changed fixture identity");
    await this.archive199ConfigureScheduling(pair, false, 3600);
    await this.waitUntil(async () => {
      const response = await this.api("GET", `/api/status?repo_id=${encodeURIComponent(id)}`);
      const observed = (response.json as any)?.observation;
      if (response.status !== 200 || observed?.repository_id !== id || observed.evidence_complete !== true || observed.registry_busy !== false
        || !Array.isArray(observed.operations) || observed.operations.some((op: any) => op.held !== true && !["completed", "cancelled"].includes(op.state))) return false;
      const windows = await this.sqlRows("SELECT key,value FROM kv_state WHERE key LIKE 'pair_refresh_writer_window_%';");
      for (const window of windows) {
        const doc = JSON.parse(window.value);
        if (typeof window.key !== "string" || !window.key.startsWith("pair_refresh_writer_window_v1:document:")
          || !doc || typeof doc.id !== "string" || !doc.id || !["preparing", "unjournalled_publication_possible"].includes(doc.phase)
          || !(doc.repo_id === null || typeof doc.repo_id === "string" && doc.repo_id.length > 0)) throw new Error("malformed guard drain writer window");
        if (doc.repo_id === null || doc.repo_id === id) return false;
      }
      const config = await this.api("GET", `/api/repos/${id}`);
      if (config.status !== 200 || (config.json as any)?.id !== id || (config.json as any)?.enabled !== false
        || (config.json as any)?.svn_url !== pair.svnUrl || (config.json as any)?.git_repo !== pair.gitRepo) return false;
      this.observation("archive199-post-refusal-drain", {pairId: id, enabled: false, status: response.json, windows, durableHoldsPreserved: true});
      return true;
    }, 90_000);
  }

  async archive199WitnessRefusal(original: Pair, kind: "busy" | "held") {
    if (!original.parentId || !original.gitBranch?.startsWith(`archive199-${kind}`) || !/^https:\/\/svn:8443\/svn\/[A-Za-z0-9_.-]+$/.test(original.svnUrl)) throw new Error("refusal requires its dedicated synthetic child on the lab SVN origin");
    if (!/^[A-Za-z0-9_-]{8,80}$/.test(this.project)) throw new Error("invalid owned gate run identity");
    await this.admin();
    await this.archive199ConfigureProxyFraming();
    const pair = {...original, svnUrl: original.svnUrl.replace("https://svn:8443/", "https://fault-proxy:8443/")};
    const saved = await this.api("PUT", `/api/repos/${pair.id}`, {svn_url: pair.svnUrl});
    if (saved.status !== 200 || (saved.json as any)?.svn_url !== pair.svnUrl || (saved.json as any)?.id !== pair.id) this.gap("BLOCKED: dedicated guard fixture could not be routed through the owned proxy");
    const choices = {archive_pair: true, delete_git: true, delete_svn: false};
    // Establish an admissible review against the exact rerouted configuration.
    const initialReview = await this.api("POST", `/api/repos/${pair.id}/removal/review`, choices);
    if (initialReview.status !== 200 || !(initialReview.json as any)?.review_pin) this.gap("BLOCKED: fresh proxy-configured archive review is unavailable");
    const ownerToken = randomBytes(32).toString("hex"), requestId = `archive199-${randomBytes(12).toString("hex")}`;
    this.rememberSecret(ownerToken);
    const identity = {owner_token: ownerToken, run_id: this.project, request_id: requestId};
    if (!/^branches\/[A-Za-z0-9_.-]+$/.test(pair.svnBranch || "")) throw new Error("invalid owned producer branch path");
    const scope = new URL(pair.svnUrl).pathname;
    const target = `${scope}/${pair.svnBranch}`, phase = kind === "busy" ? "before-forward" : "after-forward";
    const arm = {...identity, action: "arm", method: "MERGE", target, scope_path: scope, phase, timeout_seconds: 120};
    let gate = await this.archive199GateControl(arm), terminal = false, primaryFailed = false;
    const journal = async () => {
      const rows = await this.sqlRows(`SELECT value FROM kv_state WHERE key=(SELECT 'git_to_svn_commit_v1:document:'||value FROM kv_state WHERE key='git_to_svn_commit_v1:active:${safe(pair.id, "repo id")}');`);
      return rows.length === 1 ? JSON.parse(rows[0].value) as Record<string, any> : null;
    };
    try {
      const file = `archive199-producer-${randomBytes(5).toString("hex")}.txt`;
      const source = await this.commitGit(pair, `printf '%s' 'owned archive refusal producer' > ${file}\ngit add ${file}\ngit commit -m 'archive199 producer barrier'\n`);
      const trigger = await this.triggerGit(pair.gitRepo, false);
      if (trigger !== 200) this.gap("BLOCKED: actual source producer was not triggered");
      try {
        await this.waitUntil(async () => {
          gate = await this.archive199GateControl();
          if (["aborted", "timed_out"].includes(gate.state)) this.gap(`BLOCKED: owned producer barrier ended without a witness: ${gate.error || gate.state}`);
          return gate.state === "blocked";
        }, 60_000);
      } catch (error) {
        this.observation("archive199-missing-producer-barrier", {kind, source, gate, error: String(error)});
        this.gap("BLOCKED: no exact owned SVN MERGE producer barrier was witnessed");
      }
      if (gate.run_id !== this.project || gate.request_id !== requestId || gate.method !== "MERGE" || gate.target !== target || gate.scope_path !== scope || gate.phase !== phase || !gate.matched_at || !gate.witnessed_at) this.gap("BLOCKED: producer barrier identity or phase does not match this operation");
      const barrier = {...gate};
      if (kind === "held") {
        if (gate.upstream_response_complete !== true || !gate.response_status || gate.response_status < 200 || gate.response_status >= 300) this.gap("BLOCKED: no complete successful upstream producer response was witnessed");
        await this.archive199GateControl({...identity, action: "abort"}); terminal = true;
        // Dropping a response is NOT itself a durable hold. Observe the actual
        // producer journal; a completed/reconciled operation must not pass.
        try { await this.waitUntil(async () => (await journal())?.state === "reconciliation_required", 30_000); }
        catch (error) {
          this.observation("archive199-held-not-demonstrated", {barrier, producerJournal: await journal(), error: String(error)});
          this.gap("BLOCKED: after-forward lost response did not produce an observed durable reconciliation hold");
        }
      }
      const producerJournal = await journal();
      const expectedStates = kind === "busy" ? ["running", "queued", "intent_recorded"] : ["reconciliation_required", "held"];
      if (!producerJournal?.id || producerJournal.repo_id !== pair.id || producerJournal.source_git_sha !== source || !expectedStates.includes(producerJournal.state)) this.gap("BLOCKED: barrier has no matching actual durable producer journal/state");
      // Source advanced after the initial review. Recapture now, under the
      // witnessed barrier, so stale-pin rejection cannot masquerade as busy.
      const freshReview = await this.api("POST", `/api/repos/${pair.id}/removal/review`, choices);
      if (freshReview.status !== 200 || !(freshReview.json as any)?.review_pin) this.gap("BLOCKED: current-target review could not be captured under producer barrier");
      const before = await this.archive199Observe(pair);
      const response = await this.api("POST", `/api/repos/${pair.id}/removal/approve`, {...choices, review_pin: (freshReview.json as any).review_pin, request_id: `archive199-refuse-${randomBytes(12).toString("hex")}`});
      const after = await this.archive199Observe(pair);
      const finalJournal = await journal();
      const finalGate = await this.archive199GateControl();
      if (!finalJournal || finalJournal.id !== producerJournal.id || !expectedStates.includes(finalJournal.state) || (kind === "busy" && finalGate.state !== "blocked")) this.gap("BLOCKED: actual producer barrier/hold did not cover the complete refusal observation");
      const reason = kind === "busy" ? `writer ${producerJournal.state}` : `reconciliation hold: ${producerJournal.outcome_detail || producerJournal.state}`;
      const evidence = {kind, before, after, response: {status: response.status, json: response.json},
        sourceWitness: {gitSha: source, file, trigger}, barrier,
        witness: {owner: createHash("sha256").update(ownerToken).digest("hex"), runId: this.project, requestId, method: "MERGE", target, phase,
          matched: true, producerJournal, reason, finalJournal, finalGate}};
      this.observation(`archive199-${kind}-refusal`, evidence);
      return evidence;
    } catch (error) {
      primaryFailed = true;
      throw error;
    } finally {
      if (!terminal) {
        try {
          const current = await this.archive199GateControl();
          if (current.run_id === this.project && current.request_id === requestId && ["armed", "inflight", "blocked"].includes(current.state)) await this.archive199GateControl({...identity, action: "abort"});
        } catch (cleanupError) {
          if (!primaryFailed) throw cleanupError;
          // Keep the original refusal failure authoritative; cleanup is still
          // bounded and its failure remains visible in the local test receipt.
          this.info.annotations.push({type: "labCleanupFailure", description: "owned archive199 gate cleanup failed after the primary test failure"});
        }
      }
    }
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
