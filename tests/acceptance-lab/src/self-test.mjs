import { mkdtemp, readFile, writeFile, rm, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { buildReport, detectorSelfTest, walkPlaywright } from "./aggregate.mjs";
import { planCleanup } from "./cleanup.mjs";
import { scanTree } from "./scan.mjs";
import { redactText, scrubSelfCheck } from "./scrub.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");

function caseRow(id, ok, reason) {
  return { id, status: ok ? "PASS" : "FAIL", reason: ok ? "" : reason, source: "self-test" };
}

const fixture = JSON.parse(await readFile(join(root, "fixtures/known-bad-baseline.json"), "utf8"));
const detected = detectorSelfTest(fixture);
const directory = await mkdtemp(join(tmpdir(), "acceptance-lab-self-"));
const canaryDir = join(directory, "canary");
await mkdir(canaryDir);
await writeFile(join(canaryDir, "leak.txt"), "CANARY_SECRET_SHOULD_NOT_LEAK\n");
const scan = await scanTree(canaryDir, []);
const foreign = planCleanup({
  runId: "rsabc123",
  project: "rsaccrsabc123",
  resources: [{ id: "container-other", labels: { "acceptance-lab.run-id": "rsother", "acceptance-lab.disposable": "true" } }],
  commands: ["docker system prune -f"],
});
const goodPlan = planCleanup({
  runId: "rsabc123",
  project: "rsaccrsabc123",
  resources: [{ id: "rsaccrsabc123-svn", labels: { "acceptance-lab.run-id": "rsabc123", "acceptance-lab.disposable": "true" }, project: "rsaccrsabc123" }],
  commands: ["docker compose -p rsaccrsabc123 down -v"],
});
const appPlan = planCleanup({
  runId: "rsabc123",
  project: "rsaccrsabc123",
  resources: [{ id: "rsaccrsabc123-reposync-1", labels: { "acceptance-lab.run-id": "rsabc123", "acceptance-lab.disposable": "true" }, project: "rsaccrsabc123" }],
  commands: ["docker compose --profile app -p rsaccrsabc123 down -v"],
});
const prunedApp = planCleanup({
  runId: "rsabc123",
  project: "rsaccrsabc123",
  resources: [{ id: "rsaccrsabc123-reposync-1", labels: { "acceptance-lab.run-id": "rsabc123", "acceptance-lab.disposable": "true" }, project: "rsaccrsabc123" }],
  commands: ["docker system prune -f"],
});
const retryWalk = walkPlaywright({
  suites: [{
    specs: [{
      title: "retried case",
      annotations: [{ type: "labCase", description: "LAB_RETRY" }],
      tests: [{
        annotations: [{ type: "labStatus", description: "PASS" }],
        results: [
          { status: "failed", retry: 0, error: { message: "first attempt failed" } },
          { status: "passed", retry: 1 },
        ],
      }],
    }],
  }],
});
const reportSecret = "lab-secret-value-0123456789";
const redactedReport = redactText(JSON.stringify({ suite_result: "FAIL", reason: `denied ${reportSecret}` }), [reportSecret]);
const parsedRedacted = JSON.parse(redactedReport);

const catalogPath = join(directory, "catalog.json");
const matrixPath = join(directory, "matrix.json");
const rulesPath = join(directory, "rules.json");
const playwrightPath = join(directory, "playwright.json");
const emptyPath = join(directory, "empty.json");
await writeFile(catalogPath, JSON.stringify({
  catalog_status: "self-test",
  operations: [
    { id: "LAB_REQUIRED", title: "required", suite: "full", disposition: "execute", r_criteria: ["R01"], supports_workflow: true },
  ],
}));
await writeFile(matrixPath, JSON.stringify({ harness_case_ids: [], scenarios: [] }));
await writeFile(rulesPath, JSON.stringify({ reasons: [] }));
await writeFile(playwrightPath, JSON.stringify({
  suites: [{
    specs: [{
      title: "skipped without reason",
      annotations: [{ type: "labCase", description: "LAB_REQUIRED" }],
      tests: [{ annotations: [], results: [{ status: "skipped", retry: 0 }] }],
    }],
  }],
}));
await writeFile(emptyPath, JSON.stringify({ suites: [] }));
const omittedReport = await buildReport({
  catalogPath, matrixPath, rulesPath, playwrightPath: emptyPath, suite: "full", outPath: "",
});
const skipReport = await buildReport({
  catalogPath, matrixPath, rulesPath, playwrightPath, suite: "full", outPath: "",
});

const cases = [
  caseRow("HARNESS_BAD_BASELINE", detected.bad_tree, "known-bad tree was not flagged"),
  caseRow("HARNESS_DUPLICATE_CONTENT", detected.duplicates, "duplicate mappings were not flagged"),
  caseRow("HARNESS_MISSING_MAPPING", detected.missing, "missing mapping was not flagged"),
  caseRow("HARNESS_FAILED_DIRECTORY", detected.directory, "missing directory was accepted"),
  caseRow("HARNESS_ZERO_DISCOVERY", omittedReport.zero_discovery === true, "empty Playwright report was not zero discovery"),
  caseRow("HARNESS_OMITTED_CASE", omittedReport.omitted.includes("LAB_REQUIRED"), "omitted execute case was not flagged"),
  caseRow("HARNESS_UNEXPECTED_SKIP", skipReport.unexpected_skips.length > 0, "unexpected skip was not flagged"),
  caseRow("HARNESS_MISSING_BROWSER", detected.browser, "empty browser version was accepted"),
  caseRow("HARNESS_STARTUP_FAILURE", detected.services, "unhealthy service was accepted"),
  caseRow("HARNESS_SCAN_FAILURE", scan.ok === false && scan.findings.some((item) => item.reason === "canary"), "canary was not flagged"),
  caseRow("HARNESS_CLEANUP_FAILURE", foreign.ok === false && goodPlan.ok === true && goodPlan.remove.length === 1, "cleanup planner accepted a foreign or global prune"),
  caseRow("HARNESS_OMITTED_NOT_PASS", omittedReport.suite_result === "FAIL" && omittedReport.zero_discovery === true && (omittedReport.rows || []).some((row) => row.id === "LAB_REQUIRED" && row.status === "NOT_RUN"), "an omitted or zero-discovery row was PASS"),
  caseRow("HARNESS_RETRY_IS_FAIL", retryWalk.cases[0]?.status === "FAIL" && retryWalk.attempts[0]?.first_status === "failed" && retryWalk.attempts[0]?.final_status === "passed", "a later pass replaced the failed first attempt"),
  caseRow("HARNESS_REDACTED_REPORT", scrubSelfCheck() === "PASS" && parsedRedacted.suite_result === "FAIL" && !redactedReport.includes(reportSecret) && redactedReport.includes("[redacted]"), "a scrubbed failing report was not parseable"),
  caseRow("HARNESS_APP_PROFILE_CLEANUP", appPlan.ok === true && appPlan.remove.includes("rsaccrsabc123-reposync-1") && prunedApp.ok === false, "cleanup dropped the application profile or accepted a global prune"),
];

const outDir = process.argv[2] || join(directory, "out");
await mkdir(outDir, { recursive: true });
const payload = { cases };
await writeFile(join(outDir, "self-test.json"), JSON.stringify(payload, null, 2));
const failed = cases.filter((row) => row.status !== "PASS");
await rm(directory, { recursive: true, force: true });
if (failed.length) {
  console.error(JSON.stringify(failed, null, 2));
  process.exit(1);
}
console.log(`self-test PASS ${cases.length}`);
