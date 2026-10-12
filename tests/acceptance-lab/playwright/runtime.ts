import { execFile } from "node:child_process";
import { createHmac, randomBytes } from "node:crypto";
import { appendFileSync } from "node:fs";
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
};

type Snap = { svn: string; git: string; maps: string; records: string; watermark: string };

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
    return { status: response.status, json, text };
  }

  async admin(): Promise<string> {
    if (this.token) return this.token;
    const result = await this.api("POST", "/api/auth/login", {
      username: this.adminUser,
      password: this.adminPassword,
    }, "");
    const token = (result.json as { token?: string } | null)?.token;
    if (result.status !== 200 || !token) {
      throw new Error(`admin login failed (${result.status})`);
    }
    this.token = token;
    return token;
  }

  async login(username: string, password: string): Promise<{ status: number; json: unknown }> {
    const result = await this.api("POST", "/api/auth/login", { username, password }, "");
    return result;
  }

  async repos(): Promise<Array<{ id: string; git_repo: string; name: string; svn_url: string }>> {
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

  async waitImport(id: string): Promise<void> {
    await expect.poll(async () => {
      const status = await this.api("GET", `/api/repos/${id}/import/status`);
      const body = status.json as { phase?: string; lifecycle?: string } | null;
      return body?.phase || body?.lifecycle || `http-${status.status}`;
    }, { timeout: 180_000, intervals: [1000, 2000, 3000] }).toMatch(/completed|Completed/);
  }

  async createPair(name: string): Promise<Pair> {
    const repo = safe(name, "repo");
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
    await this.sh("gitea", `
set -eu
code=$(curl --silent --show-error --output /tmp/repo.out --write-out '%{http_code}' --cacert /certs/ca.crt \
  -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" \
  -d '{"username":"lab","visibility":"private"}' \
  https://gitea:3000/api/v1/orgs || true)
curl --silent --show-error --cacert /certs/ca.crt \
  -H "Authorization: token $GIT_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"${repo}","private":true,"auto_init":false,"default_branch":"main"}' \
  https://gitea:3000/api/v1/orgs/lab/repos >/tmp/repo2.out || true
`);
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
      enabled: true,
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
    await this.waitImport(id);
    return { id, name: repo, svnUrl: `https://svn:8443/svn/${repo}`, gitRepo: `lab/${repo}` };
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
  --show-item revision ${pair.svnUrl}/trunk
`)).trim();
    const git = (await this.observer(`
set -eu
auth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')
git -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt \
  ls-remote https://gitea:3000/${pair.gitRepo}.git refs/heads/main | awk '{print $1}'
`)).trim();
    const counts = (await this.sh("reposync", `
set -eu
sqlite3 /var/lib/reposync/reposync.db "SELECT COUNT(*) FROM commit_map WHERE repo_id='${pair.id}'; SELECT COUNT(*) FROM sync_records WHERE repo_id='${pair.id}'; SELECT COALESCE((SELECT value FROM kv_state WHERE key='last_svn_rev_${pair.id}'), 'none');"
`, "root")).trim().split(/\s+/);
    return {
      svn,
      git,
      maps: counts[0] || "0",
      records: counts[1] || "0",
      watermark: counts[2] || "none",
    };
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
    const rel = safe(path, "path");
    return this.observer(`
set -eu
svn cat --non-interactive --config-dir "$HOME/.svn" --username svc-sync --password "$SVN_PASSWORD" \
  ${pair.svnUrl}/trunk/${rel}
`);
  }

  async gitBytes(pair: Pair, path: string): Promise<string> {
    const rel = safe(path, "path");
    const dir = `/work/read-${pair.name}`;
    return this.observer(`
set -eu
auth=$(printf '%s:%s' "$GIT_USER" "$GIT_TOKEN" | base64 | tr -d '\\n')
if [ ! -d ${dir}/.git ]; then
  git -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt \
    clone --branch main https://gitea:3000/${pair.gitRepo}.git ${dir}
fi
git -C ${dir} -c http.extraHeader="Authorization: Basic $auth" -c http.sslCAInfo=/certs/ca.crt fetch origin main
git -C ${dir} checkout -q FETCH_HEAD
cat ${dir}/${rel}
`);
  }

  async commitSvn(pair: Pair, script: string, username = "svc-sync"): Promise<string> {
    const dir = `/work/svn-${pair.name}-${randomBytes(3).toString("hex")}`;
    const user = safe(username, "svn user");
    const password = user === "alice" ? "$ALICE_PASSWORD" : user === "bob" ? "$BOB_PASSWORD" : "$SVN_PASSWORD";
    const output = await this.observer(`
set -eu
svn checkout --non-interactive --config-dir "$HOME/.svn" --username ${user} --password "${password}" \
  ${pair.svnUrl}/trunk ${dir}
cd ${dir}
${script}
svn commit --non-interactive --username ${user} --password "${password}" -m "lab ${pair.name}" .
svn info --non-interactive --username ${user} --password "${password}" --show-item revision .
`);
    const lines = output.trim().split("\n");
    return lines[lines.length - 1].trim();
  }

  async commitGit(pair: Pair, script: string, ref = "main"): Promise<string> {
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
git push origin ${branch}
git rev-parse HEAD
`);
    const lines = output.trim().split("\n");
    return lines[lines.length - 1].trim();
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
        && now.watermark === before.watermark;
      return same && Date.now() - start > 12_000;
    }, { timeout: 25_000, intervals: [1000] }).toBe(true);
  }

  async enableLdap(): Promise<void> {
    await this.admin();
    const saved = await this.api("PUT", "/api/admin/ldap", {
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
    });
    if (saved.status !== 200) throw new Error(`save ldap failed (${saved.status})`);
  }

  async uiLogin(username: string, password: string): Promise<void> {
    await this.page.goto("/login");
    await this.page.evaluate(() => localStorage.clear());
    await this.page.goto("/login");
    await this.page.locator('input[name="username"]').fill(username);
    await this.page.locator('input[type="password"]').fill(password);
    await this.page.getByRole("button", { name: "Sign in" }).click();
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
