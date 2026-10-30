import { createHash } from "node:crypto";
import { lstatSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const PATCHED_GPUI_COMPONENT_DIR = join(ROOT, "vendor/gpui-component-0.7.3");
const PATCHED_GPUI_COMPONENT_VERSION = "0.7.3";
// Published crates.io archive; see the vendored PATCHES.md for the local change.
export const PATCHED_GPUI_COMPONENT_ARCHIVE_SHA256 = "465a363235cd4062ef5f0953fc096fc764ed6f53dfbd9f409f669f93ea3b1ff9";
export const PATCHED_GPUI_COMPONENT_TREE_SHA256 = "f383e311fde82a55e998dcec15cf609d5bfe22cd9002fcff77beaace9e2948d4";

export function patchedGpuiComponentTreeSha256(root = PATCHED_GPUI_COMPONENT_DIR) {
  const rootInfo = lstatSync(root);
  if (!rootInfo.isDirectory() || rootInfo.isSymbolicLink()) throw new Error("vendored gpui-component root is not a directory");

  const files = [];
  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const info = lstatSync(path);
      if (info.isSymbolicLink()) throw new Error("vendored gpui-component contains a symbolic link");
      if (info.isDirectory()) visit(path);
      else if (info.isFile()) files.push(path);
      else throw new Error("vendored gpui-component contains a non-regular file");
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

export function auditedPatchedGpuiComponent(pkg) {
  const manifest = join(PATCHED_GPUI_COMPONENT_DIR, "Cargo.toml");
  if (
    !pkg ||
    pkg.name !== "gpui-component" ||
    pkg.version !== PATCHED_GPUI_COMPONENT_VERSION ||
    pkg.source !== null ||
    typeof pkg.manifest_path !== "string" ||
    resolve(pkg.manifest_path) !== manifest
  ) {
    return null;
  }
  if (realpathSync(pkg.manifest_path) !== realpathSync(manifest)) {
    throw new Error("vendored gpui-component manifest resolves outside its pinned directory");
  }
  if (patchedGpuiComponentTreeSha256() !== PATCHED_GPUI_COMPONENT_TREE_SHA256) {
    throw new Error("vendored gpui-component source does not match its pinned patch");
  }
  return { name: pkg.name, version: pkg.version, source: null };
}
