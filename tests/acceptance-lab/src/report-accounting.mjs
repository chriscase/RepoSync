// Additive report.v1 accounting designed for the PR195 correction. A selected
// executor result never removes an obligation from whole-plan qualification.
export function operationSelected(suite, operation) {
  if (operation.suite === "soak") return suite === "soak";
  if (suite === "smoke") return operation.suite === "smoke";
  if (suite === "full") return operation.suite === "smoke" || operation.suite === "full";
  return false;
}

export function validateQualificationPlan(plan) {
  if (plan?.schema !== "reposync.acceptance-lab.qualification-plan.v1"
      || !Array.isArray(plan.required_case_ids) || plan.required_case_ids.length === 0
      || plan.required_case_ids.some((id) => typeof id !== "string" || id.trim() !== id || !id)
      || new Set(plan.required_case_ids).size !== plan.required_case_ids.length
      || !plan.r_criteria_by_case || typeof plan.r_criteria_by_case !== "object" || Array.isArray(plan.r_criteria_by_case)
      || plan.required_case_ids.some((id) => !Array.isArray(plan.r_criteria_by_case[id])
        || plan.r_criteria_by_case[id].some((criterion) => typeof criterion !== "string")
        || new Set(plan.r_criteria_by_case[id]).size !== plan.r_criteria_by_case[id].length)) {
    throw new Error("qualification plan must contain unique required_case_ids, retained criterion arrays for every id, and a supported schema");
  }
  return plan;
}

// Validate observations before any Map merges them. Invalid or duplicate raw
// observations become FAIL rows and diagnostics, regardless of arrival order.
export function validateExecutionInputs(groups) {
  const observations = [];
  const errors = [];
  const counts = new Map();
  const usedIds = new Set(groups.flatMap((group) => Array.isArray(group.rows)
    ? group.rows.map((row) => row?.id).filter((id) => typeof id === "string") : []));
  const statuses = new Set(["PASS", "FAIL", "PARTIAL", "NOT_RUN", "SKIP"]);
  for (const { source, rows } of groups) {
    if (!Array.isArray(rows)) {
      errors.push({ source, code: "invalid_collection" });
      continue;
    }
    for (const row of rows) {
      let id = row?.id;
      const problems = [];
      if (typeof id !== "string" || !id || id.trim() !== id) {
        let suffix = observations.length;
        do { id = `INPUT_INVALID_ID_${suffix++}`; } while (usedIds.has(id));
        usedIds.add(id);
        problems.push("invalid_identity");
      }
      const status = source === "real-engine-suite" && typeof row?.status === "string"
        ? row.status.toUpperCase().replace(" ", "_") : row?.status;
      if (!statuses.has(status)) problems.push("invalid_status");
      for (const code of problems) errors.push({ source, id, code, reported_status: row?.status });
      counts.set(id, (counts.get(id) || 0) + 1);
      observations.push({ ...row, id, source, status, problems });
    }
  }
  const rows = observations.map(({ problems, ...row }) => {
    if (counts.get(row.id) > 1) {
      problems.push("duplicate_identity");
      errors.push({ source: row.source, id: row.id, code: "duplicate_identity", reported_status: row.status });
    }
    return problems.length ? {
      ...row, reported_status: row.status, status: "FAIL", qualification: "FAIL",
      reason: `${row.reason ? row.reason + "; " : ""}invalid execution input: ${problems.join(", ")}`,
    } : row;
  });
  return { rows, errors };
}

function countsFor(rows) {
  const counts = { PASS: 0, FAIL: 0, PARTIAL: 0, NOT_RUN: 0, SKIP: 0 };
  for (const row of rows) counts[row.status] = (counts[row.status] || 0) + 1;
  return counts;
}

function passes(row) {
  return row?.status === "PASS" && row.qualification !== "UNQUALIFIED";
}

export function accountReport({ rows, selectedIds, requiredIds, blocked = false }) {
  const byId = new Map(rows.map((row) => [row.id, row]));
  const selected = [...new Set(selectedIds)];
  const selectedRows = selected.map((id) => byId.get(id)).filter(Boolean);
  const unique = byId.size === rows.length;
  const selectedPass = unique && !blocked && selected.length > 0 && selected.every((id) => passes(byId.get(id)));
  const wholePass = unique && !blocked && requiredIds.length > 0 && rows.length > 0
    && requiredIds.every((id) => passes(byId.get(id))) && rows.every(passes);
  return {
    counts: countsFor(rows),
    overall: wholePass ? "PASS" : "FAIL",
    selected_suite_result: selectedPass ? "PASS" : "FAIL",
    selected_suite_counts: countsFor(selectedRows),
    selected_suite_ids: selected,
  };
}
