import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import test from 'node:test';
import { MACOS_MINIMUM_VERSION, parseVersionText, plistMinimumSystemVersion, sameVersion } from './finalize-native-release.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const script = path.join(root, 'scripts/release.sh');

function run(...args) {
  return spawnSync(script, args, { encoding: 'utf8' });
}

function git(cwd, ...args) {
  const result = spawnSync('git', args, { cwd, encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout;
}

// A repository a candidate can be cut from: the four files the release edits,
// on a branch called `main`, with a remote to push to. The remote is a
// directory of its own, so the working tree stays clean.
function candidateRepository() {
  const dir = mkdtempSync(path.join(tmpdir(), 'muster-candidate-'));
  const remote = mkdtempSync(path.join(tmpdir(), 'muster-candidate-remote-'));
  mkdirSync(path.join(dir, 'scripts'), { recursive: true });
  mkdirSync(path.join(dir, 'app'), { recursive: true });
  copyFileSync(path.join(root, 'scripts/release.sh'), path.join(dir, 'scripts/release.sh'));
  for (const file of ['app/Cargo.toml', 'Cargo.lock', 'CHANGELOG.md', 'README.md']) {
    copyFileSync(path.join(root, file), path.join(dir, file));
  }
  git(dir, 'init', '--quiet');
  git(dir, 'checkout', '--quiet', '-b', 'main');
  git(dir, 'config', 'user.email', 'test@example.test');
  git(dir, 'config', 'user.name', 'Test');
  git(dir, 'config', 'commit.gpgsign', 'false');
  git(dir, 'add', '--all');
  git(dir, 'commit', '--quiet', '-m', 'initial');
  git(dir, 'init', '--quiet', '--bare', remote);
  git(dir, 'remote', 'add', 'origin', remote);
  git(dir, 'push', '--quiet', '-u', 'origin', 'main');
  return { dir, remote };
}

test('help describes every command without requiring Rust', () => {
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

test('the candidate command refuses a version it does not publish, and a missing one', () => {
  // The final release is a deliberate act of its own, so it is not a shape
  // this command publishes: it is refused before the tree is touched.
  const final = run('candidate', '1.0.0');
  assert.equal(final.status, 2);
  assert.match(final.stderr, /unsupported version: 1\.0\.0/);
  const missing = run('candidate');
  assert.equal(missing.status, 2);
  assert.match(missing.stderr, /unsupported version: <none>/);
});

test('the candidate command writes the version where the release reads it, and publishes nothing', () => {
  const { dir, remote } = candidateRepository();
  try {
    const before = git(dir, 'rev-parse', 'HEAD').trim();
    const result = spawnSync(path.join(dir, 'scripts/release.sh'), ['candidate', '1.0.0-rc.12', '--dry-run'], { cwd: dir, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /nothing was committed, pushed or published/);
    // The manifest and the lock file are what the build and the workflow read
    // the version from, and they have to carry the same one.
    assert.match(readFileSync(path.join(dir, 'app/Cargo.toml'), 'utf8'), /^version = "1\.0\.0-rc\.12"$/m);
    assert.match(readFileSync(path.join(dir, 'Cargo.lock'), 'utf8'), /^version = "1\.0\.0-rc\.12"$/m);
    // The changelog keeps the newest work under `Unreleased` and renames that
    // heading to the version when it is tagged.
    assert.match(readFileSync(path.join(dir, 'CHANGELOG.md'), 'utf8'), /^## 1\.0\.0-rc\.12$/m);
    // The README names the candidate people download, in its links and in its
    // one line about it.
    const readme = readFileSync(path.join(dir, 'README.md'), 'utf8');
    assert.match(readme, /The current candidate is \*\*v1\.0\.0-rc\.12\*\*:/);
    assert.match(readme, /releases\/download\/v1\.0\.0-rc\.12\/Muster_1\.0\.0-rc\.12_arm64\.zip/);
    assert.match(readme, /releases\/download\/v1\.0\.0-rc\.12\/muster_1\.0\.0-rc\.12_amd64\.deb/);
    // The sentence about the candidates that carry all four builds is about
    // the past, so it keeps the version it was written for.
    assert.match(readme, /Candidates before v1\.0\.0-rc\.11 may carry only a macOS archive/);
    // A dry run publishes nothing: the commit is the one it started from, and
    // the remote has no tag.
    assert.equal(git(dir, 'rev-parse', 'HEAD').trim(), before);
    assert.equal(git(dir, 'ls-remote', '--tags', 'origin').trim(), '');
  } finally {
    rmSync(dir, { recursive: true, force: true });
    rmSync(remote, { recursive: true, force: true });
  }
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
