import { createHash } from "node:crypto";
import { lstatSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const PATCHED_GHERKIN_DIR = join(ROOT, "vendor/tree-sitter-gherkin-ceeb7c8");
const PATCHED_GHERKIN_VERSION = "0.0.1";
// The upstream archive this grammar was vendored from: the repository's own
// tarball at the commit `vendor/tree-sitter-gherkin-ceeb7c8/PATCHES.md` names.
// There is no crates.io release to hash, so the upstream commit is the pin and
// the archive is what proves which tree it was.
export const PATCHED_GHERKIN_ARCHIVE_SHA256 = "7fbdc4a871fb2de855d043f00c0b7414241c2c0c5ca5bfe7a237cd14e57dc5d5";
export const PATCHED_GHERKIN_TREE_SHA256 = "5b9770b0f6e3fe5fcc3c13fa2820760d4a8d4903d41109ed360679c0fe3b1e4d";

export function patchedGherkinTreeSha256(root = PATCHED_GHERKIN_DIR) {
  const rootInfo = lstatSync(root);
  if (!rootInfo.isDirectory() || rootInfo.isSymbolicLink()) throw new Error("vendored gherkin root is not a directory");

  const files = [];
  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const info = lstatSync(path);
      if (info.isSymbolicLink()) throw new Error("vendored gherkin contains a symbolic link");
      if (info.isDirectory()) visit(path);
      else if (info.isFile()) files.push(path);
      else throw new Error("vendored gherkin contains a non-regular file");
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

export function auditedPatchedGherkin(pkg) {
  const manifest = join(PATCHED_GHERKIN_DIR, "Cargo.toml");
  if (
    !pkg ||
    pkg.name !== "tree-sitter-gherkin" ||
    pkg.version !== PATCHED_GHERKIN_VERSION ||
    pkg.source !== null ||
    typeof pkg.manifest_path !== "string" ||
    resolve(pkg.manifest_path) !== manifest
  ) {
    return null;
  }
  if (realpathSync(pkg.manifest_path) !== realpathSync(manifest)) {
    throw new Error("vendored gherkin manifest resolves outside its pinned directory");
  }
  if (patchedGherkinTreeSha256() !== PATCHED_GHERKIN_TREE_SHA256) {
    throw new Error("vendored gherkin source does not match its pinned grammar");
  }
  return { name: pkg.name, version: pkg.version, source: null };
}
