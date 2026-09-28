// Real Node consumer of serialized candidate HTTP bytes. The `next` mode
// builds the next request from the returned cursor; Rust dispatches it.
import assert from 'node:assert/strict';
import fs from 'node:fs';

const SAFE = BigInt(Number.MAX_SAFE_INTEGER);
function safeIntegers(raw) {
  let quoted = false;
  let escaped = false;
  for (let i = 0; i < raw.length; i += 1) {
    const char = raw[i];
    if (quoted) {
      if (escaped) escaped = false;
      else if (char === '\\') escaped = true;
      else if (char === '"') quoted = false;
      continue;
    }
    if (char === '"') { quoted = true; continue; }
    if (char !== '-' && !/[0-9]/.test(char)) continue;
    let end = i + 1;
    while (end < raw.length && /[0-9eE+.-]/.test(raw[end])) end += 1;
    const token = raw.slice(i, end);
    if (!/^-?(0|[1-9][0-9]*)$/.test(token)) throw new Error('unsupported numeric token in copy-read v1');
    const value = BigInt(token);
    if (value > SAFE || value < -SAFE) throw new Error('unsafe integer in copy-read v1');
    i = end - 1;
  }
}

function decode(wire, operation, schema = 'reposync.copy_read.scoped.v1') {
  assert.equal(wire.status, 200, wire.label);
  assert.match(wire.content_type, /^application\/json/, wire.label);
  safeIntegers(wire.body);
  const response = JSON.parse(wire.body);
  assert.equal(response.schema, schema, wire.label);
  assert.equal(response.request.operation, operation, wire.label);
  assert.equal(response.data.kind, operation, wire.label);
  return response;
}

