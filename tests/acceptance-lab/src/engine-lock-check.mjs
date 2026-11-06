import {createHash} from 'node:crypto';
import {existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {installEngineLock} from './engine-lock.mjs';

const root = mkdtempSync(join(tmpdir(), 'lab-engine-lock-'));
const candidate = 'a'.repeat(40), bytes = Buffer.from('version = 4\n'), hash = createHash('sha256').update(bytes).digest('hex');
let checks = 0;
try {
  for (const fault of ['control', 'existing-same', 'wrong-candidate', 'wrong-image', 'missing-id', 'missing-lock', 'empty-lock', 'wrong-checksum', 'wrong-image-checksum', 'missing-checksum', 'existing-different']) {
    const graph = join(root, fault); mkdirSync(graph);
    writeFileSync(join(graph, 'image-identity.txt'), `candidate=${fault === 'wrong-candidate' ? 'b'.repeat(40) : candidate}\n`);
    writeFileSync(join(graph, 'reposync-image.txt'), `reposync_image=rsacc-reposync:${fault === 'wrong-image' ? 'b'.repeat(40) : candidate}\nreposync_id=${fault === 'missing-id' ? '' : 'sha256:' + 'c'.repeat(64)} size=1\nlock_sha256=${fault === 'wrong-image-checksum' ? 'd'.repeat(64) : hash}\n`);
    if (fault !== 'missing-lock') writeFileSync(join(graph, 'Cargo.lock'), fault === 'empty-lock' ? '' : bytes);
    if (fault !== 'missing-checksum') writeFileSync(join(graph, 'Cargo.lock.sha256'), `${fault === 'wrong-checksum' ? 'd'.repeat(64) : hash} Cargo.lock\n`);
    const destination = join(graph, 'installed.lock'), receipt = join(graph, 'receipt.json');
    if (fault.startsWith('existing-')) writeFileSync(destination, fault === 'existing-same' ? bytes : 'different');
    const before = existsSync(destination) ? readFileSync(destination) : null;
    const call = () => installEngineLock({graph, candidate, destination, receipt});
    if (fault === 'control' || fault === 'existing-same') {
      assert.equal(call().lock_sha256, hash); assert.deepEqual(readFileSync(destination), bytes); assert.equal(JSON.parse(readFileSync(receipt)).candidate, candidate);
    } else {
      assert.throws(call); assert.equal(existsSync(receipt), false);
      if (before) assert.deepEqual(readFileSync(destination), before); else assert.equal(existsSync(destination), false);
    }
    checks++;
  }
  console.log(`engine lock checks PASS ${checks}`);
} finally { rmSync(root, {recursive: true, force: true}); }
