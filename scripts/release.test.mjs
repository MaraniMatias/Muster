import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import test from 'node:test';
import { MACOS_MINIMUM_VERSION, parseVersionText, plistMinimumSystemVersion, sameVersion } from './finalize-native-release.mjs';

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

test('the macOS floor is one decision in the config, the bundle and the release check', () => {
  // Cargo's `[env]` table leaves a variable the environment already carries
  // alone, so an inherited `MACOSX_DEPLOYMENT_TARGET` would quietly lower the
  // floor the bundle then claims. `force = true` is what makes the floor the
  // one written down whatever the shell says.
  const root = path.resolve(path.dirname(script), '..');
  const config = readFileSync(path.join(root, '.cargo/config.toml'), 'utf8');
  assert.match(config, /MACOSX_DEPLOYMENT_TARGET = \{ value = "15\.0", force = true \}/);
  // The floor is one decision written down three times: the build enforces
  // it, the bundle declares it, and the release check refuses a binary that
  // states another one. All three have to be the same version.
  const enforced = /MACOSX_DEPLOYMENT_TARGET = \{ value = "([^"]+)", force = true \}/.exec(config);
  assert.ok(enforced, 'the build config does not force a macOS deployment target');
  assert.ok(sameVersion(parseVersionText(enforced[1]), MACOS_MINIMUM_VERSION));
  const bundle = readFileSync(path.join(root, 'scripts/bundle-macos.sh'), 'utf8');
  assert.ok(sameVersion(plistMinimumSystemVersion(bundle), MACOS_MINIMUM_VERSION));
});
