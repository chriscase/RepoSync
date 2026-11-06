import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import {
  NUL_SAMPLE_SHA256,
  cancelOutcome,
  contentIntegrityOutcome,
  egressOutcome,
  generationOutcome,
  linearPublicationOutcome,
  restorePrewriteOutcome,
} from "../playwright/oracles.ts";

const tip = "c".repeat(40);
const mid = "b".repeat(40);
const first = "a".repeat(40);

test("no-op generation probe is not a PASS", () => {
  const outcome = generationOutcome({
    generationChanged: false,
    staleReceiptChecked: false,
    echoReceiptChecked: false,
    preservationAsserted: false,
  });
  assert.notEqual(outcome.status, "PASS");
  assert.equal(outcome.qualification, "UNQUALIFIED");
  assert.match(outcome.reason, /no-op generation probe/);
  assert.match(outcome.reason, /old-install hold/);
});

test("backup-only restore is not a PASS", () => {
  const outcome = restorePrewriteOutcome({
    backupRead: true,
    oldDatabaseRestored: false,
    oldBinaryRestored: false,
    restoredBeforeExternalWrite: false,
    preservationAsserted: false,
  });
  assert.notEqual(outcome.status, "PASS");
  assert.equal(outcome.qualification, "UNQUALIFIED");
  assert.match(outcome.reason, /backup-only restore/);
  assert.match(outcome.reason, /old-install hold/);
});

test("prefix-only publication is not a PASS", () => {
  const outcome = linearPublicationOutcome({
    commits: [first, mid, tip],
    receipts: { [first]: 1 },
    intermediates: ["0\n", null, null],
    expectedIntermediates: ["0\n", "1\n", "2\n"],
    finalBytes: "0\n",
    expectedFinal: "2\n",
    checkpoints: { git: first, watermark: first, column: first },
    idle: false,
  });
  assert.notEqual(outcome.status, "PASS");
  assert.match(outcome.reason, /prefix-only/);
});

test("squash-only publication is not a PASS", () => {
  const outcome = linearPublicationOutcome({
    commits: [first, mid, tip],
    receipts: { [tip]: 1 },
    intermediates: [null, null, "C\n"],
    expectedIntermediates: ["A\n", "B\n", "C\n"],
    finalBytes: "C\n",
    expectedFinal: "C\n",
    checkpoints: { git: tip, watermark: tip, column: tip },
    idle: true,
  });
  assert.notEqual(outcome.status, "PASS");
  assert.match(outcome.reason, /squash-only/);
});

test("complete linear publication can PASS", () => {
  const outcome = linearPublicationOutcome({
    commits: [first, mid, tip],
    receipts: { [first]: 1, [mid]: 1, [tip]: 1 },
    intermediates: ["A\n", "B\n", "C\n"],
    expectedIntermediates: ["A\n", "B\n", "C\n"],
    finalBytes: "C\n",
    expectedFinal: "C\n",
    checkpoints: { git: tip, watermark: tip, column: tip },
    idle: true,
  });
  assert.equal(outcome.status, "PASS");
});

test("retained deleted path is not a PASS", () => {
  const outcome = contentIntegrityOutcome({
    inventory: ["keep.txt", "drop.txt"],
    expectedPresent: [{ path: "keep.txt", bytes: "keep\n" }],
    expectedAbsent: ["drop.txt"],
    observedBytes: { "keep.txt": "keep\n" },
    observedSha256: {},
  });
  assert.notEqual(outcome.status, "PASS");
  assert.match(outcome.reason, /retained-deleted-path drop\.txt/);
});

test("copy instead of rename is not a PASS", () => {
  const outcome = contentIntegrityOutcome({
    inventory: ["from.txt", "to.txt"],
    expectedPresent: [{ path: "to.txt", bytes: "renamed\n" }],
    expectedAbsent: ["from.txt"],
    observedBytes: { "to.txt": "renamed\n" },
    observedSha256: {},
  });
  assert.notEqual(outcome.status, "PASS");
  assert.match(outcome.reason, /copy-instead-of-rename from\.txt/);
});

test("NUL loss is not a PASS", () => {
  const lost = createHash("sha256").update(Buffer.from("ab\n")).digest("hex");
  const outcome = contentIntegrityOutcome({
    inventory: ["binary.bin"],
    expectedPresent: [{ path: "binary.bin", sha256: NUL_SAMPLE_SHA256 }],
    expectedAbsent: [],
    observedBytes: {},
    observedSha256: { "binary.bin": lost },
  });
  assert.notEqual(outcome.status, "PASS");
  assert.match(outcome.reason, /NUL-loss binary\.bin/);
  assert.notEqual(lost, NUL_SAMPLE_SHA256);
});

test("exact bytes and absence can PASS", () => {
  const outcome = contentIntegrityOutcome({
    inventory: ["keep.txt", "README.txt"],
    expectedPresent: [{ path: "keep.txt", bytes: "keep\n" }],
    expectedAbsent: ["drop.txt"],
    observedBytes: { "keep.txt": "keep\n" },
    observedSha256: {},
  });
  assert.equal(outcome.status, "PASS");
});

test("cancel acknowledgment is not completion", () => {
  const outcome = cancelOutcome({
    acknowledged: true,
    phaseBarrier: false,
    finalState: "cancel_requested",
    childCleanup: false,
    checkpointPreserved: false,
  });
  assert.equal(outcome.status, "PARTIAL");
  assert.equal(outcome.qualification, "UNQUALIFIED");
  assert.notEqual(outcome.status, "PASS");
});

test("egress rejects successful non-HTML responses and distinguishes the probe", () => {
  for (const body of ["", "{\"ok\":true}", "<HTML>example</HTML>"]) {
    const status = body === "" ? 204 : 200;
    const outcome = egressOutcome({
      tool: "present", context: "application", exitCode: 0, httpStatus: status, connects: 1, body,
    });
    assert.equal(outcome.status, "FAIL", body || "empty");
    assert.match(outcome.reason, /established external contact/);
  }
  const redirect = egressOutcome({
    tool: "present", context: "application", exitCode: 0, httpStatus: 302, connects: 1, body: "",
  });
  assert.equal(redirect.status, "FAIL");
  const missing = egressOutcome({
    tool: "absent", context: "application", exitCode: null, httpStatus: null, connects: null, body: "",
  });
  assert.equal(missing.status, "NOT_RUN");
  assert.match(missing.reason, /probe tool unavailable/);
  const observer = egressOutcome({
    tool: "present", context: "observer", exitCode: 7, httpStatus: 0, connects: 0, body: "",
  });
  assert.notEqual(observer.status, "PASS");
  assert.match(observer.reason, /application context/);
  const rejected = egressOutcome({
    tool: "present", context: "application", exitCode: 7, httpStatus: 0, connects: 0, body: "",
  });
  assert.equal(rejected.status, "PASS");
});
