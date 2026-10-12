import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const RELEASE_SCRIPT = fileURLToPath(new URL("./release.mjs", import.meta.url));
const RELEASE_SHELL = fileURLToPath(new URL("./release.sh", import.meta.url));
const WORKFLOW_DIR = fileURLToPath(new URL("../.github/workflows/", import.meta.url));

function fixture(t, lockVersion = "0.9.0") {
  const cwd = mkdtempSync(join(tmpdir(), "muster-release-"));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  mkdirSync(join(cwd, "src-tauri"));
  writeFileSync(join(cwd, "package.json"), '{\n  "version": "0.9.0"\n}\n');
  writeFileSync(join(cwd, "src-tauri", "tauri.conf.json"), '{\n  "version": "0.9.0"\n}\n');
  writeFileSync(join(cwd, "src-tauri", "Cargo.toml"), '[package]\nname = "muster"\nversion = "0.9.0"\n');
  writeFileSync(
    join(cwd, "src-tauri", "Cargo.lock"),
    `version = 4\n\n[[package]]\nname = "muster"\nversion = "${lockVersion}"\n`,
  );
  execFileSync("git", ["init", "-q"], { cwd });
  execFileSync("git", ["config", "user.name", "Release Test"], { cwd });
  execFileSync("git", ["config", "user.email", "release-test@example.com"], { cwd });
  execFileSync("git", ["add", "."], { cwd });
  execFileSync("git", ["commit", "-qm", "fixture"], { cwd });
  return cwd;
}

function run(cwd, ...args) {
  return spawnSync(process.execPath, [RELEASE_SCRIPT, ...args], { cwd, encoding: "utf8" });
}

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8" }).trim();
}

function status(cwd) {
  return execFileSync("git", ["status", "--porcelain"], { cwd, encoding: "utf8" }).trimEnd();
}

test("every workflow literal run block parses as Bash", () => {
  let checked = 0;
  for (const file of readdirSync(WORKFLOW_DIR).filter((name) => /\.ya?ml$/.test(name))) {
    const lines = readFileSync(join(WORKFLOW_DIR, file), "utf8").split(/\r?\n/);
    for (let i = 0; i < lines.length; i++) {
      const match = lines[i].match(/^([ ]*)run:\s*\|\s*$/);
      if (!match) continue;
      const baseIndent = match[1].length;
      const block = [];
      for (let j = i + 1; j < lines.length; j++) {
        const line = lines[j];
        const indent = line.match(/^([ ]*)/)[1].length;
        if (line.trim() && indent <= baseIndent) break;
        block.push(line);
      }
      const contentIndent = Math.min(
        ...block.filter((line) => line.trim()).map((line) => line.match(/^([ ]*)/)[1].length),
      );
      const script = block.map((line) => (line.trim() ? line.slice(contentIndent) : "")).join("\n");
      const result = spawnSync("bash", ["-n"], { input: script, encoding: "utf8" });
      assert.equal(result.status, 0, file + ": run block at line " + (i + 1) + "\n" + result.stderr);
      checked++;
    }
  }
  assert.ok(checked > 0, "expected at least one literal run block");
});

test("tagged recovery skips builds and requires the retained final artifact", () => {
  const workflow = readFileSync(join(WORKFLOW_DIR, "release.yml"), "utf8");
  const dollar = "$";
  assert.ok(workflow.includes("recovery: " + dollar + "{{ steps.validate.outputs.recovery }}"));
  const skip = "if: " + dollar + "{{ needs.validate.outputs.recovery != 'true' }}";
  assert.equal(workflow.split(skip).length - 1, 3);
  assert.ok(
    workflow.includes(
      "if: ${{ always() && inputs.validation_only == false && needs.validate.result == 'success' && (needs.validate.outputs.recovery == 'true' || needs.validate-artifacts.result == 'success') }}",
    ),
  );
  assert.ok(workflow.includes('[[ "$VALIDATION_ONLY" != true ]] ||'));
  const tagIdentity = 'expected_tag_subject="Muster $RELEASE_VERSION at $RELEASE_SHA; workflow run $GITHUB_RUN_ID"';
  assert.equal(workflow.split(tagIdentity).length - 1, 2);
  assert.ok(workflow.includes('notes=(--notes "$curated")'));
  assert.ok(workflow.includes('--generate-notes "${notes[@]}"'));
  const download = workflow.indexOf("name: release-final-assets");
  const tag = workflow.indexOf("name: Verify final payload and create the immutable tag");
  assert.ok(download >= 0 && download < tag, "download the retained artifact before verifying or writing a tag");
});

