import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import test from 'node:test';
import { finalize, plistMinimumSystemVersion, readMachOMinimumOs, verifyMacBundleMinimumOs, verifyPayload, MACOS_MINIMUM_VERSION, RELEASE_TARGETS } from './finalize-native-release.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = file => readFileSync(path.join(root, file), 'utf8');
const sha = 'a'.repeat(40);

// The two load commands that state a Mach-O's minimum macOS version, and a
// thin arm64 header carrying them.
const LC_VERSION_MIN_MACOSX = 0x24;
const LC_BUILD_VERSION = 0x32;

function packVersion(version) {
  // Arithmetic rather than a bit shift: a major of 32768 does not fit in the
  // sixteen bits a packed version gives it, and shifting it left would turn it
  // into a negative number.
  const [major, minor = 0, patch = 0] = version.split('.').map(Number);
  return major * 0x10000 + minor * 0x100 + patch;
}

function buildVersionCommand(minos, sdk = '15.0') {
  const command = Buffer.alloc(24);
  command.writeUInt32LE(LC_BUILD_VERSION, 0);
  command.writeUInt32LE(24, 4);
  command.writeUInt32LE(1, 8);
  command.writeUInt32LE(packVersion(minos), 12);
  command.writeUInt32LE(packVersion(sdk), 16);
  command.writeUInt32LE(0, 20);
  return command;
}

function versionMinCommand(version, sdk = '15.0') {
  const command = Buffer.alloc(16);
  command.writeUInt32LE(LC_VERSION_MIN_MACOSX, 0);
  command.writeUInt32LE(16, 4);
  command.writeUInt32LE(packVersion(version), 8);
  command.writeUInt32LE(packVersion(sdk), 12);
  return command;
}

function machO(arch, ...commands) {
  const header = Buffer.alloc(32);
  header.writeUInt32LE(0xfeedfacf, 0);
  header.writeUInt32LE(arch === 'x86_64' ? 0x01000007 : 0x0100000c, 4);
  header.writeUInt32LE(2, 12);
  header.writeUInt32LE(commands.length, 16);
  header.writeUInt32LE(commands.reduce((total, command) => total + command.length, 0), 20);
  return Buffer.concat([header, ...commands]);
}

const minimumPlist = '<plist><dict><key>LSMinimumSystemVersion</key><string>15.0</string></dict></plist>';

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

test('the packaged binary states its minimum macOS version in its load commands', () => {
  assert.deepEqual(readMachOMinimumOs(machO('arm64', buildVersionCommand('15.0'))), { major: 15, minor: 0, patch: 0 });
  assert.deepEqual(readMachOMinimumOs(machO('arm64', versionMinCommand('15.0'))), { major: 15, minor: 0, patch: 0 });
  // The CPU type is what says which architecture a binary is, so a bundle
  // that carries the other one is refused rather than published.
  assert.throws(() => readMachOMinimumOs(machO('x86_64', buildVersionCommand('15.0')), 'arm64'), /not macOS arm64/);
  assert.deepEqual(readMachOMinimumOs(machO('x86_64', buildVersionCommand('15.0')), 'x86_64'), { major: 15, minor: 0, patch: 0 });
  // A binary that says nothing about the floor states no floor.
  assert.throws(() => readMachOMinimumOs(machO('arm64')), /declares no minimum macOS version/);
  // Two commands that disagree leave nothing to check against.
  assert.throws(() => readMachOMinimumOs(machO('arm64', buildVersionCommand('15.0'), versionMinCommand('14.0'))), /conflicting minimum macOS versions/);
  // A command that runs past the end of the load commands is not a floor.
  const overrun = machO('arm64', buildVersionCommand('15.0'));
  overrun.writeUInt32LE(64, 36);
  assert.throws(() => readMachOMinimumOs(overrun), /malformed Mach-O load command bounds/);
  // A header that claims more commands than it carries.
  const liar = machO('arm64', buildVersionCommand('15.0'));
  liar.writeUInt32LE(4, 16);
  assert.throws(() => readMachOMinimumOs(liar), /malformed Mach-O load commands/);
  // A platform that is not macOS is not this app's floor.
  const other = buildVersionCommand('15.0');
  other.writeUInt32LE(6, 8);
  assert.throws(() => readMachOMinimumOs(machO('arm64', other)), /non-macOS platform/);
});

