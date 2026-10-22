import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  auditPackages,
  AUDITED_MANIFESTS,
  cargoMetadataArgs,
  isBlocking,
  loadLockedPackages,
  parseCargoMetadata,
} from "./audit-rust.mjs";
import {
  auditedPatchedAlacritty,
  isAuditedAlacritty,
  PATCHED_ALACRITTY_ARCHIVE_SHA256,
  PATCHED_ALACRITTY_DIR,
  PATCHED_ALACRITTY_TREE_SHA256,
  patchedAlacrittyTreeSha256,
} from "./patched-alacritty.mjs";
import {
  auditedPatchedGherkin,
  PATCHED_GHERKIN_ARCHIVE_SHA256,
  PATCHED_GHERKIN_DIR,
  PATCHED_GHERKIN_TREE_SHA256,
  patchedGherkinTreeSha256,
} from "./patched-gherkin.mjs";
import {
  auditedPatchedGitignore,
  PATCHED_GITIGNORE_ARCHIVE_SHA256,
  PATCHED_GITIGNORE_DIR,
  PATCHED_GITIGNORE_TREE_SHA256,
  patchedGitignoreTreeSha256,
} from "./patched-gitignore.mjs";

function response(results, ok = true, status = 200) {
  return { ok, status, json: async () => ({ results }) };
}

function jsonResponse(value, ok = true, status = 200) {
  return { ok, status, json: async () => value };
}

function vuln(id, aliases, severity = "MODERATE") {
  return { id, aliases, database_specific: { severity } };
}

function queryResult(ids) {
  return response(ids.map((id) => ({ vulns: id ? [{ id }] : [] })));
}

const APP_ID = "path+file:///fixture#app@0.1.0";
const CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index";

function patchedAlacrittyPackage() {
  const manifestPath = join(PATCHED_ALACRITTY_DIR, "Cargo.toml");
  return parseCargoMetadata({
    workspace_members: [APP_ID],
    packages: [
      {
        id: `path+file://${manifestPath}#alacritty_terminal@0.26.0`,
        name: "alacritty_terminal",
        version: "0.26.0",
        source: null,
        manifest_path: manifestPath,
      },
    ],
  })[0];
}

function patchedGitignorePackage() {
  const manifestPath = join(PATCHED_GITIGNORE_DIR, "Cargo.toml");
  return parseCargoMetadata({
    workspace_members: [APP_ID],
    packages: [
      {
        id: `path+file://${manifestPath}#konomanoasa-tree-sitter-gitignore@0.3.1`,
        name: "konomanoasa-tree-sitter-gitignore",
        version: "0.3.1",
        source: null,
        manifest_path: manifestPath,
      },
    ],
  })[0];
}

function patchedGherkinPackage() {
  const manifestPath = join(PATCHED_GHERKIN_DIR, "Cargo.toml");
  return parseCargoMetadata({
    workspace_members: [APP_ID],
    packages: [
      {
        id: `path+file://${manifestPath}#tree-sitter-gherkin@0.0.1`,
        name: "tree-sitter-gherkin",
        version: "0.0.1",
        source: null,
        manifest_path: manifestPath,
      },
    ],
  })[0];
}

test("malformed Cargo.lock fails before any advisory lookup", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "muster-audit-lock-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const manifestPath = join(directory, "Cargo.toml");
  writeFileSync(manifestPath, '[package]\nname = "audit-fixture"\nversion = "0.1.0"\nedition = "2021"\n');
  writeFileSync(join(directory, "Cargo.lock"), "this is not valid TOML = [\n");
  assert.throws(() => loadLockedPackages({ manifestPaths: [manifestPath] }), /could not validate Cargo\.lock/);
});

test("metadata selects locked all-feature dependencies without host-platform filtering", () => {
  const args = cargoMetadataArgs("fixture/Cargo.toml");
  assert.ok(args.includes("--locked"));
  assert.ok(args.includes("--all-features"));
  assert.ok(!args.includes("--filter-platform"));

  const packages = parseCargoMetadata({
    workspace_members: [APP_ID],
    packages: [
      { id: "registry+crates.io#windows-only@1.0.0", name: "windows-only", version: "1.0.0", source: CRATES_IO_SOURCE },
      {
        id: "registry+crates.io#optional-feature@2.0.0",
        name: "optional-feature",
        version: "2.0.0",
        source: CRATES_IO_SOURCE,
      },
      { id: APP_ID, name: "app", version: "0.1.0", source: null },
    ],
  });
  assert.deepEqual(
    packages.map(({ name }) => name),
    ["optional-feature", "windows-only"],
  );
  assert.ok(packages.every(({ source }) => source === CRATES_IO_SOURCE));
});

