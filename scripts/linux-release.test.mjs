import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import test from 'node:test';
import { verifyPayload } from './finalize-native-release.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = file => readFileSync(path.join(root, file), 'utf8');
const sha = 'a'.repeat(40);

function temporary(run) {
  const dir = mkdtempSync(path.join(tmpdir(), 'muster-release-'));
  try { run(dir); } finally { rmSync(dir, { recursive: true, force: true }); }
}

test('Debian versions preserve RC upgrade ordering and reject invalid input', () => {
  const run = version => spawnSync('bash', [path.join(root, 'scripts/debian-version.sh'), version], { encoding: 'utf8' });
  assert.equal(run('1.0.0-rc.2').stdout.trim(), '1.0.0~rc.2');
  assert.equal(run('0.4.3+build.7').stdout.trim(), '0.4.3+build.7');
  for (const invalid of ['1.0.0-', 'garbage', '1.0.0\nDepends: evil']) assert.notEqual(run(invalid).status, 0);
});

test('finalization refuses wrong SHA, tampering, missing targets and unexpected files', () => temporary(dir => {
  const filename = 'muster_1.0.0~rc.2_amd64.deb';
  const bytes = Buffer.from('test payload');
  const checksum = createHash('sha256').update(bytes).digest('hex');
  writeFileSync(path.join(dir, filename), bytes);
  writeFileSync(path.join(dir, 'SOURCE_SHA'), `${sha}\n`);
  writeFileSync(path.join(dir, 'SHA256SUMS'), `${checksum}  ${filename}\n`);
  assert.deepEqual(verifyPayload(dir, sha, filename), bytes);
  assert.throws(() => verifyPayload(dir, 'b'.repeat(40), filename), /source SHA mismatch/);
  assert.throws(() => verifyPayload(dir, sha, 'missing.zip'), /unexpected artifact files/);
  writeFileSync(path.join(dir, filename), 'tampered');
  assert.throws(() => verifyPayload(dir, sha, filename), /checksum mismatch/);
  writeFileSync(path.join(dir, 'extra'), 'unapproved');
  assert.throws(() => verifyPayload(dir, sha, filename), /unexpected artifact files/);
}));

function smoke(dir, app, window) {
  const executable = (name, body) => {
    const file = path.join(dir, name);
    writeFileSync(file, `#!/bin/bash\n${body}\n`, { mode: 0o755 });
    return file;
  };
  const appPath = executable('app', app);
  executable('setsid', 'exec "$@"');
  executable('timeout', 'shift; exec "$@"');
  executable('sleep', '/bin/sleep 0.03');
  executable('xwininfo', window);
  return spawnSync('bash', [path.join(root, 'scripts/linux-window-smoke.sh'), path.join(dir, 'log'), appPath], {
    env: { ...process.env, PATH: `${dir}:${process.env.PATH}` }, encoding: 'utf8', timeout: 5000,
  });
}

test('startup smoke rejects a clean exit before any window', () => temporary(dir => {
  const result = smoke(dir, 'exit 0', 'exit 1');
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /app exited before window/);
}));

test('startup smoke rejects a crash even if a stale window has the expected title', () => temporary(dir => {
  const result = smoke(dir, 'exit 7', '/bin/sleep 0.05; echo Muster');
  assert.equal(result.status, 1, result.stderr);
}));

test('startup smoke requires a window, not just a live process', () => temporary(dir => {
  const result = smoke(dir, 'exec /bin/sleep 10', 'exit 1');
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /window not observed/);
}));

test('startup smoke accepts a window only while the process remains alive', () => temporary(dir => {
  const result = smoke(dir, 'exec /bin/sleep 10', 'echo Muster');
  assert.equal(result.status, 0, result.stderr);
}));

test('publication is downstream of read-only finalization and never rebuilds source', () => {
  const workflow = read('.github/workflows/linux-release.yml');
  assert.match(workflow, /needs: \[source, linux, macos\]/);
  assert.match(workflow, /if: inputs\.mode == 'publish'/);
  const publish = workflow.slice(workflow.indexOf('\n  publish:'));
  assert.match(publish, /needs: finalize/);
  assert.match(publish, /--prerelease --latest=false/);
  assert.match(publish, /Tag already exists/);
  assert.match(publish, /status=\$\?; \[\[ \$status == 2 \]\]/);
  assert.match(publish, /--draft/);
  assert.doesNotMatch(publish, /uses: actions\/checkout|cargo |node |git push/);
});