test('the bundle declares the floor the build enforces', () => {
  assert.deepEqual(plistMinimumSystemVersion(minimumPlist), MACOS_MINIMUM_VERSION);
  assert.throws(() => plistMinimumSystemVersion('<plist><dict></dict></plist>'), /declares no LSMinimumSystemVersion/);
  assert.throws(() => plistMinimumSystemVersion('<plist><dict><key>LSMinimumSystemVersion</key><string>fifteen</string></dict></plist>'), /malformed version/);
  // A version text is three bounded numbers, so a long one is malformed
  // rather than a number too large to hold.
  assert.throws(() => plistMinimumSystemVersion('<plist><dict><key>LSMinimumSystemVersion</key><string>1234567890123</string></dict></plist>'), /malformed version/);
  assert.throws(() => plistMinimumSystemVersion(`<plist><dict><key>LSMinimumSystemVersion</key><string>${'1'.repeat(64)}</string></dict></plist>`), /malformed version/);
  assert.throws(() => plistMinimumSystemVersion('<plist><dict><key>LSMinimumSystemVersion</key><string>15.0.0.0</string></dict></plist>'), /malformed version/);
});

test('the binary and the bundle must agree on the floor', () => {
  assert.deepEqual(verifyMacBundleMinimumOs(minimumPlist, machO('arm64', buildVersionCommand('15.0'))), { major: 15, minor: 0, patch: 0 });
  // The floor is what this build claims to need, so a binary that states a
  // different one is refused whichever way it differs: older as much as
  // newer, because a binary that says otherwise was not built by the decision
  // it is shipped under.
  assert.throws(() => verifyMacBundleMinimumOs(minimumPlist, machO('arm64', buildVersionCommand('14.0'))), /the binary states macOS 14\.0\.0/);
  // macOS 15 is followed by 26, so that is the next system a build could ask for.
  assert.throws(() => verifyMacBundleMinimumOs(minimumPlist, machO('arm64', buildVersionCommand('26.0'))), /the binary states macOS 26\.0\.0/);
  // A major that does not fit in the sixteen bits a packed version gives it
  // used to read as a negative number, and a negative number compared as
  // older than the floor. It is refused now, like any other mismatch.
  assert.throws(() => verifyMacBundleMinimumOs(minimumPlist, machO('arm64', buildVersionCommand('32768.0'))), /the binary states macOS 32768\.0\.0/);
  // A bundle that lowered its own claim is not the floor this build enforces,
  // and neither is one that raised it.
  const lowered = '<plist><dict><key>LSMinimumSystemVersion</key><string>14.0</string></dict></plist>';
  assert.throws(() => verifyMacBundleMinimumOs(lowered, machO('arm64', buildVersionCommand('15.0'))), /the bundle declares macOS 14\.0\.0/);
  const raised = '<plist><dict><key>LSMinimumSystemVersion</key><string>26.0</string></dict></plist>';
  assert.throws(() => verifyMacBundleMinimumOs(raised, machO('arm64', buildVersionCommand('15.0'))), /the bundle declares macOS 26\.0\.0/);
  // A bundle that claims an architecture it does not carry is refused too.
  assert.throws(() => verifyMacBundleMinimumOs(minimumPlist, machO('arm64', buildVersionCommand('15.0')), 'x86_64'), /not macOS x86_64/);
});

