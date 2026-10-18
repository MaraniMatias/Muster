/**
 * The one place a version is written, and the check that every copy of it agrees.
 *
 * The app version lives in three manifests because three toolchains read it: pnpm from
 * `package.json`, Cargo from `src-tauri/Cargo.toml`, and the bundler from `src-tauri/tauri.conf.json`.
 * Cargo also records the crate version in `src-tauri/Cargo.lock`.
 * A release tag that disagrees with any of them publishes a binary whose name and contents claim
 * different versions, so this checks and writes all four version records as one release.
 * Bumping by hand is what put that risk in place; the tag is still a deliberate act, and this does
 * the bookkeeping around it.
 *
 * Usage:
 *   node scripts/release.mjs --check [<tag>]
 *   node scripts/release.mjs --bump <version> [--dry-run] [--write-only]
 *   node scripts/release.mjs --restore <lower-version>
 *
 * `--check` can validate a version supplied by CI or the manual release dispatch; it does not
 * inspect or create Git tags.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** Semver, with the prerelease and build metadata Cargo and the bundler both accept. */
const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

/** The first `version = "..."` under `[package]`, which is the crate's own and not a dependency's. */
const CARGO_VERSION = /^version\s*=\s*"([^"]+)"/m;
const CARGO_PACKAGE = /^\[package\][\s\S]*?^version\s*=\s*"[^"]+"/m;
const CARGO_LOCK_VERSION = /^(\[\[package\]\]\nname = "muster"\nversion = ")([^"]+)(")/m;
const JSON_VERSION = /^(\s*"version":\s*")[^"]+(")/m;

/** Reads and writes one version field, touching nothing else in the file. */
function jsonManifest(path, readVersion) {
  return {
    path,
    read: () => readVersion(readFileSync(path, "utf8")),
    write(version) {
      const content = readFileSync(path, "utf8");
      const matches = content.match(new RegExp(JSON_VERSION.source, "gm"));
      if (matches?.length !== 1) {
        throw new Error(`${path} has ${matches?.length ?? 0} version fields, expected exactly one`);
      }
      writeFileSync(path, content.replace(JSON_VERSION, `$1${version}$2`));
    },
  };
}

function cargoLockManifest(path) {
  function matches(content) {
    return [...content.matchAll(new RegExp(CARGO_LOCK_VERSION.source, "gm"))];
  }
  return {
    path,
    read() {
      const versions = matches(readFileSync(path, "utf8"));
      if (versions.length !== 1) {
        throw new Error(`${path} has ${versions.length} muster package entries, expected exactly one`);
      }
      return versions[0][2];
    },
    write(version) {
      const content = readFileSync(path, "utf8");
      if (matches(content).length !== 1) {
        throw new Error(`${path} does not contain exactly one muster package entry`);
      }
      writeFileSync(path, content.replace(CARGO_LOCK_VERSION, `$1${version}$3`));
    },
  };
}

const MANIFESTS = [
  jsonManifest("package.json", (content) => JSON.parse(content).version),
  jsonManifest("src-tauri/tauri.conf.json", (content) => JSON.parse(content).version),
  {
    path: join("src-tauri", "Cargo.toml"),
    read: () => readFileSync(join("src-tauri", "Cargo.toml"), "utf8").match(CARGO_VERSION)[1],
    write(version) {
      const path = join("src-tauri", "Cargo.toml");
      writeFileSync(
        path,
        readFileSync(path, "utf8").replace(CARGO_PACKAGE, (match) =>
          match.replace(CARGO_VERSION, `version = "${version}"`),
        ),
      );
    },
  },
  cargoLockManifest(join("src-tauri", "Cargo.lock")),
];

function fail(message) {
  console.error(message);
  process.exit(1);
}

function check(tag) {
  const versions = new Map(MANIFESTS.map((manifest) => [manifest.path, manifest.read()]));
  const [path, version] = versions.entries().next().value;
  for (const [otherPath, otherVersion] of versions) {
    if (otherVersion !== version) {
      fail(`${otherPath} is ${otherVersion}, ${path} is ${version}: the version files disagree`);
    }
  }
  if (tag !== undefined) {
    const tagged = tag.replace(/^v/, "");
    if (tagged !== version) {
      fail(`tag ${tag} does not match the ${version} in ${path}`);
    }
  }
  console.log(`every version file is ${version}${tag ? `, matching ${tag}` : ""}`);
}

function bump(version, { dryRun }) {
  const bare = version.replace(/^v/, "");
  if (!SEMVER.test(bare) || !bare.startsWith("0.")) {
    fail("\"" + version + "\" is not a v0 semantic version");
  }
  version = bare;
  const versions = MANIFESTS.map((manifest) => [manifest.path, manifest.read()]);
  const [path, current] = versions[0];
  for (const [otherPath, otherVersion] of versions) {
    if (otherVersion !== current) {
      fail(otherPath + " is " + otherVersion + ", " + path + " is " + current + ": the version files disagree");
    }
  }
  const core = (value) => value.split(/[-+]/)[0].split(".").map(Number);
  const [currentCore, nextCore] = [core(current), core(version)];
  const forward = nextCore[0] > currentCore[0] || (nextCore[0] === currentCore[0] &&
    (nextCore[1] > currentCore[1] || (nextCore[1] === currentCore[1] && nextCore[2] >= currentCore[2])));
  if (!forward) fail(path + " is already " + current + ": " + version + " does not move the version forward");
  if (dryRun) {
    for (const manifest of MANIFESTS) console.log(manifest.path + ": " + manifest.read() + " -> " + version);
    console.log("dry run: would write all four version files; no commit, tag or push");
    return;
  }
  for (const manifest of MANIFESTS) manifest.write(version);
  console.log("wrote version " + version + " to all four version files; no commit, tag or push");
}

function restore(version) {
  const target = version.replace(/^v/, "");
  if (!SEMVER.test(target) || !target.startsWith("0.")) {
    fail("\"" + version + "\" is not a v0 semantic version");
  }
  const versions = MANIFESTS.map((manifest) => [manifest.path, manifest.read()]);
  const [path, current] = versions[0];
  for (const [otherPath, otherVersion] of versions) {
    if (otherVersion !== current) fail(otherPath + " is " + otherVersion + ", " + path + " is " + current + ": the version files disagree");
  }
  const core = (value) => value.split(/[-+]/)[0].split(".").map(Number);
  const [targetCore, currentCore] = [core(target), core(current)];
  const lower = targetCore[0] < currentCore[0] || (targetCore[0] === currentCore[0] &&
    (targetCore[1] < currentCore[1] || (targetCore[1] === currentCore[1] && targetCore[2] < currentCore[2])));
  if (!lower) fail("--restore target " + target + " must be lower than current version " + current);
  for (const manifest of MANIFESTS) manifest.write(target);
  console.log("restored all four version files to " + target + "; no commit, tag or push");
}

const args = process.argv.slice(2);
if (args.includes("--check")) {
  const tag = args.find((argument) => argument.startsWith("v") && SEMVER.test(argument.slice(1)));
  check(tag);
} else if (args[0] === "--restore") {
  if (args.length !== 2 || args[1].startsWith("--")) {
    fail("--restore requires exactly one lower version, for example: node scripts/release.mjs --restore 0.13.2");
  }
  restore(args[1]);
} else if (args.includes("--bump")) {
  const version = args.slice(args.indexOf("--bump") + 1).find((argument) => !argument.startsWith("--"));
  if (!version) fail("--bump needs a version, for example: node scripts/release.mjs --bump 0.21.0");
  if (args.some((argument) => argument.startsWith("--") && !["--bump", "--dry-run", "--write-only"].includes(argument))) {
    fail("--bump accepts only --dry-run and --write-only");
  }
  bump(version, { dryRun: args.includes("--dry-run") });
} else {
  fail(
    "usage: node scripts/release.mjs --check [<tag>]\n" +
      "       node scripts/release.mjs --bump <version> [--dry-run] [--write-only]\n" +
      "       node scripts/release.mjs --restore <lower-version>",
  );
}
