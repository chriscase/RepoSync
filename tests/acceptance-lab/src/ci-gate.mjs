// Workflow inputs arrive in the environment. This process accepts only an
// allowlisted suite and an integer seed, then applies that pair as data.
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..", "..");
const publishRequired = ["playwright.json", "self-test.json", "report.json"];

export function resolveSelection(eventName, suiteInput, seedInput) {
  const event = eventName || "";
  let suite = suiteInput == null ? "" : String(suiteInput);
  let seed = seedInput == null ? "" : String(seedInput);
  const reasons = [];
  if (event !== "workflow_dispatch") suite = "smoke";
  if (seed === "") seed = "1";
  if (!["smoke", "full", "soak"].includes(suite)) {
    reasons.push("infrastructure FAIL: suite must be smoke, full, or soak");
    suite = "";
  }
  if (!/^[0-9]+$/.test(seed)) {
    reasons.push("infrastructure FAIL: seed must be an integer");
    seed = "";
  }
  return { ok: reasons.length === 0, event, suite, seed, reasons };
}

function selectionFromEnv() {
  return resolveSelection(process.env.LAB_EVENT_NAME, process.env.LAB_SUITE_INPUT, process.env.LAB_SEED_INPUT);
}

export function listNamed(root, name) {
  const found = [];
  if (!root || !existsSync(root)) return found;
  const stack = [root];
  while (stack.length) {
    const current = stack.pop();
    let entries;
    try {
      entries = readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const full = join(current, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) stack.push(full);
      else if (entry.isFile() && entry.name === name) {
        try {
          if (statSync(full).isFile()) found.push(full);
        } catch {
          // An unreadable file is not a usable artifact.
        }
      }
    }
  }
  return found;
}

function identityErrors(identity) {
  const errors = [];
  if (!identity || typeof identity !== "object" || Array.isArray(identity)) return ["identity must be an object"];
  if (typeof identity.candidate_sha !== "string" || !/^[0-9a-f]{40}$/.test(identity.candidate_sha)) errors.push("candidate_sha must be a 40 character lowercase sha");
  if (typeof identity.suite !== "string" || !["smoke", "full", "soak"].includes(identity.suite)) errors.push("suite is missing or invalid");
  if (!validSeed(identity.seed)) errors.push("seed is missing or invalid");
  if (!safeRunId(identity.run_id)) errors.push("run_id is missing or invalid");
  return errors;
}

function validSeed(value) {
  return (typeof value === "number" && Number.isSafeInteger(value) && value >= 0)
    || (typeof value === "string" && /^[0-9]+$/.test(value) && Number.isSafeInteger(Number(value)));
}

export function bindEvidenceIdentity({ evidence, candidate, suite, seed }) {
  const reasons = [];
  const files = listNamed(evidence, "identity.json");
  if (files.length === 0) return { ok: false, reasons: ["infrastructure FAIL: identity.json is missing"], identity: null };
  const docs = [];
  for (const file of files) {
    try {
      const doc = JSON.parse(readFileSync(file, "utf8"));
      const invalid = identityErrors(doc);
      if (invalid.length) reasons.push(...invalid.map((reason) => `infrastructure FAIL: evidence ${reason}`));
      else docs.push(doc);
    } catch {
      reasons.push("infrastructure FAIL: identity.json is not readable");
    }
  }
  if (reasons.length) return { ok: false, reasons, identity: null };
  const identity = docs[0];
  const signature = (doc) => JSON.stringify([doc.candidate_sha, doc.suite, Number(doc.seed), doc.run_id]);
  if (docs.some((doc) => signature(doc) !== signature(identity))) reasons.push(`infrastructure FAIL: identity.json copies disagree (${files.length} files)`);
  if (identity.candidate_sha !== candidate) reasons.push(`infrastructure FAIL: evidence candidate ${identity.candidate_sha} does not match ${candidate}`);
  if (identity.suite !== suite) reasons.push(`infrastructure FAIL: evidence suite ${identity.suite} does not match ${suite}`);
  if (!validSeed(seed) || Number(identity.seed) !== Number(seed)) reasons.push(`infrastructure FAIL: evidence seed ${identity.seed} does not match ${seed}`);
  return { ok: reasons.length === 0, reasons, identity };
}

