import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { CARGO_REGISTRY_SOURCE } from "./audit-rust.mjs";
import { packageLicenseFiles, pinnedSourceLicenseFiles } from "./build-third-party-notices.mjs";

function fixture(t) {
  const base = mkdtempSync(join(tmpdir(), "muster-license-files-"));
  const dependency = join(base, "dependency");
  mkdirSync(dependency);
  t.after(() => rmSync(base, { recursive: true, force: true }));
  return { base, dependency };
}

test("reads multiple in-root license texts, including dual licenses", (t) => {
  const { dependency } = fixture(t);
  mkdirSync(join(dependency, "licenses"));
  writeFileSync(join(dependency, "LICENSE-MIT"), "MIT text\n");
  writeFileSync(join(dependency, "LICENSE-APACHE"), "Apache text\n");
  writeFileSync(join(dependency, "licenses/NOTICE.txt"), "Additional notice\n");

  const files = packageLicenseFiles(dependency, "licenses/NOTICE.txt");
  assert.deepEqual(
    files.map(({ name, text }) => [name, text]).sort(([a], [b]) => a.localeCompare(b)),
    [
      ["LICENSE-APACHE", "Apache text\n"],
      ["LICENSE-MIT", "MIT text\n"],
      ["NOTICE.txt", "Additional notice\n"],
    ],
  );
});

test("rejects escaped and absolute license_file metadata without exposing external content", (t) => {
  const { base, dependency } = fixture(t);
  const secret = "external package content must never appear in errors";
  const outside = join(base, "secret.txt");
  writeFileSync(outside, secret);

  for (const licenseFile of ["../secret.txt", outside]) {
    assert.throws(
      () => packageLicenseFiles(dependency, licenseFile),
      (error) => error.message === "Package license file escapes dependency root" && !error.message.includes(secret),
    );
  }
});

test("rejects license-name symlinks that resolve outside the dependency", (t) => {
  const { base, dependency } = fixture(t);
  const secret = "outside symlink content";
  const outside = join(base, "secret.txt");
  writeFileSync(outside, secret);
  symlinkSync(outside, join(dependency, "LICENSE-OUTSIDE"));

  assert.throws(
    () => packageLicenseFiles(dependency),
    (error) => error.message === "Package license file escapes dependency root" && !error.message.includes(secret),
  );
});

test("rejects oversized license files before reading their contents", (t) => {
  const { dependency } = fixture(t);
  writeFileSync(join(dependency, "LICENSE-LARGE"), Buffer.alloc(1024 * 1024 + 1, 0x61));

  assert.throws(() => packageLicenseFiles(dependency), /Package license file exceeds size limit/);
});

test("pinned sidecars match only the exact canonical crates.io package version", () => {
  const packageInfo = { name: "alloc-stdlib", version: "0.3.0", source: CARGO_REGISTRY_SOURCE };
  const files = pinnedSourceLicenseFiles(packageInfo);
  assert.equal(files.length, 1);
  assert.match(files[0].source, /rust-alloc-no-stdlib\/blob\/0a81fd6928ea3b33c8cd484aa4575d50ffb98012/);
  assert.deepEqual(pinnedSourceLicenseFiles({ ...packageInfo, version: "0.3.1" }), []);
});

test("rejects Git and foreign-registry packages even when name and version match a sidecar", () => {
  const packageInfo = { name: "alloc-stdlib", version: "0.3.0" };
  for (const source of ["git+https://example.test/repo", "registry+https://example.test/index"]) {
    assert.throws(
      () => pinnedSourceLicenseFiles({ ...packageInfo, source }),
      (error) => error.message === "unsupported Cargo dependency source for alloc-stdlib@0.3.0",
    );
  }
});

test("rejects a changed pinned license sidecar by its expected SHA-256", (t) => {
  const { base } = fixture(t);
  const sidecarRoot = join(base, "sidecars");
  mkdirSync(sidecarRoot);
  writeFileSync(join(sidecarRoot, "alloc-stdlib-0.3.0-LICENSE.txt"), "changed legal text\n");

  assert.throws(
    () =>
      pinnedSourceLicenseFiles(
        { name: "alloc-stdlib", version: "0.3.0", source: CARGO_REGISTRY_SOURCE },
        { sidecarRoot },
      ),
    /Pinned license sidecar hash mismatch for alloc-stdlib@0\.3\.0/,
  );
});
