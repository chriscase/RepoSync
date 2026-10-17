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
  assert.equal(account(rows, { selectedIds: [] }).overall, "FAIL");
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
    { ...plan, required_case_ids: [" "] }, { ...plan, required_case_ids: [42] },
    { ...plan, r_criteria_by_case: undefined }, { ...plan, r_criteria_by_case: {} },
    { ...plan, r_criteria_by_case: { ...plan.r_criteria_by_case, LIVE_ENTERPRISE: [42] } }]) {
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
    const input = options.catalog || catalog;
    const selected = input.operations.filter((op) => op?.disposition === "execute" && operationSelected(suite, op));
    const playwright = options.playwright || playwrightFor(selected, options.change);
    const engine = options.engineRows || input.operations.filter((op) => op?.disposition === "engine").map((op) => pass(op.id));
    const selfTest = options.selfTestRows || controls.filter((id) => id !== options.missingControl).map((id) => pass(id, options.failedControl === id ? { status: "FAIL" } : {}));
    const paths = { playwrightPath: join(scratch, "playwright.json"), selfTestPath: join(scratch, "self-test.json"), markdownPath: join(scratch, "report.md") };
    await writeFile(paths.playwrightPath, options.malformed ? "{broken" : JSON.stringify(playwright));
    await writeFile(paths.selfTestPath, JSON.stringify(Object.hasOwn(options, "selfTestDocument") ? options.selfTestDocument : { cases: selfTest }));
    if (suite === "full" || options.engineRows || Object.hasOwn(options, "engineDocument")) {
      paths.enginePath = join(scratch, "engine.json");
      await writeFile(paths.enginePath, JSON.stringify(Object.hasOwn(options, "engineDocument") ? options.engineDocument : { scenarios: engine }));
    }
    let inputCatalog = catalogPath;
    if (options.removeOperation || options.catalog) {
      inputCatalog = join(scratch, "operations.json");
      await writeFile(inputCatalog, JSON.stringify({ ...input, operations: options.removeOperation ? input.operations.filter((op) => op?.id !== options.removeOperation) : input.operations }));
    }
    let inputMatrix = matrixPath;
    if (options.matrix) {
      inputMatrix = join(scratch, "matrix.json");
      await writeFile(inputMatrix, JSON.stringify(options.matrix));
    }
    if (options.qualificationPlan) {
      paths.qualificationPlanPath = join(scratch, "qualification-plan.json");
      await writeFile(paths.qualificationPlanPath, JSON.stringify(options.qualificationPlan));
    }
    if (options.auditDocuments) {
      paths.auditDir = join(scratch, "audit");
      await mkdir(paths.auditDir);
      for (const [name, data] of Object.entries(options.auditDocuments)) await writeFile(join(paths.auditDir, name), JSON.stringify(data));
    } else if (options.auditDir) paths.auditDir = options.auditDir;
    const report = await buildReport({ catalogPath: inputCatalog, matrixPath: inputMatrix, rulesPath, ...paths, suite,
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
  const baseline = await syntheticReport("full");
  const { report, markdown } = await syntheticReport("full", { removeOperation: "LIVE_ENTERPRISE" });
  const retained = report.rows.find((row) => row.id === "LIVE_ENTERPRISE");
  assert.equal(report.rows.length, 519);
  assert.equal(retained.status, "NOT_RUN");
  assert.equal(retained.source, "qualification-plan");
  assert.deepEqual(retained.r_criteria, ["R21"]);
  assert.deepEqual(report.r_coverage.R21, baseline.report.r_coverage.R21);
  assert.match(markdown, /\| LIVE_ENTERPRISE \| NOT_RUN \| qualification-plan/);
  assert.equal(report.full_qualification, "FAIL");
});

// A minimal complete plan makes an ignored extra/invalid input observable as a
// false PASS, rather than letting the production plan's339NOT_RUN hide it.
const minimalOperations = [
  { id: "BROWSER_CASE", suite: "smoke", disposition: "execute", r_criteria: ["R01"] },
  { id: "ENGINE_CASE", suite: "full", disposition: "engine", r_criteria: ["R01"] },
];
const minimalIds = [...controls, ...minimalOperations.map((op) => op.id)];
const minimalOptions = {
  catalog: { operations: minimalOperations }, matrix: { scenarios: [], harness_case_ids: [] },
  qualificationPlan: { ...plan, required_case_ids: minimalIds,
    r_criteria_by_case: Object.fromEntries(minimalIds.map((id) => [id, id.startsWith("HARNESS_") ? [] : ["R01"]])) },
};

test("the minimal buildReport baseline is fully passing before raw-input mutations", async () => {
  const { report } = await syntheticReport("full", minimalOptions);
  assert.equal(report.overall, "PASS");
  assert.equal(report.suite_result, "PASS");
  assert.deepEqual(report.input_validation_errors, []);
});

for (const source of ["playwright", "self-test", "real-engine-suite"]) {
  for (const reverse of [false, true]) {
    test(`buildReport rejects ${source} duplicate PASS/UNKNOWN in ${reverse ? "reverse" : "forward"} order`, async () => {
      const options = { ...minimalOptions };
      let id;
      if (source === "playwright") {
        id = "BROWSER_CASE";
        const data = playwrightFor([minimalOperations[0], minimalOperations[0]]);
        data.suites[0].specs[reverse ? 0 : 1].tests[0].annotations.push({ type: "labStatus", description: "UNKNOWN" });
        options.playwright = data;
      } else if (source === "self-test") {
        id = controls[0];
        options.selfTestRows = controls.slice(1).map((control) => pass(control));
        const duplicates = [pass(id), pass(id, { status: "UNKNOWN" })];
        options.selfTestRows.push(...(reverse ? duplicates.reverse() : duplicates));
      } else {
        id = "ENGINE_CASE";
        const duplicates = [pass(id), pass(id, { status: "UNKNOWN" })];
        options.engineRows = reverse ? duplicates.reverse() : duplicates;
      }
      const { report, markdown } = await syntheticReport("full", options);
      assert.equal(report.rows.find((row) => row.id === id).status, "FAIL");
      assert.equal(report.overall, "FAIL");
      assert.equal(report.suite_result, "FAIL");
      assert.equal(report.selected_suite_result, "FAIL");
      assert.ok(report.input_validation_errors.some((error) => error.id === id && error.code === "duplicate_identity"));
      assert.ok(report.input_validation_errors.some((error) => error.id === id && error.code === "invalid_status"));
      assert.match(markdown, /Input validation errors: [1-9]/);
    });
  }
  test(`buildReport rejects a single invalid ${source} status before canonicalization`, async () => {
    const options = { ...minimalOptions };
    if (source === "playwright") options.playwright = playwrightFor([minimalOperations[0]], { id: "BROWSER_CASE", annotation: "UNKNOWN" });
    else if (source === "self-test") options.selfTestRows = controls.map((id) => pass(id, id === controls[0] ? { status: "UNKNOWN" } : {}));
    else options.engineRows = [pass("ENGINE_CASE", { status: "UNKNOWN" })];
    const { report } = await syntheticReport("full", options);
    assert.equal(report.overall, "FAIL");
    assert.equal(report.suite_result, "FAIL");
    assert.ok(report.input_validation_errors.some((error) => error.source === source && error.code === "invalid_status"));
  });
}

for (const status of ["FAIL", "PARTIAL", "PASS"]) {
  test(`buildReport retains an uncatalogued engine ${status} in rows, counts and Markdown`, async () => {
    const { report, markdown } = await syntheticReport("full", {
      ...minimalOptions, engineRows: [pass("ENGINE_CASE"), pass("UNMAPPED_ENGINE_CASE", { status })],
    });
    const found = report.rows.find((row) => row.id === "UNMAPPED_ENGINE_CASE");
    assert.equal(found.status, status);
    assert.equal(found.source, "real-engine-suite");
    assert.equal(found.supports_workflow, false);
    assert.equal(report.rows.length, minimalIds.length + 1);
    assert.equal(report.counts[status], status === "PASS" ? minimalIds.length + 1 : 1);
    assert.equal(report.selected_suite_result, "PASS");
    assert.equal(report.overall, "FAIL");
    assert.equal(report.full_qualification, "FAIL");
    if (status === "PASS") assert.equal(found.qualification, "UNQUALIFIED");
    assert.ok(markdown.includes(`| UNMAPPED_ENGINE_CASE | ${status} | real-engine-suite |`));
  });
}

test("duplicate PASS observations fail buildReport even without an invalid status", async () => {
  for (const options of [
    { ...minimalOptions, playwright: playwrightFor([minimalOperations[0], minimalOperations[0]]) },
    { ...minimalOptions, selfTestRows: [...controls.map((id) => pass(id)), pass(controls[0])] },
    { ...minimalOptions, engineRows: [pass("ENGINE_CASE"), pass("ENGINE_CASE")] },
  ]) {
    const { report } = await syntheticReport("full", options);
    assert.equal(report.suite_result, "FAIL");
    assert.ok(report.input_validation_errors.some((error) => error.code === "duplicate_identity"));
    assert.ok(!report.input_validation_errors.some((error) => error.code === "invalid_status"));
  }
});

test("removing a matrix criterion and its subcases preserves their retained criterion coverage", async () => {
  const baseline = await syntheticReport("full");
  const { report } = await syntheticReport("full", {
    matrix: { ...matrix, scenarios: matrix.scenarios.filter((row) => row.id !== "R21") },
  });
  assert.equal(report.rows.length, 519);
  assert.deepEqual(report.r_coverage.R21, baseline.report.r_coverage.R21);
});

for (const actual of ["failed", "passed", "skipped", "timedOut", "interrupted"]) {
  test(`an extra unannotated Playwright ${actual} test blocks an otherwise passing buildReport`, async () => {
    const data = playwrightFor([minimalOperations[0]]);
    data.suites[0].specs.push({ title: "unannotated extra", tests: [{ annotations: [], results: [{ status: actual, retry: 0 }] }] });
    const { report, markdown } = await syntheticReport("full", { ...minimalOptions, playwright: data });
    assert.equal(report.suite_result, "FAIL");
    assert.equal(report.selected_suite_result, "FAIL");
    assert.equal(report.counts.FAIL, 1);
    assert.equal(report.discovered_results, 19);
    assert.ok(report.input_validation_errors.some((error) => error.code === "missing_labCase_annotation"));
    assert.ok(report.attempts.some((attempt) => attempt.ids.length === 0 && attempt.final_status === actual));
    assert.match(markdown, /\| INPUT_INVALID_ID_\d+ \| FAIL \| playwright/);
  });
}

test("duplicate catalog operation ids fail despite only one passing execution", async () => {
  const { report } = await syntheticReport("full", {
    ...minimalOptions,
    catalog: { operations: [...minimalOperations, { ...minimalOperations[0] }] },
    playwright: playwrightFor([minimalOperations[0]]),
  });
  assert.equal(report.discovered_results, 19);
  assert.equal(report.suite_result, "FAIL");
  assert.ok(report.input_validation_errors.some((error) => error.source === "operation-catalog" && error.id === "BROWSER_CASE" && error.code === "duplicate_identity"));
});

for (const operation of [
  { ...minimalOperations[0], id: "" },
  { ...minimalOperations[0], id: 42 },
  { ...minimalOperations[0], suite: "unknown" },
  { ...minimalOperations[0], disposition: "unknown" },
]) {
  test(`malformed catalog declaration ${JSON.stringify(operation)} cannot join a passing report`, async () => {
    const { report } = await syntheticReport("full", {
      ...minimalOptions, catalog: { operations: [...minimalOperations, operation] },
      playwright: playwrightFor([minimalOperations[0]]),
    });
    assert.equal(report.suite_result, "FAIL");
    assert.ok(report.input_validation_errors.some((error) => error.source === "operation-catalog"));
  });
}

for (const [name, mutate, code] of [
  ["conflicting PASS/UNKNOWN annotations", (d) => d.suites[0].specs[0].tests[0].annotations.push({ type: "labStatus", description: "PASS" }, { type: "labStatus", description: "UNKNOWN" }), "invalid_status"],
  ["empty status annotation", (d) => d.suites[0].specs[0].tests[0].annotations.push({ type: "labStatus", description: "" }), "invalid_status"],
  ["failed first result then unnumbered PASS", (d) => d.suites[0].specs[0].tests[0].results.unshift({ status: "failed", retry: 0 }), "multiple_attempts"],
  ["multiple case identities", (d) => d.suites[0].specs[0].tests[0].annotations.push({ type: "labCase", description: "EXTRA_CASE" }), "ambiguous_labCase_annotation"],
  ["declaration without tests", (d) => d.suites[0].specs.push({ title: "empty test declaration", tests: [] }), "missing_tests"],
  ["malformed errors collection", (d) => { d.errors = "runner failure"; }, "invalid_collection"],
  ["malformed annotation", (d) => d.suites[0].specs[0].tests[0].annotations.push(null), "invalid_annotation"],
  ["reported runner failure", (d) => { d.stats = { unexpected: 1, flaky: 0 }; }, "reported_runner_failure"],
  ["reported spec failure", (d) => { d.suites[0].specs[0].ok = false; }, "reported_spec_failure"],
  ["reported unexpected test outcome", (d) => { d.suites[0].specs[0].tests[0].status = "unexpected"; }, "reported_test_failure"],
  ["unexpected positive result", (d) => { d.suites[0].specs[0].tests[0].expectedStatus = "failed"; }, "unexpected_pass"],
  ["reported result error", (d) => { d.suites[0].specs[0].tests[0].results[0].error = { message: "hidden failure" }; }, "reported_result_error"],
  ["conflicting result annotation", (d) => { d.suites[0].specs[0].tests[0].results[0].annotations = [{ type: "labStatus", description: "FAIL" }]; }, "inconsistent_result_annotation"],
  ["malformed result annotation", (d) => { d.suites[0].specs[0].tests[0].results[0].annotations = [{}]; }, "invalid_annotation"],
  ["wrong-source harness identity", (d) => d.suites[0].specs.push(...playwrightFor([{ id: controls[0] }]).suites[0].specs), "unexpected_execution_identity"],
]) {
  test(`adjacent Playwright ${name} cannot be silently dropped`, async () => {
    const data = playwrightFor([minimalOperations[0]]);
    mutate(data);
    const { report } = await syntheticReport("full", { ...minimalOptions, playwright: data });
    assert.equal(report.suite_result, "FAIL");
    assert.ok(report.input_validation_errors.some((error) => error.code === code), JSON.stringify(report.input_validation_errors));
  });
}

for (const [name, options, code] of [
  ["engine alias hides a failure", { engineDocument: { scenarios: [pass("ENGINE_CASE")], results: [pass("HIDDEN_ENGINE_FAIL", { status: "FAIL" })] } }, "ambiguous_result_collections"],
  ["engine malformed primary alias", { engineDocument: { scenarios: null, results: [pass("ENGINE_CASE")] } }, "invalid_collection"],
  ["engine failed overall with passing rows", { engineDocument: { scenarios: [pass("ENGINE_CASE")], overall: "FAIL" } }, "reported_overall_not_pass"],
  ["engine inconsistent counts", { engineDocument: { scenarios: [pass("ENGINE_CASE")], counts: { PASS: 1, FAIL: 1 } } }, "reported_counts_mismatch"],
  ["engine unavailable toolchain", { engineDocument: { scenarios: [pass("ENGINE_CASE")], toolchain: { status: "NOT RUN" } } }, "reported_toolchain_not_pass"],
  ["self-test malformed document", { selfTestDocument: [] }, "invalid_document"],
  ["self-test contradictory overall", { selfTestDocument: { cases: controls.map((id) => pass(id)), overall: "FAIL" } }, "reported_overall_not_pass"],
  ["unknown self-test identity", { selfTestRows: [...controls.map((id) => pass(id)), pass("UNDECLARED_CONTROL")] }, "unexpected_execution_identity"],
  ["negative PASS qualification", { selfTestRows: controls.map((id) => pass(id, id === controls[0] ? { qualification: "PARTIAL" } : {})) }, "invalid_qualification"],
]) {
  test(`adjacent ${name} blocks full qualification`, async () => {
    const { report } = await syntheticReport("full", { ...minimalOptions, ...options });
    assert.equal(report.suite_result, "FAIL");
    assert.ok(report.input_validation_errors.some((error) => error.code === code), JSON.stringify(report.input_validation_errors));
    if (name === "engine alias hides a failure") assert.ok(report.rows.some((row) => row.id === "HIDDEN_ENGINE_FAIL" && row.status === "FAIL"));
  });
}

test("valid legacy engine alias and summary counts retain the passing baseline", async () => {
  const { report } = await syntheticReport("full", { ...minimalOptions,
    engineDocument: { results: [pass("ENGINE_CASE")], overall: "PASS", toolchain: { status: "PASS" }, counts: { PASS: 1, FAIL: 0, PARTIAL: 0, "NOT RUN": 0, SKIP: 0 } },
  });
  assert.equal(report.suite_result, "PASS");
  assert.deepEqual(report.input_validation_errors, []);
});

test("valid Playwright outcome fields and mirrored result annotations retain PASS", async () => {
  const data = playwrightFor([minimalOperations[0]]);
  const spec = data.suites[0].specs[0];
  spec.ok = true;
  const observed = spec.tests[0];
  observed.status = "expected";
  observed.expectedStatus = "passed";
  observed.results[0].errors = [];
  observed.results[0].annotations = [...observed.annotations];
  data.errors = [];
  data.stats = { unexpected: 0, flaky: 0 };
  const { report } = await syntheticReport("full", { ...minimalOptions, playwright: data });
  assert.equal(report.suite_result, "PASS");
  assert.deepEqual(report.input_validation_errors, []);
});

test("matrix declaration duplicates block while legitimate cross-criterion references remain allowed", async () => {
  const { report } = await syntheticReport("full", { ...minimalOptions,
    matrix: { harness_case_ids: [], scenarios: [{ id: "BROWSER_CASE" }, { id: "BROWSER_CASE" }] },
  });
  assert.equal(report.suite_result, "FAIL");
  assert.ok(report.input_validation_errors.some((error) => error.source === "acceptance-matrix:scenarios" && error.code === "duplicate_identity"));
  const valid = await syntheticReport("full", { auditDir: join(labRoot, "catalog") });
  assert.deepEqual(valid.report.input_validation_errors, []);
  assert.equal(valid.report.rows.length, 519);
});

test("audit declaration duplicates and ambiguous aliases cannot disappear before the join", async () => {
  for (const document of [
    { operations: [{ id: "BROWSER_CASE" }, { id: "BROWSER_CASE" }] },
    { operations: [{ id: "BROWSER_CASE" }], cases: [{ id: "EXTRA_AUDIT_CASE" }] },
  ]) {
    const { report } = await syntheticReport("full", { ...minimalOptions, auditDocuments: { "audit.json": document } });
    assert.equal(report.suite_result, "FAIL");
    assert.ok(report.input_validation_errors.some((error) => error.source.startsWith("audit:") && ["duplicate_identity", "ambiguous_catalog_collections"].includes(error.code)));
  }
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
