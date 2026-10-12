import { readdir, readFile, writeFile, stat } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

function redactText(text, secrets) {
  // Match a credential, not the rest of the line. Playwright writes the git
  // diff as one JSON line, and eating through the closing quote made
  // report.json impossible to parse on run 38188946403.
  const credential = "[A-Za-z0-9._~+/=-]+";
  let next = text
    .replace(new RegExp(`Authorization:\\s+Basic\\s+${credential}`, "gi"), "Authorization: [redacted]")
    .replace(new RegExp(`Authorization:\\s+Bearer\\s+${credential}`, "gi"), "Authorization: [redacted]")
    .replace(new RegExp(`Authorization:\\s+token\\s+(?!\\$)${credential}`, "gi"), "Authorization: [redacted]")
    .replace(new RegExp(`Bearer\\s+${credential}`, "g"), "Bearer [redacted]")
    .replace(/Authorization:\s+\[redacted\]\s+[A-Za-z0-9+/=]{8,}/g, "Authorization: [redacted]")
    .replace(/-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g, "[redacted-private-key]");
  const ordered = [...secrets].filter((secret) => secret && secret.length >= 8).sort((a, b) => b.length - a.length);
  for (const secret of ordered) {
    next = next.split(secret).join("[redacted]");
  }
  return next;
}

function redactBytes(raw, secrets) {
  const text = Buffer.from(raw).toString("latin1");
  return Buffer.from(redactText(text, secrets), "latin1");
}

export async function scrubTree(root, secrets = []) {
  let files = 0;
  async function walk(dir) {
    const entries = await readdir(dir, { withFileTypes: true });
    for (const entry of entries) {
      const path = join(dir, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) {
        await walk(path);
        continue;
      }
      const info = await stat(path);
      if (!info.isFile() || info.size > 20_000_000) continue;
      const raw = await readFile(path);
      if (raw.includes(0)) {
        const next = redactBytes(raw, secrets);
        if (!next.equals(raw)) {
          await writeFile(path, next);
          files += 1;
        }
        continue;
      }
      const text = raw.toString("utf8");
      const next = redactText(text, secrets);
      if (next !== text) {
        await writeFile(path, next);
        files += 1;
      }
    }
  }
  await walk(root);
  return { files };
}

export function scrubSelfCheck() {
  const secret = "0123456789abcdef";
  const jsonLine = JSON.stringify({
    gitDiff: 'curl -H "Authorization: token $LAB_TOKEN" https://gitea\nnext',
    actualWorkers: 1,
  });
  const redacted = redactText(jsonLine, [secret]);
  const parsed = JSON.parse(redacted);
  if (!parsed.gitDiff.includes("$LAB_TOKEN")) {
    throw new Error("source placeholder was removed from the JSON line");
  }
  if (!String(parsed.actualWorkers)) {
    throw new Error("JSON field after the diff was lost");
  }
  const header = `prefix Authorization: Bearer ${secret}\nAuthorization: token ${secret}\n`;
  const headerOut = redactText(header, [secret]);
  if (headerOut.includes(secret)) throw new Error("credential remained");
  if (!headerOut.includes("Authorization: [redacted]")) throw new Error("header was not redacted");
  const bearer = "bearer-secret-value-0123456789";
  const token = "token-secret-value-0123456789";
  const basicPayload = Buffer.from("lab-user:lab-secret-value").toString("base64");
  for (const [scheme, value] of [["Basic", basicPayload], ["Bearer", bearer], ["token", token]]) {
    const raw = JSON.stringify({ header: `Authorization: ${scheme} ${value}`, keep: true });
    const out = redactText(raw, []);
    const parsed = JSON.parse(out);
    if (parsed.keep !== true) throw new Error(`${scheme} JSON field was lost`);
    if (parsed.header !== "Authorization: [redacted]") throw new Error(`${scheme} header was ${parsed.header}`);
    if (out.includes(value)) throw new Error(`${scheme} payload survived JSON redaction`);
  }
  return "PASS";
}

export { redactText };

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
  if (args.includes("--self-check")) {
    console.log(`scrub self-check ${scrubSelfCheck()}`);
    process.exit(0);
  }
  const root = get("--root");
  if (!root) {
    console.error("usage: node scrub.mjs --root <dir> [--secrets-file <file>]");
    process.exit(2);
  }
  const result = await scrubTree(root, await secretsFromFile(get("--secrets-file")));
  console.log(JSON.stringify(result));
}
