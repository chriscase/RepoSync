import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { buildReport } from "./aggregate.mjs";
import { enforceReport } from "./ci-gate.mjs";
import { accountReport, operationSelected, validateQualificationPlan } from "./report-accounting.mjs";

// These tests process synthetic JSON only. They do not run a browser, a fixture
// executable, the launcher, Docker, SVN, Git, or the production engine.
const labRoot = fileURLToPath(new URL("../", import.meta.url));
const repoRoot = fileURLToPath(new URL("../../../", import.meta.url));
const catalogPath = join(labRoot, "catalog/operations.json");
const matrixPath = join(repoRoot, "docs/reliability/acceptance-matrix.json");
const rulesPath = join(labRoot, "catalog/wiring-rules.json");
const planPath = join(labRoot, "catalog/qualification-plan.json");
const catalog = JSON.parse(await readFile(catalogPath, "utf8"));
const matrix = JSON.parse(await readFile(matrixPath, "utf8"));
const plan = JSON.parse(await readFile(planPath, "utf8"));
const controls = plan.required_case_ids.filter((id) => id.startsWith("HARNESS_"));
const pass = (id, extra = {}) => ({ id, status: "PASS", qualification: "executed", ...extra });
const account = (rows, options = {}) => accountReport({ rows, selectedIds: ["selected"], requiredIds: ["selected", "other"], ...options });

test("the retained plan contains all 519 accepted obligations and the 325 matrix-only ids", async () => {
  validateQualificationPlan(plan);
  const ids = new Set(controls);
  const matrixIds = new Set(matrix.harness_case_ids || []);
  for (const row of matrix.scenarios) {
    matrixIds.add(row.id);
    for (const id of [...(row.required_case_ids || []), ...(row.supporting_case_ids || [])]) matrixIds.add(id);
  }
  const operationIds = new Set(catalog.operations.map((op) => op.id));
  for (const id of [...operationIds, ...matrixIds]) ids.add(id);
  const frozen = JSON.parse(await readFile(join(labRoot, "catalog/operation-catalog.json"), "utf8"));
  for (const op of frozen.operations) ids.add(op.id);
  assert.equal(controls.length, 18);
  assert.equal([...matrixIds].filter((id) => !operationIds.has(id)).length, 325);
  assert.equal(ids.size, 519);
  assert.deepEqual([...ids].sort(), [...plan.required_case_ids].sort());
});

test("all required rows passing is necessary for whole-plan PASS", () => {
  assert.equal(account([pass("selected"), pass("other")]).overall, "PASS");
  const missing = account([pass("selected")]);
  assert.equal(missing.selected_suite_result, "PASS");
  assert.equal(missing.overall, "FAIL");
});

for (const status of ["FAIL", "PARTIAL", "NOT_RUN", "SKIP", "UNKNOWN"]) {
  test(`${status} outside the selected subset blocks whole qualification and remains counted`, () => {
    const result = account([pass("selected"), pass("other", { status })]);
    assert.equal(result.selected_suite_result, "PASS");
    assert.equal(result.overall, "FAIL");
    assert.equal(result.counts[status], 1);
    assert.equal(result.selected_suite_counts.PASS, 1);
  });
  test(`${status} inside the selected subset fails both results`, () => {
    const result = account([pass("selected", { status }), pass("other")]);
    assert.equal(result.selected_suite_result, "FAIL");
    assert.equal(result.overall, "FAIL");
  });
}

test("an UNQUALIFIED PASS annotation cannot qualify a workflow", () => {
  const result = account([pass("selected", { qualification: "UNQUALIFIED" }), pass("other")]);
  assert.equal(result.selected_suite_result, "FAIL");
  assert.equal(result.overall, "FAIL");
});

test("empty selection, absent selected row, duplicate rows and empty required set fail closed", () => {
  const rows = [pass("selected"), pass("other")];
  assert.equal(account(rows, { selectedIds: [] }).selected_suite_result, "FAIL");
  assert.equal(account(rows, { selectedIds: ["missing"] }).selected_suite_result, "FAIL");
  assert.equal(account([...rows, pass("selected")]).overall, "FAIL");
  assert.equal(account(rows, { requiredIds: [] }).overall, "FAIL");
});

test("a control failure blocks both results even when every row is PASS", () => {
  const result = account([pass("selected"), pass("other")], { blocked: true });
  assert.equal(result.overall, "FAIL");
  assert.equal(result.selected_suite_result, "FAIL");
});