export function findNamed(root, name) {
  if (!root || !existsSync(root)) return "";
  const stack = [root];
  while (stack.length) {
    const current = stack.pop();
    let entries;
    try {
      entries = readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const full = join(current, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) stack.push(full);
      else if (entry.isFile() && entry.name === name) {
        try {
          if (statSync(full).size > 0) return full;
        } catch {
          // An unreadable file is not a present artifact.
        }
      }
    }
  }
  return "";
}

function publishDirs(root) {
  const found = [];
  if (!root || !existsSync(root)) return found;
  const stack = [root];
  while (stack.length) {
    const current = stack.pop();
    let entries;
    try {
      entries = readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (entry.isSymbolicLink()) continue;
      if (!entry.isDirectory()) continue;
      const full = join(current, entry.name);
      if (entry.name === "publish") found.push(full);
      else stack.push(full);
    }
  }
  return found;
}

function fileReady(path) {
  try {
    return existsSync(path) && statSync(path).isFile() && statSync(path).size > 0;
  } catch {
    return false;
  }
}

export function assessPublish(root, selection) {
  const dirs = publishDirs(root);
  const complete = dirs.some((dir) => publishRequired.every((name) => fileReady(join(dir, name))));
  const scattered = publishRequired.filter((name) => !dirs.some((dir) => fileReady(join(dir, name))));
  const missing = complete ? [] : (scattered.length ? scattered : publishRequired);
  const reasons = [...selection.reasons];
  if (missing.length) reasons.push(`infrastructure FAIL: missing required runtime artifacts: ${missing.join(", ")}`);
  return { ok: reasons.length === 0, reasons, missing, dirs };
}

export function notePublish(root, selection = selectionFromEnv()) {
  mkdirSync(root, { recursive: true });
  let assessment = assessPublish(root, selection);
  if (assessment.dirs.length === 0) {
    mkdirSync(join(root, "missing-artifact", "publish"), { recursive: true });
    assessment = assessPublish(root, selection);
  }
  if (!assessment.ok) {
    const text = `${assessment.reasons.join("\n")}\n`;
    for (const dir of assessment.dirs) writeFileSync(join(dir, "infrastructure-failure.txt"), text);
  }
  return assessment;
}

export function stageEngine(root, dest) {
  mkdirSync(dest, { recursive: true });
  const summary = findNamed(root, "summary.json");
  if (summary) {
    copyFileSync(summary, join(dest, "summary.json"));
    return { ok: true, reasons: [] };
  }
  const reason = "infrastructure FAIL: missing required runtime artifacts: engine summary.json";
  writeFileSync(join(dest, "infrastructure-failure.txt"), `${reason}\n`);
  return { ok: false, reasons: [reason] };
}

function runtimeReasons(selection, evidence, engine) {
  const reasons = [...selection.reasons];
  const missing = [];
  if (!findNamed(evidence, "playwright.json")) missing.push("playwright.json");
  if (!findNamed(evidence, "self-test.json")) missing.push("self-test.json");
  if ((selection.suite === "full" || selection.suite === "soak") && !findNamed(engine, "summary.json")) {
    missing.push("engine summary.json");
  }
  if (missing.length) reasons.push(`infrastructure FAIL: missing required runtime artifacts: ${missing.join(", ")}`);
  return reasons;
}

function writeReasons(reportDir, reasons) {
  const dest = join(reportDir, "infrastructure-failure.txt");
  if (reasons.length) writeFileSync(dest, `${reasons.join("\n")}\n`);
  else if (existsSync(dest)) rmSync(dest);
}

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return null;
  }
}

function safeRunId(value) {
  return typeof value === "string" && value !== "unknown" && /^[A-Za-z0-9]+$/.test(value);
}

export function reportRunId(identity, env = process.env) {
  const fromIdentity = identity && typeof identity.run_id === "string" ? identity.run_id : "";
  if (safeRunId(fromIdentity)) return fromIdentity;
  const run = String(env.GITHUB_RUN_ID || "");
  const attempt = String(env.GITHUB_RUN_ATTEMPT || "");
  if (/^[0-9]+$/.test(run) && /^[0-9]+$/.test(attempt)) return `gha${run}a${attempt}`;
  const lab = String(env.LAB_RUN_ID || "");
  if (safeRunId(lab)) return lab;
  return `norun${process.pid}`;
}