test("--check rejects a Cargo.lock version mismatch", (t) => {
  const cwd = fixture(t, "0.8.0");
  const result = run(cwd, "--check");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Cargo\.lock is 0\.8\.0, package\.json is 0\.9\.0/);
});

test("--bump rejects a major-version change", (t) => {
  const cwd = fixture(t);
  const head = git(cwd, "rev-parse", "HEAD");
  const result = run(cwd, "--bump", "1.0.0");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /not a v0 semantic version/);
  assert.equal(git(cwd, "rev-parse", "HEAD"), head);
  assert.equal(status(cwd), "");
});

test("--bump writes all four versions without Git or tag side effects", (t) => {
  const cwd = fixture(t);
  const head = git(cwd, "rev-parse", "HEAD");
  const result = run(cwd, "--bump", "0.10.0");
  assert.equal(result.status, 0, result.stderr);
  assert.equal(git(cwd, "rev-parse", "HEAD"), head);
  assert.equal(git(cwd, "tag", "--list"), "");
  assert.deepEqual(status(cwd).split("\n").sort(), [
    " M package.json",
    " M src-tauri/Cargo.lock",
    " M src-tauri/Cargo.toml",
    " M src-tauri/tauri.conf.json",
  ]);
  assert.match(readFileSync(join(cwd, "src-tauri", "Cargo.lock"), "utf8"), /name = "muster"\nversion = "0\.10\.0"/);
  const check = run(cwd, "--check", "v0.10.0");
  assert.equal(check.status, 0);
  assert.match(check.stdout, /every version file is 0\.10\.0, matching v0\.10\.0/);
});

test("--write-only updates versions without Git side effects", (t) => {
  const cwd = fixture(t);
  const head = git(cwd, "rev-parse", "HEAD");
  const tags = git(cwd, "tag", "--list");
  const result = run(cwd, "--bump", "0.10.0", "--write-only");
  assert.equal(result.status, 0, result.stderr);
  assert.equal(git(cwd, "rev-parse", "HEAD"), head);
  assert.equal(git(cwd, "tag", "--list"), tags);
  assert.equal(git(cwd, "diff", "--cached", "--name-only"), "");
  assert.deepEqual(status(cwd).split("\n").sort(), [
    " M package.json",
    " M src-tauri/Cargo.lock",
    " M src-tauri/Cargo.toml",
    " M src-tauri/tauri.conf.json",
  ]);
  const check = run(cwd, "--check");
  assert.equal(check.status, 0);
  assert.match(check.stdout, /every version file is 0\.10\.0/);
});

test("--restore writes a lower version to all manifests without Git side effects", (t) => {
  const cwd = fixture(t);
  const head = git(cwd, "rev-parse", "HEAD");
  const tags = git(cwd, "tag", "--list");
  const result = run(cwd, "--restore", "0.8.0");
  assert.equal(result.status, 0, result.stderr);
  assert.equal(git(cwd, "rev-parse", "HEAD"), head);
  assert.equal(git(cwd, "tag", "--list"), tags);
  assert.equal(git(cwd, "diff", "--cached", "--name-only"), "");
  assert.deepEqual(status(cwd).split("\n").sort(), [
    " M package.json",
    " M src-tauri/Cargo.lock",
    " M src-tauri/Cargo.toml",
    " M src-tauri/tauri.conf.json",
  ]);
  const check = run(cwd, "--check");
  assert.equal(check.status, 0);
  assert.match(check.stdout, /every version file is 0\.8\.0/);
});

test("--restore rejects mismatched or non-lower target versions without writing", (t) => {
  const mismatched = fixture(t, "0.8.0");
  const mismatchHead = git(mismatched, "rev-parse", "HEAD");
  const mismatch = run(mismatched, "--restore", "0.8.0");
  assert.equal(mismatch.status, 1);
  assert.match(mismatch.stderr, /version files disagree/);
  assert.equal(git(mismatched, "rev-parse", "HEAD"), mismatchHead);
  assert.equal(status(mismatched), "");

  const current = fixture(t);
  const currentHead = git(current, "rev-parse", "HEAD");
  const forward = run(current, "--restore", "0.10.0");
  assert.equal(forward.status, 1);
  assert.match(forward.stderr, /must be lower than current version/);
  assert.equal(git(current, "rev-parse", "HEAD"), currentHead);
  assert.equal(status(current), "");
});

test("--write-only --dry-run leaves files and Git untouched", (t) => {
  const cwd = fixture(t);
  const head = git(cwd, "rev-parse", "HEAD");
  const result = run(cwd, "--bump", "0.10.0", "--write-only", "--dry-run");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /dry run: would write all four version files/);
  assert.equal(git(cwd, "rev-parse", "HEAD"), head);
  assert.equal(git(cwd, "tag", "--list"), "");
  assert.equal(status(cwd), "");
});

