import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

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

export function finalize(base, sha, version, output) {
  assert.match(version, /^(?:0\.\d+\.\d+|1\.0\.0-rc\.\d+)$/, 'unsupported release version');
  const debVersion = version.replace('-', '~');
  const deb = `muster_${debVersion}_amd64.deb`;
  const mac = `Muster_${version}_arm64.zip`;
  const linuxDir = path.join(base, `linux-${sha}`);
  const macDir = path.join(base, `macos-${sha}`);
  verifyPayload(linuxDir, sha, deb);
  verifyPayload(macDir, sha, mac);
  const debPath = path.join(linuxDir, deb);
  const macPath = path.join(macDir, mac);
  const run = (cmd, args) => execFileSync(cmd, args, { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  for (const [field, expected] of [['Package', 'muster'], ['Version', debVersion], ['Architecture', 'amd64']]) {
    assert.equal(run('dpkg-deb', ['-f', debPath, field]).trim(), expected);
  }
  const contents = run('dpkg-deb', ['--contents', debPath]);
  for (const file of ['usr/bin/Muster', 'usr/share/applications/muster.desktop', 'usr/share/icons/hicolor/512x512/apps/muster.png', 'usr/share/doc/muster/copyright']) {
    assert.ok(contents.includes(`./${file}`), `missing ${file}`);
  }
  assert.ok(!contents.includes('./debian/'), 'source metadata leaked into package');
  const plist = run('unzip', ['-p', macPath, 'Muster.app/Contents/Info.plist']);
  assert.equal(plist.match(/<key>CFBundleShortVersionString<\/key>\s*<string>([^<]*)<\/string>/)?.[1], version);
  const executable = execFileSync('unzip', ['-p', macPath, 'Muster.app/Contents/MacOS/Muster'], { maxBuffer: 256 * 1024 * 1024 });
  assert.equal(executable.readUInt32LE(0), 0xfeedfacf, 'not a 64-bit Mach-O');
  assert.equal(executable.readUInt32LE(4), 0x100000c, 'not macOS arm64');
  mkdirSync(output, { recursive: true });
  for (const [directory, filename] of [[linuxDir, deb], [macDir, mac]]) copyFileSync(path.join(directory, filename), path.join(output, filename));
  writeFileSync(path.join(output, 'SOURCE_SHA'), `${sha}\n`);
  writeFileSync(path.join(output, 'SHA256SUMS'), [deb, mac].map(filename => `${createHash('sha256').update(readFileSync(path.join(output, filename))).digest('hex')}  ${filename}\n`).join(''));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [, , base, sha, version, output] = process.argv;
  finalize(base, sha, version, output);
}
