import assert from "node:assert/strict";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const SCRIPT = fileURLToPath(new URL("./release-artifacts.mjs", import.meta.url));
const VERSION = "0.21.0";
const SHA = "0123456789abcdef0123456789abcdef01234567";
const TARGETS = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "muster-release-artifacts-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, "bin");
  mkdirSync(bin);
  writeFileSync(
    join(bin, "dpkg-deb"),
    '#!/bin/sh\nif [ "$3" = Version ]; then echo 0.21.0; elif grep -q arm64 "$2"; then echo arm64; else echo amd64; fi\n',
  );
  writeFileSync(
    join(bin, "file"),
    '#!/bin/sh\nif [ "$1" = "-b" ]; then shift; prefix=""; else prefix="$1: "; fi\nif grep -q ARM64_PAYLOAD "$1"; then printf "%sELF 64-bit LSB executable, ARM aarch64\n" "$prefix"; else printf "%sELF 64-bit LSB executable, x86-64\n" "$prefix"; fi\n',
  );
  chmodSync(join(bin, "dpkg-deb"), 0o755);
  chmodSync(join(bin, "file"), 0o755);
  const env = { ...process.env, PATH: bin + ":" + process.env.PATH };
  return { root, env };
}

function run(env, ...args) {
  return spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8", env });
}

function collect(f, target, filenames, contents = {}) {
  const source = join(f.root, "bundles-" + target);
  const out = join(f.root, "release-" + target);
  mkdirSync(source);
  for (const name of filenames) {
    const payload =
      contents[name] ??
      (name.endsWith(".AppImage") ? (/aarch64|arm64/i.test(name) ? "ARM64_PAYLOAD" : "X86_64_PAYLOAD") : name);
    writeFileSync(join(source, name), payload);
  }
  const result = run(
    f.env,
    "collect",
    "--root",
    source,
    "--target",
    target,
    "--version",
    VERSION,
    "--sha",
    SHA,
    "--out",
    out,
  );
  assert.equal(result.status, 0, result.stderr);
  return out;
}

function allTargets(f, omit) {
  const names = {
    "aarch64-apple-darwin": ["Muster_0.21.0_aarch64.dmg"],
    "x86_64-unknown-linux-gnu": ["muster_0.21.0_amd64.deb", "Muster_0.21.0_amd64.AppImage"],
    "aarch64-unknown-linux-gnu": ["muster_0.21.0_arm64.deb", "Muster_0.21.0_aarch64.AppImage"],
  };
  return TARGETS.filter((target) => target !== omit).map((target) => collect(f, target, names[target]));
}

test("finalize accepts only complete target artifacts and emits verified checksums", (t) => {
  const f = fixture(t);
  allTargets(f);
  const out = join(f.root, "final");
  const result = run(f.env, "finalize", "--root", f.root, "--out", out, "--version", VERSION, "--sha", SHA);
  assert.equal(result.status, 0, result.stderr);
  const manifest = JSON.parse(readFileSync(join(out, "release-manifest.json"), "utf8"));
  assert.equal(manifest.assets.length, 5);
  assert.deepEqual(manifest.targets, TARGETS);
  assert.equal(readFileSync(join(out, "SHA256SUMS"), "utf8").trim().split("\n").length, 6);
  writeFileSync(join(out, manifest.assets[0].name), "tampered");
  const verify = run(f.env, "verify", "--root", out, "--version", VERSION, "--sha", SHA);
  assert.equal(verify.status, 1);
  assert.match(verify.stderr, /hash mismatch/);
});

test("finalize rejects missing platforms and a target built from another SHA", (t) => {
  const missing = fixture(t);
  allTargets(missing, "aarch64-unknown-linux-gnu");
  const omitted = run(
    missing.env,
    "finalize",
    "--root",
    missing.root,
    "--out",
    join(missing.root, "final"),
    "--version",
    VERSION,
    "--sha",
    SHA,
  );
  assert.equal(omitted.status, 1);
  assert.match(omitted.stderr, /exactly one successful build for every release target/);

  const wrongSha = fixture(t);
  const paths = allTargets(wrongSha);
  const metadataPath = join(paths[0], "release-target.json");
  const metadata = JSON.parse(readFileSync(metadataPath, "utf8"));
  metadata.sha = "f".repeat(40);
  writeFileSync(metadataPath, JSON.stringify(metadata));
  const mismatch = run(
    wrongSha.env,
    "finalize",
    "--root",
    wrongSha.root,
    "--out",
    join(wrongSha.root, "final"),
    "--version",
    VERSION,
    "--sha",
    SHA,
  );
  assert.equal(mismatch.status, 1);
  assert.match(mismatch.stderr, /version\/SHA/);
});

test("AppImage architecture checks ignore target-looking filenames", (t) => {
  const cases = [
    {
      target: "x86_64-unknown-linux-gnu",
      deb: "muster_0.21.0_amd64.deb",
      appImage: "Muster_0.21.0_x86_64.AppImage",
      payload: "ARM64_PAYLOAD",
    },
    {
      target: "aarch64-unknown-linux-gnu",
      deb: "muster_0.21.0_arm64.deb",
      appImage: "Muster_0.21.0_aarch64.AppImage",
      payload: "X86_64_PAYLOAD",
    },
  ];
  for (const item of cases) {
    const f = fixture(t);
    const source = join(f.root, "wrong-architecture");
    mkdirSync(source);
    writeFileSync(join(source, item.deb), "deb");
    writeFileSync(join(source, item.appImage), item.payload);
    const result = run(
      f.env,
      "collect",
      "--root",
      source,
      "--target",
      item.target,
      "--version",
      VERSION,
      "--sha",
      SHA,
      "--out",
      join(f.root, "out"),
    );
    assert.equal(result.status, 1);
    assert.match(result.stderr, /architecture does not match/);
  }
});

test("collect rejects wrong-version bundles before upload", (t) => {
  const f = fixture(t);
  const source = join(f.root, "bad");
  mkdirSync(source);
  writeFileSync(join(source, "Muster_0.20.0_aarch64.dmg"), "not current");
  const result = run(
    f.env,
    "collect",
    "--root",
    source,
    "--target",
    "aarch64-apple-darwin",
    "--version",
    VERSION,
    "--sha",
    SHA,
    "--out",
    join(f.root, "out"),
  );
  assert.equal(result.status, 1);
  assert.match(result.stderr, /does not contain release version/);
});