test("foreign registry alone fails closed instead of returning an empty audit set", () => {
  assert.throws(
    () =>
      parseCargoMetadata({
        workspace_members: [APP_ID],
        packages: [
          { id: "registry+private#dep@1.0.0", name: "dep", version: "1.0.0", source: "registry+https://private" },
        ],
      }),
    /unsupported registry source for dep@1\.0\.0/,
  );
});

test("same name and version from crates.io and a foreign registry fail before deduplication", () => {
  assert.throws(
    () =>
      parseCargoMetadata({
        workspace_members: [APP_ID],
        packages: [
          { id: "registry+crates.io#dep@1.0.0", name: "dep", version: "1.0.0", source: CRATES_IO_SOURCE },
          { id: "registry+private#dep@1.0.0", name: "dep", version: "1.0.0", source: "registry+https://private" },
        ],
      }),
    /unsupported registry source for dep@1\.0\.0/,
  );
});

test("external git and path packages fail closed", () => {
  const packages = [
    { id: "git+https://example/repo#dep@1.0.0", name: "dep", version: "1.0.0", source: "git+https://example/repo" },
    { id: "path+file:///external#dep@1.0.0", name: "dep", version: "1.0.0", source: null },
  ];
  assert.throws(
    () => parseCargoMetadata({ workspace_members: [APP_ID], packages: [packages[0]] }),
    /unsupported external dependency source for dep@1\.0\.0/,
  );
  assert.throws(
    () => parseCargoMetadata({ workspace_members: [APP_ID], packages: [packages[1]] }),
    /unsupported external path dependency for dep@1\.0\.0/,
  );
});

test("the pinned alacritty patch is accepted and its pinned digest describes the vendored tree", () => {
  assert.equal(patchedAlacrittyTreeSha256(), PATCHED_ALACRITTY_TREE_SHA256);
  assert.match(PATCHED_ALACRITTY_ARCHIVE_SHA256, /^[0-9a-f]{64}$/);

  const alacritty = patchedAlacrittyPackage();
  assert.equal(alacritty.name, "alacritty_terminal");
  assert.equal(alacritty.version, "0.26.0");
  assert.equal(alacritty.source, null);
  assert.ok(isAuditedAlacritty(alacritty));
});

test("the Gitignore grammar is accepted only with its pinned source tree", () => {
  assert.equal(patchedGitignoreTreeSha256(), PATCHED_GITIGNORE_TREE_SHA256);
  assert.match(PATCHED_GITIGNORE_ARCHIVE_SHA256, /^[0-9a-f]{64}$/);

  const grammar = patchedGitignorePackage();
  assert.equal(grammar.name, "konomanoasa-tree-sitter-gitignore");
  assert.equal(grammar.version, "0.3.1");
  assert.equal(grammar.source, null);
});

test("editing the vendored Gitignore grammar breaks its pinned hash", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "muster-vendored-gitignore-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const copy = join(directory, "gitignore");
  cpSync(PATCHED_GITIGNORE_DIR, copy, { recursive: true });
  assert.equal(patchedGitignoreTreeSha256(copy), PATCHED_GITIGNORE_TREE_SHA256);

  const patched = join(copy, "src/scanner.c");
  writeFileSync(patched, `${readFileSync(patched, "utf8")}\n/* quiet edit */\n`);
  assert.notEqual(patchedGitignoreTreeSha256(copy), PATCHED_GITIGNORE_TREE_SHA256);
});

test("the Gherkin grammar is accepted only with its pinned source tree", () => {
  assert.equal(patchedGherkinTreeSha256(), PATCHED_GHERKIN_TREE_SHA256);
  assert.match(PATCHED_GHERKIN_ARCHIVE_SHA256, /^[0-9a-f]{64}$/);

  const grammar = patchedGherkinPackage();
  assert.equal(grammar.name, "tree-sitter-gherkin");
  assert.equal(grammar.version, "0.0.1");
  assert.equal(grammar.source, null);
});

