#!/usr/bin/env bash
# Validate locally, commit/push only the version change, then let GitHub validate and publish.
# --dry-run may push the prepared commit and build all bundles, but never creates a tag/release.
set -euo pipefail

cd "$(dirname "$0")/.."
step() { printf '\n\033[1m==> %s\033[0m\n' "$1"; }
die() { printf '%s\n' "$1" >&2; exit 1; }
usage() {
  cat <<'USAGE'
usage: scripts/release.sh <v0.minor.patch> [--dry-run]
       bun run release:preview -- v0.minor.patch  # no mutation
       bun run release:validate                  # local checks and tests
       bun run release -- v0.minor.patch --dry-run # prepare + GitHub builds; no tag/publish
USAGE
}

dry_run=false
version=""
for argument in "$@"; do
  case "$argument" in
    --dry-run) dry_run=true ;;
    -h|--help) usage; exit 0 ;;
    --) ;;
    -*) die "unknown option: $argument" ;;
    *) [[ -z "$version" ]] || die "provide only one version"; version="$argument" ;;
  esac
done
[[ -n "$version" ]] || { usage >&2; exit 1; }
version="${version#v}"
[[ "$version" =~ ^0\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] ||
  die "\"$version\" is not a v0 version like 0.21.0"
tag="v$version"

for tool in git gh node; do command -v "$tool" >/dev/null || die "$tool is not installed"; done
gh auth status >/dev/null 2>&1 || die "gh is not authenticated; run: gh auth login"
repository="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
[[ "$(git branch --show-current)" == main ]] || die "releases must be cut from main"
[[ -z "$(git status --porcelain)" ]] || die "the working tree has uncommitted changes; commit or stash them first"
wait_for_run() {
  local run_id="$1" tag_name="$2" validation_only="${3:-false}" resume="${4:-false}"
  local status conclusion jobs reported="" deadline rerun_attempted=false
  echo "run $run_id: https://github.com/$repository/actions/runs/$run_id"
  deadline=$((SECONDS + 3600))
  while (( SECONDS < deadline )); do
    status=""
    if ! status="$(gh run view "$run_id" --json status --jq .status 2>/dev/null)"; then
      echo "  GitHub status request failed; retrying without treating it as a failed run" >&2
      sleep 20
      continue
    fi
    if [[ "$status" == completed ]]; then
      conclusion="$(gh run view "$run_id" --json conclusion --jq .conclusion 2>/dev/null)" || { sleep 20; continue; }
      if [[ "$conclusion" == success ]]; then
        if [[ "$validation_only" == true ]]; then
          echo "All validation builds succeeded; no tag or release was requested."
          return 0
        fi
        if ! gh release view "$tag_name" --repo "$repository" --json assets,isDraft >/dev/null 2>&1; then
          echo "  Release metadata is not available yet; retrying" >&2
          sleep 20
          continue
        fi
        local is_draft asset_count
        is_draft="$(gh release view "$tag_name" --repo "$repository" --json isDraft --jq .isDraft)" || { sleep 20; continue; }
        asset_count="$(gh release view "$tag_name" --repo "$repository" --json assets --jq '.assets | length')" || { sleep 20; continue; }
        [[ "$is_draft" == false && "$asset_count" == 7 ]] || die "$tag_name release is incomplete; inspect https://github.com/$repository/actions/runs/$run_id"
        printf 'Published %s with %s assets.\n' "$tag_name" "$asset_count"
        gh release view "$tag_name" --repo "$repository" --json url --jq .url
        return 0
      fi
      if [[ "$conclusion" == failure ]]; then
        if [[ "$resume" == true && "$rerun_attempted" == false ]] && git ls-remote --exit-code --tags origin "refs/tags/$tag_name" >/dev/null 2>&1; then
          echo "  Retrying only failed jobs for the tagged workflow; successful build artifacts are reused."
          gh run rerun "$run_id" --failed || die "could not retry tagged workflow $run_id; no tag was changed"
          rerun_attempted=true
          sleep 5
          continue
        fi
        if git ls-remote --exit-code --tags origin "refs/tags/$tag_name" >/dev/null 2>&1; then
          die "release run $run_id failed after tagging; retry with: gh run rerun $run_id --failed"
        fi
        die "release run $run_id failed before the tag; no release tag was created"
      fi
      die "release run $run_id concluded $conclusion; inspect https://github.com/$repository/actions/runs/$run_id"
    fi
    jobs="$(gh run view "$run_id" --json jobs --jq '[.jobs[] | [.name,.status] | join(\": \")] | join(\"  \")' 2>/dev/null)" || jobs=""
    if [[ -n "$jobs" && "$jobs" != "$reported" ]]; then printf '  %s\n' "$jobs"; reported="$jobs"; fi
    sleep 20
  done
  die "timed out waiting for run $run_id; it may still be active: https://github.com/$repository/actions/runs/$run_id"
}

remote_tag_commit() {
  local refs peeled direct
  refs="$(git ls-remote --tags origin "refs/tags/$tag" "refs/tags/$tag^{}")" || return 2
  [[ -n "$refs" ]] || return 1
  peeled="$(awk -v ref="refs/tags/$tag" '$2 == ref "^{}" { print $1 }' <<< "$refs")"
  direct="$(awk -v ref="refs/tags/$tag" '$2 == ref { print $1 }' <<< "$refs")"
  printf '%s\n' "${peeled:-$direct}"
}

local_tag_commit=""
if git show-ref --verify --quiet "refs/tags/$tag"; then
  local_tag_commit="$(git rev-parse "$tag^{commit}")"
fi
remote_sha=""
if remote_sha="$(remote_tag_commit)"; then :; else
  result=$?
  [[ "$result" == 1 ]] || die "could not query origin for tag $tag"
fi
if [[ -n "$local_tag_commit" && -z "$remote_sha" ]]; then
  die "local tag $tag is not on origin; refusing to push or replace it"
fi
if [[ -n "$remote_sha" && -n "$local_tag_commit" && "$remote_sha" != "$local_tag_commit" ]]; then
  die "$tag differs locally ($local_tag_commit) and remotely ($remote_sha); refusing to move it"
fi

current="$(node -p "require('./package.json').version")"
node scripts/release.mjs --check >/dev/null || die "version files disagree; fix them before releasing"
if [[ "$current" == "$version" ]]; then
  node scripts/release.mjs --check "$tag" >/dev/null || die "prepared version does not match $tag"
fi

if [[ -n "$remote_sha" && "$current" == "$version" && "$dry_run" == false ]]; then
  step "Resuming the already-tagged $tag without rebuilding it"
  if [[ -z "$local_tag_commit" ]]; then git fetch --no-tags origin "refs/tags/$tag:refs/tags/$tag"; fi
  tag_subject="$(git for-each-ref --format='%(contents:subject)' "refs/tags/$tag")"
  [[ "$tag_subject" =~ workflow\ run\ ([0-9]+)$ ]] || die "$tag exists but is not an annotated tag from the release workflow"
  run="${BASH_REMATCH[1]}"
  details="$(gh run view "$run" --json event,workflowName,displayTitle --jq '[.event,.workflowName,.displayTitle] | @tsv')" || die "cannot inspect tagged workflow run $run"
  expected_prefix="Release $tag at $remote_sha publish ["
  [[ "$details" == *$'workflow_dispatch\tRelease\t'*"$expected_prefix"* ]] || die "tag $tag belongs to a different workflow or source commit"
  wait_for_run "$run" "$tag" false true
  exit 0
fi

if [[ -n "$remote_sha" && "$current" != "$version" ]]; then
  die "$tag already exists, but the checked-out manifests are $current; refusing to resume a different version"
fi

if [[ -z "$remote_sha" ]]; then
  command -v bun >/dev/null || die "Bun is required to run the release validation gate"
  original_head="$(git rev-parse HEAD)"
  original_branch="$(git branch --show-current)"
  [[ "$original_branch" == main ]] || die "releases must be cut from main"
  step "Preparing version $tag before validation"
  node scripts/release.mjs --bump "$version" || die "could not prepare version $tag"
  node scripts/release.mjs --check "$tag" || die "prepared version does not match $tag"
  expected_status="$(git status --porcelain --untracked-files=all)"
  allowed_version_status=$' M package.json\n M src-tauri/Cargo.lock\n M src-tauri/Cargo.toml\n M src-tauri/tauri.conf.json'
  case "$expected_status" in
    ""|"$allowed_version_status") ;;
    *) die "version preparation changed files outside the four version manifests; no commit, push, or tag was made" ;;
  esac
  expected_diff="$(git diff --binary)"
  git diff --cached --quiet || die "version preparation unexpectedly staged changes"
  step "Running local release checks for $tag"
  validation_status=0
  bun run release:validate || validation_status=$?
  [[ "$(git rev-parse HEAD)" == "$original_head" ]] || die "local validation changed HEAD; prepared version edits remain and no release commit, push, or tag was made"
  [[ "$(git branch --show-current)" == "$original_branch" ]] || die "local validation changed branches; prepared version edits remain and no release commit, push, or tag was made"
  [[ "$(git status --porcelain --untracked-files=all)" == "$expected_status" ]] || die "local validation changed files outside the prepared version manifests; no commit, push, or tag was made"
  [[ "$(git diff --binary)" == "$expected_diff" ]] || die "local validation changed the prepared version diff; no commit, push, or tag was made"
  git diff --cached --quiet || die "local validation staged changes; no commit, push, or tag was made"
  if [[ "$validation_status" != 0 ]]; then
    if [[ -n "$expected_status" ]]; then
      die "local validation failed; prepared version edits remain in the working tree; no commit, push, or tag was made"
    else
      die "local validation failed; the already-prepared version remains unchanged; no commit, push, or tag was made"
    fi
  fi
  if [[ -n "$expected_status" ]]; then
    step "Committing validated version $tag"
    git add package.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json
    git commit -m "chore: release $tag"
  else
    step "Resuming the already-prepared untagged version $tag"
  fi
  [[ -z "$(git status --porcelain --untracked-files=all)" ]] || die "unexpected changes remain after preparing $tag"
  release_sha="$(git rev-parse HEAD)"
  if remote_sha="$(remote_tag_commit)"; then die "$tag appeared on origin during validation; inspect it before retrying"; else
    result=$?
    [[ "$result" == 1 ]] || die "could not recheck origin for tag $tag"
  fi
  step "Pushing only commit $release_sha to main"
  git push origin "$release_sha:refs/heads/main" || die "main advanced or push failed; no tag was created"
