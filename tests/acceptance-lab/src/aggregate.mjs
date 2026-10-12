import { createHash } from "node:crypto";
import { readFile, writeFile, readdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { assessBrowser, assessServices, compareTrees, directoryUsable, duplicateMappings, missingMappings } from "./oracle.mjs";

const PASS = "PASS";
const FAIL = "FAIL";
const PARTIAL = "PARTIAL";
const NOT_RUN = "NOT_RUN";

export function walkPlaywright(report) {
  const cases = [];
  const skipped = [];
  const attempts = [];
  function visitSuite(suite) {
    for (const spec of suite.specs || []) {
      for (const test of spec.tests || []) {
        const annotations = [...(spec.annotations || []), ...(test.annotations || [])];
        const ids = annotations.filter((item) => item.type === "labCase").map((item) => item.description);
        const statusAnn = annotations.find((item) => item.type === "labStatus")?.description;
        const reason = annotations.find((item) => item.type === "labReason")?.description || "";
        const qualification = annotations.find((item) => item.type === "labQualification")?.description || "";
        const results = test.results || [];
        const retries = results.filter((result) => result.retry > 0).length;
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
        const discovered = ids.length > 0;
        if (!discovered) {
          skipped.push({ title: spec.title, reason: "missing_labCase_annotation" });
          continue;
        }
        const actual = final?.status || "missing";
        const blocksPass = actual === "failed" || actual === "timedOut" || actual === "interrupted" || actual === "missing";
        let status = PASS;
        if (blocksPass) status = FAIL;
        else if (actual === "skipped") status = statusAnn === NOT_RUN ? NOT_RUN : FAIL;
        else if (actual !== "passed") status = FAIL;
        else if (statusAnn && statusAnn !== PASS) status = statusAnn;
        if (retries > 0 && status === PASS) status = FAIL;
        if (Array.isArray(report?.errors) && report.errors.length > 0 && status === PASS) status = FAIL;
        for (const id of ids) {
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
    for (const child of suite.suites || []) visitSuite(child);
  }
  for (const suite of report.suites || []) visitSuite(suite);
  return {
    cases,
    skipped,
    attempts,
    discovered: cases.length,
    runner_error: Array.isArray(report?.errors) && report.errors.length > 0,
  };
}

function matrixIds(matrix) {
  const ids = new Set(matrix.harness_case_ids || []);
  for (const scenario of matrix.scenarios || []) {
    ids.add(scenario.id);
    for (const id of scenario.required_case_ids || []) ids.add(id);
    for (const id of scenario.supporting_case_ids || []) ids.add(id);
  }
  return [...ids];
}

function reasonFor(id, rules) {
  for (const rule of rules.reasons || []) {
    if (new RegExp(rule.match).test(id)) return rule.reason;
  }
  return "No acceptance-lab executor is mapped to this id. It is recorded and is not a PASS.";
}

function engineIndex(summary) {
  const map = new Map();
  if (!summary) return map;
  const rows = summary.scenarios || summary.results || [];
  for (const row of rows) {
    if (row && row.id) map.set(row.id, row);
  }
  return map;
}

async function loadAudit(dir) {
  if (!dir) return { status: "NOT_AVAILABLE", operations: [], files: [] };
  let names = [];
  try {
    names = await readdir(dir);
  } catch {
    return { status: "NOT_AVAILABLE", operations: [], files: [] };
  }
  const files = names.filter((name) => name.endsWith(".json"));
  const operations = [];
  let recognized = false;
  for (const name of files) {
    let parsed;
    try {
      parsed = JSON.parse(await readFile(join(dir, name), "utf8"));
    } catch {
      continue;
    }
    const rows = Array.isArray(parsed)
      ? parsed
      : parsed.operations || parsed.cases || parsed.catalog || null;
    if (!Array.isArray(rows)) continue;
    recognized = true;
    for (const row of rows) {
      const id = typeof row === "string" ? row : row.id;
      if (id) {
        operations.push({
          id,
          title: row.title || row.operation || id,
          r_criteria: row.r_criteria || row.r_ids || row.r || [],
          contract: row.product_contract || row.contract || "",
        });
      }
    }
  }
  if (!files.length) return { status: "NOT_AVAILABLE", operations: [], files };
  if (!recognized) {
    return {
      status: "UNRECOGNIZED",
      operations: [],
      files,
      gap: "Audit report files were present but none matched an operation catalog schema. They were not treated as the frozen catalog.",
    };
  }
  return { status: "INGESTED", operations, files };
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
  const catalog = JSON.parse(await readFile(options.catalogPath, "utf8"));
  const matrix = JSON.parse(await readFile(options.matrixPath, "utf8"));
  const rules = JSON.parse(await readFile(options.rulesPath, "utf8"));
  let playwright = { suites: [] };
  let playwrightParseError = "";
  if (options.playwrightPath) {
    const loaded = await readGeneratedJson(options.playwrightPath);
    if (loaded.value) playwright = loaded.value;
    else playwrightParseError = `playwright.json: ${loaded.error}`;
  }
  const engine = options.enginePath ? JSON.parse(await readFile(options.enginePath, "utf8")) : null;
  const selfTest = options.selfTestPath ? JSON.parse(await readFile(options.selfTestPath, "utf8")) : { cases: [] };
  const audit = await loadAudit(options.auditDir);
  let frozenCatalogSha256 = "";
  try {
    const frozenPath = join(dirname(options.catalogPath), "operation-catalog.json");
    frozenCatalogSha256 = createHash("sha256").update(await readFile(frozenPath)).digest("hex");
  } catch {
    frozenCatalogSha256 = "";
  }
  const walked = walkPlaywright(playwright);
  const engines = engineIndex(engine);
  const byId = new Map();

  function put(row) {
    const existing = byId.get(row.id);
    if (!existing || rank(row.status) > rank(existing.status)) byId.set(row.id, row);
  }
  for (const row of selfTest.cases || []) put({ ...row, source: row.source || "self-test" });
  for (const row of walked.cases) put(row);

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

  for (const id of matrixIds(matrix)) {
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

  const rows = [...byId.values()].sort((a, b) => a.id.localeCompare(b.id));
  const counts = { PASS: 0, FAIL: 0, PARTIAL: 0, NOT_RUN: 0, SKIP: 0 };
  for (const row of rows) counts[row.status] = (counts[row.status] || 0) + 1;
  const omitted = rows.filter((row) => row.omitted);
  const unexpectedSkips = walked.skipped.filter((item) => item.reason === "unexpected_skip");
  const requiredGate = [
    "HARNESS_BAD_BASELINE",
    "HARNESS_DUPLICATE_CONTENT",
    "HARNESS_MISSING_MAPPING",
    "HARNESS_FAILED_DIRECTORY",
    "HARNESS_ZERO_DISCOVERY",
    "HARNESS_OMITTED_CASE",
    "HARNESS_UNEXPECTED_SKIP",
    "HARNESS_MISSING_BROWSER",
    "HARNESS_STARTUP_FAILURE",
    "HARNESS_SCAN_FAILURE",
    "HARNESS_CLEANUP_FAILURE",
    "HARNESS_OMITTED_NOT_PASS",
    "HARNESS_RETRY_IS_FAIL",
    "HARNESS_REDACTED_REPORT",
    "HARNESS_APP_PROFILE_CLEANUP",
    "HARNESS_FALSE_PASS_ANNOTATION",
    "HARNESS_RUNNER_ERROR",
    "HARNESS_BASIC_REDACTION",
  ];
  const gate = requiredGate.map((id) => byId.get(id) || { id, status: FAIL, reason: "gate case missing" });
  const gateFailed = gate.filter((row) => row.status !== PASS);
  const discovered = walked.discovered + (selfTest.cases || []).length;
  const zeroDiscovery = suite !== "self-test" && options.playwrightPath && walked.discovered === 0;
  const flaky = walked.attempts.filter((item) => item.flaky);

  const smokeRows = rows.filter((row) => (catalog.operations || []).some((op) => op.id === row.id && op.suite === "smoke" && op.disposition === "execute"));
  const smokeFail = smokeRows.filter((row) => row.status !== PASS);
  let suiteResult = PASS;
  if (gateFailed.length || unexpectedSkips.length || omitted.length || zeroDiscovery || walked.runner_error || walked.cases.some((row) => row.status === FAIL)) suiteResult = FAIL;
  if (suite === "smoke" && smokeFail.length) suiteResult = FAIL;
  if (suite === "full") {
    const blocking = rows.filter((row) => row.supports_workflow && row.status !== PASS);
    const incomplete = rows.filter((row) => row.status === NOT_RUN || row.status === PARTIAL || row.status === FAIL || row.status === "SKIP");
    if (blocking.length || incomplete.length || suiteResult === FAIL) suiteResult = FAIL;
  }
  if (suite === "soak") {
    const soakRows = rows.filter((row) => (catalog.operations || []).some((op) => op.id === row.id && op.suite === "soak"));
    if (soakRows.some((row) => row.status !== PASS) || gateFailed.length) suiteResult = FAIL;
  }

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
    counts,
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

function operationSelected(suite, operation) {
  if (operation.suite === "soak") return suite === "soak";
  if (suite === "smoke") return operation.suite === "smoke";
  if (suite === "full") return operation.suite === "smoke" || operation.suite === "full";
  if (suite === "soak") return false;
  return false;
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
    `Full qualification: ${report.full_qualification}`,
    `Candidate: ${report.candidate || "(unset)"}`,
    `Audit inspected: ${report.audit_inspected_sha || "(unset)"}`,
    `Audit covers candidate: ${report.audit_covers_candidate ? "yes" : "no"}`,
    `Seed: ${report.seed}`,
    `Run: ${report.run_id}`,
    ``,
    report.qualification_gap ? `Qualification gap: ${report.qualification_gap}` : `Audit catalog: ${report.audit.status}`,
    ``,
    `Counts: PASS ${report.counts.PASS || 0}, FAIL ${report.counts.FAIL || 0}, PARTIAL ${report.counts.PARTIAL || 0}, NOT_RUN ${report.counts.NOT_RUN || 0}`,
    ``,
    `| ID | Status | Source | Workflow |`,
    `| --- | --- | --- | --- |`,
  ];
  for (const row of report.rows) {
    if (row.source === "acceptance-matrix") continue;
    lines.push(`| ${row.id} | ${row.status} | ${row.source} | ${row.supports_workflow ? "yes" : "no"} |`);
  }
  lines.push(``, `Matrix subcases are included in the JSON rows and are not repeated here.`, ``);
  return lines.join("\n");
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
