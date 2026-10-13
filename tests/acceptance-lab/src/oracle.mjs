/** Tree, mapping, and directory oracles. Pure functions so the harness can prove it sees bad fixtures. */

export function compareTrees(expected, actual) {
  const missing = [];
  const changed = [];
  const extra = [];
  for (const [path, content] of Object.entries(expected || {})) {
    if (!Object.prototype.hasOwnProperty.call(actual || {}, path)) missing.push(path);
    else if (actual[path] !== content) changed.push(path);
  }
  for (const path of Object.keys(actual || {})) {
    if (!Object.prototype.hasOwnProperty.call(expected || {}, path)) extra.push(path);
  }
  missing.sort();
  changed.sort();
  extra.sort();
  return { ok: missing.length === 0 && changed.length === 0 && extra.length === 0, missing, changed, extra };
}

export function mappingKey(row) {
  return [row.repo_id || "", row.direction || "", String(row.svn_rev), row.git_sha || ""].join("|");
}

export function duplicateMappings(rows) {
  const seen = new Map();
  const dupes = [];
  for (const row of rows || []) {
    const key = mappingKey(row);
    if (seen.has(key)) dupes.push(row);
    else seen.set(key, row);
  }
  const byRev = new Map();
  for (const row of rows || []) {
    const key = `${row.repo_id || ""}|${row.direction || ""}|${row.svn_rev}`;
    if (!byRev.has(key)) byRev.set(key, []);
    byRev.get(key).push(row.git_sha);
  }
  const conflicting = [];
  for (const [key, shas] of byRev) {
    const unique = [...new Set(shas)];
    if (unique.length > 1) conflicting.push({ key, shas: unique });
  }
  return { duplicate_rows: dupes, conflicting_revisions: conflicting };
}

export function missingMappings(expected, rows) {
  const have = new Set((rows || []).map(mappingKey));
  return (expected || []).filter((row) => !have.has(mappingKey(row)));
}

export function directoryUsable(stat) {
  if (!stat || stat.exists !== true) return { ok: false, reason: "missing" };
  if (stat.readable !== true) return { ok: false, reason: "unreadable" };
  if (stat.is_directory !== true) return { ok: false, reason: "not_a_directory" };
  return { ok: true, reason: "" };
}

export function assessBrowser(versionText) {
  const text = String(versionText || "").trim();
  if (!text) return { ok: false, reason: "missing_browser" };
  if (!/chrome|chromium|playwright/i.test(text)) return { ok: false, reason: "unrecognized_browser" };
  return { ok: true, reason: "" };
}

export function assessServices(services) {
  const failed = [];
  for (const [name, status] of Object.entries(services || {})) {
    if (status !== "healthy" && status !== "running") failed.push({ name, status: status || "missing" });
  }
  if (Object.keys(services || {}).length === 0) failed.push({ name: "*", status: "zero_services" });
  return { ok: failed.length === 0, failed };
}

export function logicalTree(changes) {
  const tree = {};
  for (const change of changes || []) {
    if (change.kind === "delete") delete tree[change.path];
    else if (change.kind === "rename") {
      if (change.from) delete tree[change.from];
      tree[change.path] = change.content ?? tree[change.from];
    } else tree[change.path] = change.content;
  }
  return tree;
}

export function mulberry32(seed) {
  let value = Number(seed) >>> 0;
  return function next() {
    value |= 0;
    value = (value + 0x6d2b79f5) | 0;
    let t = Math.imul(value ^ (value >>> 15), 1 | value);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
