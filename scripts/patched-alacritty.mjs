import { createHash } from "node:crypto";
import { lstatSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const PATCHED_ALACRITTY_DIR = join(ROOT, "native/vendor/alacritty_terminal-0.26.0");
const PATCHED_ALACRITTY_VERSION = "0.26.0";
// Original crates.io archive checksum for alacritty_terminal 0.26.0, the latest published release.
// `Cargo.lock` cannot supply it: a `[patch.crates-io]` path replacement drops the registry
// `source` and `checksum` lines from the entry. This is the `cksum` field of the 0.26.0 entry in
// the crates.io sparse index, confirmed by hashing the archive downloaded from static.crates.io.
// Upstream has published no advisory for the panic this patch fixes; see
// `native/vendor/alacritty_terminal-0.26.0/PATCHES.md` for the defect and the one-line fix.
export const PATCHED_ALACRITTY_ARCHIVE_SHA256 = "bda177466b9524d59f1b12f0dd30b68696788e9992a7e959021c4a0ed96fcf59";
export const PATCHED_ALACRITTY_TREE_SHA256 = "b502cd0ca3c120dad7886a31005276b1003c3d4be4cd3d2c2cbb0ea4208ee0e1";
const VERIFIED_PATCH = Symbol("verified patched alacritty");

export function patchedAlacrittyTreeSha256(root = PATCHED_ALACRITTY_DIR) {
  const rootInfo = lstatSync(root);
  if (!rootInfo.isDirectory() || rootInfo.isSymbolicLink()) throw new Error("vendored alacritty root is not a directory");

  const files = [];
  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const info = lstatSync(path);
      if (info.isSymbolicLink()) throw new Error("vendored alacritty contains a symbolic link");
      if (info.isDirectory()) visit(path);
      else if (info.isFile()) files.push(path);
      else throw new Error("vendored alacritty contains a non-regular file");
    }
  }
  visit(root);

  const hash = createHash("sha256");
  for (const path of files.sort((a, b) => {
    const left = relative(root, a);
    const right = relative(root, b);
    return left < right ? -1 : left > right ? 1 : 0;
  })) {
    const contents = readFileSync(path);
    hash.update(relative(root, path).split(sep).join("/")).update("\0");
    hash.update(String(contents.length)).update("\0").update(contents).update("\0");
  }
  return hash.digest("hex");
}

export function isPatchedAlacrittyMetadata(pkg) {
  if (
    !pkg ||
    pkg.name !== "alacritty_terminal" ||
    pkg.version !== PATCHED_ALACRITTY_VERSION ||
    pkg.source !== null ||
    typeof pkg.manifest_path !== "string" ||
    resolve(pkg.manifest_path) !== join(PATCHED_ALACRITTY_DIR, "Cargo.toml")
  ) {
    return false;
  }
  if (realpathSync(pkg.manifest_path) !== realpathSync(join(PATCHED_ALACRITTY_DIR, "Cargo.toml"))) {
    throw new Error("vendored alacritty manifest resolves outside its pinned directory");
  }
  if (patchedAlacrittyTreeSha256() !== PATCHED_ALACRITTY_TREE_SHA256) {
    throw new Error("vendored alacritty source does not match its pinned patch");
  }
  return true;
}

export function auditedPatchedAlacritty(pkg) {
  if (!isPatchedAlacrittyMetadata(pkg)) return null;
  return Object.defineProperty({ name: pkg.name, version: pkg.version, source: null }, VERIFIED_PATCH, { value: true });
}

export function isAuditedAlacritty(pkg) {
  return pkg?.[VERIFIED_PATCH] === true && pkg.name === "alacritty_terminal" && pkg.version === PATCHED_ALACRITTY_VERSION;
}