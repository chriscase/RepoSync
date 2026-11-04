import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { expect, it } from 'vitest';

it('runs the unchanged Rust-hosted Node strip-types behavioral scripts without a bundler or loader', () => {
  const root = fileURLToPath(new URL('../../', import.meta.url));
  const source = readFileSync(new URL('../../crates/web/tests/repo_detail_removal_ui.rs', import.meta.url), 'utf8');
  const scripts = [...source.matchAll(/let script = r#"([\s\S]*?)"#;/g)].map(match => match[1]);
  expect(scripts).toHaveLength(2);
  for (const script of scripts) {
    const result = spawnSync(process.execPath, ['--experimental-strip-types', '--input-type=module', '-e', script], {
      cwd: root, encoding: 'utf8', timeout: 10_000, maxBuffer: 1024 * 1024,
    });
    expect(result.error, result.stderr).toBeUndefined();
    expect(result.signal, result.stderr).toBeNull();
    expect(result.status, result.stderr).toBe(0);
  }
});
