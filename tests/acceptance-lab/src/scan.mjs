import { mkdir, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { join, relative } from "node:path";
import { pathToFileURL } from "node:url";
import { crc32, inflateRawSync, gunzipSync } from "node:zlib";

const PRIVATE_KEY = /-----BEGIN [A-Z ]*PRIVATE KEY-----/;
const CANARY = "CANARY_SECRET_SHOULD_NOT_LEAK";
const LIVE_BASIC = /Authorization:\s+Basic\s+[A-Za-z0-9+/=]{8,}/i;
const LIVE_BEARER = /Authorization:\s+Bearer\s+\S{8,}/i;
const LIVE_TOKEN = /Authorization:\s+token\s+(?!\$)[A-Za-z0-9._~+/-]{8,}/i;
const LEFTOVER_PAYLOAD = /Authorization:\s+\[redacted\]\s+[A-Za-z0-9+/=]{8,}/;

function entryFindings(rel, bytes, secrets) {
  const findings = [];
  const decoded = bytes.toString("utf8");
  if (decoded.includes(CANARY)) findings.push({ path: rel, reason: "canary" });
  if (PRIVATE_KEY.test(decoded)) findings.push({ path: rel, reason: "private_key" });
  if (LIVE_BASIC.test(decoded) || LIVE_BEARER.test(decoded) || LIVE_TOKEN.test(decoded) || LEFTOVER_PAYLOAD.test(decoded)) {
    findings.push({ path: rel, reason: "credential_header" });
  }
  for (const secret of secrets) {
    if (secret && secret.length >= 8 && decoded.includes(secret)) {
      findings.push({ path: rel, reason: "secret_value" });
      break;
    }
  }
  return findings;
}

function safeEntryName(name) {
  if (!name || name.length > 240 || name.includes("\\") || name.startsWith("/") || name.split("/").some((part) => !part || part === "." || part === "..")) {
    return "";
  }
  return name;
}

export function archiveEntries(bytes) {
  if (bytes.length >= 2 && bytes[0] === 0x1f && bytes[1] === 0x8b) {
    return [{ name: "gunzip", data: gunzipSync(bytes) }];
  }
  if (bytes.length < 4 || bytes.readUInt32LE(0) !== 0x04034b50) return null;
  const entries = [];
  let offset = 0;
  while (offset + 30 <= bytes.length) {
    if (bytes.readUInt32LE(offset) !== 0x04034b50) break;
    const flags = bytes.readUInt16LE(offset + 6);
    const method = bytes.readUInt16LE(offset + 8);
    const compressedSize = bytes.readUInt32LE(offset + 18);
    const nameLength = bytes.readUInt16LE(offset + 26);
    const extraLength = bytes.readUInt16LE(offset + 28);
    const nameStart = offset + 30;
    const dataStart = nameStart + nameLength + extraLength;
    const name = safeEntryName(bytes.toString("utf8", nameStart, nameStart + nameLength));
    if (!name || (flags & 0x1) !== 0 || (flags & 0x8) !== 0 || (method !== 0 && method !== 8) || compressedSize > 20_000_000 || dataStart + compressedSize > bytes.length) {
      throw new Error("unsafe or unsupported archive entry");
    }
    const compressed = bytes.subarray(dataStart, dataStart + compressedSize);
    if (method === 0) {
      const expected = crc32(compressed) >>> 0;
      const recorded = bytes.readUInt32LE(offset + 14) >>> 0;
      if (expected !== recorded) throw new Error("archive entry checksum mismatch");
      entries.push({ name, data: Buffer.from(compressed) });
    } else {
      const data = inflateRawSync(compressed);
      const expected = crc32(data) >>> 0;
      const recorded = bytes.readUInt32LE(offset + 14) >>> 0;
      if (expected !== recorded || data.length !== bytes.readUInt32LE(offset + 22)) throw new Error("archive entry checksum mismatch");
      if (data.length > 20_000_000) throw new Error("archive entry is oversized");
      entries.push({ name, data });
    }
    offset = dataStart + compressedSize;
  }
  return entries;
}

export function isArchive(name, bytes) {
  return /\.(zip|gz)$/i.test(name) || (bytes.length >= 4 && bytes.readUInt32LE(0) === 0x04034b50) || (bytes.length >= 2 && bytes[0] === 0x1f && bytes[1] === 0x8b);
}

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
      if (isArchive(entry.name, text)) {
        let entries;
        try {
          entries = archiveEntries(text);
        } catch (error) {
          findings.push({ path: rel, reason: "archive_withheld", detail: error.message });
          continue;
        }
        if (!entries) {
          findings.push({ path: rel, reason: "archive_withheld", detail: "unrecognized archive" });
          continue;
        }
        for (const item of entries) {
          if (item.data.length > 20_000_000) {
            findings.push({ path: `${rel}!${item.name}`, reason: "oversized" });
            continue;
          }
          findings.push(...entryFindings(`${rel}!${item.name}`, item.data, secrets));
        }
        continue;
      }
      findings.push(...entryFindings(rel, text, secrets));
    }
  }
  await walk(root);
  return { ok: findings.length === 0, findings };
}

export async function materializeDiagnostics(source, dest, secrets = []) {
  await rm(dest, { recursive: true, force: true });
  await mkdir(dest, { recursive: true });
  const findings = [];
  const diagnostics = [];
  async function walk(dir) {
    const entries = await readdir(dir, { withFileTypes: true });
    for (const entry of entries) {
      const path = join(dir, entry.name);
      const rel = relative(source, path);
      if (entry.isDirectory()) {
        await walk(path);
        continue;
      }
      if (!entry.isFile()) {
        findings.push({ path: rel, reason: "symlink" });
        continue;
      }
      const bytes = await readFile(path);
      if (isArchive(entry.name, bytes)) {
        let unpacked;
        try {
          unpacked = archiveEntries(bytes);
        } catch (error) {
          findings.push({ path: rel, reason: "archive_withheld", detail: error.message });
          continue;
        }
        if (!unpacked) {
          findings.push({ path: rel, reason: "archive_withheld" });
          continue;
        }
        for (const item of unpacked) {
          const itemFindings = entryFindings(`${rel}!${item.name}`, item.data, secrets);
          findings.push(...itemFindings);
          if (itemFindings.length === 0 && !item.data.includes(0) && item.data.length <= 1_000_000) {
            const diagnostic = join(dest, "diagnostics", `${rel.replaceAll("/", "__")}__${item.name.replaceAll("/", "__")}.txt`);
            await mkdir(join(dest, "diagnostics"), { recursive: true });
            await writeFile(diagnostic, item.data);
            diagnostics.push(relative(dest, diagnostic));
          }
        }
        continue;
      }
      const plain = entryFindings(rel, bytes, secrets);
      findings.push(...plain);
      if (plain.length === 0) {
        const copied = join(dest, rel);
        await mkdir(join(copied, ".."), { recursive: true });
        await writeFile(copied, bytes);
      }
    }
  }
  await walk(source);
  return { ok: findings.length === 0, findings, diagnostics };
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
  const dest = get("--dest");
  if (!root) {
    console.error("usage: node scan.mjs --root <dir> [--dest <dir>] [--secrets-file <file>] [--out <file>]");
    process.exit(2);
  }
  const secrets = await secretsFromFile(get("--secrets-file"));
  const result = dest ? await materializeDiagnostics(root, dest, secrets) : await scanTree(root, secrets);
  const payload = JSON.stringify(result, null, 2);
  const out = get("--out");
  if (out) await writeFile(out, payload);
  console.log(payload);
  process.exit(result.ok ? 0 : 1);
}
