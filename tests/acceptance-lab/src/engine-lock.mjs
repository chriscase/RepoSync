import {createHash} from 'node:crypto';
import {copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {pathToFileURL} from 'node:url';

export function installEngineLock({graph, candidate, destination, receipt}) {
  if (!/^[0-9a-f]{40}$/.test(candidate || '')) throw new Error('invalid engine candidate');
  const identity = readFileSync(join(graph, 'image-identity.txt'), 'utf8');
  const image = readFileSync(join(graph, 'reposync-image.txt'), 'utf8');
  if (!identity.split('\n').includes(`candidate=${candidate}`)) throw new Error('build graph candidate mismatch');
  if (!image.split('\n').includes(`reposync_image=rsacc-reposync:${candidate}`)) throw new Error('daemon image candidate mismatch');
  const imageId = image.match(/^reposync_id=(sha256:[0-9a-f]{64})(?:\s|$)/m)?.[1];
  if (!imageId) throw new Error('missing immutable daemon image identity');
  const source = join(graph, 'Cargo.lock');
  const bytes = readFileSync(source);
  if (!bytes.length) throw new Error('empty engine lockfile');
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  const recorded = readFileSync(join(graph, 'Cargo.lock.sha256'), 'utf8').match(/^([0-9a-f]{64})\s/)?.[1];
  if (sha256 !== recorded || !image.split('\n').includes(`lock_sha256=${sha256}`)) throw new Error('build graph lockfile checksum mismatch');
  if (existsSync(destination) && !readFileSync(destination).equals(bytes)) throw new Error('refusing to replace a different existing Cargo.lock');
  copyFileSync(source, destination);
  const result = {candidate, daemon_image_id: imageId, lock_sha256: sha256, source, destination, scope: 'disposable engine job checkout; no committed root lockfile'};
  mkdirSync(dirname(receipt), {recursive: true});
  writeFileSync(receipt, JSON.stringify(result, null, 2) + '\n');
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const flag = name => {
    const i = process.argv.indexOf(name);
    if (i < 0 || !process.argv[i + 1]) throw new Error(`missing ${name}`);
    return process.argv[i + 1];
  };
  try {
    console.log(JSON.stringify(installEngineLock({graph: flag('--graph'), candidate: flag('--candidate'), destination: flag('--destination'), receipt: flag('--receipt')})));
  } catch (error) {
    console.error(`engine lock setup failed: ${error.message}`);
    process.exit(1);
  }
}
