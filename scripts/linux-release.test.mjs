import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import test from 'node:test';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = (file) => readFileSync(path.join(root, file), 'utf8');

test('Linux package derives runtime dependencies from the linked binary and validates package contents', () => {
  const script = read('scripts/package-linux-deb.sh');
  assert.match(script, /dpkg-shlibdeps -O -e/);
  assert.match(script, /dpkg-deb -f/);
  assert.match(script, /\.\/debian\//);
  assert.match(script, /\[\[ "\$binary" = \/\* \]\]/);
  assert.match(script, /\[\[ "\$out" = \/\* \]\]/);
  assert.match(script, /readelf -h/);
  assert.match(script, /usr\/share\/doc\/muster\/copyright/);
  assert.match(script, /only amd64 is supported/);
  assert.match(script, /Architecture: amd64/);
  assert.match(script, /muster\.desktop/);
  assert.match(script, /native\/assets\/icon\.png/);
});

test('workflow builds the requested SHA once and publishes its retained artifact only in publish mode', () => {
  const workflow = read('.github/workflows/linux-release.yml');
  assert.match(workflow, /\^\[0-9a-f\]\{40\}\$/);
  assert.match(workflow, /cargo build --locked --release -p muster/);
  assert.match(workflow, /needs: \[linux, macos\]/);
  assert.match(workflow, /--repo/);
  assert.match(workflow, /--latest=false/);
  assert.match(workflow, /git ls-remote/);
  assert.match(workflow, /if: inputs\.mode == 'publish'/);
  assert.match(workflow, /actions\/download-artifact/);
  assert.match(workflow, /docker run --rm --network host/);
  assert.match(workflow, /dist\/SHA256SUMS/);
  assert.match(workflow, /dist\/SOURCE_SHA/);
  assert.match(workflow, /macos-15-arm64|test "\$\(uname -m\)" = arm64/);
  assert.doesNotMatch(workflow, /dist\/\{Muster-macos-arm64/);
  assert.match(workflow, /gh release create/);
  assert.doesNotMatch(workflow, /git push/);
  assert.match(workflow, /git\/refs.*POST/);
});

test('Debian version conversion preserves stable ordering and prerelease semantics', () => {
  const script = path.join(root, 'scripts/debian-version.sh');
  const run = (version) => spawnSync('bash', [script, version], { encoding: 'utf8' });
  assert.equal(run('1.0.0-rc.2').stdout.trim(), '1.0.0~rc.2');
  assert.equal(run('0.4.3+build.7').stdout.trim(), '0.4.3+build.7');
  assert.notEqual(run('1.0.0-').status, 0);
});