test("editing the vendored Gherkin grammar breaks its pinned hash", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "muster-vendored-gherkin-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const copy = join(directory, "gherkin");
  cpSync(PATCHED_GHERKIN_DIR, copy, { recursive: true });
  assert.equal(patchedGherkinTreeSha256(copy), PATCHED_GHERKIN_TREE_SHA256);

  const patched = join(copy, "src/parser.c");
  writeFileSync(patched, `${readFileSync(patched, "utf8")}\n/* quiet edit */\n`);
  assert.notEqual(patchedGherkinTreeSha256(copy), PATCHED_GHERKIN_TREE_SHA256);
});

test("a registry-sourced alacritty_terminal is not the patched package", () => {
  assert.equal(
    auditedPatchedAlacritty({
      name: "alacritty_terminal",
      version: "0.26.0",
      source: CRATES_IO_SOURCE,
      manifest_path: join(PATCHED_ALACRITTY_DIR, "Cargo.toml"),
    }),
    null,
  );

  const [pkg] = parseCargoMetadata({
    workspace_members: [APP_ID],
    packages: [
      {
        id: "registry+crates.io#alacritty_terminal@0.26.0",
        name: "alacritty_terminal",
        version: "0.26.0",
        source: CRATES_IO_SOURCE,
      },
    ],
  });
  assert.equal(pkg.source, CRATES_IO_SOURCE);
  assert.equal(isAuditedAlacritty(pkg), false);
});

test("an alacritty_terminal outside the pinned identity is refused", () => {
  const pinnedManifest = join(PATCHED_ALACRITTY_DIR, "Cargo.toml");
  for (const pkg of [
    { name: "alacritty_terminal", version: "0.25.0", manifest_path: pinnedManifest },
    { name: "alacritty-terminal", version: "0.26.0", manifest_path: pinnedManifest },
    { name: "alacritty_terminal", version: "0.26.0", manifest_path: "/external/alacritty/Cargo.toml" },
  ]) {
    assert.equal(auditedPatchedAlacritty({ source: null, ...pkg }), null);
    assert.throws(
      () =>
        parseCargoMetadata({
          workspace_members: [APP_ID],
          packages: [{ id: `path+file:///external#${pkg.name}@0.0.0`, source: null, ...pkg }],
        }),
      (error) => error.message === `unsupported external path dependency for ${pkg.name}@${pkg.version}`,
    );
  }
});

test("editing the vendored alacritty tree breaks the pinned hash", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "muster-vendored-alacritty-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const copy = join(directory, "alacritty_terminal-0.26.0");
  cpSync(PATCHED_ALACRITTY_DIR, copy, { recursive: true });

  // The digest covers names and contents, not the directory the tree happens to live in.
  assert.equal(patchedAlacrittyTreeSha256(copy), PATCHED_ALACRITTY_TREE_SHA256);

  const patched = join(copy, "src/term/mod.rs");
  writeFileSync(patched, `${readFileSync(patched, "utf8")}\n// quiet edit\n`);
  assert.notEqual(patchedAlacrittyTreeSha256(copy), PATCHED_ALACRITTY_TREE_SHA256);
});

test("a path dependency sitting beside the backports is still refused", () => {
  assert.throws(
    () =>
      parseCargoMetadata({
        workspace_members: [APP_ID],
        packages: [
          {
            id: "path+file:///vendor/rogue@1.0.0",
            name: "rogue",
            version: "1.0.0",
            source: null,
            manifest_path: join(PATCHED_ALACRITTY_DIR, "../rogue/Cargo.toml"),
          },
        ],
      }),
    /unsupported external path dependency for rogue@1\.0\.0/,
  );
});

test("loadLockedPackages audits both manifests and deduplicates shared packages", () => {
  const alacrittyManifest = join(PATCHED_ALACRITTY_DIR, "Cargo.toml");
  const shared = { id: "registry+crates.io#shared@1.0.0", name: "shared", version: "1.0.0", source: CRATES_IO_SOURCE };
  const outputs = new Map([
    [
      AUDITED_MANIFESTS[0],
      JSON.stringify({
        workspace_members: [APP_ID],
        packages: [{ id: APP_ID, name: "app", version: "0.1.0", source: null }, shared],
      }),
    ],
    [
      AUDITED_MANIFESTS[1],
      JSON.stringify({
        workspace_members: ["path+file:///native#muster@0.0.0"],
        packages: [
          { id: "path+file:///native#muster@0.0.0", name: "muster", version: "0.0.0", source: null },
          shared,
          {
            id: `path+file://${alacrittyManifest}#alacritty_terminal@0.26.0`,
            name: "alacritty_terminal",
            version: "0.26.0",
            source: null,
            manifest_path: alacrittyManifest,
          },
        ],
      }),
    ],
  ]);

  const requested = [];
  const packages = loadLockedPackages({
    run: (_command, args) => {
      requested.push(args.at(-1));
      return { status: 0, stdout: outputs.get(args.at(-1)) };
    },
  });

  assert.deepEqual(requested, [...AUDITED_MANIFESTS]);
  assert.deepEqual(
    packages.map(({ name, version }) => `${name}@${version}`),
    ["alacritty_terminal@0.26.0", "shared@1.0.0"],
  );
});

