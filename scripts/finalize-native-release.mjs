import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

// The floor is one decision written down three times: `Info.plist` says
// `LSMinimumSystemVersion 15.0`, `.cargo/config.toml` builds against a forced
// `MACOSX_DEPLOYMENT_TARGET 15.0`, and the shipped binary says what it was
// built against in its own load commands. All three are the same version, so
// all three are checked against this one.
export const MACOS_MINIMUM_VERSION = { major: 15, minor: 0, patch: 0 };

const MACH_MAGIC_64 = 0xfeedfacf;
const CPU_TYPE_ARM64 = 0x0100000c;
const CPU_TYPE_X86_64 = 0x01000007;
const MACH_HEADER_64_SIZE = 32;
const LC_VERSION_MIN_MACOSX = 0x24;
const LC_BUILD_VERSION = 0x32;
const PLATFORM_MACOS = 1;

function decodeVersion(encoded) {
  return { major: (encoded >>> 16) & 0xffff, minor: (encoded >>> 8) & 0xff, patch: encoded & 0xff };
}

// Two versions are the same version when their parts are the same numbers.
// They are never compared as one packed integer: packing a major into sixteen
// bits and shifting it left turns a major of 32768 into a negative number, and
// a binary claiming that would then read as older than the floor instead of
// being refused.
export function sameVersion(left, right) {
  return left.major === right.major && left.minor === right.minor && left.patch === right.patch;
}

export function formatVersion(version) {
  return `${version.major}.${version.minor}.${version.patch}`;
}

// A version is three bounded numbers. Each part is at most nine digits, so the
// text is short by construction and every part is an exact integer. The text
// in the message is cut short, because a malformed one is not short.
export function parseVersionText(text) {
  const trimmed = String(text).trim();
  const shown = trimmed.length > 64 ? `${trimmed.slice(0, 64)}…` : trimmed;
  assert.ok(trimmed.length <= 32, `malformed version: ${shown}`);
  const match = /^(\d{1,9})(?:\.(\d{1,9}))?(?:\.(\d{1,9}))?$/.exec(trimmed);
  assert.ok(match, `malformed version: ${shown}`);
  const major = Number(match[1]);
  const minor = Number(match[2] ?? 0);
  const patch = Number(match[3] ?? 0);
  assert.ok([major, minor, patch].every(Number.isSafeInteger), `malformed version: ${shown}`);
  return { major, minor, patch };
}

// The floor the bundle claims, read from the one key that carries it.
export function plistMinimumSystemVersion(plist) {
  const match = /<key>LSMinimumSystemVersion<\/key>\s*<string>([^<]*)<\/string>/.exec(plist);
  assert.ok(match, 'the bundle declares no LSMinimumSystemVersion');
  return parseVersionText(match[1]);
}

// The two macOS architectures a release ships. The CPU type is what says
// which one a binary is, so a bundle that claims one and carries the other
// is refused rather than published.
export const MACHO_ARCHITECTURES = { arm64: CPU_TYPE_ARM64, x86_64: CPU_TYPE_X86_64 };

// The floor the binary itself carries, read from its load commands. A Mach-O
// states it in `LC_BUILD_VERSION` (the modern command) or in the older
// `LC_VERSION_MIN_MACOSX`; either one says it, and a binary that says two
// different things says nothing.
export function readMachOMinimumOs(bytes, arch = 'arm64') {
  assert.ok(bytes.length >= MACH_HEADER_64_SIZE, 'truncated Mach-O header');
  assert.equal(bytes.readUInt32LE(0), MACH_MAGIC_64, 'not a 64-bit Mach-O');
  const expected = MACHO_ARCHITECTURES[arch];
  assert.ok(expected, `unknown macOS architecture: ${arch}`);
  assert.equal(bytes.readUInt32LE(4), expected, `not macOS ${arch}`);
  const commands = bytes.readUInt32LE(16);
  const commandBytes = bytes.readUInt32LE(20);
  const end = MACH_HEADER_64_SIZE + commandBytes;
  assert.ok(end <= bytes.length, 'truncated Mach-O load commands');
  const claims = [];
  let offset = MACH_HEADER_64_SIZE;
  for (let index = 0; index < commands; index++) {
    assert.ok(offset + 8 <= end, 'malformed Mach-O load commands');
    const command = bytes.readUInt32LE(offset);
    const size = bytes.readUInt32LE(offset + 4);
    assert.ok(size >= 8 && size % 4 === 0, 'malformed Mach-O load command size');
    assert.ok(offset + size <= end, 'malformed Mach-O load command bounds');
    if (command === LC_BUILD_VERSION) {
      assert.ok(size >= 24, 'malformed LC_BUILD_VERSION');
      assert.equal(bytes.readUInt32LE(offset + 8), PLATFORM_MACOS, 'the binary declares a non-macOS platform');
      claims.push(decodeVersion(bytes.readUInt32LE(offset + 12)));
    } else if (command === LC_VERSION_MIN_MACOSX) {
      assert.ok(size >= 16, 'malformed LC_VERSION_MIN_MACOSX');
      claims.push(decodeVersion(bytes.readUInt32LE(offset + 8)));
    }
    offset += size;
  }
  assert.equal(offset, end, 'malformed Mach-O load commands');
  assert.ok(claims.length > 0, 'the binary declares no minimum macOS version');
  for (const claim of claims) {
    assert.ok(sameVersion(claim, claims[0]), 'the binary declares conflicting minimum macOS versions');
  }
  return claims[0];
}