test('finalization validates every target and refuses wrong bundle versions', { skip: process.platform !== 'linux' }, () => temporary(dir => {
  const version = '1.0.0-rc.2';
  const stage = path.join(dir, 'stage');
  mkdirSync(path.join(stage, 'DEBIAN'), { recursive: true });
  for (const file of ['usr/bin/Muster', 'usr/share/applications/dev.muster.workspace.desktop', 'usr/share/icons/hicolor/512x512/apps/muster.png', 'usr/share/doc/muster/copyright']) {
    mkdirSync(path.dirname(path.join(stage, file)), { recursive: true });
    writeFileSync(path.join(stage, file), 'fixture');
  }
  const run = (command, args, options = {}) => {
    const result = spawnSync(command, args, { encoding: 'utf8', ...options });
    assert.equal(result.status, 0, result.stderr);
  };
  const record = (directory, filename) => {
    writeFileSync(path.join(directory, 'SOURCE_SHA'), `${sha}\n`);
    writeFileSync(path.join(directory, 'SHA256SUMS'), `${createHash('sha256').update(readFileSync(path.join(directory, filename))).digest('hex')}  ${filename}\n`);
  };
  // Each target is a directory of its own carrying one payload, so a payload
  // is checked against its own checksum and its own architecture.
  for (const arch of ['amd64', 'arm64']) {
    writeFileSync(path.join(stage, 'DEBIAN/control'), `Package: muster\nVersion: 1.0.0~rc.2\nArchitecture: ${arch}\nMaintainer: Test <test@example.test>\nDescription: Test fixture\n`);
    const linuxDir = path.join(dir, `linux-${arch}-${sha}`);
    mkdirSync(linuxDir);
    const deb = `muster_1.0.0~rc.2_${arch}.deb`;
    run('dpkg-deb', ['--build', stage, path.join(linuxDir, deb)]);
    record(linuxDir, deb);
  }
  const contents = path.join(dir, 'Muster.app/Contents');
  mkdirSync(path.join(contents, 'MacOS'), { recursive: true });
  const plist = '<plist><dict><key>CFBundleShortVersionString</key><string>1.0.0-rc.2</string><key>LSMinimumSystemVersion</key><string>15.0</string></dict></plist>';
  writeFileSync(path.join(contents, 'Info.plist'), plist);
  const archive = (arch) => {
    writeFileSync(path.join(contents, 'MacOS/Muster'), machO(arch, buildVersionCommand('15.0')));
    const macDir = path.join(dir, `macos-${arch}-${sha}`);
    mkdirSync(macDir, { recursive: true });
    const mac = `Muster_1.0.0-rc.2_${arch}.zip`;
    rmSync(path.join(macDir, mac), { force: true });
    run('zip', ['-qr', path.join(macDir, mac), 'Muster.app'], { cwd: dir });
    record(macDir, mac);
  };
  archive('arm64');
  archive('x86_64');
  const output = path.join(dir, 'final');
  finalize(dir, sha, version, output);
  assert.match(readFileSync(path.join(output, 'SHA256SUMS'), 'utf8'), /muster_1\.0\.0-rc\.2_amd64\.deb/);
  assert.match(readFileSync(path.join(output, 'SHA256SUMS'), 'utf8'), /muster_1\.0\.0-rc\.2_arm64\.deb/);
  assert.match(readFileSync(path.join(output, 'SHA256SUMS'), 'utf8'), /Muster_1\.0\.0-rc\.2_arm64\.zip/);
  assert.match(readFileSync(path.join(output, 'SHA256SUMS'), 'utf8'), /Muster_1\.0\.0-rc\.2_x86_64\.zip/);
  assert.deepEqual(readFileSync(path.join(output, 'muster_1.0.0-rc.2_amd64.deb')), readFileSync(path.join(dir, `linux-amd64-${sha}/muster_1.0.0~rc.2_amd64.deb`)));
  // A package that names one architecture and carries the other's is refused:
  // it installs and then refuses to start.
  writeFileSync(path.join(stage, 'DEBIAN/control'), 'Package: muster\nVersion: 1.0.0~rc.2\nArchitecture: amd64\nMaintainer: Test <test@example.test>\nDescription: Test fixture\n');
  const linuxArm = path.join(dir, `linux-arm64-${sha}`);
  rmSync(path.join(linuxArm, 'muster_1.0.0~rc.2_arm64.deb'));
  run('dpkg-deb', ['--build', stage, path.join(linuxArm, 'muster_1.0.0~rc.2_arm64.deb')]);
  record(linuxArm, 'muster_1.0.0~rc.2_arm64.deb');
  assert.throws(() => finalize(dir, sha, version, output));
  writeFileSync(path.join(stage, 'DEBIAN/control'), 'Package: muster\nVersion: 1.0.0~rc.2\nArchitecture: arm64\nMaintainer: Test <test@example.test>\nDescription: Test fixture\n');
  rmSync(path.join(linuxArm, 'muster_1.0.0~rc.2_arm64.deb'));
  run('dpkg-deb', ['--build', stage, path.join(linuxArm, 'muster_1.0.0~rc.2_arm64.deb')]);
  record(linuxArm, 'muster_1.0.0~rc.2_arm64.deb');
  // A bundle whose binary is the other architecture is refused too.
  const macIntel = path.join(dir, `macos-x86_64-${sha}`);
  writeFileSync(path.join(contents, 'MacOS/Muster'), machO('arm64', buildVersionCommand('15.0')));
  rmSync(path.join(macIntel, 'Muster_1.0.0-rc.2_x86_64.zip'));
  run('zip', ['-qr', path.join(macIntel, 'Muster_1.0.0-rc.2_x86_64.zip'), 'Muster.app'], { cwd: dir });
  record(macIntel, 'Muster_1.0.0-rc.2_x86_64.zip');
  assert.throws(() => finalize(dir, sha, version, output), /not macOS x86_64/);
  archive('x86_64');
  writeFileSync(path.join(contents, 'Info.plist'), plist.replace('1.0.0-rc.2', '1.0.0-rc.1'));
  const macArm = path.join(dir, `macos-arm64-${sha}`);
  rmSync(path.join(macArm, 'Muster_1.0.0-rc.2_arm64.zip'));
  run('zip', ['-qr', path.join(macArm, 'Muster_1.0.0-rc.2_arm64.zip'), 'Muster.app'], { cwd: dir });
  record(macArm, 'Muster_1.0.0-rc.2_arm64.zip');
  assert.throws(() => finalize(dir, sha, version, output));
  // A binary that states a floor other than the one the bundle declares is
  // refused too, whichever way it differs.
  writeFileSync(path.join(contents, 'Info.plist'), plist);
  writeFileSync(path.join(contents, 'MacOS/Muster'), machO('arm64', buildVersionCommand('26.0')));
  rmSync(path.join(macArm, 'Muster_1.0.0-rc.2_arm64.zip'));
  run('zip', ['-qr', path.join(macArm, 'Muster_1.0.0-rc.2_arm64.zip'), 'Muster.app'], { cwd: dir });
  record(macArm, 'Muster_1.0.0-rc.2_arm64.zip');
  assert.throws(() => finalize(dir, sha, version, output), /the binary states macOS 26\.0\.0/);
  writeFileSync(path.join(contents, 'MacOS/Muster'), machO('arm64', buildVersionCommand('14.0')));
  rmSync(path.join(macArm, 'Muster_1.0.0-rc.2_arm64.zip'));
  run('zip', ['-qr', path.join(macArm, 'Muster_1.0.0-rc.2_arm64.zip'), 'Muster.app'], { cwd: dir });
  record(macArm, 'Muster_1.0.0-rc.2_arm64.zip');
  assert.throws(() => finalize(dir, sha, version, output), /the binary states macOS 14\.0\.0/);
}));