test("--write-only rejects mismatches with and without --dry-run", (t) => {
  for (const args of [["--write-only"], ["--write-only", "--dry-run"]]) {
    const cwd = fixture(t, "0.8.0");
    const head = git(cwd, "rev-parse", "HEAD");
    const result = run(cwd, "--bump", "0.10.0", ...args);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Cargo\.lock is 0\.8\.0, package\.json is 0\.9\.0/);
    assert.equal(git(cwd, "rev-parse", "HEAD"), head);
    assert.equal(git(cwd, "tag", "--list"), "");
    assert.equal(status(cwd), "");
    assert.match(readFileSync(join(cwd, "src-tauri", "Cargo.lock"), "utf8"), /version = "0\.8\.0"/);
  }
});

function releaseFixture(t) {
  const cwd = fixture(t);
  mkdirSync(join(cwd, "scripts"));
  copyFileSync(RELEASE_SCRIPT, join(cwd, "scripts", "release.mjs"));
  copyFileSync(RELEASE_SHELL, join(cwd, "scripts", "release.sh"));
  chmodSync(join(cwd, "scripts", "release.sh"), 0o755);
  git(cwd, "add", "scripts");
  git(cwd, "commit", "-qm", "fixture release scripts");
  git(cwd, "branch", "-M", "main");
  const remote = mkdtempSync(join(tmpdir(), "muster-release-remote-"));
  t.after(() => rmSync(remote, { recursive: true, force: true }));
  execFileSync("git", ["init", "--bare", "-q", remote]);
  git(cwd, "remote", "add", "origin", remote);
  git(cwd, "push", "-q", "-u", "origin", "main");
  const bin = mkdtempSync(join(tmpdir(), "muster-release-bin-"));
  t.after(() => rmSync(bin, { recursive: true, force: true }));
  const ghLog = join(bin, "gh.log");
  const bunLog = join(bin, "bun.log");
  const bunVersionLog = join(bin, "bun-version.log");
  const dispatchFile = join(bin, "dispatch.txt");
  writeFileSync(ghLog, "");
  writeFileSync(bunLog, "");
  writeFileSync(bunVersionLog, "");
  writeFileSync(
    join(bin, "bun"),
    [
      "#!/bin/sh",
      'printf \'%s\\n\' "$*" >> "$BUN_LOG"',
      'node -p "require(\'./package.json\').version" >> "$BUN_VERSION_LOG"',
      'case "$BUN_MUTATION" in',
      "  tracked) printf '\\n# validation mutation\\n' >> scripts/release.sh ;;",
      "  untracked) printf 'unexpected\\n' > validation-extra.txt ;;",
      "  branch) git checkout -qb validation-race ;;",
      "  head) git commit --allow-empty -qm validation-race ;;",
      "esac",
      'if [ -n "$BUN_FAIL" ]; then echo "$BUN_FAIL" >&2; exit 1; fi',
    ].join("\n") + "\n",
  );
  writeFileSync(
    join(bin, "gh"),
    [
      "#!/bin/sh",
      'printf \'%s\\n\' "$*" >> "$GH_LOG"',
      'if [ "$1" = auth ]; then exit 0; fi',
      'if [ "$1" = repo ]; then echo owner/repo; exit 0; fi',
      'if [ "$1" = workflow ] && [ "$2" = run ]; then',
      "  shift 2; commit= version= validation= request=",
      '  while [ "$#" -gt 0 ]; do',
      '    if [ "$1" = -f ]; then shift; case "$1" in',
      "      commit_sha=*) commit=$(printf '%s' \"$1\" | cut -d= -f2-) ;;",
      "      version=*) version=$(printf '%s' \"$1\" | cut -d= -f2-) ;;",
      "      validation_only=*) validation=$(printf '%s' \"$1\" | cut -d= -f2-) ;;",
      "      request_id=*) request=$(printf '%s' \"$1\" | cut -d= -f2-) ;;",
      "    esac; fi",
      "    shift",
      "  done",
      '  printf \'%s\\n\' "$commit|$version|$validation|$request" > "$GH_DISPATCH"',
      '  if [ "$GH_ADVANCE_MAIN" = 1 ]; then echo later > concurrent.txt; git add concurrent.txt; git commit -qm concurrent; git push -q origin HEAD:main; fi',
      "  exit 0",
      "fi",
      'if [ "$1" = run ] && [ "$2" = list ]; then echo 101; exit 0; fi',
      'if [ "$1" = run ] && [ "$2" = view ]; then',
      '  case "$*" in',
      "    *event,workflowName,displayTitle*) printf 'workflow_dispatch\\tRelease\\t%s\\n' \"$GH_RUN_TITLE\" ;;",
      '    *--json\\ status*) echo "$GH_STATUS" ;;',
      '    *--json\\ conclusion*) echo "$GH_CONCLUSION" ;;',
      "    *--json\\ jobs*) echo '[]' ;;",
      "  esac",
      "  exit 0",
      "fi",
      'if [ "$1" = run ] && [ "$2" = rerun ]; then exit 0; fi',
      'if [ "$1" = release ] && [ "$2" = view ]; then',
      '  case "$*" in',
      "    *--jq\\ .isDraft*) echo false ;;",
      "    *--jq\\ .assets\\ \\|\\ length*) echo 7 ;;",
      "    *--jq\\ .url*) echo https://example.invalid/release ;;",
      "  esac",
      "  exit 0",
      "fi",
      "exit 2",
    ].join("\n") + "\n",
  );
  chmodSync(join(bin, "bun"), 0o755);
  chmodSync(join(bin, "gh"), 0o755);
  return {
    cwd,
    remote,
    ghLog,
    bunLog,
    bunVersionLog,
    dispatchFile,
    env: {
      ...process.env,
      PATH: bin + ":" + process.env.PATH,
      GH_LOG: ghLog,
      BUN_LOG: bunLog,
      BUN_VERSION_LOG: bunVersionLog,
      BUN_MUTATION: "",
      GH_DISPATCH: dispatchFile,
      GH_STATUS: "completed",
      GH_CONCLUSION: "success",
      GH_RUN_TITLE: "",
      GH_ADVANCE_MAIN: "0",
      BUN_FAIL: "",
    },
  };
}