else
  release_sha="$remote_sha"
fi

request_id="rel_${release_sha:0:12}_$(date +%s)_$$_$RANDOM"
mode=publish
$dry_run && mode=validation-only
run_name="Release $tag at $release_sha $mode [$request_id]"
step "Dispatching $mode workflow for exact SHA $release_sha"
gh workflow run release.yml --ref main \
  -f commit_sha="$release_sha" -f version="$tag" -f validation_only="$dry_run" -f request_id="$request_id"

find_dispatched_run() {
  gh run list --workflow release.yml --event workflow_dispatch --limit 50 \
    --json databaseId,displayTitle --jq ".[] | select(.displayTitle | contains(\"$request_id\")) | .databaseId | tostring"
}
run=""
for _ in $(seq 1 36); do
  candidate=""
  if candidate="$(find_dispatched_run 2>/dev/null)"; then run="$candidate"; fi
  [[ -n "$run" ]] && break
  sleep 5
done
[[ -n "$run" ]] || die "no correlated Release run appeared; inspect https://github.com/$repository/actions"
wait_for_run "$run" "$tag" "$dry_run" false

if $dry_run; then
  step "Dry run completed; no tag or release was created"
  printf 'Validated builds for %s at %s.\n' "$tag" "$release_sha"
  exit 0
fi

step "Verifying the remote tag created after all builds succeeded"
pushed_sha="$(remote_tag_commit)" || die "workflow succeeded but tag $tag is not on origin"
[[ "$pushed_sha" == "$release_sha" ]] || die "$tag points to $pushed_sha, expected $release_sha"
if ! git show-ref --verify --quiet "refs/tags/$tag"; then git fetch --no-tags origin "refs/tags/$tag:refs/tags/$tag"; fi
[[ "$(git rev-parse "$tag^{commit}")" == "$release_sha" ]] || die "fetched tag $tag does not match validated SHA"
gh release view "$tag" --repo "$repository" --json assets,isDraft,url --jq .url
exit 0