export function aggregateReport({ evidence, engine, reportDir, candidate }) {
  mkdirSync(reportDir, { recursive: true });
  const selection = selectionFromEnv();
  const reasons = runtimeReasons(selection, evidence, engine);
  if (!/^[0-9a-f]{40}$/.test(candidate || "")) {
    reasons.push("infrastructure FAIL: candidate must be a 40 character lowercase sha");
  }
  writeReasons(reportDir, reasons);
  let aggregateExit = 0;
  let identityOk = true;
  if (selection.ok && /^[0-9a-f]{40}$/.test(candidate || "")) {
    const bound = bindEvidenceIdentity({ evidence, candidate, suite: selection.suite, seed: selection.seed });
    if (!bound.ok) {
      identityOk = false;
      reasons.push(...bound.reasons);
    }
  }
  if (selection.ok && identityOk && /^[0-9a-f]{40}$/.test(candidate || "")) {
    const identityPath = findNamed(evidence, "identity.json");
    const identity = identityPath ? readJson(identityPath) : null;
    const runId = reportRunId(identity);
    const args = [
      join(here, "aggregate.mjs"),
      "--catalog", join(repoRoot, "tests/acceptance-lab/catalog/operations.json"),
      "--matrix", join(repoRoot, "docs/reliability/acceptance-matrix.json"),
      "--rules", join(repoRoot, "tests/acceptance-lab/catalog/wiring-rules.json"),
      "--audit-dir", join(repoRoot, "tests/acceptance-lab/catalog"),
      "--out", join(reportDir, "report.json"),
      "--markdown", join(reportDir, "report.md"),
      "--suite", selection.suite,
      "--candidate", candidate,
      "--seed", selection.seed,
      "--run-id", String(runId),
    ];
    const playwright = findNamed(evidence, "playwright.json");
    const selfTest = findNamed(evidence, "self-test.json");
    const summary = findNamed(engine, "summary.json");
    if (playwright) args.push("--playwright", playwright);
    if (selfTest) args.push("--self-test", selfTest);
    if (summary) args.push("--engine", summary);
    const result = spawnSync(process.execPath, args, { cwd: repoRoot, stdio: "inherit" });
    aggregateExit = result.status === null ? 1 : result.status;
    if (result.status === null) reasons.push("infrastructure FAIL: aggregate exited without a status");
  }
  if (!fileReady(join(reportDir, "report.json"))) reasons.push("infrastructure FAIL: report.json was not produced");
  writeReasons(reportDir, reasons);
  return { ok: reasons.length === 0 && aggregateExit === 0, reasons, aggregateExit };
}

export function enforceReport({ evidence, engine, reportDir }) {
  const selection = selectionFromEnv();
  const reasons = runtimeReasons(selection, evidence, engine);
  const reportPath = join(reportDir, "report.json");
  const report = fileReady(reportPath) ? readJson(reportPath) : null;
  if (!report) reasons.push("infrastructure FAIL: report.json was not produced");
  const bound = bindEvidenceIdentity({ evidence, candidate: report?.candidate, suite: selection.suite, seed: selection.seed });
  reasons.push(...bound.reasons);
  if (report) {
    reasons.push(...identityErrors({ ...report, candidate_sha: report.candidate }).map((reason) => `infrastructure FAIL: report ${reason}`));
    if (bound.identity) {
      const identity = bound.identity;
      if (report.suite !== identity.suite) reasons.push("infrastructure FAIL: report suite does not match evidence");
      if (!validSeed(report.seed) || Number(report.seed) !== Number(identity.seed)) reasons.push("infrastructure FAIL: report seed does not match evidence");
      if (report.run_id !== identity.run_id) reasons.push("infrastructure FAIL: report run does not match evidence");
    }
  }
  writeReasons(reportDir, reasons);
  const suiteResult = report?.suite_result || "";
  if (reasons.length) {
    for (const reason of reasons) console.log(reason);
    if (suiteResult) console.log(`suite_result ${suiteResult}`);
    return { ok: false, reasons, suiteResult };
  }
  console.log(`suite_result ${suiteResult}`);
  return { ok: suiteResult === "PASS", reasons, suiteResult };
}