test("invalid, empty or duplicate qualification plans are rejected", () => {
  for (const invalid of [null, {}, { ...plan, schema: "unknown" },
    { ...plan, required_case_ids: [] }, { ...plan, required_case_ids: ["id", "id"] },
    { ...plan, required_case_ids: [" "] }, { ...plan, required_case_ids: [42] }]) {
    assert.throws(() => validateQualificationPlan(invalid), /qualification plan/);
  }
});

function playwrightFor(operations, change = {}) {
  return { suites: [{ specs: operations.map((op) => ({
    title: op.id,
    tests: [{
      annotations: [{ type: "labCase", description: op.id },
        ...(change.id === op.id && change.annotation ? [{ type: "labStatus", description: change.annotation }] : [])],
      results: [{ status: change.id === op.id ? change.actual || "passed" : "passed", retry: 0 }],
    }],
  })) }] };
}

async function syntheticReport(suite, options = {}) {
  const scratch = await mkdtemp(join(tmpdir(), "rs-accounting-"));
  try {
    const selected = catalog.operations.filter((op) => op.disposition === "execute" && operationSelected(suite, op));
    const playwright = options.playwright || playwrightFor(selected, options.change);
    const engine = catalog.operations.filter((op) => op.disposition === "engine").map((op) => pass(op.id));
    const selfTest = controls.filter((id) => id !== options.missingControl).map((id) => pass(id, options.failedControl === id ? { status: "FAIL" } : {}));
    const paths = { playwrightPath: join(scratch, "playwright.json"), selfTestPath: join(scratch, "self-test.json"), markdownPath: join(scratch, "report.md") };
    await writeFile(paths.playwrightPath, options.malformed ? "{broken" : JSON.stringify(playwright));
    await writeFile(paths.selfTestPath, JSON.stringify({ cases: selfTest }));
    if (suite === "full") {
      paths.enginePath = join(scratch, "engine.json");
      await writeFile(paths.enginePath, JSON.stringify({ scenarios: engine }));
    }
    let inputCatalog = catalogPath;
    if (options.removeOperation) {
      inputCatalog = join(scratch, "operations.json");
      await writeFile(inputCatalog, JSON.stringify({ ...catalog, operations: catalog.operations.filter((op) => op.id !== options.removeOperation) }));
    }
    const report = await buildReport({ catalogPath: inputCatalog, matrixPath, rulesPath, ...paths, suite,
      candidate: "a".repeat(40), seed: 1, runId: "syntheticaccounting1" });
    return { report, markdown: await readFile(paths.markdownPath, "utf8") };
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

test("counterfactual full executor success is 180 PASS / 339 NOT_RUN, never full PASS", async () => {
  const { report, markdown } = await syntheticReport("full");
  assert.equal(report.schema, "reposync.acceptance-lab.report.v1");
  assert.equal(report.rows.length, 519);
  assert.deepEqual(report.counts, { PASS: 180, FAIL: 0, PARTIAL: 0, NOT_RUN: 339, SKIP: 0 });
  assert.deepEqual(report.selected_suite_counts, { PASS: 162, FAIL: 0, PARTIAL: 0, NOT_RUN: 0, SKIP: 0 });
  assert.equal(report.selected_suite_result, "PASS");
  assert.equal(report.suite_result, "FAIL");
  assert.equal(report.overall, "FAIL");
  assert.equal(report.full_qualification, "FAIL");
  assert.equal(report.discovered_results, 159); // 141 browser annotations + 18 controls, not engine receipts.
  assert.deepEqual(report.gate_failed, []);
  assert.equal(report.qualification_plan.required_count, 519);
  assert.match(report.qualification_plan.sha256, /^[a-f0-9]{64}$/);
  assert.equal(report.rows.filter((row) => row.source === "acceptance-matrix").length, 325);
  const renderedIds = markdown.split("\n").filter((line) => line.startsWith("| ") && !line.startsWith("| ID ") && !line.startsWith("| --- "))
    .map((line) => line.split(" | ")[0].slice(2));
  assert.deepEqual(renderedIds, report.rows.map((row) => row.id));
  assert.ok(report.r_coverage.R01.related.length > 0);
  assert.match(markdown, /Whole-plan overall: FAIL/);
  assert.match(markdown, /SKIP 0/);
});

for (const [suite, selectedCount] of [["smoke", 16], ["soak", 2]]) {
  test(`${suite} keeps its successful legacy gate while whole-plan qualification fails`, async () => {
    const { report } = await syntheticReport(suite);
    assert.equal(report.suite_result, "PASS");
    assert.equal(report.selected_suite_result, "PASS");
    assert.equal(report.selected_suite_counts.PASS, selectedCount);
    assert.equal(report.counts.PASS, selectedCount + 18);
    assert.equal(report.rows.length, 519);
    assert.equal(report.overall, "FAIL");
    assert.equal(report.full_qualification, "NOT_RUN");
  });
}

test("existing CI enforcement still accepts smoke and rejects incomplete full reports", async () => {
  const scratch = await mkdtemp(join(tmpdir(), "rs-accounting-gate-"));
  const keys = ["LAB_EVENT_NAME", "LAB_SUITE_INPUT", "LAB_SEED_INPUT"];
  const saved = Object.fromEntries(keys.map((key) => [key, process.env[key]]));
  try {
    const evidence = join(scratch, "evidence");
    const engine = join(scratch, "engine");
    const reportDir = join(scratch, "report");
    for (const path of [evidence, engine, reportDir]) await mkdir(path);
    await writeFile(join(evidence, "playwright.json"), "{}");
    await writeFile(join(evidence, "self-test.json"), "{}");
    await writeFile(join(engine, "summary.json"), "{}");
    process.env.LAB_EVENT_NAME = "workflow_dispatch";
    process.env.LAB_SEED_INPUT = "1";
    for (const suite of ["smoke", "full"]) {
      const { report } = await syntheticReport(suite);
      process.env.LAB_SUITE_INPUT = suite;
      await writeFile(join(evidence, "identity.json"), JSON.stringify({ candidate_sha: report.candidate, suite, seed: 1, run_id: report.run_id }));
      await writeFile(join(reportDir, "report.json"), JSON.stringify(report));
      const enforced = enforceReport({ evidence, engine, reportDir });
      assert.deepEqual(enforced.reasons, []);
      assert.equal(enforced.ok, suite === "smoke");
    }
  } finally {
    for (const key of keys) {
      if (saved[key] === undefined) delete process.env[key];
      else process.env[key] = saved[key];
    }
    await rm(scratch, { recursive: true, force: true });
  }
});

test("deleting an unavailable operation cannot delete its accepted obligation", async () => {
  const { report, markdown } = await syntheticReport("full", { removeOperation: "LIVE_ENTERPRISE" });
  const retained = report.rows.find((row) => row.id === "LIVE_ENTERPRISE");
  assert.equal(report.rows.length, 519);
  assert.equal(retained.status, "NOT_RUN");
  assert.equal(retained.source, "qualification-plan");
  assert.match(markdown, /\| LIVE_ENTERPRISE \| NOT_RUN \| qualification-plan/);
  assert.equal(report.full_qualification, "FAIL");
});

for (const [name, options, predicate] of [
  ["gate failure", { failedControl: controls[0] }, (r) => r.gate_failed.includes(controls[0])],
  ["missing gate", { missingControl: controls[0] }, (r) => r.gate_failed.includes(controls[0]) && r.rows.length === 519],
  ["omission and zero discovery", { playwright: { suites: [] } }, (r) => r.zero_discovery && r.omitted.length === 16],
  ["unexpected skip", { change: { id: "LAB_SMOKE_STACK", actual: "skipped" } }, (r) => r.unexpected_skips.length === 1],
  ["runner error", { playwright: { ...playwrightFor(catalog.operations.filter((op) => op.suite === "smoke")), errors: [{ message: "runner failed" }] } }, (r) => r.rows.some((row) => row.source === "playwright" && row.status === "FAIL")],
  ["malformed Playwright JSON", { malformed: true }, (r) => r.playwright_parse_error.length > 0],
  ["explicit NOT_RUN", { change: { id: "LAB_SMOKE_STACK", actual: "skipped", annotation: "NOT_RUN" } }, (r) => r.unexpected_skips.length === 0 && r.selected_suite_counts.NOT_RUN === 1],
  ["partial annotation", { change: { id: "LAB_SMOKE_STACK", annotation: "PARTIAL" } }, (r) => r.selected_suite_counts.PARTIAL === 1],
]) {
  test(`${name} still blocks legacy and selected-suite gates`, async () => {
    const { report } = await syntheticReport("smoke", options);
    assert.equal(report.suite_result, "FAIL");
    assert.equal(report.selected_suite_result, "FAIL");
    assert.equal(report.overall, "FAIL");
    assert.ok(predicate(report));
  });
}
