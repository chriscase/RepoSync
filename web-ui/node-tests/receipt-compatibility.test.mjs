import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import test from 'node:test';

test('unchanged Rust-hosted Node strip-types behavioral scripts run without a bundler or loader', () => {
  const root = fileURLToPath(new URL('../../', import.meta.url));
  const source = readFileSync(new URL('../../crates/web/tests/repo_detail_removal_ui.rs', import.meta.url), 'utf8');
  const scripts = [...source.matchAll(/let script = r#"([\s\S]*?)"#;/g)].map(match => match[1]);
  assert.equal(scripts.length, 2);
  for (const script of scripts) {
    const result = spawnSync(process.env.REPOSYNC_NODE_BIN ?? process.execPath, ['--experimental-strip-types', '--input-type=module', '-e', script], {
      cwd: root, encoding: 'utf8', timeout: 10_000, maxBuffer: 1024 * 1024,
    });
    assert.ifError(result.error);
    assert.equal(result.signal, null, result.stderr);
    assert.equal(result.status, 0, result.stderr);
  }
});