// The bundle's claim, the floor the build enforces and the binary's own floor
// are one decision, so all three are the same version. A binary that states an
// older floor is refused as much as one that states a newer one: the floor is
// what this build claims to need, and a binary that says otherwise was not
// built by the decision it is shipped under.
export function verifyMacBundleMinimumOs(plist, executable, arch = 'arm64') {
  const declared = plistMinimumSystemVersion(plist);
  assert.ok(
    sameVersion(declared, MACOS_MINIMUM_VERSION),
    `the bundle declares macOS ${formatVersion(declared)}; this build enforces ${formatVersion(MACOS_MINIMUM_VERSION)}, and the two must be the same version`,
  );
  const binary = readMachOMinimumOs(executable, arch);
  assert.ok(
    sameVersion(binary, MACOS_MINIMUM_VERSION),
    `the binary states macOS ${formatVersion(binary)}; this build enforces ${formatVersion(MACOS_MINIMUM_VERSION)}, and the two must be the same version`,
  );
  return binary;
}

export function verifyPayload(directory, sha, filename) {
  assert.match(sha, /^[0-9a-f]{40}$/);
  assert.equal(readFileSync(path.join(directory, 'SOURCE_SHA'), 'utf8').trim(), sha, 'source SHA mismatch');
  assert.deepEqual(readdirSync(directory).sort(), ['SHA256SUMS', 'SOURCE_SHA', filename].sort(), 'unexpected artifact files');
  const bytes = readFileSync(path.join(directory, filename));
  assert.ok(bytes.length > 0, 'empty artifact');
  const digest = createHash('sha256').update(bytes).digest('hex');
  assert.equal(readFileSync(path.join(directory, 'SHA256SUMS'), 'utf8').trim(), `${digest}  ${filename}`, 'checksum mismatch');
  return bytes;
}

// The four targets a release carries. Each one is a directory of its own with
// exactly one payload in it, so a payload is verified against its own
// checksum and its own source SHA rather than against a mixed set.
export const RELEASE_TARGETS = [
  { directory: 'linux-amd64', arch: 'amd64', format: 'deb' },
  { directory: 'linux-arm64', arch: 'arm64', format: 'deb' },
  { directory: 'macos-arm64', arch: 'arm64', format: 'zip' },
  { directory: 'macos-x86_64', arch: 'x86_64', format: 'zip' },
];

export function finalize(base, sha, version, output) {
  assert.match(version, /^(?:0\.\d+\.\d+|1\.0\.0-rc\.\d+)$/, 'unsupported release version');
  const debVersion = version.replace('-', '~');
  const run = (cmd, args) => execFileSync(cmd, args, { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  const payloads = RELEASE_TARGETS.map(target => {
    const directory = path.join(base, `${target.directory}-${sha}`);
    const file = target.format === 'deb'
      ? `muster_${debVersion}_${target.arch}.deb`
      : `Muster_${version}_${target.arch}.zip`;
    // GitHub replaces '~' in asset names; keep Debian ordering inside the package only.
    const published = target.format === 'deb'
      ? `muster_${version}_${target.arch}.deb`
      : file;
    verifyPayload(directory, sha, file);
    const source = path.join(directory, file);
    if (target.format === 'deb') {
      for (const [field, expected] of [['Package', 'muster'], ['Version', debVersion], ['Architecture', target.arch]]) {
        assert.equal(run('dpkg-deb', ['-f', source, field]).trim(), expected);
      }
      const contents = run('dpkg-deb', ['--contents', source]);
      for (const entry of ['usr/bin/Muster', 'usr/share/applications/dev.muster.workspace.desktop', 'usr/share/icons/hicolor/512x512/apps/muster.png', 'usr/share/doc/muster/copyright']) {
        assert.ok(contents.includes(`./${entry}`), `missing ${entry}`);
      }
      assert.ok(!contents.includes('./debian/'), 'source metadata leaked into package');
    } else {
      const plist = run('unzip', ['-p', source, 'Muster.app/Contents/Info.plist']);
      assert.equal(plist.match(/<key>CFBundleShortVersionString<\/key>\s*<string>([^<]*)<\/string>/)?.[1], version);
      const executable = execFileSync('unzip', ['-p', source, 'Muster.app/Contents/MacOS/Muster'], { maxBuffer: 256 * 1024 * 1024 });
      verifyMacBundleMinimumOs(plist, executable, target.arch);
    }
    return { published, source };
  });
  mkdirSync(output, { recursive: true });
  for (const payload of payloads) copyFileSync(payload.source, path.join(output, payload.published));
  writeFileSync(path.join(output, 'SOURCE_SHA'), `${sha}\n`);
  writeFileSync(path.join(output, 'SHA256SUMS'), payloads.map(payload => `${createHash('sha256').update(readFileSync(path.join(output, payload.published))).digest('hex')}  ${payload.published}\n`).join(''));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [, , base, sha, version, output] = process.argv;
  finalize(base, sha, version, output);
}
