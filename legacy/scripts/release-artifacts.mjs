import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const TARGETS = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];
const EXTENSIONS = {
  "aarch64-apple-darwin": [".dmg"],
  "x86_64-unknown-linux-gnu": [".deb", ".AppImage"],
  "aarch64-unknown-linux-gnu": [".deb", ".AppImage"],
};

function options(args) {
  const result = {};
  for (let i = 0; i < args.length; i += 2) {
    if (!args[i].startsWith("--") || !args[i + 1]) throw new Error("expected --option value");
    result[args[i].slice(2)] = args[i + 1];
  }
  return result;
}

function digest(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function filesUnder(root) {
  const result = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) result.push(...filesUnder(path));
    else if (entry.isFile() && Object.values(EXTENSIONS).flat().some((ext) => entry.name.endsWith(ext))) result.push(path);
  }
  return result;
}

function hasVersion(name, version) {
  let index = name.indexOf(version);
  while (index !== -1) {
    const before = index === 0 || "_-".includes(name[index - 1]);
    const next = index + version.length;
    const after = next === name.length || "_-.".includes(name[next]);
    if (before && after) return true;
    index = name.indexOf(version, index + 1);
  }
  return false;
}

function command(name, args) {
  const result = spawnSync(name, args, { encoding: "utf8" });
  if (result.error || result.status !== 0) throw new Error(result.stderr || "failed to run " + name);
  return result.stdout.trim();
}

function validateTarget(root, target, version) {
  if (!EXTENSIONS[target]) throw new Error("unsupported target: " + target);
  const assets = filesUnder(root).filter((path) => EXTENSIONS[target].some((ext) => path.endsWith(ext)));
  const expected = EXTENSIONS[target];
  if (assets.length !== expected.length || expected.some((ext) => assets.filter((path) => path.endsWith(ext)).length !== 1)) {
    throw new Error(target + " expected exactly " + expected.join(" and ") + "; found " + assets.map(basename).join(", "));
  }
  const names = new Set();
  for (const path of assets) {
    const name = basename(path);
    if (!statSync(path).size) throw new Error("empty release artifact: " + name);
    if (!hasVersion(name, version)) throw new Error(name + " does not contain release version " + version);
    if (names.has(name)) throw new Error("duplicate artifact name: " + name);
    names.add(name);
    if (path.endsWith(".dmg")) {
      if (!name.includes("aarch64")) throw new Error("DMG name does not identify aarch64: " + name);
    } else if (path.endsWith(".deb")) {
      const arch = command("dpkg-deb", ["-f", path, "Architecture"]);
      const debVersion = command("dpkg-deb", ["-f", path, "Version"]);
      const wantedArch = target.startsWith("x86_64") ? "amd64" : "arm64";
      if (arch !== wantedArch || debVersion !== version) throw new Error(name + " has Debian metadata " + debVersion + "/" + arch + ", expected " + version + "/" + wantedArch);
    } else {
      const description = command("file", ["-b", path]);
      const matches = target.startsWith("x86_64") ? /x86-64|x86_64/i.test(description) : /aarch64|ARM64/i.test(description);
      if (!matches) throw new Error(name + " architecture does not match " + target + ": " + description);
    }
  }
  return assets.map((path) => ({ name: basename(path), size: statSync(path).size, sha256: digest(path), path }));
}

function collect(args) {
  const opts = options(args);
  const root = resolve(opts.root);
  const out = resolve(opts.out);
  const version = opts.version;
  const sha = opts.sha;
  if (!/^0\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version || "")) throw new Error("release version must be v0.*.*");
  if (!/^[0-9a-f]{40}$/.test(sha || "")) throw new Error("release SHA must be a full lowercase commit SHA");
  const target = opts.target;
  const assets = validateTarget(root, target, version);
  mkdirSync(out, { recursive: true });
  for (const asset of assets) {
    const path = join(out, asset.name);
    if (resolve(asset.path) !== path && readdirSync(out).includes(asset.name)) throw new Error("duplicate artifact output: " + asset.name);
    copyFileSync(asset.path, path);
    delete asset.path;
  }
  writeFileSync(join(out, "release-target.json"), JSON.stringify({ target, version, sha, assets }, null, 2) + "\n");
  console.log(target + ": verified " + assets.map((asset) => asset.name).join(", "));
}

