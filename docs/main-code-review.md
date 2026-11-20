# Main branch code review

## Scope and verdict

- **Reviewed repository:** Muster (`/Users/matiasmarani/Trabajo/Marvis`).
- **Final source reference:** `865b48a39d63d57d501f5691da8921c26c31042d` (`main`, v1.3.3).
- **Scope:** ⚖️ Broad — repository baseline, not a branch diff or a GitLab MR.
- **Method:** the requested `skills/common/mr-review` skill, loaded from `/Users/matiasmarani/Trabajo/xt/xstudio/webapp/skills/common/mr-review/SKILL.md`; project rules take precedence. GitLab discovery, posting and issue creation were omitted because the requested deliverable is this local report.
- **Verdict:** **⚠️ Fix before the next release — three major findings.** No confirmed blocker is reported.

The worktree was clean when the final reference was checked. During review, `main` advanced from `c0271c906b766af375dd2327f4d90ec5bda4c726` to the reference above. The intervening commit changes only release metadata in `CHANGELOG.md`, `README.md`, `app/Cargo.toml` and `Cargo.lock`; that diff was inspected and does not change the production paths cited below.

This is a risk-based static review across the repository, not a claim that every line has been exhaustively verified. Deep inspection concentrated on filesystem boundaries, persistent state, destructive operations, asynchronous document transitions, terminal input/output, agent delivery and image ownership. Large UI composition modules and tests received a lighter structural pass. No application source was modified.

## Ranked checklist

1. [ ] **🟠 Major — Security / availability:** bound decoded document images, not just their encoded files.
2. [ ] **🟠 Major — Correctness:** let Uncommitted changes work without a default-branch merge base.
3. [ ] **🟠 Major — Resource lifetime:** retire document images from GPUI's application-wide asset cache.

All three were challenged against callers, existing guards and the locked dependency implementation. They are source-confirmed; runtime reproductions and regression checks remain to be executed. Findings 1 and 3 affect the same feature but have independent causes: one image can exceed a decode budget, and many individually acceptable images can remain cached after navigation.

## 1. Bound decoded document images before handing them to GPUI

**Severity:** 🟠 Major. **Categories:** security boundary, availability, memory bounds.

**Primary anchors:** `app/src/document/media.rs:321-328` and `app/src/document/media.rs:356-361`.

**Supporting paths:** `app/src/inspector.rs:36-54`, `app/src/document.rs:1131-1140`, `app/src/document/media.rs:206-223` and `app/src/document/media.rs:464-479`.

### Evidence and impact

Both inline local pictures and direct image previews construct `Image::from_bytes` after identifying the format. The file reader limits encoded bytes, and `MediaStore::finish` accounts for `image.bytes.len()`. Neither path validates decoded dimensions, total RGBA bytes or animation frame accumulation. The display's `max_w`/`max_h` and object-fit settings constrain layout, not decoding.

The locked implementation confirms that this is not protected elsewhere:

- `gpui-pre 0.3.9`, `src/platform.rs:3318-3359`: GIF decoding constructs `GifDecoder::new`, iterates its frames and retains each decoded frame.
- `image 0.25.10`, `src/codecs/gif.rs:57-66`: that constructor uses `Limits::no_limits()`.
- The same decoder, `src/codecs/gif.rs:279-290`, allocates the logical canvas before reading the next frame. A GIF can declare a 65535 × 65535 canvas: one RGBA canvas requires **17,179,344,900 bytes**, regardless of how small its encoded payload is.

A repository author controls an image referenced by a Markdown document, or a file opened in the preview. Rendering it crosses from lower-trust file contents into unrestricted application memory allocation. Memory pressure or allocation failure affects the entire desktop application and its live work, not merely the preview. This is not a claim of code execution or a measured crash; no large allocation was attempted.

**Disproof check:** the no-symlink file reader protects which file is read, not its decoded size. The encoded-byte quota and visual scaling do not close this path. The terminal's Kitty decoder already validates dimensions and allocation (`app/src/terminal/graphics.rs:434-503`), but document images do not use that decoder.

### Smallest effective correction

Decode or validate document images on the existing background path with explicit dimension and allocation limits before publication. For animations, enforce a cumulative decoded-byte/frame budget, not just a per-frame size. Apply this to both direct previews and inline pictures. Prefer the existing `image` dependency and the terminal decoder's established limit-setting pattern; do not duplicate the terminal protocol layer.

**Regression checks:** a tiny encoded GIF with an oversized logical canvas is rejected before canvas allocation; an animation exceeding the aggregate budget is refused; normal PNG/GIF previews still render. Run rejection tests under a memory-limited isolated process. No one-click suggestion is provided because the correction changes decoding and the published image representation across multiple call sites.

## 2. Uncommitted changes unnecessarily require a merge base

**Severity:** 🟠 Major. **Category:** correctness.

**Primary anchors:** `core/src/navigation/changes.rs:188-190` and `core/src/navigation/changes.rs:259-263`.

### Evidence and impact

`status` and `file_diff_before_git` call `merge_base(...)` unconditionally, before selecting the origin. Only afterwards does `DiffOrigin::WorkingTree` choose `HEAD`. The helper executes `git merge-base <default-ref> HEAD` and propagates its failure (`core/src/navigation/changes.rs:558-561`).

A concrete affected checkout is a linked orphan branch with its own initial commit, while the primary worktree remains on `main`. Modify a tracked file on the orphan checkout. `git status` and a working-tree diff against `HEAD` are valid, but there is no common ancestor with `main`. Both application APIs return an error before reading those valid uncommitted changes.

The result is internally inconsistent: `diff_totals_for_checkout` already selects `HEAD` without asking for a merge base in WorkingTree mode (`core/src/navigation/diffs.rs:32-51`). The sidebar can therefore count changes that the Changes panel and diff cannot open.

