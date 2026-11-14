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
  linearReceiptEvidence,
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
    revisions: ["2", "3", "4"],
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
    revisions: ["2", "3", "4"],
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
    revisions: ["2", "3", "4"],
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

test("a still-pending receipt is not a duplicate or unordered mapping", () => {
  const commits = [first, mid, tip];
  const empty = linearReceiptEvidence(commits, []);
  assert.equal(empty.duplicate, false);
  assert.equal(empty.unordered, false);
  assert.deepEqual(Object.values(empty.receipts), [0, 0, 0]);
  const prefix = linearReceiptEvidence(commits, [{ git_sha: first, svn_rev: "2" }]);
  assert.equal(prefix.duplicate, false);
  assert.equal(prefix.unordered, false);
  assert.equal(prefix.receipts[first], 1);
  assert.equal(prefix.receipts[mid], 0);
  const duplicated = linearReceiptEvidence(commits, [
    { git_sha: first, svn_rev: "2" },
    { git_sha: first, svn_rev: "3" },
    { git_sha: mid, svn_rev: "4" },
    { git_sha: tip, svn_rev: "5" },
  ]);
  assert.equal(duplicated.duplicate, true);
  const unordered = linearReceiptEvidence(commits, [
    { git_sha: first, svn_rev: "5" },
    { git_sha: mid, svn_rev: "4" },
    { git_sha: tip, svn_rev: "6" },
  ]);
  assert.equal(unordered.duplicate, false);
  assert.equal(unordered.unordered, true);
});

test("duplicate and unordered revision mappings are not a PASS", () => {
  const duplicate = linearPublicationOutcome({
    commits: [first, mid, tip],
    receipts: { [first]: 2, [mid]: 1, [tip]: 1 },
    revisions: ["2", "3", "4"],
    intermediates: ["A\n", "B\n", "C\n"],
    expectedIntermediates: ["A\n", "B\n", "C\n"],
    finalBytes: "C\n",
    expectedFinal: "C\n",
    checkpoints: { git: tip, watermark: tip, column: tip },
    idle: true,
  });
  assert.notEqual(duplicate.status, "PASS");
  assert.match(duplicate.reason, /duplicate receipts/);
  const unordered = linearPublicationOutcome({
    commits: [first, mid, tip],
    receipts: { [first]: 1, [mid]: 1, [tip]: 1 },
    revisions: ["4", "3", "5"],
    intermediates: ["A\n", "B\n", "C\n"],
    expectedIntermediates: ["A\n", "B\n", "C\n"],
    finalBytes: "C\n",
    expectedFinal: "C\n",
    checkpoints: { git: tip, watermark: tip, column: tip },
    idle: true,
  });
  assert.notEqual(unordered.status, "PASS");
  assert.match(unordered.reason, /unordered revision mappings/);
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
  const missingConnects = egressOutcome({
    tool: "present", context: "application", exitCode: 7, httpStatus: 0, connects: null, body: "",
  });
  assert.notEqual(missingConnects.status, "PASS");
  assert.match(missingConnects.reason, /missing or truncated/);
  const missingExit = egressOutcome({
    tool: "present", context: "application", exitCode: null, httpStatus: 0, connects: 0, body: "",
  });
  assert.notEqual(missingExit.status, "PASS");
  assert.match(missingExit.reason, /missing or truncated/);
  const internal = egressOutcome({
    tool: "present", context: "application", exitCode: 27, httpStatus: 0, connects: 0, body: "",
  });
  assert.equal(internal.status, "FAIL");
  assert.match(internal.reason, /internal curl error/);
});
