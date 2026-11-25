import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

// The real-engine suite is outside this reservation. It can fail while building
// and still leave an older summary.json under the shared artifact tree.
// This selector keeps those files and never turns them into the current run.
export function currentEngineDocument({
  preexisting,
  discovered,
  exitCode,
  candidate,
  runId,
  buildGraph,
  readSummary,
}) {
  const prior = new Set(preexisting);
  const fresh = discovered.filter((path) => !prior.has(path));
  const retained = [...preexisting];
  const failure = (reason, code) => ({
    adopt: false,
    retained,
    exitCode: code,
    document: {
      candidate,
      run_id: runId,
      build_graph: buildGraph || "absent",
      invocation: "current",
      process_exit: exitCode,
      scenarios: [],
      overall: "FAIL",
      reason,
    },
  });
  if (!Number.isInteger(exitCode)) return failure("engine exit was not recorded", 1);
  if (exitCode !== 0) {
    return failure("process failure; preexisting summaries were not adopted", exitCode);
  }
  if (fresh.length !== 1) {
    return failure(fresh.length === 0 ? "no current-invocation summary" : "ambiguous current-invocation summaries", 1);
  }
  let summary;
  try {
    summary = readSummary(fresh[0]);
  } catch (error) {
    return failure(`current summary was unreadable: ${error.message}`, 1);
  }
  if (!summary || typeof summary !== "object" || Array.isArray(summary)) {
    return failure("current summary was not an object", 1);
  }
  if (summary.candidate && summary.candidate !== candidate) return failure("candidate mismatch", 1);
  if (summary.run_id && summary.run_id !== runId) return failure("run mismatch", 1);
  if (buildGraph && summary.build_graph && summary.build_graph !== buildGraph) return failure("build-graph mismatch", 1);
  const rows = Array.isArray(summary.scenarios) ? summary.scenarios : Array.isArray(summary.results) ? summary.results : null;
  if (!rows) return failure("current summary has no scenario collection", 1);
  const passRows = rows.filter((row) => String(row?.status || "").toUpperCase().replace(" ", "_") === "PASS");
  const overall = String(summary.overall || "PASS").toUpperCase().replace(" ", "_");
  if (passRows.length > 0 && overall !== "PASS") return failure("passing rows under a failed overall", 1);
  return {
    adopt: true,
    retained,
    exitCode: 0,
    document: {
      ...summary,
      candidate,
      run_id: runId,
      build_graph: buildGraph || summary.build_graph || "absent",
      invocation: "current",
    },
  };
}

function flag(args, name) {
  const index = args.indexOf(name);
  return index === -1 ? "" : args[index + 1] || "";
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  if (!args.includes("--out")) process.exit(0);
  const lines = (path) => readFileSync(path, "utf8").split("\n").map((line) => line.trim()).filter(Boolean);
  const exitCode = Number(flag(args, "--exit"));
  const result = currentEngineDocument({
    preexisting: lines(flag(args, "--before")),
    discovered: lines(flag(args, "--after")),
    exitCode,
    candidate: flag(args, "--candidate"),
    runId: flag(args, "--run-id"),
    buildGraph: flag(args, "--build-graph"),
    readSummary: (path) => JSON.parse(readFileSync(path, "utf8")),
  });
  writeFileSync(flag(args, "--out"), `${JSON.stringify(result.document, null, 2)}\n`);
  const exitOut = flag(args, "--exit-out");
  if (exitOut) {
    writeFileSync(exitOut, `engine_exit=${exitCode}\nadopted=${result.adopt ? "yes" : "no"}\n`);
  }
  process.exit(result.exitCode);
}