const mode = process.argv[2];
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
if (mode === 'next') {
  const response = decode(input.wire, 'list');
  const ids = response.data.rows.map((row) => row.legacy.id);
  const cursor = response.data.next_after_id;
  const nextPath = cursor === null ? null :
    `${input.base}/list?repository=${encodeURIComponent(response.request.repository)}` +
    `&generation=${response.request.generation}&direction=${response.request.direction}` +
    `&after_id=${cursor}&limit=${response.request.limit}`;
  // `decode` checked every numeric token before either ID or cursor was used.
  console.log(JSON.stringify({ ids, next_path: nextPath }));
} else if (mode === 'verify') {
  const wires = input.wires;
  const byLabel = new Map(wires.map((wire) => [wire.label, wire]));
  assert.equal(byLabel.size, wires.length);
  const checked = new Set();
  function get(label, operation, schema) {
    const wire = byLabel.get(label);
    assert.ok(wire, label);
    checked.add(label);
    return decode(wire, operation, schema);
  }
  function rejected(label, status) {
    const wire = byLabel.get(label);
    assert.ok(wire, label);
    assert.equal(wire.status, status, label);
    assert.doesNotMatch(wire.body, /OWNERLESS_CANARY|A_CANARY|B_CANARY|reposync\.db|CREATE TABLE|SELECT .* FROM|fixture-password/i);
    if (status === 403) assert.deepEqual(JSON.parse(wire.body), { error: 'forbidden' });
    if (status === 401 && label !== 'v12_populated_unauthenticated') {
      assert.deepEqual(JSON.parse(wire.body), { error: 'unauthorized' });
    }
    checked.add(label);
  }
  for (const [actor, own, hidden] of [['a', 'A_CANARY', 'B_CANARY'], ['b', 'B_CANARY', 'A_CANARY']]) {
    for (const op of ['lookup', 'list', 'status', 'last-emitted']) {
      const label = `${actor}_${op}`;
      const result = get(label, op.replace('-', '_'));
      assert.doesNotMatch(byLabel.get(label).body, new RegExp(`${hidden}|OWNERLESS_CANARY`));
      if (op === 'lookup') assert.match(byLabel.get(label).body, new RegExp(own));
      if (op === 'status') assert.equal(result.data.scope.state, 'qualified');
      if (op === 'last-emitted') assert.equal(result.data.result.scope.state, 'qualified');
      rejected(`${actor}_denied_${op}`, 403);
    }
  }
  const ownerlessOnly = get('a_ownerless_only', 'lookup');
  assert.deepEqual(ownerlessOnly.data.canonical, { state: 'unresolved', reason: 'scoped_view_incomplete' });
  assert.deepEqual(ownerlessOnly.data.legacy, []);
  assert.doesNotMatch(byLabel.get('a_ownerless_only').body, /OWNERLESS_CANARY|B_CANARY/);
  assert.match(byLabel.get('operator_diagnostic').body, /OWNERLESS_CANARY/);
  get('operator_diagnostic', 'lookup', 'reposync.copy_read.v1');
  assert.match(byLabel.get('operator_diagnostic_list').body, /OWNERLESS_CANARY/);
  get('operator_diagnostic_list', 'list', 'reposync.copy_read.v1');
  for (const label of ['diagnostic_revoked', 'role_downgraded']) {
    get(label, 'lookup');
    assert.doesNotMatch(byLabel.get(label).body, /OWNERLESS_CANARY|B_CANARY/);
  }
  assert.equal(get('a_wrong_generation', 'status').data.scope.state, 'missing_generation');
  assert.equal(get('a_missing_generation', 'status').data.scope.state, 'missing_generation');
  assert.equal(get('a_disabled_repository', 'status').data.scope.state, 'disabled');
  assert.equal(get('other_copy_unqualified', 'status').data.scope.state, 'not_qualified');

  const pages = wires.filter((wire) => /^scoped_page_[0-9]+$/.test(wire.label))
    .sort((a, b) => Number(a.label.slice(12)) - Number(b.label.slice(12)));
  assert.ok(pages.length >= 3);
  const ids = [];
  let preceding = -11;
  for (let i = 0; i < pages.length; i += 1) {
    assert.equal(pages[i].label, `scoped_page_${i}`);
    const response = get(pages[i].label, 'list');
    assert.equal(response.request.after_id, preceding);
    assert.doesNotMatch(pages[i].body, /OWNERLESS_CANARY|B_CANARY/);
    ids.push(...response.data.rows.map((row) => row.legacy.id));
    preceding = response.data.next_after_id;
    if (i < pages.length - 1) assert.notEqual(preceding, null);
  }
  assert.deepEqual(ids, input.expected_ids);
  assert.equal(new Set(ids).size, ids.length);
  assert.ok(ids.includes(-8) && ids.includes(0));
  assert.deepEqual(decode(pages.at(-1), 'list').data.rows, []);
  assert.equal(preceding, null);

  for (const label of ['no_grant', 'unknown_repo', 'other_copy_denied', 'grant_revoked', 'policy_failure']) rejected(label, 403);
  for (const label of ['operator_logged_out', 'disabled_principal', 'expired_named_session',
    'missing_principal', 'malformed_expiry', 'legacy_token_refused', 'anonymous',
    'b_disabled_after_success', 'a_expired_after_success']) rejected(label, 401);
  for (const label of ['query_path_refused', 'query_principal_refused', 'direction_refused']) rejected(label, 400);
  rejected('method_refused', 405);

  const populated = byLabel.get('v12_populated');
  assert.equal(populated.status, 200);
  assert.match(populated.content_type, /^application\/json/);
  const v12 = JSON.parse(populated.body);
  assert.equal(v12.total, 2);
  assert.deepEqual(v12.entries.map((entry) => entry.id), [502, 501]);
  for (const entry of v12.entries) {
    assert.equal(entry.repo_id, 'pair');
    assert.equal(entry.git_sha.length, 40);
    assert.ok(entry.svn_author.startsWith('Old '));
    assert.equal(entry.git_author, 'old-git-author');
    assert.equal(entry.synced_at, '2020-01-01T00:00:00Z');
    assert.equal(entry.direction, 'svn_to_git');
    assert.ok(Number.isInteger(entry.svn_rev));
  }
  checked.add('v12_populated');
  const filtered = byLabel.get('v12_filtered');
  assert.equal(filtered.status, 200);
  assert.deepEqual(JSON.parse(filtered.body).entries.map((entry) => entry.id), [502]);
  checked.add('v12_filtered');
  const unfiltered = byLabel.get('v12_unfiltered');
  assert.equal(unfiltered.status, 200);
  assert.deepEqual(JSON.parse(unfiltered.body).entries.map((entry) => entry.id), [503, 502, 501]);
  checked.add('v12_unfiltered');
  rejected('v12_populated_unauthenticated', 401);
  rejected('default_candidate_absent_scoped', 404);
  assert.equal(checked.size, wires.length);
  assert.throws(() => safeIntegers('{"next_after_id":9007199254740993}'), /unsafe integer/);
  assert.throws(() => safeIntegers('{"next_after_id":-9007199254740993}'), /unsafe integer/);
  console.log(JSON.stringify({ status: 'PASS', checked_http_responses: checked.size,
    actor_routes: 16, response_driven_pages: pages.length, permitted_ids: ids,
    ownerless_neighbor_nondisclosure: true, populated_v12: true, unsafe_cursor_refusals: 2 }));
} else {
  throw new Error('unknown scoped consumer mode');
}