**Disproof check:** discovery does not require a merge base (`core/src/navigation/discovery.rs:102-179`). `changed_files` explicitly returns the porcelain status directly for WorkingTree mode (`core/src/navigation/changes.rs:575-584`). The existing origin test (`core/src/navigation/tests.rs:1300`) uses related branches and does not exercise this failure. The requirement to calculate an ahead count from the merge base should not make the independent Uncommitted file list unavailable.

### Smallest effective correction

Resolve a merge base only for Branch-origin comparisons, as the totals implementation already does. For WorkingTree status, treat unavailable branch-ahead metadata separately rather than failing the whole status read. Keep Branch mode's error explicit; do not silently substitute an incorrect base.

**Regression checks:** create related and unrelated linked checkouts, modify a tracked file, and assert that WorkingTree totals, status and file diff all succeed against `HEAD`; Branch mode still refuses an unavailable comparison. No one-click suggestion is provided because both read paths and the status metadata/error contract need to be handled together.

## 3. Closing a document does not release its cached decoded images

**Severity:** 🟠 Major. **Categories:** resource lifetime, performance.

**Primary anchor:** `app/src/document.rs:1817-1818`.

**Supporting paths:** `app/src/document.rs:1131-1140` and `app/src/document/media.rs:464-479`.

### Evidence and impact

The image views use `ImageSource::Image`. Clearing the document drops `open_document` and `document_view`, including their image/store references, but does not remove the corresponding GPUI asset entries.

In the locked `gpui-pre 0.3.9` implementation:

- `src/elements/img.rs:552` loads this source through `window.use_asset::<AssetLogger<ImageDecoder>>`.
- `src/app.rs:3031-3041` inserts the load into the application's `loading_assets` map.
- `src/asset_cache.rs:20-23,35-37` retains the completed result, including the decoded `Arc<RenderImage>`.
- `src/app.rs:3009-3012` provides explicit removal; its public cache documentation at lines 3023-3026 states that results remain cached until `remove_asset`.

A search of application source found no `remove_asset` call. Weak ownership in `DocumentMedia::prepare` correctly prevents a late file read from reviving a closed document, but it does not evict a decode already registered in the application cache.

Browsing distinct, ordinary images therefore grows the retained decoded-image set for the application lifetime even though only one document remains open. Closing the preview does not restore the intended resource lifetime. This retention mechanism is source-confirmed; no resident-memory measurement is claimed.

**Disproof check:** GPUI's view subscription controls completion notifications, not cache eviction. Dropping the document's own `Arc` cannot free a render image still held by `CachedLoadState::Loaded`. Repeated identical content may reuse a cache key, but distinct images still accumulate.

### Smallest effective correction

Track the images admitted to a document's asset lifecycle and explicitly retire their asset entries when that document is replaced or closed. Cover direct images, SVG readings and inline local media, and avoid evicting an image still owned by another active consumer. Preserve the existing weak late-result guard. A bounded cache is another valid approach only if it actually covers `ImageSource::Image`, not just URL resources.

**Regression checks:** render distinct images, wait for decoding, replace/close the document and check that retired assets are no longer cached. Repeat across several documents and include a decode still in flight at close. GPUI exposes `Image::is_asset_cached` under test support, so this can be deterministic without an RSS threshold. No one-click suggestion is provided because retirement requires access to the application context and coordination across document lifecycle paths.

## Coverage and limitations

| Area | Inspection performed |
| --- | --- |
| Checkout files and persistence | Read/write/export boundaries, bounded reads, path traversal, compare-before-replace, publication/durability errors, workspace save, instance lock and writer slot lifecycle. |
| Git and destructive worktree actions | Checkout identity, worktree preview/removal/creation, subprocess collection, status/diff origins and snapshot reads; selected existing tests inspected. |
| Document and review workflows | Load/save/navigation/close guards, image rendering and ownership, note completion and durable OpenCode send/queue/flush paths. |
| Terminal and agents | Parser boundary recovery, graphics decode quotas, session shutdown ownership, input/paste and status parser structure, observation and relocation entry points. Not a terminal protocol conformance audit. |
| UI composition | Structural pass over sidebar, titlebar, settings, inspector, diff and terminal view; targeted lifecycle code inspected in depth. No interactive accessibility, keyboard or visual certification. |
| Build and distribution | Workspace/dependency declarations and lock references, CI/release workflow sections, macOS bundle/installer and Debian packaging paths. No package build or installation. |
| Tests, generated and third-party code | Existing tests inspected where relevant to findings. Generated icon data and vendored trees excluded from line-by-line review; only dependency implementations decisive to the findings were traced. No current advisory database query. |

### Verification status

- **Performed:** read-only Git status/reference checks, release-only intervening diff inspection, source/caller/test tracing, and inspection of the exact locked GPUI/image implementations available locally. `git diff --check` passed before this report was written.
- **Not performed:** Cargo tests, Clippy, formatting check, Node test suites, application launch, runtime memory reproduction, package smoke tests or live CI inspection. An OS-enforced execution sandbox satisfying the loaded security guidance was not established, so target code was not executed. No test or pipeline pass is claimed.
- **Suggested focused validation after fixes:** `cargo test --locked -p muster-core navigation::tests::the_working_tree_origin`, targeted new document-image UI tests, then the repository's normal checks with its pinned Rust 1.97.1 toolchain. Run hostile-image cases with bounded memory and no network.

The protections inspected include strong filesystem traversal checks, durable-before-send review intent and explicit handling of uncertain document-save durability. No migration, compatibility reader, architectural rewrite, version bump or dependency addition is recommended by this review.
