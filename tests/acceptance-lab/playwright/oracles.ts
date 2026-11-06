import { createHash } from "node:crypto";

export type LabStatus = "PASS" | "FAIL" | "PARTIAL" | "NOT_RUN";

export type Outcome = {
  status: LabStatus;
  qualification: "executed" | "UNQUALIFIED";
  reason: string;
};

const partial = (reason: string): Outcome => ({ status: "PARTIAL", qualification: "UNQUALIFIED", reason });
const failed = (reason: string): Outcome => ({ status: "FAIL", qualification: "UNQUALIFIED", reason });
const passed = (reason: string): Outcome => ({ status: "PASS", qualification: "executed", reason });

export const NUL_SAMPLE_SHA256 = createHash("sha256").update(Buffer.from("a\0b\n")).digest("hex");

export function generationOutcome(input: {
  generationChanged: boolean;
  staleReceiptChecked: boolean;
  echoReceiptChecked: boolean;
  preservationAsserted: boolean;
}): Outcome {
  if (input.generationChanged && input.staleReceiptChecked && input.echoReceiptChecked && input.preservationAsserted) {
    return passed("generation changed and stale, echo, and preservation receipts were checked");
  }
  return partial("no-op generation probe: generation was not changed and stale, echo, and preservation receipts were not established; the old-install hold remains");
}

export function restorePrewriteOutcome(input: {
  backupRead: boolean;
  oldDatabaseRestored: boolean;
  oldBinaryRestored: boolean;
  restoredBeforeExternalWrite: boolean;
  preservationAsserted: boolean;
}): Outcome {
  if (input.oldDatabaseRestored && input.oldBinaryRestored && input.restoredBeforeExternalWrite && input.preservationAsserted) {
    return passed("old database and old binary were restored before any external write, and preservation was asserted");
  }
  const backup = input.backupRead ? "a current-database backup was read; " : "";
  return partial(`${backup}backup-only restore: an old database and old binary were not restored before an external write; the old-install hold remains`);
}

export function linearPublicationOutcome(input: {
  commits: string[];
  receipts: Record<string, number>;
  intermediates: Array<string | null>;
  expectedIntermediates: string[];
  finalBytes: string | null;
  expectedFinal: string;
  checkpoints: { git: string; watermark: string; column: string };
  idle: boolean;
}): Outcome {
  const reasons: string[] = [];
  const commits = input.commits;
  if (commits.length === 0 || commits.length !== input.expectedIntermediates.length || new Set(commits).size !== commits.length) {
    reasons.push("source commits");
  }
  const applied = commits.filter((sha) => input.receipts[sha] === 1);
  if (applied.length !== commits.length) reasons.push("source receipts");
  if (input.intermediates.length !== commits.length || input.intermediates.some((value, index) => value !== input.expectedIntermediates[index])) {
    reasons.push(applied.length < commits.length && applied.length > 0 && applied.length < input.expectedIntermediates.length
      ? "prefix-only"
      : "ordered intermediate content");
  }
  const tip = commits[commits.length - 1] || "";
  if (commits.length > 1 && applied.length === 1 && applied[0] === tip && input.finalBytes === input.expectedFinal) {
    reasons.push("squash-only");
  }
  if (input.finalBytes !== input.expectedFinal) reasons.push("final bytes");
  if (input.checkpoints.git !== tip || input.checkpoints.watermark !== tip || input.checkpoints.column !== tip) {
    reasons.push("final directional checkpoints");
  }
  if (!input.idle) reasons.push("idle repeat");
  if (reasons.length) return failed(reasons.join("; "));
  return passed("every source receipt, ordered intermediate, final bytes, directional checkpoint, and idle repeat matched");
}

export function contentIntegrityOutcome(input: {
  inventory: string[];
  expectedPresent: Array<{ path: string; bytes?: string; sha256?: string }>;
  expectedAbsent: string[];
  observedBytes: Record<string, string | null>;
  observedSha256: Record<string, string | null>;
}): Outcome {
  const inventory = new Set(input.inventory);
  const reasons: string[] = [];
  for (const path of input.expectedAbsent) {
    if (!inventory.has(path)) continue;
    reasons.push(`retained-deleted-path ${path}`);
    if (input.expectedPresent.some((item) => inventory.has(item.path))) reasons.push(`copy-instead-of-rename ${path}`);
  }
  for (const item of input.expectedPresent) {
    if (!inventory.has(item.path)) reasons.push(`missing ${item.path}`);
    if (item.bytes !== undefined && input.observedBytes[item.path] !== item.bytes) reasons.push(`bytes ${item.path}`);
    if (item.sha256 !== undefined && input.observedSha256[item.path] !== item.sha256) reasons.push(`NUL-loss ${item.path}`);
  }
  if (reasons.length) return failed(reasons.join("; "));
  return passed("destination inventory, absence, and bytes or hashes matched");
}

export function cancelOutcome(input: {
  acknowledged: boolean;
  phaseBarrier: boolean;
  finalState: string;
  childCleanup: boolean;
  checkpointPreserved: boolean;
}): Outcome {
  if (input.phaseBarrier && input.finalState === "cancelled" && input.childCleanup && input.checkpointPreserved) {
    return passed("phase barrier, final cancellation, child cleanup, and durable checkpoints were observed");
  }
  const acknowledged = input.acknowledged ? "cancel was acknowledged; " : "";
  return partial(`${acknowledged}partial evidence only: a phase barrier, the original operation's final cancelled state, child cleanup, and durable checkpoint preservation are not established`);
}

export function egressOutcome(input: {
  tool: "present" | "absent";
  context: "application" | "observer" | "unknown";
  exitCode: number | null;
  httpStatus: number | null;
  connects: number | null;
  body: string;
}): Outcome {
  if (input.tool === "absent") {
    return { status: "NOT_RUN", qualification: "UNQUALIFIED", reason: "probe tool unavailable" };
  }
  if (input.context !== "application") {
    return partial("probe did not run in the application context");
  }
  const status = input.httpStatus;
  const body = input.body.trim();
  const responded = status !== null && status !== 0;
  const html = /<html/i.test(input.body);
  const structured = body.startsWith("{") || body.startsWith("[");
  const connected = (input.connects ?? 0) > 0 || responded || html || structured || body.length > 0;
  if (connected) return failed("established external contact");
  if (input.exitCode !== 0) return passed("application context recorded no external connection");
  return failed("egress evidence did not show a rejected connection");
}
