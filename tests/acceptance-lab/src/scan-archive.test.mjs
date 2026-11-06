import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { deflateRawSync, crc32 } from "node:zlib";
import { materializeDiagnostics, scanTree } from "./scan.mjs";

function zip(files) {
  const locals = [];
  let offset = 0;
  for (const file of files) {
    const name = Buffer.from(file.name);
    const stored = file.method !== 8;
    const data = stored ? file.data : deflateRawSync(file.data);
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50, 0);
    header.writeUInt16LE(20, 4);
    header.writeUInt16LE(0, 8);
    header.writeUInt32LE(crc32(file.data) >>> 0, 14);
    header.writeUInt32LE(data.length, 18);
    header.writeUInt32LE(file.data.length, 22);
    header.writeUInt16LE(name.length, 26);
    if (!stored) header.writeUInt16LE(8, 8);
    locals.push({ header, name, data, offset });
    offset += header.length + name.length + data.length;
  }
  const parts = [];
  const central = [];
  let centralOffset = 0;
  for (const local of locals) {
    parts.push(local.header, local.name, local.data);
    const item = Buffer.alloc(46);
    item.writeUInt32LE(0x02014b50, 0);
    item.writeUInt16LE(20, 4);
    item.writeUInt16LE(20, 6);
    item.writeUInt16LE(local.header.readUInt16LE(8), 10);
    item.writeUInt32LE(local.header.readUInt32LE(14), 16);
    item.writeUInt32LE(local.data.length, 20);
    item.writeUInt32LE(local.header.readUInt32LE(22), 24);
    item.writeUInt16LE(local.name.length, 28);
    item.writeUInt32LE(local.offset, 42);
    central.push(item, local.name);
    centralOffset += item.length + local.name.length;
  }
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  eocd.writeUInt16LE(locals.length, 8);
  eocd.writeUInt16LE(locals.length, 10);
  eocd.writeUInt32LE(centralOffset, 12);
  eocd.writeUInt32LE(offset, 16);
  return Buffer.concat([...parts, ...central, eocd]);
}

test("a compressed secret canary fails the scan and readable diagnostics remain", async () => {
  const root = mkdtempSync(join(tmpdir(), "scan-archive-"));
  try {
    const secret = Buffer.from("CANARY_SECRET_SHOULD_NOT_LEAK");
    const readable = Buffer.from("trace step reached the login diagnostic\n");
    writeFileSync(join(root, "trace.zip"), zip([
      { name: "secret.txt", data: secret, method: 8 },
      { name: "steps.txt", data: readable, method: 8 },
    ]));
    writeFileSync(join(root, "notes.txt"), "plain diagnostic stayed readable\n");
    const scanned = await scanTree(root, []);
    assert.equal(scanned.ok, false);
    assert.ok(scanned.findings.some((item) => item.reason === "canary" && item.path.includes("trace.zip!secret.txt")));
    const dest = join(root, "publish");
    const published = await materializeDiagnostics(root, dest, []);
    assert.equal(published.ok, false);
    assert.equal(readFileSync(join(dest, "notes.txt"), "utf8"), "plain diagnostic stayed readable\n");
    const diagnostic = readFileSync(join(dest, "diagnostics", "trace.zip__steps.txt.txt"), "utf8");
    assert.match(diagnostic, /trace step reached the login diagnostic/);
    assert.equal(readFileSync(join(dest, "diagnostics", "trace.zip__steps.txt.txt"), "utf8").includes("CANARY_SECRET_SHOULD_NOT_LEAK"), false);
    const publishedScan = await scanTree(dest, []);
    assert.equal(publishedScan.ok, true);
    assert.throws(() => readFileSync(join(dest, "trace.zip")));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