function runRelease(f, version, ...args) {
  return spawnSync("bash", [join(f.cwd, "scripts", "release.sh"), version, ...args], {
    cwd: f.cwd,
    env: f.env,
    encoding: "utf8",
  });
}

test("validation sees the prepared version and failure leaves it uncommitted", (t) => {
  const f = releaseFixture(t);
  const head = git(f.cwd, "rev-parse", "HEAD");
  const remoteHead = git(f.cwd, "ls-remote", "origin", "refs/heads/main").split(" ")[0];
  f.env.BUN_FAIL = "simulated Docker/Linux validation failure";
  const result = runRelease(f, "0.10.0");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /prepared version edits remain in the working tree; no commit, push, or tag was made/);
  assert.equal(readFileSync(f.bunVersionLog, "utf8").trim(), "0.10.0");
  assert.equal(git(f.cwd, "rev-parse", "HEAD"), head);
  assert.equal(git(f.cwd, "ls-remote", "origin", "refs/heads/main").split(" ")[0], remoteHead);
  assert.equal(git(f.cwd, "tag", "--list"), "");
  assert.equal(JSON.parse(readFileSync(join(f.cwd, "package.json"), "utf8")).version, "0.10.0");
  assert.deepEqual(status(f.cwd).split("\n").sort(), [
    " M package.json",
    " M src-tauri/Cargo.lock",
    " M src-tauri/Cargo.toml",
    " M src-tauri/tauri.conf.json",
  ]);
});

test("validation rejects extra files and HEAD/branch changes without pushing", (t) => {
  for (const mutation of ["tracked", "untracked", "branch", "head"]) {
    const f = releaseFixture(t);
    const head = git(f.cwd, "rev-parse", "HEAD");
    const remoteHead = git(f.cwd, "ls-remote", "origin", "refs/heads/main").split(" ")[0];
    f.env.BUN_MUTATION = mutation;
    const result = runRelease(f, "0.10.0");
    assert.equal(result.status, 1, mutation + ": " + result.stderr);
    if (mutation === "branch") assert.match(result.stderr, /changed branches/);
    else if (mutation === "head") assert.match(result.stderr, /changed HEAD/);
    else assert.match(result.stderr, /changed files outside the prepared version manifests/);
    assert.equal(git(f.cwd, "ls-remote", "origin", "refs/heads/main").split(" ")[0], remoteHead);
    assert.equal(git(f.cwd, "tag", "--list"), "");
    assert.equal(git(f.cwd, "ls-remote", "--tags", "origin").trim(), "");
    assert.doesNotMatch(readFileSync(f.ghLog, "utf8"), /workflow run/);
    if (mutation === "head") {
      assert.notEqual(git(f.cwd, "rev-parse", "HEAD"), head);
      assert.equal(git(f.cwd, "show", "-s", "--format=%s"), "validation-race");
    } else {
      assert.equal(git(f.cwd, "rev-parse", "HEAD"), head);
    }
  }
});

