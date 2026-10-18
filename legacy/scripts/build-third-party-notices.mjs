import { createHash } from "node:crypto";
import {
  closeSync,
  constants as fsConstants,
  fstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  readSync,
  realpathSync,
  readdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { CARGO_REGISTRY_SOURCE } from "./audit-rust.mjs";
import { isPatchedGlibMetadata } from "./patched-glib.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CARGO_MANIFEST = join(ROOT, "src-tauri/Cargo.toml");
const DIST = join(ROOT, "dist");
const LICENSE_FILE = /^(?:license|licence|copying|copyright|notice)(?:[._-].*)?$/i;
const MAX_LICENSE_BYTES = 1024 * 1024;
const PINNED_SOURCE_LICENSES = {
  "alloc-stdlib@0.3.0": [
    {
      file: "alloc-stdlib-0.3.0-LICENSE.txt",
      sha256: "c0c56f26d9c051cac4d200c34c84e7ae9aaa853e01a982a1df08b09931e518ae",
      source: "https://github.com/dropbox/rust-alloc-no-stdlib/blob/0a81fd6928ea3b33c8cd484aa4575d50ffb98012/LICENSE",
    },
  ],
  "defmt-parser@1.0.0": [
    {
      file: "defmt-parser-1.0.0-LICENSE-MIT.txt",
      sha256: "0d17b75c1867fd568bcbb735f329d0d4253846c4b756a65e4d440c1e4bd59187",
      source: "https://github.com/knurling-rs/defmt/blob/4a8cdb44891ed57b8ff5a023b6bec7137c48708f/LICENSE-MIT",
    },
  ],
  "dlopen2@0.8.2": [
    {
      file: "dlopen2-0.8.2-LICENSE.txt",
      sha256: "39fa265207450e77c62e90c5594a06c085b655d8374c7ced4bf7894b6bd95dd2",
      source: "https://github.com/OpenByteDev/dlopen2/blob/cc80e4a0a90d499b677fdf7743699b4b3a43a989/LICENSE",
    },
  ],
  "dlopen2_derive@0.4.3": [
    {
      file: "dlopen2-0.8.2-LICENSE.txt",
      sha256: "39fa265207450e77c62e90c5594a06c085b655d8374c7ced4bf7894b6bd95dd2",
      source: "https://github.com/OpenByteDev/dlopen2/blob/cc80e4a0a90d499b677fdf7743699b4b3a43a989/LICENSE",
    },
  ],
  "ndk@0.9.0": [
    {
      file: "ndk-0.9.0-LICENSE-MIT.txt",
      sha256: "508a77d2e7b51d98adeed32648ad124b7b30241a8e70b2e72c99f92d8e5874d1",
      source: "https://github.com/rust-mobile/ndk/blob/49bbbba16c58ff63cb8a0ad0eca5a9fb7ecaec25/LICENSE-MIT",
    },
  ],
  "ndk-sys@0.6.0+11769913": [
    {
      file: "ndk-0.9.0-LICENSE-MIT.txt",
      sha256: "508a77d2e7b51d98adeed32648ad124b7b30241a8e70b2e72c99f92d8e5874d1",
      source: "https://github.com/rust-mobile/ndk/blob/49bbbba16c58ff63cb8a0ad0eca5a9fb7ecaec25/LICENSE-MIT",
    },
  ],
  "clipboard-win@5.4.1": [
    {
      file: "clipboard-win-5.4.1-LICENSE.txt",
      sha256: "c9bff75738922193e67fa726fa225535870d2aa1059f91452c411736284ad566",
      source: "https://github.com/DoumanAsh/clipboard-win/blob/3b27cf2bfd1adcfa6e0264eb51c1025ddaf0f342/LICENSE",
    },
  ],
};

function run(command, args, { shell = false } = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    shell,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`${command} failed: ${result.error?.message ?? result.stderr.trim()}`);
  }
  return result.stdout;
}

class LicenseFileError extends Error {}

function isWithin(root, path) {
  const relativePath = relative(root, path);
  return (
    relativePath !== "" && relativePath !== ".." && !relativePath.startsWith(`..${sep}`) && !isAbsolute(relativePath)
  );
}

