import { createHash } from "node:crypto";
import { lstatSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const PATCHED_GLIB_DIR = join(ROOT, "src-tauri/vendor/glib-0.18.5");
export const PATCHED_GLIB_ADVISORY_IDS = Object.freeze(["RUSTSEC-2024-0429", "GHSA-WRW7-89JP-8Q8G"]);
const PATCHED_GLIB_VERSION = "0.18.5";
// Original crates.io archive checksum from Cargo.lock before applying gtk-rs/gtk-rs-core#1343.
export const PATCHED_GLIB_ARCHIVE_SHA256 = "233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5";
const PATCHED_GLIB_TREE_SHA256 = "cf39ca9b08530abb4d928d004438d2ad99a220339aefe2b5280663a8fb45aef0";
const VERIFIED_PATCH = Symbol("verified patched glib");

export function patchedGlibTreeSha256(root = PATCHED_GLIB_DIR) {
  const rootInfo = lstatSync(root);
  if (!rootInfo.isDirectory() || rootInfo.isSymbolicLink()) throw new Error("vendored glib root is not a directory");

  const files = [];
  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const info = lstatSync(path);
      if (info.isSymbolicLink()) throw new Error("vendored glib contains a symbolic link");
      if (info.isDirectory()) visit(path);
      else if (info.isFile()) files.push(path);
      else throw new Error("vendored glib contains a non-regular file");
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

export function isPatchedGlibMetadata(pkg) {
  if (
    !pkg ||
    pkg.name !== "glib" ||
    pkg.version !== PATCHED_GLIB_VERSION ||
    pkg.source !== null ||
    typeof pkg.manifest_path !== "string" ||
    resolve(pkg.manifest_path) !== join(PATCHED_GLIB_DIR, "Cargo.toml")
  ) {
    return false;
  }
  if (realpathSync(pkg.manifest_path) !== realpathSync(join(PATCHED_GLIB_DIR, "Cargo.toml"))) {
    throw new Error("vendored glib manifest resolves outside its pinned directory");
  }
  if (patchedGlibTreeSha256() !== PATCHED_GLIB_TREE_SHA256) {
    throw new Error("vendored glib source does not match its pinned backport");
  }
  return true;
}

export function auditedPatchedGlib(pkg) {
  if (!isPatchedGlibMetadata(pkg)) return null;
  return Object.defineProperty({ name: pkg.name, version: pkg.version, source: null }, VERIFIED_PATCH, { value: true });
}

export function isAuditedPatchedGlib(pkg) {
  return pkg?.[VERIFIED_PATCH] === true && pkg.name === "glib" && pkg.version === PATCHED_GLIB_VERSION;
}
