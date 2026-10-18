import { createHash } from "node:crypto";
import { lstatSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const PATCHED_GITIGNORE_DIR = join(ROOT, "native/vendor/konomanoasa-tree-sitter-gitignore-0.3.1");
const PATCHED_GITIGNORE_VERSION = "0.3.1";
export const PATCHED_GITIGNORE_ARCHIVE_SHA256 = "060cb9f087373dda1b2899edb48edbdb004d7c212f1fddb2c42f977a321fd91c";
export const PATCHED_GITIGNORE_TREE_SHA256 = "168240244e0570b6ba2c4b263a336f7230f4372edae601684951ff447f57b83d";

export function patchedGitignoreTreeSha256(root = PATCHED_GITIGNORE_DIR) {
  const rootInfo = lstatSync(root);
  if (!rootInfo.isDirectory() || rootInfo.isSymbolicLink()) throw new Error("vendored gitignore root is not a directory");

  const files = [];
  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const info = lstatSync(path);
      if (info.isSymbolicLink()) throw new Error("vendored gitignore contains a symbolic link");
      if (info.isDirectory()) visit(path);
      else if (info.isFile()) files.push(path);
      else throw new Error("vendored gitignore contains a non-regular file");
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

export function auditedPatchedGitignore(pkg) {
  const manifest = join(PATCHED_GITIGNORE_DIR, "Cargo.toml");
  if (
    !pkg ||
    pkg.name !== "konomanoasa-tree-sitter-gitignore" ||
    pkg.version !== PATCHED_GITIGNORE_VERSION ||
    pkg.source !== null ||
    typeof pkg.manifest_path !== "string" ||
    resolve(pkg.manifest_path) !== manifest
  ) {
    return null;
  }
  if (realpathSync(pkg.manifest_path) !== realpathSync(manifest)) {
    throw new Error("vendored gitignore manifest resolves outside its pinned directory");
  }
  if (patchedGitignoreTreeSha256() !== PATCHED_GITIGNORE_TREE_SHA256) {
    throw new Error("vendored gitignore source does not match its pinned grammar");
  }
  return { name: pkg.name, version: pkg.version, source: null };
}