function flag(name) {
  const index = process.argv.indexOf(name);
  if (index === -1 || !process.argv[index + 1]) {
    console.error(`missing ${name}`);
    process.exit(2);
  }
  return process.argv[index + 1];
}

function failIf(result) {
  if (!result.ok) {
    for (const reason of result.reasons) console.log(reason);
    process.exit(1);
  }
}

function commandResolve() {
  const selection = selectionFromEnv();
  if (!selection.ok) {
    for (const reason of selection.reasons) console.error(reason);
    process.exit(2);
  }
  const out = flag("--out");
  mkdirSync(dirname(out), { recursive: true });
  writeFileSync(out, `SUITE=${selection.suite}\nSEED=${selection.seed}\n`);
}

function commandAggregate() {
  const result = aggregateReport({
    evidence: flag("--evidence"),
    engine: flag("--engine"),
    reportDir: flag("--report-dir"),
    candidate: flag("--candidate"),
  });
  process.exit(result.ok ? 0 : 1);
}

function commandEnforce() {
  const result = enforceReport({
    evidence: flag("--evidence"),
    engine: flag("--engine"),
    reportDir: flag("--report-dir"),
  });
  process.exit(result.ok ? 0 : 1);
}

function writeJson(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(value));
}

function assertCheck(ok, message) {
  if (!ok) throw new Error(message);
}

function runNode(args, env) {
  return spawnSync(process.execPath, [fileURLToPath(import.meta.url), ...args], {
    env: { ...process.env, ...env },
    encoding: "utf8",
  });
}

