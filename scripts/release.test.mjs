import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import test from 'node:test';

const script = path.resolve(path.dirname(fileURLToPath(import.meta.url)), 'release.sh');

function run(...args) {
  return spawnSync(script, args, { encoding: 'utf8' });
}

test('help describes local non-publishing commands without requiring Rust', () => {
  const result = run('--help');
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /check\s+Run formatting/);
  assert.match(result.stdout, /release\s+Run check\/build and package/);
  assert.match(result.stdout, /does\nnot publish/);
});

test('unknown commands fail before doing build work', () => {
  const result = run('publish');
  assert.equal(result.status, 2);
  assert.match(result.stderr, /unknown command: publish/);
});

test('the release profile leaves rustc from stripping, which breaks every proc macro', () => {
  // Cargo's default `strip = "debuginfo"` is not a linker flag: rustc rewrites
  // the finished Mach-O itself. On this toolchain that rewrite leaves a
  // dylib's __LINKEDIT string pool at an offset that is not a multiple of
  // eight, dyld refuses the file with "mis-aligned LINKEDIT string pool", and
  // every proc macro built for release fails to load with E0463. A full clean
  // reproduces it, so it is not a stale artifact. Release emits no debuginfo
  // to strip, so the root manifest turns the step off.
  const manifest = readFileSync(path.resolve(path.dirname(script), '..', 'Cargo.toml'), 'utf8');
  assert.match(manifest, /\[profile\.release\]\s*\nstrip = "none"/);
});
