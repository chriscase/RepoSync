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
      || new Set(plan.required_case_ids).size !== plan.required_case_ids.length) {
    throw new Error("qualification plan must contain a nonempty, unique required_case_ids list and a supported schema");
  }
  return plan;
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