function verifyTargetDirectory(directory, version, sha) {
  const metadataPath = join(directory, "release-target.json");
  const metadata = JSON.parse(readFileSync(metadataPath, "utf8"));
  if (!TARGETS.includes(metadata.target) || metadata.version !== version || metadata.sha !== sha) throw new Error("target manifest does not match requested version/SHA");
  const found = validateTarget(directory, metadata.target, version);
  const recorded = new Map(metadata.assets.map((asset) => [asset.name, asset]));
  if (found.length !== recorded.size) throw new Error("target manifest asset count mismatch for " + metadata.target);
  for (const asset of found) {
    const old = recorded.get(asset.name);
    if (!old || old.size !== asset.size || old.sha256 !== asset.sha256) throw new Error("target manifest hash mismatch: " + asset.name);
    delete asset.path;
  }
  return { target: metadata.target, assets: found };
}

function shaFile(root) {
  const manifest = JSON.parse(readFileSync(join(root, "release-manifest.json"), "utf8"));
  const lines = manifest.assets.map((asset) => asset.sha256 + "  " + asset.name);
  lines.push(digest(join(root, "release-manifest.json")) + "  release-manifest.json");
  return lines.sort().join("\n") + "\n";
}

function verifyFinal(root, version, sha) {
  const manifest = JSON.parse(readFileSync(join(root, "release-manifest.json"), "utf8"));
  if (manifest.version !== version || manifest.sha !== sha || manifest.targets.length !== TARGETS.length) throw new Error("release manifest version/SHA/target count mismatch");
  const assets = manifest.assets;
  if (assets.length !== 5 || new Set(assets.map((asset) => asset.name)).size !== assets.length) throw new Error("expected five uniquely named release bundles");
  for (const asset of assets) {
    const path = join(root, asset.name);
    if (!statSync(path).isFile() || statSync(path).size !== asset.size || digest(path) !== asset.sha256) throw new Error("release asset hash mismatch: " + asset.name);
  }
  const actualNames = readdirSync(root).filter((name) => !["release-manifest.json", "SHA256SUMS"].includes(name)).sort();
  if (actualNames.join("\n") !== assets.map((asset) => asset.name).sort().join("\n")) throw new Error("unexpected or missing final release assets");
  const expectedChecksums = shaFile(root);
  if (readFileSync(join(root, "SHA256SUMS"), "utf8") !== expectedChecksums) throw new Error("SHA256SUMS does not match release manifest");
  console.log("verified final assets for " + version + " at " + sha);
}

function finalize(args) {
  const opts = options(args);
  const root = resolve(opts.root);
  const out = resolve(opts.out);
  const version = opts.version;
  const sha = opts.sha;
  const manifests = [];
  function findManifests(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) findManifests(path);
      else if (entry.isFile() && entry.name === "release-target.json") manifests.push(path);
    }
  }
  findManifests(root);
  const targets = manifests.map((path) => verifyTargetDirectory(resolve(path, ".."), version, sha));
  if (targets.length !== TARGETS.length || TARGETS.some((name) => targets.filter((item) => item.target === name).length !== 1)) throw new Error("artifacts do not include exactly one successful build for every release target");
  const names = new Set();
  const assets = [];
  mkdirSync(out, { recursive: true });
  for (const target of targets) {
    for (const asset of target.assets) {
      if (names.has(asset.name)) throw new Error("duplicate release artifact name: " + asset.name);
      names.add(asset.name);
      copyFileSync(join(root, "release-" + target.target, asset.name), join(out, asset.name));
      assets.push({ ...asset, target: target.target });
    }
  }
  const manifest = { schema: 1, version, sha, targets: TARGETS, assets: assets.sort((a, b) => a.name.localeCompare(b.name)) };
  writeFileSync(join(out, "release-manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  writeFileSync(join(out, "SHA256SUMS"), shaFile(out));
  verifyFinal(out, version, sha);
}

const [mode, ...args] = process.argv.slice(2);
try {
  if (mode === "collect") collect(args);
  else if (mode === "finalize") finalize(args);
  else if (mode === "verify") {
    const opts = options(args);
    verifyFinal(resolve(opts.root), opts.version, opts.sha);
  } else throw new Error("usage: release-artifacts.mjs collect|finalize|verify --root path --version v0.x.y --sha <full-sha> [--target target] [--out path]");
} catch (error) {
  console.error("release artifact validation failed: " + error.message);
  process.exitCode = 1;
}