test("network timeout fails closed", async () => {
  const fetchImpl = (_url, { signal }) =>
    new Promise((_resolve, reject) => {
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
    });
  await assert.rejects(
    auditPackages([{ name: "fixture", version: "1.0.0" }], { fetchImpl, timeoutMs: 5 }),
    /timed out/,
  );
});

test("network errors fail closed without echoing underlying error details", async () => {
  await assert.rejects(
    auditPackages([{ name: "fixture", version: "1.0.0" }], {
      fetchImpl: async () => {
        throw new Error("secret transport detail");
      },
    }),
    (error) => error.message === "OSV request failed (network or TLS error)" && !error.message.includes("secret"),
  );
});

test("HTTP failure fails closed without exposing response contents", async () => {
  await assert.rejects(
    auditPackages([{ name: "fixture", version: "1.0.0" }], {
      fetchImpl: async () => response([], false, 503),
    }),
    /HTTP 503/,
  );
});

test("informational unmaintained advisories warn without becoming a false clean result", async () => {
  const advisories = await auditPackages([{ name: "fixture", version: "1.0.0" }], {
    fetchImpl: async (url, { method }) =>
      method === "POST"
        ? queryResult(["RUSTSEC-2024-0001"])
        : jsonResponse({ id: "RUSTSEC-2024-0001", summary: "fixture is unmaintained" }),
  });
  assert.equal(advisories.length, 1);
  assert.equal(advisories[0].severity, "INFO");
  assert.equal(isBlocking(advisories[0]), false);
});

test("the RustSec 'no longer maintained' phrasing is also informational", async () => {
  const advisories = await auditPackages([{ name: "fixture", version: "1.0.0" }], {
    fetchImpl: async (url, { method }) =>
      method === "POST"
        ? queryResult(["RUSTSEC-2024-0436"])
        : jsonResponse({ id: "RUSTSEC-2024-0436", summary: "paste - no longer maintained" }),
  });
  assert.equal(advisories.length, 1);
  assert.equal(advisories[0].severity, "INFO");
  assert.equal(isBlocking(advisories[0]), false);
});

test("moderate advisories block the audit", async () => {
  const advisories = await auditPackages([{ name: "glib", version: "0.18.5" }], {
    fetchImpl: async (url, { method }) =>
      method === "POST"
        ? queryResult(["RUSTSEC-2024-0429"])
        : jsonResponse(vuln("RUSTSEC-2024-0429", ["GHSA-aaaa-bbbb-cccc"])),
  });
  assert.equal(advisories[0].severity, "MODERATE");
  assert.equal(isBlocking(advisories[0]), true);
});

test("OSV identifiers and aliases deduplicate across affected packages", async () => {
  const packages = [
    { name: "crate-a", version: "1.0.0" },
    { name: "crate-b", version: "2.0.0" },
  ];
  const advisories = await auditPackages(packages, {
    fetchImpl: async (url, { method }) => {
      if (method === "POST") return queryResult(["RUSTSEC-2024-0429", "GHSA-aaaa-bbbb-cccc"]);
      const id = decodeURIComponent(new URL(url).pathname.split("/").at(-1));
      return jsonResponse(
        id === "RUSTSEC-2024-0429" ? vuln(id, ["GHSA-aaaa-bbbb-cccc"]) : { id, aliases: ["RUSTSEC-2024-0429"] },
      );
    },
  });
  assert.equal(advisories.length, 1);
  assert.equal(advisories[0].severity, "MODERATE");
  assert.deepEqual(advisories[0].packages, ["crate-a@1.0.0", "crate-b@2.0.0"]);
});
