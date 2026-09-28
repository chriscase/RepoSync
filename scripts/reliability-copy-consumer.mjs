// Actual JavaScript consumer of Axum's serialized copy-read HTTP body bytes.
// v1 uses JSON integer tokens for i64. Refuse unsafe tokens before JSON.parse
// can round them; later UI work needs an explicit versioned lossless transport.
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

function decode(raw, operation) {
  safeIntegers(raw);
  const response = JSON.parse(raw);
  if (response.schema !== 'reposync.copy_read.v1') throw new Error('unsupported copy-read version');
  if (response.request?.operation !== operation || response.data?.kind !== operation) {
    throw new Error('copy-read operation mismatch');
  }
  return response;
}

// The transport wrapper contains only small HTTP status numbers and quoted
// response bodies. Every response body passes the integer check independently.
const wires = JSON.parse(fs.readFileSync(0, 'utf8'));
const byLabel = new Map(wires.map((wire) => [wire.label, wire]));
assert.equal(byLabel.size, wires.length);
const checked = [];

function get(label, operation) {
  const wire = byLabel.get(label);
  assert.ok(wire, label);
  assert.equal(wire.status, 200, label);
  assert.match(wire.content_type, /^application\/json/, label);
  const response = decode(wire.body, operation);
  checked.push(label);
  return response;
}

function rejected(label, status) {
  const wire = byLabel.get(label);
  assert.ok(wire, label);
  assert.equal(wire.status, status, label);
  assert.doesNotMatch(wire.body, /\/etc\/passwd|reposync\.db|CREATE TABLE|SELECT .* FROM|structural_fixture_only|fixture-password/i);
  checked.push(label);
}

const inMapped = get('lookup_in_mapped', 'lookup');
assert.equal(inMapped.request.repository, 'pair');
assert.equal(inMapped.request.generation, 1);
assert.equal(inMapped.request.direction, 'svn_to_git');
assert.equal(inMapped.data.canonical.state, 'mapped');
assert.equal(inMapped.data.canonical.target.value, 'd'.repeat(40));
assert.equal(inMapped.data.canonical.authority, 'http-in-applied');

const outMapped = get('lookup_out_mapped', 'lookup');
assert.equal(outMapped.request.direction, 'git_to_svn');
assert.equal(outMapped.data.canonical.state, 'mapped');
assert.deepEqual(outMapped.data.canonical.target, { kind: 'svn', value: 3 });

assert.equal(get('lookup_in_no_target', 'lookup').data.canonical.state, 'no_target');
assert.equal(get('lookup_out_no_target', 'lookup').data.canonical.outcome, 'semantic_no_delta');
const unknown = get('lookup_missing_unknown', 'lookup');
assert.equal(unknown.data.canonical.state, 'missing');
assert.deepEqual(unknown.data.nonhandled.records.map((r) => r.state).sort(), ['effect_unknown', 'pending']);
const retrySafe = false; // No canonical mapping is not permission to replay.
assert.equal(retrySafe, false);

const nullRow = get('lookup_null', 'lookup').data.legacy[0];
assert.equal(nullRow.git_sha.type, 'null');
assert.deepEqual(nullRow.svn_author, { type: 'text', value: '' });
const duplicate = get('lookup_duplicate', 'lookup');
assert.equal(duplicate.data.canonical.state, 'unresolved');
assert.equal(duplicate.data.legacy.length, 2);
assert.equal(duplicate.data.legacy.find((r) => r.id === 204).git_sha.value, '<img src=x onerror=alert(1)>');
const ownerless = get('lookup_ownerless', 'lookup');
assert.equal(ownerless.data.canonical.state, 'unresolved');
assert.equal(ownerless.data.legacy[0].repo_id.type, 'null');
assert.equal(get('lookup_wrong_gen', 'lookup').data.scope.state, 'missing_generation');
assert.equal(get('lookup_missing_gen', 'lookup').data.scope.state, 'missing_generation');
assert.equal(get('lookup_disabled', 'lookup').data.scope.state, 'disabled');
assert.equal(get('lookup_missing_repo', 'lookup').data.scope.state, 'missing_repository');
assert.equal(get('not_qualified', 'lookup').data.scope.state, 'not_qualified');

const pairStatus = get('status_pair', 'status').data;
assert.equal(pairStatus.scope.state, 'qualified');
assert.equal(pairStatus.frontiers.length, 2);
for (const [direction, target] of [['svn_to_git', { kind: 'git', value: 'd'.repeat(40) }], ['git_to_svn', { kind: 'svn', value: 3 }]]) {
  const frontier = pairStatus.frontiers.find((item) => item.direction === direction);
  assert.ok(frontier);
  assert.equal(frontier.current_target, null);
  assert.deepEqual(frontier.last_emitted.target, target);
}
assert.equal(get('status_neighbor', 'status').data.frontiers.length, 2);
assert.deepEqual(get('last_in', 'last_emitted').data.result.target, { kind: 'git', value: 'd'.repeat(40) });
assert.deepEqual(get('last_out', 'last_emitted').data.result.target, { kind: 'svn', value: 3 });

const signed = get('page_signed', 'list');
assert.deepEqual(signed.data.rows.map((row) => row.legacy.id), [-10, 0]);
assert.equal(signed.data.next_after_id, 0);
const continuation = get('page_continue', 'list');
assert.deepEqual(continuation.data.rows.map((row) => row.legacy.id), [200, 201]);
assert.equal(continuation.data.next_after_id, 201);
const empty = get('page_empty', 'list');
assert.deepEqual(empty.data.rows, []);
assert.equal(empty.data.next_after_id, null);
for (const label of ['page_unsafe_positive', 'page_unsafe_negative']) {
  const wire = byLabel.get(label);
  assert.equal(wire.status, 200);
  assert.match(wire.content_type, /^application\/json/);
  assert.throws(() => decode(wire.body, 'list'), /unsafe integer/);
  checked.push(label);
}
assert.doesNotThrow(() => safeIntegers('{"text":"9007199254740993"}'));
assert.throws(() => safeIntegers('{"cursor":9007199254740993}'), /unsafe integer/);
assert.throws(() => safeIntegers('{"cursor":-9007199254740993}'), /unsafe integer/);
assert.throws(() => decode(JSON.stringify({ ...inMapped, schema: 'reposync.copy_read.v2' }), 'lookup'), /version/);
assert.throws(() => decode(JSON.stringify({ ...inMapped, request: { ...inMapped.request, operation: 'list' } }), 'lookup'), /operation mismatch/);

rejected('unauthenticated', 401);
rejected('invalid_session', 401);
rejected('invalid_source', 400);
rejected('path_selection_refused', 400);
rejected('post_refused', 405);
rejected('default_candidate_absent', 404);
const v12 = byLabel.get('v12_commit_map');
assert.equal(v12.status, 200);
assert.match(v12.content_type, /^application\/json/);
assert.deepEqual(JSON.parse(v12.body), { entries: [], total: 0 });
checked.push('v12_commit_map');
rejected('v12_unauthenticated', 401);

assert.equal(checked.length, wires.length);
console.log(JSON.stringify({ status: 'PASS', checks: checked, checked_http_responses: checked.length,
  unsafe_i64_refusals: 2, signed_zero_continuation: true, default_v12_compatibility: true,
  missing_does_not_imply_retry_safety: true }));
