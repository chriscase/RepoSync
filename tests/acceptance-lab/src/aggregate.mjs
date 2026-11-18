import { createHash } from "node:crypto";
import { readFile, writeFile, readdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { assessBrowser, assessServices, compareTrees, directoryUsable, duplicateMappings, missingMappings } from "./oracle.mjs";
import { accountReport, collectExecutionRows, operationSelected, validateCatalogOperations, validateExecutionInputs, validateIdentityList, validateQualificationPlan } from "./report-accounting.mjs";

const PASS = "PASS";
const FAIL = "FAIL";
const PARTIAL = "PARTIAL";
const NOT_RUN = "NOT_RUN";

export function walkPlaywright(report) {
  const cases = [];
  const skipped = [];
  const attempts = [];
  const validationErrors = [];
  const issue = (code, detail = {}) => validationErrors.push({ source: "playwright", code, ...detail });
  const list = (value, field, required = false) => {
    if (value === undefined && !required) return [];
    if (Array.isArray(value)) return value;
    issue("invalid_collection", { field });
    return [];
  };
  function visitSuite(suite) {
    if (!suite || typeof suite !== "object" || Array.isArray(suite)) { issue("invalid_suite"); return; }
    for (const spec of list(suite.specs, "suite.specs")) {
      if (!spec || typeof spec !== "object" || Array.isArray(spec)) { issue("invalid_spec"); continue; }
      const tests = list(spec.tests, "spec.tests", true);
      if (!tests.length) issue("missing_tests", { title: spec.title });
      for (const test of tests) {
        if (!test || typeof test !== "object" || Array.isArray(test)) { issue("invalid_test", { title: spec.title }); continue; }
        const annotations = [...list(spec.annotations, "spec.annotations"), ...list(test.annotations, "test.annotations")]
          .filter((annotation) => {
            if (annotation && typeof annotation === "object" && typeof annotation.type === "string") return true;
            issue("invalid_annotation", { title: spec.title }); return false;
          });
        const ids = annotations.filter((item) => item.type === "labCase").map((item) => item.description);
        const before = validationErrors.length;
        const detail = { title: spec.title, id: ids[0] };
        if (spec.ok !== undefined && spec.ok !== true) issue("reported_spec_failure", detail);
        if (test.status !== undefined && !["expected", "skipped"].includes(test.status)) issue("reported_test_failure", detail);
        if (ids.length !== 1) issue(ids.length ? "ambiguous_labCase_annotation" : "missing_labCase_annotation", detail);
        const statusAnnotations = annotations.filter((item) => item.type === "labStatus");
        for (const item of statusAnnotations) {
          if (![PASS, FAIL, PARTIAL, NOT_RUN, "SKIP"].includes(item.description)) issue("invalid_status", { ...detail, reported_status: item.description });
        }
        if (new Set(statusAnnotations.map((item) => item.description)).size > 1) issue("ambiguous_status_annotation", detail);
        const qualifications = annotations.filter((item) => item.type === "labQualification").map((item) => item.description);
        if (qualifications.some((value) => typeof value !== "string") || new Set(qualifications).size > 1) issue("ambiguous_qualification_annotation", detail);
        const statusAnn = annotations.find((item) => item.type === "labStatus")?.description;
        const reason = annotations.find((item) => item.type === "labReason")?.description || "";
        const qualification = annotations.find((item) => item.type === "labQualification")?.description || "";
        const results = list(test.results, "test.results", true);
        if (!results.length) issue("missing_results", detail);
        for (const result of results) {
          if (!result || !["passed", "failed", "skipped", "timedOut", "interrupted"].includes(result.status)) issue("invalid_result_status", detail);
          if (result?.retry !== undefined && (!Number.isSafeInteger(result.retry) || result.retry < 0)) issue("invalid_retry", detail);
          if (result?.error !== undefined && result.error !== null) issue("reported_result_error", detail);
          if (list(result?.errors, "result.errors").length) issue("reported_result_error", detail);
          for (const annotation of list(result?.annotations, "result.annotations")) {
            if (!annotation || typeof annotation !== "object" || typeof annotation.type !== "string") { issue("invalid_annotation", detail); continue; }
            if (["labCase", "labStatus", "labQualification"].includes(annotation.type)
                && !annotations.some((item) => item.type === annotation.type && item.description === annotation.description)) {
              issue("inconsistent_result_annotation", detail);
            }
          }
        }
        if (results.length > 1) issue("multiple_attempts", detail);
        const retries = Math.max(results.length - 1, results.filter((result) => result?.retry > 0).length);
        const final = results[results.length - 1];
        const first = results[0];
        attempts.push({
          title: spec.title,
          ids,
          first_status: first?.status || "missing",
          final_status: final?.status || "missing",
          retries,
          flaky: retries > 0 && final?.status === "passed",
        });
        if (!ids.length) {
          skipped.push({ title: spec.title, reason: "missing_labCase_annotation" });
        }
        const actual = final?.status || "missing";
        if (actual === "passed" && test.expectedStatus !== undefined && test.expectedStatus !== "passed") issue("unexpected_pass", detail);
        if (test.status === "skipped" && actual !== "skipped") issue("inconsistent_test_status", detail);
        const blocksPass = actual === "failed" || actual === "timedOut" || actual === "interrupted" || actual === "missing";
        let status = PASS;
        if (blocksPass) status = FAIL;
        else if (actual === "skipped") status = statusAnn === NOT_RUN ? NOT_RUN : FAIL;
        else if (actual !== "passed") status = FAIL;
        else if (statusAnn && statusAnn !== PASS) status = statusAnn;
        if (validationErrors.length > before || (retries > 0 && status === PASS)) status = FAIL;
        if (Array.isArray(report?.errors) && report.errors.length > 0 && status === PASS) status = FAIL;
        for (const id of ids.length ? ids : [undefined]) {
          cases.push({
            id,
            status,
            reason: reason || (status === FAIL ? first?.error?.message || final?.error?.message || "assertion failed" : ""),
            qualification: qualification || (status === PASS ? "executed" : status),
            source: "playwright",
            title: spec.title,
            retries,
            flaky: retries > 0 && final?.status === "passed",
            first_status: first?.status || "missing",
          });
        }
        if (final?.status === "skipped" && statusAnn !== NOT_RUN) {
          skipped.push({ title: spec.title, ids, reason: "unexpected_skip" });
        }
      }
    }
    for (const child of list(suite.suites, "suite.suites")) visitSuite(child);
  }
  if (!report || typeof report !== "object" || Array.isArray(report)) issue("invalid_document");
  for (const suite of list(report?.suites, "report.suites", true)) visitSuite(suite);
  list(report?.errors, "report.errors");
  if (report?.stats !== undefined) {
    if (!report.stats || typeof report.stats !== "object" || Array.isArray(report.stats)) issue("invalid_stats");
    else for (const key of ["unexpected", "flaky"]) {
      if (report.stats[key] !== undefined && (!Number.isSafeInteger(report.stats[key]) || report.stats[key] !== 0)) issue("reported_runner_failure", { field: key });
    }
  }
  return {
    cases,
    skipped,
    attempts,
    discovered: cases.filter((row) => typeof row.id === "string" && row.id && row.id.trim() === row.id).length,
    runner_error: Array.isArray(report?.errors) && report.errors.length > 0,
    validation_errors: validationErrors,
  };
}

function matrixIds(matrix, errors) {
  const checked = (values, source, getId) => {
    const result = validateIdentityList(values, source, getId);
    errors.push(...result.errors);
    return result.values;
  };
  const ids = new Set(checked(matrix?.harness_case_ids === undefined ? [] : matrix.harness_case_ids, "acceptance-matrix:harness", (id) => id));
  for (const scenario of checked(matrix?.scenarios, "acceptance-matrix:scenarios")) {
    ids.add(scenario.id);
    const required = checked(scenario.required_case_ids === undefined ? [] : scenario.required_case_ids, `acceptance-matrix:${scenario.id}:required`, (id) => id);
    const supporting = checked(scenario.supporting_case_ids === undefined ? [] : scenario.supporting_case_ids, `acceptance-matrix:${scenario.id}:supporting`, (id) => id);
    for (const id of checked([...required, ...supporting], `acceptance-matrix:${scenario.id}:all-cases`, (id) => id)) ids.add(id);
  }
  return [...ids];
}

function reasonFor(id, rules) {
  for (const rule of rules.reasons || []) {
    if (new RegExp(rule.match).test(id)) return rule.reason;
  }
  return "No acceptance-lab executor is mapped to this id. It is recorded and is not a PASS.";
}

function engineIndex(rows) {
  const map = new Map();
  for (const row of rows) {
    if (row && row.id) map.set(row.id, row);
  }
  return map;
}

async function loadAudit(dir) {
  if (!dir) return { status: "NOT_AVAILABLE", operations: [], files: [], errors: [] };
  let names = [];
  try {
    names = await readdir(dir);
  } catch {
    return { status: "NOT_AVAILABLE", operations: [], files: [], errors: [] };
  }
  const files = names.filter((name) => name.endsWith(".json"));
  const operations = [];
  const errors = [];
  let recognized = false;
  for (const name of files) {
    let parsed;
    try {
      parsed = JSON.parse(await readFile(join(dir, name), "utf8"));
    } catch {
      errors.push({ source: `audit:${name}`, code: "invalid_json" });
      continue;
    }
    const collections = Array.isArray(parsed) ? [{ field: "array", rows: parsed }]
      : ["operations", "cases", "catalog"].filter((field) => parsed && Object.hasOwn(parsed, field))
        .map((field) => ({ field, rows: parsed[field] }));
    if (!collections.length) continue;
    if (collections.length > 1) errors.push({ source: `audit:${name}`, code: "ambiguous_catalog_collections" });
    recognized = true;
    for (const { field, rows } of collections) {
      const checked = validateIdentityList(rows, `audit:${name}:${field}`, (row) => typeof row === "string" ? row : row?.id);
      errors.push(...checked.errors);
      for (const row of checked.values) {
        const id = typeof row === "string" ? row : row.id;
        operations.push({
          id,
          title: row.title || row.operation || id,
          r_criteria: row.r_criteria || row.r_ids || row.r || [],
          contract: row.product_contract || row.contract || "",
        });
      }
    }
  }
  if (!files.length) return { status: "NOT_AVAILABLE", operations: [], files, errors };
  if (!recognized) {
    return {
      status: "UNRECOGNIZED",
      operations: [],
      files,
      errors,
      gap: "Audit report files were present but none matched an operation catalog schema. They were not treated as the frozen catalog.",
    };
  }
  return { status: "INGESTED", operations, files, errors };
}

async function readGeneratedJson(path) {
  const text = await readFile(path, "utf8");
  try {
    return { value: JSON.parse(text), error: "" };
  } catch (error) {
    return { value: null, error: error instanceof Error ? error.message : String(error) };
  }
}

export async function buildReport(options) {
  const catalogDocument = JSON.parse(await readFile(options.catalogPath, "utf8"));
  const catalogInput = validateCatalogOperations(catalogDocument);
  const catalog = { ...catalogDocument, operations: catalogInput.operations };
  const declarationErrors = [...catalogInput.errors];
  const matrix = JSON.parse(await readFile(options.matrixPath, "utf8"));
  const rules = JSON.parse(await readFile(options.rulesPath, "utf8"));
  const planText = await readFile(options.qualificationPlanPath || new URL("../catalog/qualification-plan.json", import.meta.url), "utf8");
  const plan = validateQualificationPlan(JSON.parse(planText));
  let playwright = { suites: [] };
  let playwrightParseError = "";
  if (options.playwrightPath) {
    const loaded = await readGeneratedJson(options.playwrightPath);
    if (!loaded.error) playwright = loaded.value;
    else playwrightParseError = `playwright.json: ${loaded.error}`;
  }
  const engine = options.enginePath ? JSON.parse(await readFile(options.enginePath, "utf8")) : null;
  const selfTest = options.selfTestPath ? JSON.parse(await readFile(options.selfTestPath, "utf8")) : { cases: [] };
  const selfInput = collectExecutionRows(selfTest, "self-test", ["cases"], Boolean(options.selfTestPath));
  const engineInput = collectExecutionRows(engine, "real-engine-suite", ["scenarios", "results"], Boolean(options.enginePath));
  const audit = await loadAudit(options.auditDir);
  let frozenCatalogSha256 = "";
  try {
    const frozenPath = join(dirname(options.catalogPath), "operation-catalog.json");
    frozenCatalogSha256 = createHash("sha256").update(await readFile(frozenPath)).digest("hex");
  } catch {
    frozenCatalogSha256 = "";
  }
  const walked = walkPlaywright(playwright);
  const requiredGate = [
    "HARNESS_BAD_BASELINE", "HARNESS_DUPLICATE_CONTENT", "HARNESS_MISSING_MAPPING",
    "HARNESS_FAILED_DIRECTORY", "HARNESS_ZERO_DISCOVERY", "HARNESS_OMITTED_CASE",
    "HARNESS_UNEXPECTED_SKIP", "HARNESS_MISSING_BROWSER", "HARNESS_STARTUP_FAILURE",
    "HARNESS_SCAN_FAILURE", "HARNESS_CLEANUP_FAILURE", "HARNESS_OMITTED_NOT_PASS",
    "HARNESS_RETRY_IS_FAIL", "HARNESS_REDACTED_REPORT", "HARNESS_APP_PROFILE_CLEANUP",
    "HARNESS_FALSE_PASS_ANNOTATION", "HARNESS_RUNNER_ERROR", "HARNESS_BASIC_REDACTION",
  ];
  const inputs = validateExecutionInputs([
    { source: "self-test", rows: selfInput.rows },
    { source: "playwright", rows: walked.cases },
    { source: "real-engine-suite", rows: engineInput.rows },
  ]);
  const operationById = new Map(catalog.operations.map((operation) => [operation.id, operation]));
  for (const row of inputs.rows) {
    if ((row.source === "playwright" && operationById.get(row.id)?.disposition !== "execute")
        || (row.source === "self-test" && !requiredGate.includes(row.id))) {
      inputs.errors.push({ source: row.source, id: row.id, code: "unexpected_execution_identity" });
      row.status = FAIL; row.qualification = FAIL;
      row.reason = "Execution identity is not declared for this evidence source.";
    }
  }
  const engines = engineIndex(inputs.rows.filter((row) => row.source === "real-engine-suite"));
  const byId = new Map();

  function put(row) {
    const existing = byId.get(row.id);
    if (!existing || rank(row.status) > rank(existing.status)) byId.set(row.id, row);
  }
  for (const row of inputs.rows) if (row.source !== "real-engine-suite") put(row);

  const suite = options.suite || "smoke";
  for (const operation of catalog.operations || []) {
    if (byId.has(operation.id)) {
      const row = byId.get(operation.id);
      row.r_criteria = operation.r_criteria || [];
      row.supports_workflow = operation.supports_workflow !== false && row.qualification !== "UNQUALIFIED";
      row.title = operation.title;
      row.contract = operation.contract || "";
      continue;
    }
    if (operation.disposition === "not_run") {
      put({
        id: operation.id,
        title: operation.title,
        status: NOT_RUN,
        reason: operation.reason,
        qualification: "UNQUALIFIED",
        supports_workflow: false,
        r_criteria: operation.r_criteria || [],
        source: "catalog",
      });
      continue;
    }
    if (operation.disposition === "engine") {
      const found = engines.get(operation.id);
      if (!engine) {
        put({
          id: operation.id,
          title: operation.title,
          status: NOT_RUN,
          reason: "real-engine suite was not executed in this invocation",
          qualification: NOT_RUN,
          supports_workflow: false,
          r_criteria: operation.r_criteria || [],
          source: "real-engine-suite",
        });
      } else if (!found) {
        put({
          id: operation.id,
          title: operation.title,
          status: NOT_RUN,
          reason: "real-engine suite ran but did not report this id",
          qualification: NOT_RUN,
          supports_workflow: false,
          r_criteria: operation.r_criteria || [],
          source: "real-engine-suite",
        });
      } else {
        put({
          id: operation.id,
          title: operation.title,
          status: normalizeEngine(found.status),
          reason: found.detail?.reason || found.reason || "",
          qualification: normalizeEngine(found.status) === PASS ? "executed" : normalizeEngine(found.status),
          supports_workflow: normalizeEngine(found.status) === PASS,
          r_criteria: operation.r_criteria || [],
          source: "real-engine-suite",
        });
      }
      continue;
    }
    const selected = operationSelected(suite, operation);
    put({
      id: operation.id,
      title: operation.title,
      status: NOT_RUN,
      reason: selected
        ? "selected by this suite but no Playwright result or explicit disposition was recorded"
        : `not selected by suite ${suite}`,
      qualification: NOT_RUN,
      supports_workflow: false,
      r_criteria: operation.r_criteria || [],
      contract: operation.contract || "",
      source: "missing-result",
      omitted: selected,
    });
  }

  for (const id of matrixIds(matrix, declarationErrors)) {
    if (byId.has(id)) continue;
    const engineRow = engines.get(id);
    if (engineRow) {
      put({
        id,
        status: normalizeEngine(engineRow.status),
        reason: engineRow.detail?.reason || "",
        qualification: normalizeEngine(engineRow.status),
        supports_workflow: false,
        r_criteria: [id.split("_")[0]],
        source: "real-engine-suite",
        title: id,
      });
      continue;
    }
    put({
      id,
      title: id,
      status: NOT_RUN,
      reason: reasonFor(id, rules),
      qualification: "UNQUALIFIED",
      supports_workflow: false,
      r_criteria: [id.startsWith("R") ? id.slice(0, 3) : ""],
      source: "acceptance-matrix",
    });
  }

  for (const operation of audit.operations) {
    if (byId.has(operation.id)) continue;
    put({
      id: operation.id,
      title: operation.title,
      status: NOT_RUN,
      reason: "Present in the audit catalog and not wired to an executor. Recorded so it is not omitted.",
      qualification: "UNQUALIFIED",
      supports_workflow: false,
      r_criteria: operation.r_criteria,
      source: "audit",
    });
  }

  // A new/unmapped engine id is still observed evidence. Preserve its failure
  // even when no catalog or matrix entry names it. An unmapped PASS remains
  // unqualified until its obligation/executor mapping is reviewed.
  for (const found of engines.values()) {
    const existing = byId.get(found.id);
    const status = normalizeEngine(found.status);
    put({
      ...existing, id: found.id, title: existing?.title || found.id, status,
      reason: found.detail?.reason || found.reason || "Engine id has no reviewed catalog or matrix mapping.",
      qualification: status === PASS ? existing?.qualification || "UNQUALIFIED" : status,
      supports_workflow: existing?.supports_workflow || false,
      r_criteria: existing?.r_criteria || plan.r_criteria_by_case[found.id] || [],
      source: "real-engine-suite",
    });
  }

  // Keep accepted obligations visible even if a catalog edit removes their
  // executor/matrix entry. New catalog obligations remain included as before.
  for (const id of plan.required_case_ids) {
    if (!byId.has(id)) put({
      id, title: id, status: NOT_RUN, qualification: "UNQUALIFIED",
      supports_workflow: false, source: "qualification-plan",
      r_criteria: plan.r_criteria_by_case[id],
      reason: "Required by the retained qualification plan but no catalog or execution row was recorded.",
    });
  }
  // Retain accepted associations even when another evidence source supplies a
  // row after its catalog mapping was removed or narrowed.
  for (const row of byId.values()) {
    row.r_criteria = [...new Set([...(row.r_criteria || []), ...(plan.r_criteria_by_case[row.id] || [])])];
  }
  const rows = [...byId.values()].sort((a, b) => a.id.localeCompare(b.id));
  const omitted = rows.filter((row) => row.omitted);
  const unexpectedSkips = walked.skipped.filter((item) => item.reason === "unexpected_skip");
  const gate = requiredGate.map((id) => byId.get(id) || { id, status: FAIL, reason: "gate case missing" });
  const gateFailed = gate.filter((row) => row.status !== PASS);
  const discovered = walked.discovered + selfInput.rows.length;
  const zeroDiscovery = suite !== "self-test" && options.playwrightPath && walked.discovered === 0;
  const flaky = walked.attempts.filter((item) => item.flaky);

  const selectedIds = suite === "self-test" ? requiredGate : (catalog.operations || [])
    .filter((op) => (op.disposition === "execute" && operationSelected(suite, op))
      || (op.disposition === "engine" && suite === "full"))
    .map((op) => op.id);
  const inputErrors = [...declarationErrors, ...audit.errors, ...walked.validation_errors,
    ...selfInput.errors, ...engineInput.errors, ...inputs.errors];
  const accounting = accountReport({
    rows, selectedIds, requiredIds: plan.required_case_ids,
    blocked: Boolean(gateFailed.length || unexpectedSkips.length || omitted.length || zeroDiscovery
      || walked.runner_error || playwrightParseError || inputErrors.length || walked.cases.some((row) => row.status === FAIL)),
  });
  // Preserve report.v1 consumers and CLI/CI exit semantics: full still requires
  // every obligation; smoke and soak gate their selected executors plus controls.
  const suiteResult = suite === "full" ? accounting.overall : accounting.selected_suite_result;
  const fullQualification = suite !== "full" ? NOT_RUN : suiteResult === PASS ? PASS : FAIL;
  const inspectedSha = catalog.inspected_sha || "";
  const candidate = options.candidate || "";
  const auditCoversCandidate = Boolean(candidate) && Boolean(inspectedSha) && candidate === inspectedSha;
  let qualificationGap =
    audit.status === "INGESTED"
      ? ""
      : audit.gap ||
        "Operation catalog is provisional from GOAL.md R01-R24, docs/reliability/acceptance-matrix.json, and issue #62. The frozen audit catalog was not applied.";
  if (audit.status === "INGESTED" && candidate && inspectedSha && candidate !== inspectedSha) {
    qualificationGap = `Frozen audit inspected ${inspectedSha}. Candidate ${candidate} is a different tree. The catalog was ingested and does not cover that delta.`;
  }
  const report = {
    schema: "reposync.acceptance-lab.report.v1",
    suite,
    suite_result: suiteResult,
    full_qualification: fullQualification,
    ...accounting,
    qualification_plan: {
      schema: plan.schema,
      sha256: createHash("sha256").update(planText).digest("hex"),
      required_count: plan.required_case_ids.length,
    },
    input_validation_errors: inputErrors,
    partial_is_not_pass: true,
    catalog_status: audit.status === "INGESTED" ? "audit_ingested" : catalog.catalog_status,
    frozen_catalog_sha256: frozenCatalogSha256,
    audit_inspected_sha: inspectedSha,
    audit_covers_candidate: auditCoversCandidate,
    qualification_gap: qualificationGap,
    audit,
    candidate,
    seed: options.seed ?? null,
    run_id: options.runId || "",
    gate_failed: gateFailed.map((row) => row.id),
    unexpected_skips: unexpectedSkips,
    omitted: omitted.map((row) => row.id),
    zero_discovery: Boolean(zeroDiscovery),
    discovered_results: discovered,
    flaky,
    attempts: walked.attempts,
    first_attempt_preserved: true,
    retries_are_not_pass: true,
    playwright_parse_error: playwrightParseError,
    rows,
    r_coverage: coverage(rows),
  };
  if (options.outPath) await writeFile(options.outPath, JSON.stringify(report, null, 2));
  if (options.markdownPath) await writeFile(options.markdownPath, renderMarkdown(report));
  return report;
}

function rank(status) {
  return { FAIL: 4, PARTIAL: 3, SKIP: 2, NOT_RUN: 1, PASS: 0 }[status] ?? 0;
}

function normalizeEngine(status) {
  const text = String(status || "").toUpperCase().replace(" ", "_");
  if (text === "PASS" || text === "FAIL" || text === "PARTIAL" || text === "NOT_RUN") return text;
  if (text === "NOT RUN") return NOT_RUN;
  return FAIL;
}

function coverage(rows) {
  const by = {};
  for (let index = 1; index <= 24; index += 1) {
    const id = `R${String(index).padStart(2, "0")}`;
    by[id] = { related: [], pass: 0, fail: 0, partial: 0, not_run: 0 };
  }
  for (const row of rows) {
    for (const criterion of row.r_criteria || []) {
      if (!by[criterion]) by[criterion] = { related: [], pass: 0, fail: 0, partial: 0, not_run: 0 };
      by[criterion].related.push(row.id);
      const bucket = String(row.status || "").toLowerCase();
      if (bucket === "pass") by[criterion].pass += 1;
      else if (bucket === "fail") by[criterion].fail += 1;
      else if (bucket === "partial") by[criterion].partial += 1;
      else by[criterion].not_run += 1;
    }
  }
  return by;
}

function renderMarkdown(report) {
  const lines = [
    `# Acceptance lab report`,
    ``,
    `Suite: ${report.suite}`,
    `Suite result: ${report.suite_result}`,
    `Selected-suite result: ${report.selected_suite_result} (executor subset; not release qualification)`,
    `Whole-plan overall: ${report.overall}`,
    `Full qualification: ${report.full_qualification}`,
    `Required qualification obligations: ${report.qualification_plan.required_count}`,
    `Input validation errors: ${report.input_validation_errors.length}`,
    `Candidate: ${report.candidate || "(unset)"}`,
    `Audit inspected: ${report.audit_inspected_sha || "(unset)"}`,
    `Audit covers candidate: ${report.audit_covers_candidate ? "yes" : "no"}`,
    `Seed: ${report.seed}`,
    `Run: ${report.run_id}`,
    ``,
    report.qualification_gap ? `Qualification gap: ${report.qualification_gap}` : `Audit catalog: ${report.audit.status}`,
    ``,
    `Whole-plan counts: ${formatCounts(report.counts)}`,
    `Selected executor counts (harness controls reported separately by gate_failed): ${formatCounts(report.selected_suite_counts)}`,
    ``,
    `| ID | Status | Source | Workflow |`,
    `| --- | --- | --- | --- |`,
  ];
  for (const row of report.rows) {
    lines.push(`| ${row.id} | ${row.status} | ${row.source} | ${row.supports_workflow ? "yes" : "no"} |`);
  }
  lines.push(``, `All JSON rows, including matrix-only and unavailable obligations, are listed above. Synthetic controls do not establish product runtime qualification.`, ``);
  return lines.join("\n");
}

function formatCounts(counts) {
  return ["PASS", "FAIL", "PARTIAL", "NOT_RUN", "SKIP"].map((status) => `${status} ${counts[status] || 0}`).join(", ");
}

export function detectorSelfTest(fixture) {
  const trees = compareTrees(fixture.expected_tree, fixture.actual_tree);
  const dupes = duplicateMappings(fixture.mappings);
  const missing = missingMappings(fixture.expected_mappings, fixture.mappings);
  const directory = directoryUsable({ exists: false, readable: false, is_directory: false });
  const browser = assessBrowser("");
  const services = assessServices({ ldap: "unhealthy" });
  return {
    bad_tree: trees.ok === false && trees.changed.includes("trunk/README.txt") && trees.missing.includes("trunk/keep.txt") && trees.extra.includes("trunk/extra.txt"),
    duplicates: dupes.conflicting_revisions.length > 0,
    missing: missing.some((row) => row.svn_rev === 3),
    directory: directory.ok === false,
    browser: browser.ok === false && browser.reason === "missing_browser",
    services: services.ok === false,
  };
}

function usage() {
  console.error("usage: node aggregate.mjs --catalog --matrix --rules --out --markdown --suite [--playwright] [--engine] [--self-test] [--audit-dir] [--candidate] [--seed] [--run-id]");
  process.exit(2);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const get = (flag) => {
    const index = args.indexOf(flag);
    return index === -1 ? "" : args[index + 1];
  };
  if (!get("--catalog") || !get("--out")) usage();
  const report = await buildReport({
    catalogPath: get("--catalog"),
    matrixPath: get("--matrix"),
    rulesPath: get("--rules"),
    playwrightPath: get("--playwright") || "",
    enginePath: get("--engine") || "",
    selfTestPath: get("--self-test") || "",
    auditDir: get("--audit-dir") || "",
    outPath: get("--out"),
    markdownPath: get("--markdown"),
    suite: get("--suite") || "smoke",
    candidate: get("--candidate"),
    seed: get("--seed"),
    runId: get("--run-id"),
  });
  const code = report.suite_result === PASS ? 0 : 1;
  process.exit(code);
}