function readLicenseFile(root, candidate) {
  let path;
  try {
    path = realpathSync(candidate);
  } catch {
    throw new LicenseFileError("Package license file is missing or unreadable");
  }
  if (!isWithin(root, path)) throw new LicenseFileError("Package license file escapes dependency root");

  let fd;
  try {
    fd = openSync(path, fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0));
    const before = fstatSync(fd);
    if (!before.isFile()) throw new LicenseFileError("Package license file is not a regular file");
    if (before.size > MAX_LICENSE_BYTES) throw new LicenseFileError("Package license file exceeds size limit");
    const currentPath = realpathSync(candidate);
    if (!isWithin(root, currentPath)) throw new LicenseFileError("Package license file escapes dependency root");
    const currentFile = statSync(currentPath);
    if (currentPath !== path || currentFile.dev !== before.dev || currentFile.ino !== before.ino) {
      throw new LicenseFileError("Package license file changed while being opened");
    }

    const buffer = Buffer.alloc(before.size + 1);
    let size = 0;
    while (size < buffer.length) {
      const count = readSync(fd, buffer, size, buffer.length - size, size);
      if (count === 0) break;
      size += count;
    }
    const after = fstatSync(fd);
    if (size > MAX_LICENSE_BYTES || after.size > MAX_LICENSE_BYTES) {
      throw new LicenseFileError("Package license file exceeds size limit");
    }
    if (before.size !== after.size || size !== after.size) {
      throw new LicenseFileError("Package license file changed while being read");
    }
    return { name: basename(path), text: buffer.toString("utf8", 0, size) };
  } catch (error) {
    if (error instanceof LicenseFileError) throw error;
    throw new LicenseFileError("Package license file is missing or unreadable");
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

export function pinnedSourceLicenseFiles(
  { name, version, source },
  { sidecarRoot = join(ROOT, "scripts/third-party-licenses") } = {},
) {
  const label = `${name}@${version}`;
  if (source !== CARGO_REGISTRY_SOURCE) throw new Error(`unsupported Cargo dependency source for ${label}`);
  const files = PINNED_SOURCE_LICENSES[label] ?? [];
  if (files.length === 0) return [];
  const root = realpathSync(sidecarRoot);
  return files.map(({ file, sha256, source }) => {
    const license = readLicenseFile(root, resolve(root, file));
    const digest = createHash("sha256").update(license.text).digest("hex");
    if (digest !== sha256) throw new Error(`Pinned license sidecar hash mismatch for ${label}`);
    return { ...license, source: `${source} (sidecar SHA-256: ${sha256})` };
  });
}

export function packageLicenseFiles(directory, licenseFile) {
  let root;
  let entries;
  try {
    root = realpathSync(directory);
    entries = readdirSync(root, { withFileTypes: true });
  } catch {
    throw new LicenseFileError("Dependency license root is missing or unreadable");
  }

  const paths = entries.filter((entry) => LICENSE_FILE.test(entry.name)).map((entry) => join(root, entry.name));
  if (licenseFile !== undefined && licenseFile !== null) {
    if (typeof licenseFile !== "string") throw new LicenseFileError("Invalid Cargo license_file metadata");
    paths.push(resolve(root, licenseFile));
  }
  return [...new Set(paths)].map((path) => readLicenseFile(root, path));
}

function addPackage(records, packageInfo, kind) {
  const { name, version, license, directory, licenseFile, source } = packageInfo;
  if (!name || !version || !license) {
    throw new Error(`${kind} package has incomplete installed license metadata: ${name ?? "unknown"}`);
  }
  const label = `${name}@${version}`;
  const installedFiles = packageLicenseFiles(directory, licenseFile);
  const sourceFiles =
    kind === "Rust production" && source === CARGO_REGISTRY_SOURCE
      ? pinnedSourceLicenseFiles({ name, version, source })
      : [];
  const files = [...installedFiles, ...sourceFiles];
  const textSource =
    installedFiles.length > 0
      ? ""
      : sourceFiles.length > 0
        ? " [pinned upstream license text]"
        : " [no installed or pinned license text]";
  records.packages.add(`${label} — ${license}${textSource}`);
  if (files.length === 0) records.missing.add(`${kind}: ${label} — ${license}`);
  for (const { name, text, source } of files) {
    const digest = createHash("sha256").update(text).digest("hex");
    const group = records.texts.get(digest) ?? { text, sources: new Set() };
    group.sources.add(`${label} (${name}${source ? `; ${source}` : ""})`);
    records.texts.set(digest, group);
  }
}

function installedJavascriptPackages() {
  const args = ["licenses", "list", "--prod", "--json"];
  const output = run(process.platform === "win32" ? "pnpm.cmd" : "pnpm", args, { shell: process.platform === "win32" });
  const grouped = JSON.parse(output);
  const records = { packages: new Set(), texts: new Map(), missing: new Set() };
  for (const entries of Object.values(grouped)) {
    if (!Array.isArray(entries)) throw new Error("pnpm returned malformed production license metadata");
    for (const entry of entries) {
      for (const directory of entry.paths ?? []) {
        const manifest = JSON.parse(readFileSync(join(directory, "package.json"), "utf8"));
        addPackage(
          records,
          {
            name: manifest.name,
            version: manifest.version,
            license: entry.license,
            directory,
          },
          "JavaScript production",
        );
      }
    }
  }
  return records;
}

function installedRustPackages() {
  const output = run("cargo", ["metadata", "--locked", "--format-version", "1", "--manifest-path", CARGO_MANIFEST]);
  const metadata = JSON.parse(output);
  if (!Array.isArray(metadata.packages) || !Array.isArray(metadata.workspace_members) || !metadata.resolve?.nodes) {
    throw new Error("cargo metadata returned malformed dependency data");
  }

  const packages = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const workspaceMembers = new Set(metadata.workspace_members);
  const root = metadata.packages.find((pkg) => resolve(pkg.manifest_path) === CARGO_MANIFEST);
  if (!root || !workspaceMembers.has(root.id)) throw new Error("cargo metadata did not identify the Muster package");

  const reachable = new Set();
  function visit(id) {
    if (reachable.has(id)) return;
    reachable.add(id);
    const node = nodes.get(id);
    if (!node) throw new Error(`cargo metadata has no resolved node for ${id}`);
    for (const dependency of node.deps) {
      if (dependency.dep_kinds?.some(({ kind }) => kind === null)) visit(dependency.pkg);
    }
  }
  visit(root.id);

  const records = { packages: new Set(), texts: new Map(), missing: new Set() };
  for (const id of reachable) {
    if (workspaceMembers.has(id)) continue;
    const pkg = packages.get(id);
    const patchedGlib = pkg && isPatchedGlibMetadata(pkg);
    if (!pkg || (pkg.source !== CARGO_REGISTRY_SOURCE && !patchedGlib)) {
      const label = pkg ? `${pkg.name}@${pkg.version}` : id;
      throw new Error(`unsupported Cargo dependency source for ${label}`);
    }
    addPackage(
      records,
      {
        name: pkg.name,
        version: pkg.version,
        source: pkg.source,
        license: pkg.license,
        directory: dirname(pkg.manifest_path),
        licenseFile: pkg.license_file,
      },
      "Rust production",
    );
  }
  return records;
}

function renderRecords(records) {
  const packages = [...records.packages].sort();
  const texts = [...records.texts.values()].sort((a, b) => [...a.sources].join().localeCompare([...b.sources].join()));
  return [
    ...packages.map((entry) => `- ${entry}`),
    "",
    ...texts.flatMap(({ sources, text }) => [`### ${[...sources].sort().join("; ")}`, "", text.trimEnd(), ""]),
  ].join("\n");
}

function buildNotices() {
  const javascript = installedJavascriptPackages();
  const rust = installedRustPackages();
  mkdirSync(join(DIST, "licenses"), { recursive: true });
  writeFileSync(
    join(DIST, "assets/FiraCode-OFL-1.1.txt"),
    readFileSync(join(ROOT, "src/assets/fonts/LICENSE-FiraCode.txt")),
  );
  writeFileSync(join(DIST, "licenses/Muster-MIT.txt"), readFileSync(join(ROOT, "LICENSE")));
  const musterLicense = readFileSync(join(DIST, "licenses/Muster-MIT.txt"), "utf8").trimEnd();
  const catppuccinLicense = readFileSync(join(DIST, "assets/Catppuccin-MIT.txt"), "utf8").trimEnd();
  const firaLicense = readFileSync(join(DIST, "assets/FiraCode-OFL-1.1.txt"), "utf8").trimEnd();

  const notice = [
    "Muster — third-party production notices",
    "",
    "Project license: licenses/Muster-MIT.txt.",
    "",
    "## Muster",
    "",
    musterLicense,
    "",
    "## Catppuccin icons",
    "",
    catppuccinLicense,
    "",
    "## Fira Code font files",
    "",
    firaLicense,
    "",
    "JavaScript package license files are read from pnpm's installed production package metadata.",
    "Rust package license files are read from Cargo metadata's resolved normal-dependency graph; target-conditional crates are included, but not every crate appears in every artifact.",
    "A small set of crate archives omits license texts. Exact-revision sidecars are included only when their upstream source revision is recorded and the local SHA-256 matches; builds do not fetch them.",
    "Packages without installed or pinned license text are listed by declared metadata and marked. For verified OR-licensed sidecars, the MIT option is included; the package inventory keeps the full declared expression.",
    "This crate inventory does not enumerate Linux GTK/WebKit or other dynamically linked system libraries, C/C++ libraries, or framework/FFI obligations; those require review against each target's actual runtime dependencies.",
    "The bundled text faces are FiraCode-Regular.woff2 and FiraCode-Bold.woff2, which are modified versions: TrueType hinting bytecode and the fpgm, prep, cvt and gasp tables are removed so that macOS and Linux rasterize the same outlines, and no outline, advance width or layout feature is changed. NerdSymbols.woff2 is an unmodified subset of a FiraCode Nerd Font glyph-patched file. The Fira Code license declares no reserved font name. This source tree contains the Fira Code OFL text but no verified upstream glyph-set license inventory; that OFL text does not establish separate glyph licensing, and no additional glyph license is claimed by this notice.",
    "",
    "JavaScript production dependencies",
    "-------------------------------",
    renderRecords(javascript),
    "",
    "Rust production dependencies",
    "----------------------------",
    renderRecords(rust),
  ].join("\n");

  writeFileSync(join(DIST, "licenses/THIRD-PARTY-NOTICES.txt"), `${notice.trimEnd()}\n`);
  const missing = [...javascript.missing, ...rust.missing];
  console.log(
    `Generated third-party notices (${javascript.packages.size} JS and ${rust.packages.size} Rust packages; ${missing.length} without installed license text).`,
  );
  for (const entry of missing) console.warn(`No installed license text: ${entry}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) buildNotices();