test("dry-run dispatches the exact prepared SHA and never tags, even if main advances", (t) => {
  const f = releaseFixture(t);
  f.env.GH_ADVANCE_MAIN = "1";
  const result = runRelease(f, "0.10.0", "--dry-run");
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(f.bunVersionLog, "utf8").trim(), "0.10.0");
  const [sha, version, validationOnly, requestId] = readFileSync(f.dispatchFile, "utf8").trim().split("|");
  assert.match(sha, /^[0-9a-f]{40}$/);
  assert.equal(version, "v0.10.0");
  assert.equal(validationOnly, "true");
  assert.match(requestId, /^rel_[0-9a-f]{12}_/);
  assert.match(git(f.cwd, "show", "-s", "--format=%B", sha), /chore: release v0.10.0/);
  assert.notEqual(git(f.cwd, "rev-parse", "HEAD"), sha);
  assert.equal(git(f.cwd, "merge-base", "--is-ancestor", sha, "main"), "");
  assert.equal(git(f.cwd, "tag", "--list"), "");
  assert.equal(git(f.cwd, "ls-remote", "--tags", "origin").trim(), "");
  const log = readFileSync(f.ghLog, "utf8");
  assert.match(log, /workflow_dispatch/);
  assert.match(log, new RegExp(requestId));
  assert.ok(log.includes(`contains("${requestId}")`), "correlate the run by its safe unique request id");
  assert.doesNotMatch(log, /release create|release upload/);
});

test("failed final GitHub validation leaves the prepared commit untagged", (t) => {
  const f = releaseFixture(t);
  f.env.GH_CONCLUSION = "failure";
  const result = runRelease(f, "0.10.0");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /failed before the tag/);
  assert.equal(git(f.cwd, "tag", "--list"), "");
  assert.equal(git(f.cwd, "ls-remote", "--tags", "origin").trim(), "");
  assert.equal(JSON.parse(readFileSync(join(f.cwd, "package.json"), "utf8")).version, "0.10.0");
  assert.doesNotMatch(readFileSync(f.ghLog, "utf8"), /release create|release upload/);
});

test("a foreign existing tag is refused without moving or pushing it", (t) => {
  const f = releaseFixture(t);
  const original = git(f.cwd, "rev-parse", "HEAD");
  git(f.cwd, "tag", "-a", "v0.10.0", "-m", "foreign tag");
  git(f.cwd, "push", "-q", "origin", "refs/tags/v0.10.0");
  const tagObject = git(f.cwd, "rev-parse", "refs/tags/v0.10.0");
  const result = runRelease(f, "0.10.0");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /already exists|not on origin/);
  assert.equal(git(f.cwd, "rev-parse", "HEAD"), original);
  assert.equal(git(f.cwd, "rev-parse", "refs/tags/v0.10.0"), tagObject);
  assert.equal(readFileSync(f.bunLog, "utf8").trim(), "");
});

test("retry of an already tagged matching release reuses its original workflow", (t) => {
  const f = releaseFixture(t);
  const bump = run(f.cwd, "--bump", "0.10.0");
  assert.equal(bump.status, 0, bump.stderr);
  git(f.cwd, "add", "package.json", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "src-tauri/tauri.conf.json");
  git(f.cwd, "commit", "-qm", "chore: release v0.10.0");
  const sha = git(f.cwd, "rev-parse", "HEAD");
  git(f.cwd, "tag", "-a", "v0.10.0", "-m", "Muster v0.10.0 at " + sha + "; workflow run 101");
  git(f.cwd, "push", "-q", "origin", "main");
  git(f.cwd, "push", "-q", "origin", "refs/tags/v0.10.0");
  const tagObject = git(f.cwd, "rev-parse", "refs/tags/v0.10.0");
  f.env.GH_RUN_TITLE = "Release v0.10.0 at " + sha + " publish [request-1]";
  const result = runRelease(f, "0.10.0");
  assert.equal(result.status, 0, result.stderr);
  assert.equal(git(f.cwd, "rev-parse", "HEAD"), sha);
  assert.equal(git(f.cwd, "rev-parse", "refs/tags/v0.10.0"), tagObject);
  assert.doesNotMatch(readFileSync(f.ghLog, "utf8"), /workflow run release|workflow run --ref/);
  assert.equal(readFileSync(f.bunLog, "utf8").trim(), "");
});