test('a release carries a build for every supported target', () => {
  assert.deepEqual(RELEASE_TARGETS, [
    { directory: 'linux-amd64', arch: 'amd64', format: 'deb' },
    { directory: 'linux-arm64', arch: 'arm64', format: 'deb' },
    { directory: 'macos-arm64', arch: 'arm64', format: 'zip' },
    { directory: 'macos-x86_64', arch: 'x86_64', format: 'zip' },
  ]);
  const workflow = read('.github/workflows/linux-release.yml');
  // Each target is built on a runner of its own architecture, so the binary
  // in a package is the one that runner's toolchain made.
  assert.match(workflow, /arch: amd64\n {12}runner: ubuntu-24\.04/);
  assert.match(workflow, /arch: arm64\n {12}runner: ubuntu-24\.04-arm/);
  assert.match(workflow, /arch: arm64\n {12}runner: macos-latest/);
  assert.match(workflow, /arch: x86_64\n {12}runner: macos-15-intel/);
  const publish = workflow.slice(workflow.indexOf('\n  publish:'));
  assert.match(publish, /\[\[ \$\{#files\[@\]\} == 6 \]\]/);
  for (const artifact of ['muster_${version}_amd64.deb', 'muster_${version}_arm64.deb', 'Muster_${version}_arm64.zip', 'Muster_${version}_x86_64.zip']) {
    assert.ok(publish.includes(artifact), `the release does not carry ${artifact}`);
  }
});

function smoke(dir, app, window, deadline = false) {
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
  const command = deadline ? '/usr/bin/timeout' : 'bash';
  const args = [path.join(root, 'scripts/linux-window-smoke.sh'), path.join(dir, 'log'), appPath];
  if (deadline) args.unshift('--kill-after=1s', '0.2s', 'bash');
  return spawnSync(command, args, {
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

test('an outer deadline bounds a stalled window query', { skip: process.platform !== 'linux' }, () => temporary(dir => {
  const result = smoke(dir, 'exec /bin/sleep 10', 'exec /bin/sleep 10', true);
  assert.equal(result.status, 124, result.stderr);
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

test('every workflow action is pinned to a commit and every job states its permissions', () => {
  for (const file of ['.github/workflows/ci.yml', '.github/workflows/linux-release.yml']) {
    const workflow = read(file);
    for (const [, action] of workflow.matchAll(/uses:\s*(\S+)/g)) {
      assert.match(action, /^[^@\s]+@[0-9a-f]{40}$/, `${file} does not pin ${action} to a commit`);
    }
    for (const [, tail] of workflow.matchAll(/uses:\s*actions\/checkout@[0-9a-f]{40}[^\n]*\n((?: {8,}[^\n]*\n)+)/g)) {
      assert.match(tail, /persist-credentials: false/, `${file} leaves checkout credentials persisted`);
    }
    const jobs = [...workflow.slice(workflow.indexOf('\njobs:\n')).matchAll(/^ {2}([A-Za-z0-9_-]+):$/gm)];
    assert.ok(jobs.length > 0, `${file} declares no job`);
    for (const [index, job] of jobs.entries()) {
      const body = workflow.slice(job.index, jobs[index + 1]?.index ?? workflow.length);
      const declared = /^ {4}permissions:\n {6}contents: (read|write)$/m.exec(body);
      assert.ok(declared, `${file}: job ${job[1]} does not declare its permissions`);
      assert.equal(declared[1], job[1] === 'publish' ? 'write' : 'read', `${file}: job ${job[1]} has the wrong permissions`);
    }
  }
});
