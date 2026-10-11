import { readdir, readFile, writeFile, stat } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

function redactText(text, secrets) {
  let next = text
    .replace(/Authorization:\s*[^\r\n]+/gi, "Authorization: [redacted]")
    .replace(/Bearer\s+[A-Za-z0-9._~+/-]+=*/g, "Bearer [redacted]")
    .replace(/-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g, "[redacted-private-key]");
  const ordered = [...secrets].filter((secret) => secret && secret.length >= 8).sort((a, b) => b.length - a.length);
  for (const secret of ordered) {
    next = next.split(secret).join("[redacted]");
  }
  return next;
}

function redactBytes(raw, secrets) {
  let next = Buffer.from(raw);
  const ordered = [...secrets].filter((secret) => secret && secret.length >= 8).sort((a, b) => b.length - a.length);
  for (const secret of ordered) {
    const needle = Buffer.from(secret);
    const replacement = Buffer.alloc(needle.length, 0x78);
    let index = 0;
    while (index <= next.length - needle.length) {
      const found = next.indexOf(needle, index);
      if (found === -1) break;
      replacement.copy(next, found);
      index = found + needle.length;
    }
  }
  return next;
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
  const root = get("--root");
  if (!root) {
    console.error("usage: node scrub.mjs --root <dir> [--secrets-file <file>]");
    process.exit(2);
  }
  const result = await scrubTree(root, await secretsFromFile(get("--secrets-file")));
  console.log(JSON.stringify(result));
}
