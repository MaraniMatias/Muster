import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
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

const escape = text => text.replace(/\./g, '\\.');

// The version this tree carries, and the one after it. The candidate a test
// cuts is the one after the manifest's, so a test says nothing about which
// version the tree happens to carry: a release moves it, and the tests keep
// holding.
const carried = /^version = "(.+)"$/m.exec(readFileSync(path.join(root, 'app/Cargo.toml'), 'utf8'))[1];
const next = /^1\.0\.0-rc\.(\d+)$/.test(carried)
  ? `1.0.0-rc.${Number(/^1\.0\.0-rc\.(\d+)$/.exec(carried)[1]) + 1}`
  : /^0\.(\d+)\.(\d+)$/.test(carried)
    ? `0.${/^0\.(\d+)\.(\d+)$/.exec(carried)[1]}.${Number(/^0\.(\d+)\.(\d+)$/.exec(carried)[2]) + 1}`
    : '1.0.0-rc.1';

// A repository a candidate can be cut from: the four files the release edits,
// on a branch called `main`, with a remote to push to. The remote is a
// directory of its own, so the working tree stays clean.
function candidateRepository() {
  const dir = mkdtempSync(path.join(tmpdir(), 'muster-candidate-'));
  const remote = mkdtempSync(path.join(tmpdir(), 'muster-candidate-remote-'));
  mkdirSync(path.join(dir, 'scripts'), { recursive: true });
  mkdirSync(path.join(dir, 'app'), { recursive: true });
  copyFileSync(path.join(root, 'scripts/release.sh'), path.join(dir, 'scripts/release.sh'));
  for (const file of ['app/Cargo.toml', 'Cargo.lock', 'README.md']) {
    copyFileSync(path.join(root, file), path.join(dir, file));
  }
  // The changelog keeps the newest work under `Unreleased`, and the heading
  // is what a release renames. A tree whose newest work is already released
  // has no such heading, so one is put back for the release to rename.
  const changelog = readFileSync(path.join(root, 'CHANGELOG.md'), 'utf8');
  writeFileSync(
    path.join(dir, 'CHANGELOG.md'),
    /^## Unreleased$/m.test(changelog)
      ? changelog
      : changelog.replace(/^## /m, '## Unreleased\n\n- **The newest work.** The work this candidate carries.\n\n## '),
  );
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
  const result = run('ship');
  assert.equal(result.status, 2);
  assert.match(result.stderr, /unknown command: ship/);
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
    const result = spawnSync(path.join(dir, 'scripts/release.sh'), ['candidate', next, '--dry-run'], { cwd: dir, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /nothing was committed, pushed or published/);
    // The manifest and the lock file are what the build and the workflow read
    // the version from, and they have to carry the same one.
    assert.match(readFileSync(path.join(dir, 'app/Cargo.toml'), 'utf8'), new RegExp(`^version = "${escape(next)}"$`, 'm'));
    assert.match(readFileSync(path.join(dir, 'Cargo.lock'), 'utf8'), new RegExp(`^version = "${escape(next)}"$`, 'm'));
    // The changelog keeps the newest work under `Unreleased` and renames that
    // heading to the version when it is tagged.
    assert.match(readFileSync(path.join(dir, 'CHANGELOG.md'), 'utf8'), new RegExp(`^## ${escape(next)}$`, 'm'));
    // The README names the candidate people download, in its links and in its
    // one line about it.
    const readme = readFileSync(path.join(dir, 'README.md'), 'utf8');
    assert.match(readme, new RegExp(`The current candidate is \\*\\*v${escape(next)}\\*\\*:`));
    assert.match(readme, new RegExp(`releases/download/v${escape(next)}/Muster_${escape(next)}_arm64\\.zip`));
    assert.match(readme, new RegExp(`releases/download/v${escape(next)}/muster_${escape(next)}_amd64\\.deb`));
    // A dry run publishes nothing: the commit is the one it started from, and
    // the remote has no tag.
    assert.equal(git(dir, 'rev-parse', 'HEAD').trim(), before);
    assert.equal(git(dir, 'ls-remote', '--tags', 'origin').trim(), '');
  } finally {
    rmSync(dir, { recursive: true, force: true });
    rmSync(remote, { recursive: true, force: true });
  }
});

test('the publish command refuses a version the tree does not carry', () => {
  const { dir, remote } = candidateRepository();
  try {
    const result = spawnSync(path.join(dir, 'scripts/release.sh'), ['publish', '1.0.0-rc.999'], { cwd: dir, encoding: 'utf8' });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /the tree does not carry 1\.0\.0-rc\.999/);
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
