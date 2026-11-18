// Execute an instrumented copy of the shipped catalog registration with no
// browser or services. Preserve its execution mode, callbacks and annotations.
import {mkdtempSync, readFileSync, writeFileSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import assert from 'node:assert/strict';
import {walkPlaywright} from './aggregate.mjs';

const lab = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = mkdtempSync(join(tmpdir(), 'lab-continuation-'));
try {
  let source = readFileSync(join(lab, 'specs/catalog.spec.ts'), 'utf8');
  source = source.replace('import { test } from "@playwright/test";', `import {test as base} from ${JSON.stringify(join(lab, 'node_modules/@playwright/test/index.mjs'))};\nconst test = base.extend({page: async ({}, use) => use({on() {}})});`);
  source = source.replace(/import catalog from .*?;\n/, `const catalog = {operations: ${JSON.stringify(['LAB_GITEA_API_NOT_GHE', 'FIXTURE_REAL_FAILURE', 'FIXTURE_AFTER_FAILURE'].map(id => ({id, suite: 'full', disposition: 'execute', title: id})))} };\n`);
  source = source.replace('import { runCase } from "../playwright/handlers.ts";', `import {runCase as actualRunCase} from ${JSON.stringify(join(lab, 'playwright/handlers.ts'))};\nconst runCase = async (lab, op) => { if (op.id === 'FIXTURE_REAL_FAILURE') throw new Error('intentional continuation fixture'); if (op.id === 'FIXTURE_AFTER_FAILURE') {lab.pass('later case executed'); return;} await actualRunCase(lab, op); };`);
  source = source.replace('import { Lab } from "../playwright/runtime.ts";', `import {Lab as RealLab} from ${JSON.stringify(join(lab, 'playwright/runtime.ts'))};\nclass Lab {gitUser = 'fixture_admin'; constructor(page, info) {this.info = info;} async enableLdap() {} async sh() {return '{"login":"fixture_admin"}';} mark(...args) {RealLab.prototype.mark.apply(this, args);} pass(...args) {RealLab.prototype.pass.apply(this, args);} }`);
  writeFileSync(join(root, 'catalog.spec.ts'), source);
  const report = join(root, 'playwright.json');
  writeFileSync(join(root, 'config.ts'), `import config from ${JSON.stringify(join(lab, 'playwright.config.ts'))};\nexport default {...config, testDir: ${JSON.stringify(root)}, testMatch: 'catalog.spec.ts', workers: 1, outputDir: ${JSON.stringify(join(root, 'results'))}, reporter: [['json', {outputFile: ${JSON.stringify(report)}}]]};\n`);
  const child = spawnSync(process.execPath, [join(lab, 'node_modules/@playwright/test/cli.js'), 'test', '-c', join(root, 'config.ts')], {env: {...process.env, LAB_SUITES: 'full'}, encoding: 'utf8', timeout: 60_000});
  assert.equal(child.status, 1, child.stderr || child.stdout);
  const walked = walkPlaywright(JSON.parse(readFileSync(report, 'utf8')));
  const byId = new Map(walked.cases.map(row => [row.id, row]));
  assert.equal(byId.get('LAB_GITEA_API_NOT_GHE')?.status, 'PARTIAL');
  assert.equal(byId.get('LAB_GITEA_API_NOT_GHE')?.qualification, 'UNQUALIFIED');
  assert.equal(byId.get('FIXTURE_REAL_FAILURE')?.status, 'FAIL');
  assert.equal(byId.get('FIXTURE_AFTER_FAILURE')?.status, 'PASS');
  assert.equal(walked.cases.length, 3);
  assert.equal(walked.skipped.length, 0);
  console.log('catalog continuation PASS: GHE unqualified, real failure retained, later case executed');
} finally { rmSync(root, {recursive: true, force: true}); }