function commandSelfCheck() {
  const scratch = flag("--scratch");
  const marker = join("lab-artifacts", "ci-gate-check");
  if (!scratch.endsWith(marker)) {
    console.error("scratch must be lab-artifacts/ci-gate-check");
    process.exit(2);
  }
  rmSync(scratch, { recursive: true, force: true });
  mkdirSync(scratch, { recursive: true });
  let checks = 0;
  try {
    const pwned = join(scratch, "pwned");
    const badSeed = runNode(["resolve", "--out", join(scratch, "bad.env")], {
      LAB_EVENT_NAME: "workflow_dispatch",
      LAB_SUITE_INPUT: "smoke",
      LAB_SEED_INPUT: `1; touch ${pwned}`,
    });
    checks += 1;
    assertCheck(badSeed.status === 2, `bad seed exit ${badSeed.status}`);
    checks += 1;
    assertCheck(!existsSync(join(scratch, "bad.env")), "bad seed wrote a selection file");
    checks += 1;
    assertCheck(badSeed.stderr.includes("infrastructure FAIL: seed must be an integer"), "bad seed message");
    checks += 1;
    assertCheck(!existsSync(pwned), "bad seed executed a shell command");

    const pullRequest = runNode(["resolve", "--out", join(scratch, "pr.env")], {
      LAB_EVENT_NAME: "pull_request",
      LAB_SUITE_INPUT: "full",
      LAB_SEED_INPUT: "4",
    });
    checks += 1;
    assertCheck(pullRequest.status === 0, `pull request resolve exit ${pullRequest.status}`);
    const prText = readFileSync(join(scratch, "pr.env"), "utf8");
    checks += 1;
    assertCheck(prText === "SUITE=smoke\nSEED=4\n", `pull request selection ${JSON.stringify(prText)}`);
    const sourced = spawnSync("bash", ["-c", '. "$1" && printf %s "$SUITE $SEED"', "bash", join(scratch, "pr.env")], { encoding: "utf8" });
    checks += 1;
    assertCheck(sourced.status === 0 && sourced.stdout === "smoke 4", `sourced selection ${sourced.stdout}`);

    const dispatched = runNode(["resolve", "--out", join(scratch, "dispatch.env")], {
      LAB_EVENT_NAME: "workflow_dispatch",
      LAB_SUITE_INPUT: "soak",
      LAB_SEED_INPUT: "7",
    });
    checks += 1;
    assertCheck(dispatched.status === 0, `dispatch resolve exit ${dispatched.status}`);
    checks += 1;
    assertCheck(readFileSync(join(scratch, "dispatch.env"), "utf8") === "SUITE=soak\nSEED=7\n", "dispatch selection");

    const badSuite = runNode(["resolve", "--out", join(scratch, "suite.env")], {
      LAB_EVENT_NAME: "workflow_dispatch",
      LAB_SUITE_INPUT: "smoke;echo",
      LAB_SEED_INPUT: "1",
    });
    checks += 1;
    assertCheck(badSuite.status === 2 && badSuite.stderr.includes("suite must be smoke, full, or soak"), "bad suite");

    const emptyPublish = join(scratch, "empty-publish");
    const missingPublish = runNode(["note-publish", "--root", emptyPublish], {
      LAB_EVENT_NAME: "pull_request",
      LAB_SUITE_INPUT: "",
      LAB_SEED_INPUT: "",
    });
    checks += 1;
    assertCheck(missingPublish.status === 1, `missing publish exit ${missingPublish.status}`);
    checks += 1;
    assertCheck(missingPublish.stdout.includes("infrastructure FAIL: missing required runtime artifacts: playwright.json, self-test.json, report.json"), "missing publish message");
    checks += 1;
    assertCheck(fileReady(join(emptyPublish, "missing-artifact", "publish", "infrastructure-failure.txt")), "missing publish note was not kept");
    const missingRequire = runNode(["require-publish", "--root", emptyPublish], {
      LAB_EVENT_NAME: "pull_request",
      LAB_SUITE_INPUT: "",
      LAB_SEED_INPUT: "",
    });
    checks += 1;
    assertCheck(missingRequire.status === 1, `missing publish gate exit ${missingRequire.status}`);

    const goodPublish = join(scratch, "good-run", "publish");
    for (const name of publishRequired) writeJson(join(goodPublish, name), { ok: true });
    const goodRequire = runNode(["require-publish", "--root", join(scratch, "good-run")], {
      LAB_EVENT_NAME: "workflow_dispatch",
      LAB_SUITE_INPUT: "smoke",
      LAB_SEED_INPUT: "1",
    });
    checks += 1;
    assertCheck(goodRequire.status === 0, `complete publish exit ${goodRequire.status} ${goodRequire.stdout}`);

    const withheld = join(scratch, "withheld-run", "publish");
    mkdirSync(withheld, { recursive: true });
    writeFileSync(join(withheld, "WITHHELD.txt"), "withheld\n");
    const withheldNote = runNode(["note-publish", "--root", join(scratch, "withheld-run")], {
      LAB_EVENT_NAME: "pull_request",
      LAB_SUITE_INPUT: "smoke",
      LAB_SEED_INPUT: "1",
    });
    checks += 1;
    assertCheck(withheldNote.status === 1 && fileReady(join(withheld, "infrastructure-failure.txt")) && fileReady(join(withheld, "WITHHELD.txt")), "withheld publish dropped the first attempt");
    const withheldRequire = runNode(["require-publish", "--root", join(scratch, "withheld-run")], {
      LAB_EVENT_NAME: "pull_request",
      LAB_SUITE_INPUT: "smoke",
      LAB_SEED_INPUT: "1",
    });
    checks += 1;
    assertCheck(withheldRequire.status === 1 && withheldRequire.stdout.includes("infrastructure FAIL"), "withheld publish was accepted");

    const engineDest = join(scratch, "engine-dest");
    const missingEngine = stageEngine(join(scratch, "no-engine"), engineDest);
    checks += 1;
    assertCheck(!missingEngine.ok && fileReady(join(engineDest, "infrastructure-failure.txt")) && !existsSync(join(engineDest, "summary.json")), "missing engine summary was invented");
    const engineRequire = runNode(["require-engine", "--dest", engineDest]);
    checks += 1;
    assertCheck(engineRequire.status === 1 && engineRequire.stdout.includes("engine summary.json"), "missing engine require");

    const summaryRoot = join(scratch, "engine-root", "20261011T000000Z");
    writeJson(join(summaryRoot, "summary.json"), { ok: true });
    const staged = join(scratch, "engine-staged");
    const presentEngine = stageEngine(join(scratch, "engine-root"), staged);
    checks += 1;
    assertCheck(presentEngine.ok && fileReady(join(staged, "summary.json")), "engine summary was not staged");
    const engineOk = runNode(["require-engine", "--dest", staged]);
    checks += 1;
    assertCheck(engineOk.status === 0, "present engine require");

    const evidence = join(scratch, "evidence");
    const reportDir = join(scratch, "report");
    writeJson(join(evidence, "playwright.json"), { suites: [] });
    writeJson(join(evidence, "self-test.json"), { ok: true });
    const fixtureIdentity = { candidate_sha: "a".repeat(40), suite: "smoke", seed: 1, run_id: "rsfixture1" };
    const fixtureReport = { candidate: fixtureIdentity.candidate_sha, suite: "smoke", seed: 1, run_id: "rsfixture1" };
    writeJson(join(evidence, "identity.json"), fixtureIdentity);
    writeJson(join(reportDir, "report.json"), { suite_result: "PASS" });
    const hiddenPass = runNode(["enforce", "--evidence", join(scratch, "no-evidence"), "--engine", join(scratch, "no-engine"), "--report-dir", reportDir], {
      LAB_EVENT_NAME: "pull_request",
      LAB_SUITE_INPUT: "smoke",
      LAB_SEED_INPUT: "1",
    });
    checks += 1;
    assertCheck(hiddenPass.status === 1 && hiddenPass.stdout.includes("infrastructure FAIL: missing required runtime artifacts"), "PASS report hid missing artifacts");
    checks += 1;
    assertCheck(fileReady(join(reportDir, "infrastructure-failure.txt")), "gate did not keep the infrastructure failure");

    const productFailDir = join(scratch, "product-report");
    writeJson(join(productFailDir, "report.json"), { ...fixtureReport, suite_result: "FAIL" });
    const productFail = runNode(["enforce", "--evidence", evidence, "--engine", join(scratch, "no-engine"), "--report-dir", productFailDir], {
      LAB_EVENT_NAME: "pull_request",
      LAB_SUITE_INPUT: "full",
      LAB_SEED_INPUT: "1",
    });
    checks += 1;
    assertCheck(productFail.status === 1 && productFail.stdout.includes("suite_result FAIL"), `product fail output ${productFail.stdout}`);
    checks += 1;
    assertCheck(!productFail.stdout.includes("infrastructure FAIL"), "product fail was relabeled infrastructure");

    const fullDir = join(scratch, "full-report");
    writeJson(join(fullDir, "report.json"), { suite_result: "PASS" });
    const fullMissing = runNode(["enforce", "--evidence", evidence, "--engine", join(scratch, "no-engine"), "--report-dir", fullDir], {
      LAB_EVENT_NAME: "workflow_dispatch",
      LAB_SUITE_INPUT: "full",
      LAB_SEED_INPUT: "3",
    });
    checks += 1;
    assertCheck(fullMissing.status === 1 && fullMissing.stdout.includes("engine summary.json"), "full suite ignored a missing engine summary");

    const passDir = join(scratch, "pass-report");
    writeJson(join(evidence, "identity.json"), { ...fixtureIdentity, seed: 3 });
    writeJson(join(passDir, "report.json"), { ...fixtureReport, seed: 3, suite_result: "PASS" });
    const pass = runNode(["enforce", "--evidence", evidence, "--engine", staged, "--report-dir", passDir], {
      LAB_EVENT_NAME: "workflow_dispatch",
      LAB_SUITE_INPUT: "smoke",
      LAB_SEED_INPUT: "3",
    });
    checks += 1;
    assertCheck(pass.status === 0 && pass.stdout.trim() === "suite_result PASS", `clean pass output ${pass.stdout}`);
    checks += 1;
    assertCheck(!existsSync(join(passDir, "infrastructure-failure.txt")), "clean pass wrote an infrastructure failure");

    const reviewed = "e16c321b1eff1caea4ecf9c35eb9dc8fc995b6d1";
    const other = "a".repeat(40);
    const savedMismatchEvent = process.env.LAB_EVENT_NAME;
    const savedMismatchSuite = process.env.LAB_SUITE_INPUT;
    const savedMismatchSeed = process.env.LAB_SEED_INPUT;
    process.env.LAB_EVENT_NAME = "workflow_dispatch";
    process.env.LAB_SUITE_INPUT = "smoke";
    process.env.LAB_SEED_INPUT = "1";
    const mismatchEvidence = join(scratch, "mismatch-evidence");
    writeJson(join(mismatchEvidence, "identity.json"), {
      candidate_sha: reviewed,
      suite: "smoke",
      seed: 1,
      run_id: "rs016ed3f4",
    });
    writeJson(join(mismatchEvidence, "playwright.json"), { suites: [] });
    writeJson(join(mismatchEvidence, "self-test.json"), { cases: [] });
    const mismatchReport = join(scratch, "mismatch-report");
    const mismatchAggregate = aggregateReport({
      evidence: mismatchEvidence,
      engine: staged,
      reportDir: mismatchReport,
      candidate: other,
    });
    checks += 1;
    assertCheck(!mismatchAggregate.ok, "aggregation accepted evidence from another candidate");
    writeJson(join(mismatchReport, "report.json"), {
      suite_result: "PASS",
      suite: "smoke",
      candidate: other,
      seed: 1,
      run_id: "rs016ed3f4",
    });
    const mismatchEnforce = enforceReport({
      evidence: mismatchEvidence,
      engine: staged,
      reportDir: mismatchReport,
    });
    checks += 1;
    assertCheck(!mismatchEnforce.ok && mismatchEnforce.reasons.some((reason) => reason.includes(reviewed)), "enforcement accepted evidence from another candidate");
    if (savedMismatchEvent === undefined) delete process.env.LAB_EVENT_NAME;
    else process.env.LAB_EVENT_NAME = savedMismatchEvent;
    if (savedMismatchSuite === undefined) delete process.env.LAB_SUITE_INPUT;
    else process.env.LAB_SUITE_INPUT = savedMismatchSuite;
    if (savedMismatchSeed === undefined) delete process.env.LAB_SEED_INPUT;
    else process.env.LAB_SEED_INPUT = savedMismatchSeed;

    const savedEvent = process.env.LAB_EVENT_NAME;
    const savedSuite = process.env.LAB_SUITE_INPUT;
    const savedSeed = process.env.LAB_SEED_INPUT;
    process.env.LAB_EVENT_NAME = "workflow_dispatch";
    process.env.LAB_SUITE_INPUT = "smoke";
    process.env.LAB_SEED_INPUT = `1; touch ${pwned}`;
    const rejected = aggregateReport({
      evidence: join(scratch, "no-such-evidence"),
      engine: join(scratch, "no-such-engine"),
      reportDir: join(scratch, "rejected-report"),
      candidate: "068562a90bfafd9b2a142cea524a89574814cba5",
    });
    if (savedEvent === undefined) delete process.env.LAB_EVENT_NAME;
    else process.env.LAB_EVENT_NAME = savedEvent;
    if (savedSuite === undefined) delete process.env.LAB_SUITE_INPUT;
    else process.env.LAB_SUITE_INPUT = savedSuite;
    if (savedSeed === undefined) delete process.env.LAB_SEED_INPUT;
    else process.env.LAB_SEED_INPUT = savedSeed;
    checks += 1;
    assertCheck(rejected.reasons.some((reason) => reason.includes("seed must be an integer")), "aggregate accepted a bad seed");
    checks += 1;
    assertCheck(!fileReady(join(scratch, "rejected-report", "report.json")), "bad seed produced a report");
    checks += 1;
    assertCheck(fileReady(join(scratch, "rejected-report", "infrastructure-failure.txt")), "bad seed failure was not kept");
    checks += 1;
    assertCheck(!existsSync(pwned), "aggregate executed the seed");

    checks += 1;
    assertCheck(reportRunId({ run_id: "rs0123abcd" }, {}) === "rs0123abcd", "identity run id was dropped");
    checks += 1;
    assertCheck(reportRunId({ run_id: "unknown" }, { GITHUB_RUN_ID: "381", GITHUB_RUN_ATTEMPT: "2" }) === "gha381a2", "unknown identity was kept");
    checks += 1;
    assertCheck(reportRunId({ run_id: "nope;rm" }, { GITHUB_RUN_ID: "5", GITHUB_RUN_ATTEMPT: "1", LAB_RUN_ID: "rsdeadbeef" }) === "gha5a1", "unsafe identity reached the report");
    checks += 1;
    assertCheck(reportRunId(null, { GITHUB_RUN_ID: "5", GITHUB_RUN_ATTEMPT: "1", LAB_RUN_ID: "rsdeadbeef" }) === "gha5a1", "lab run id outranked the GitHub run");
    checks += 1;
    assertCheck(reportRunId(null, { LAB_RUN_ID: "rsdeadbeef" }) === "rsdeadbeef", "lab run id was dropped");
    checks += 1;
    assertCheck(reportRunId(null, { LAB_RUN_ID: "unknown", GITHUB_RUN_ID: "5" }) === `norun${process.pid}`, "unknown was emitted");
    checks += 1;
    assertCheck(reportRunId(null, {}) === `norun${process.pid}` && reportRunId(null, {}) !== "unknown", "pid fallback was unknown");

    const idRoot = join(scratch, "id-evidence");
    writeJson(join(idRoot, "identity.json"), {
      run_id: "rs0123abcd",
      candidate_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      suite: "smoke",
      seed: 1,
    });
    const idDir = join(scratch, "id-report");
    const idRun = runNode(
      ["aggregate", "--evidence", idRoot, "--engine", join(scratch, "no-engine"), "--report-dir", idDir, "--candidate", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
      { LAB_EVENT_NAME: "pull_request", LAB_SUITE_INPUT: "", LAB_SEED_INPUT: "", GITHUB_RUN_ID: "99", GITHUB_RUN_ATTEMPT: "1", LAB_RUN_ID: "" },
    );
    const idReport = readJson(join(idDir, "report.json"));
    checks += 1;
    assertCheck(idReport && idReport.run_id === "rs0123abcd", `identity report run id ${idReport && idReport.run_id} status ${idRun.status}`);

    const ghaDir = join(scratch, "gha-report");
    const gha = runNode(
      ["aggregate", "--evidence", join(scratch, "no-identity"), "--engine", join(scratch, "no-engine"), "--report-dir", ghaDir, "--candidate", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
      { LAB_EVENT_NAME: "pull_request", LAB_SUITE_INPUT: "", LAB_SEED_INPUT: "", GITHUB_RUN_ID: "99", GITHUB_RUN_ATTEMPT: "1", LAB_RUN_ID: "" },
    );
    const ghaReport = readJson(join(ghaDir, "report.json"));
    checks += 1;
    assertCheck(gha.status === 1 && !ghaReport, "missing identity must not be replaced by a GitHub fallback");
    checks += 1;
    assertCheck(fileReady(join(ghaDir, "infrastructure-failure.txt")), "missing identity failure was not recorded");

    console.log(`ci-gate self-check PASS ${checks}`);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

function main() {
  const command = process.argv[2];
  if (command === "resolve") commandResolve();
  else if (command === "note-publish") failIf(notePublish(flag("--root")));
  else if (command === "require-publish") failIf(assessPublish(flag("--root"), selectionFromEnv()));
  else if (command === "stage-engine") failIf(stageEngine(flag("--root"), flag("--dest")));
  else if (command === "require-engine") {
    if (!fileReady(join(flag("--dest"), "summary.json"))) {
      console.log("infrastructure FAIL: missing required runtime artifacts: engine summary.json");
      process.exit(1);
    }
  } else if (command === "aggregate") commandAggregate();
  else if (command === "enforce") commandEnforce();
  else if (command === "self-check") commandSelfCheck();
  else {
    console.error("usage: node ci-gate.mjs resolve|note-publish|require-publish|stage-engine|require-engine|aggregate|enforce|self-check");
    process.exit(2);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
