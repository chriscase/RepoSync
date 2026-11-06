import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { currentEngineDocument } from "./engine-invocation.mjs";

const candidate = "d8a86e7dd3ed66dffd2b82d5c81254d8d92cfda9";
const runId = "rs0123abcd";
const buildGraph = "abc123";
const oldPath = "/artifacts/real-engine-scenarios/old/summary.json";
const newPath = "/artifacts/real-engine-scenarios/new/summary.json";
const passing = { scenarios: [{ id: "R01_SVNSERVE_MULTI_COMMIT_GIT_TO_SVN", status: "PASS" }], overall: "PASS" };

test("a preexisting passing summary plus a build failure yields no current PASS rows", () => {
  const result = currentEngineDocument({
    preexisting: [oldPath],
    discovered: [oldPath],
    exitCode: 1,
    candidate,
    runId,
    buildGraph,
    readSummary: () => passing,
  });
  assert.equal(result.adopt, false);
  assert.equal(result.exitCode, 1);
  assert.deepEqual(result.retained, [oldPath]);
  assert.deepEqual(result.document.scenarios, []);
  assert.equal(result.document.candidate, candidate);
  assert.equal(result.document.run_id, runId);
  assert.equal(result.document.build_graph, buildGraph);
  assert.equal(JSON.stringify(result.document).includes('"PASS"'), false);
});

test("a current summary is bound and an older summary is not consumed", () => {
  const result = currentEngineDocument({
    preexisting: [oldPath],
    discovered: [oldPath, newPath],
    exitCode: 0,
    candidate,
    runId,
    buildGraph,
    readSummary: (path) => {
      assert.equal(path, newPath);
      return passing;
    },
  });
  assert.equal(result.adopt, true);
  assert.equal(result.exitCode, 0);
  assert.equal(result.document.candidate, candidate);
  assert.equal(result.document.run_id, runId);
  assert.equal(result.document.build_graph, buildGraph);
  assert.equal(result.document.scenarios[0].status, "PASS");
});

test("identity mismatch and a missing current summary are not adopted", () => {
  const mismatch = currentEngineDocument({
    preexisting: [],
    discovered: [newPath],
    exitCode: 0,
    candidate,
    runId,
    buildGraph,
    readSummary: () => ({ ...passing, candidate: "0".repeat(40), run_id: runId, build_graph: buildGraph }),
  });
  assert.equal(mismatch.adopt, false);
  assert.equal(mismatch.document.scenarios.length, 0);
  const missing = currentEngineDocument({
    preexisting: [oldPath],
    discovered: [oldPath],
    exitCode: 0,
    candidate,
    runId,
    buildGraph,
    readSummary: () => passing,
  });
  assert.equal(missing.adopt, false);
  assert.equal(missing.exitCode, 1);
});

test("the command writes the current failure document and propagates the process exit", () => {
  const root = mkdtempSync(join(tmpdir(), "engine-invocation-"));
  try {
    const before = join(root, "before");
    const after = join(root, "after");
    const out = join(root, "engine-summary.json");
    const exitOut = join(root, "engine-exit.txt");
    writeFileSync(before, `${oldPath}\n`);
    writeFileSync(after, `${oldPath}\n`);
    const command = spawnSync(process.execPath, [
      join(import.meta.dirname, "engine-invocation.mjs"),
      "--before", before,
      "--after", after,
      "--exit", "1",
      "--candidate", candidate,
      "--run-id", runId,
      "--build-graph", buildGraph,
      "--out", out,
      "--exit-out", exitOut,
    ], { encoding: "utf8" });
    assert.equal(command.status, 1);
    const document = JSON.parse(readFileSync(out, "utf8"));
    assert.deepEqual(document.scenarios, []);
    assert.equal(document.candidate, candidate);
    assert.match(readFileSync(exitOut, "utf8"), /engine_exit=1/);
    assert.match(readFileSync(exitOut, "utf8"), /adopted=no/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
