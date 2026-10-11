import { readFile, readdir, stat, writeFile } from "node:fs/promises";
import { join, relative } from "node:path";
import { pathToFileURL } from "node:url";

const PRIVATE_KEY = /-----BEGIN [A-Z ]*PRIVATE KEY-----/;
const CANARY = "CANARY_SECRET_SHOULD_NOT_LEAK";

export async function scanTree(root, secrets = []) {
  const findings = [];
  async function walk(dir) {
    let entries;
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch (error) {
      findings.push({ path: relative(root, dir) || ".", reason: "unreadable", detail: error.message });
      return;
    }
    for (const entry of entries) {
      const path = join(dir, entry.name);
      const rel = relative(root, path);
      let info;
      try {
        info = await stat(path);
      } catch (error) {
        findings.push({ path: rel, reason: "unreadable", detail: error.message });
        continue;
      }
      if (entry.isSymbolicLink()) {
        findings.push({ path: rel, reason: "symlink" });
        continue;
      }
      if (entry.isDirectory()) {
        await walk(path);
        continue;
      }
      if (!info.isFile()) continue;
      if (info.size > 20_000_000) {
        findings.push({ path: rel, reason: "oversized" });
        continue;
      }
      let text;
      try {
        text = await readFile(path);
      } catch (error) {
        findings.push({ path: rel, reason: "unreadable", detail: error.message });
        continue;
      }
      const decoded = text.toString("utf8");
      if (decoded.includes(CANARY)) findings.push({ path: rel, reason: "canary" });
      if (PRIVATE_KEY.test(decoded)) findings.push({ path: rel, reason: "private_key" });
      for (const secret of secrets) {
        if (secret && secret.length >= 8 && decoded.includes(secret)) {
          findings.push({ path: rel, reason: "secret_value" });
          break;
        }
      }
    }
  }
  await walk(root);
  return { ok: findings.length === 0, findings };
}

async function secretsFromFile(path) {
  if (!path) return [];
  const text = await readFile(path, "utf8");
  return text.split(/\r?\n/).map((line) => line.trim()).filter((line) => line.length >= 8);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const get = (flag) => {
    const index = args.indexOf(flag);
    return index === -1 ? "" : args[index + 1] || "";
  };
  const root = get("--root");
  if (!root) {
    console.error("usage: node scan.mjs --root <dir> [--secrets-file <file>] [--out <file>]");
    process.exit(2);
  }
  const result = await scanTree(root, await secretsFromFile(get("--secrets-file")));
  const payload = JSON.stringify(result, null, 2);
  const out = get("--out");
  if (out) await writeFile(out, payload);
  console.log(payload);
  process.exit(result.ok ? 0 : 1);
}
