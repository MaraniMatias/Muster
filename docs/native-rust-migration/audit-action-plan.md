# Native application audit: implementation action plan

**Status:** AP-01 through AP-11 implemented and verified in the uncommitted patch over `69ae8ba`. AP-12 required automated gates and actual macOS debug-binary bundle smoke passed; the release build blocker is diagnosed and fixed and `scripts/release.sh release` completes, while release performance measurements and Linux/manual GUI/platform acceptance remain pending. This is not unconditional release readiness. Historical per-task runs below are not final release acceptance.

**Source baseline:** `7265e48` (`chore: fixes some issues`).

**Scope:** the verified findings from the revised audit of correctness, maintainability, performance, architecture/design, and style/readability.

**Execution model:** small, separately reviewable assignments suitable for delegation to different agents, with a final integration pass.

This plan supersedes recommendations from the first audit where the revised audit withdrew or narrowed them. It is not a request to redesign the application, reach full Legacy parity, or change release policy.

Line references below describe the baseline commit. Locate the named symbols before editing: line numbers will move. If a later commit already implements an item, verify the behavior and its tests before recording it as already satisfied. Do not recreate an obsolete problem.

## 1. Outcome and boundaries

The completed work must make the following statements true:

- Opening a rejected Markdown link produces a visible, dismissible error, without requiring a later render to dispatch the action.
- Checkout navigation does not confuse zero changed lines, unknown status, or a failed read with a clean checkout.
- A local Markdown image is read through the checkout's descriptor-based boundary, not validated as a path and reopened later by name.
- Local image discovery and filesystem reads do not run in Markdown parse/render callbacks on the UI thread.
- Image metadata distinguishes absent files from present non-images and recognizes the WebP signature at the correct offset.
- Markdown/Source transitions put keyboard focus on the visible reading rather than an invisible editor.
- Reading the current terminal selection does not require copying its entire grid.
- File-tree preparation avoids unnecessary collection copies while preserving current scrolling, folding, selection, and error behavior.
- The small maintainability and documentation issues identified by the audit are resolved without unrelated restructuring.
- Every behavior change has a runnable regression check, and final claims distinguish tests, static analysis, measurements, and manual observations.

### Not included

- A new renderer, terminal parser, framework, persistence system, theme system, or collection of crates.
- Editing `legacy/` or the hash-pinned Alacritty vendor tree.
- Dependency upgrades, a new dependency-audit tool, or a new benchmark framework.
- Database migrations, compatibility readers, feature flags for old behavior, or schema changes unrelated to these findings.
- General removal of `pub(crate)`, a mandatory split of `TerminalWindow`, or a function-length target.
- Full support for raw HTML media, video playback, or embedded-language highlighting in diffs.
- Developer ID signing, notarization, or a claim that ad-hoc signing establishes Gatekeeper trust.

## 2. Evidence baseline

The revised audit ran the following checks successfully against the baseline, with the actual compiler and Cargo both pinned to 1.97.1:

| Check | Observed result |
| --- | --- |
| Formatting | `cargo fmt --all --check` passed |
| Clippy | `cargo clippy --locked --workspace --all-targets -- -D warnings` passed |
| Workspace tests | 102 inline core tests, 4 core integration tests, and 285 native tests passed; 2 native performance cases were ignored |
| Ignored performance cases | Both passed when explicitly selected |
| Dependency-auditor tests | 18 tests passed with `node --test scripts/audit-rust.test.mjs` |
| macOS bundle smoke | Bundle assembly, plist validation, and ad-hoc signature verification passed with a disposable test executable |

These are historical observations, not acceptance evidence for future patches. Test counts may legitimately increase. Preserve existing coverage and explain changes; do not enforce a frozen count.

Public-core reproductions also established:

```text
LFS pointer named lfs.png:
  is_binary=false, image=Some(Png), additions=3, changed_files=1

Arbitrary binary named not-a-picture.png:
  is_binary=true, image=Some(Png), additions=0, deletions=0, changed_files=1

WebP header in an extensionless file:
  is_binary=true, image=None

Validated local pathname replaced by a symlink:
  a subsequent pathname read follows the replacement outside the checkout
```

The LFS case proves incorrect metadata, but does **not** prove the old report's claimed image notice: the text-diff branch handles that file.

Existing test-profile measurements for `painting_snapshot()` were approximately 22.190 microseconds at 80x24 and 322.949 microseconds at 300x100. These are measurements from one machine, not release timings, frame latency, or portable thresholds.

## 3. Assignment rules shared by every agent

1. Read this section, the assigned task, and the applicable `AGENTS.md` instructions before editing.
2. Inspect the current implementation, its callers, and relevant tests. The proposed implementation is a starting point, not permission to ignore better local evidence.
3. Search for an existing solution before creating a helper, state field, module, seam, dependency, or cache.
4. Implement only the assigned outcome and directly required tests. Report unrelated issues without fixing them.
5. Prefer the smallest complete change. Do not retain the old path beside the fix for compatibility.
6. Keep code, identifiers, comments, and technical documentation in English.
7. Filesystem validation and bounded reading belong in `core`; widgets, drawing, focus, and view state belong in `native`. Reuse the existing executors and lifecycle ownership.
8. Preserve the installed color roles, read-only document behavior, PTY ownership, request-token invalidation, bounded Git concurrency, and state-writer shutdown semantics.
9. Tests must use temporary fixtures and isolated state. Never read or write the person's real workspace state, launch a real browser from the automated suite, or fetch an external image to make a test pass.
10. Add a regression that fails without the relevant fix where practical. Do not substitute a hand-written copy of the algorithm for a test of production behavior.
11. A filtered Cargo command that runs zero tests is not verification. Record which tests actually executed.
12. Run focused checks during development, then the relevant integration gates. Preserve exit codes; do not infer success from `tail`, `grep`, or another command at the end of a pipeline.
13. Do not modify persistent agent configuration. Honor the requested model and reasoning effort using controls the active delegation runtime actually exposes. Do not claim that child effort was inherited merely because an environment variable says so. If the required control is unavailable, report the limitation before dispatching.
14. Run agents sequentially when they use the same model, as required by the project execution instructions. Logical independence in this plan is not permission to violate that rule.
15. Follow the repository worktree rules. Reuse a task worktree where available; otherwise use `.worktrees/<branch-name-with-slashes-as-dashes>` and a Conventional Commits branch type. Do not reset, stash, or discard somebody else's work.
16. Stop when the assigned acceptance criteria pass. Do not start the next assignment automatically.

### Reliable toolchain setup

On the audited machine, invoking the pinned Cargo alone could still select a different `rustc` through `PATH`. Use this setup in a Bash session and confirm both versions:

```bash
set -euo pipefail
TOOLCHAIN=1.97.1
TOOLCHAIN_BIN="$(dirname "$(rustup which --toolchain "$TOOLCHAIN" rustc)")"
export PATH="$TOOLCHAIN_BIN:$PATH"
rustc --version
cargo --version
```

This changes only the current shell environment. Do not edit global toolchain or agent configuration.

### Required handoff from each assignment

Provide the following in the completion report:

```text
Task: AP-XX
Source commit and dependency commits:
Result: implemented / verified already satisfied / blocked
Observed cause:
Files changed and why:
Regression checks added or extended:
Commands executed, matched test counts, and exit results:
Manual checks and measurements, if any:
Checks not run and exact reason:
Remaining limitations:
Commit or patch reference:
```

Update the task's status in this document only with supporting evidence. Do not mark a dependent task complete just because a prerequisite changed the same file.

## 4. Task register and dependencies

Priority is implementation order, not a claim of critical severity. The revised audit did not establish a critical defect. In particular, the local-media reading boundary should be fixed before extending that feature.

| Task | Outcome | Audit mapping | Priority | Dependencies | Status |
| --- | --- | --- | --- | --- | --- |
| [AP-01](#ap-01) | Dispatch links directly and show opener errors | C1, A1, part of M3 | First batch | None | Implemented and verified |
| [AP-02](#ap-02) | Separate checkout change presence from line totals | C2, part of M3 | First batch | None | Implemented and verified |
| [AP-03](#ap-03) | Prepare local media safely outside UI parsing | C3, P3, part of M3 | First batch | AP-01 first for shared document code | Implemented and verified |
| [AP-04](#ap-04) | Correct image format metadata | C4, part of M3 | First batch | None | Implemented and verified |
| [AP-05](#ap-05) | Make Markdown focus follow the visible reading | C5, part of M3 | First batch | AP-01 and AP-03 | Implemented and verified |
| [AP-06](#ap-06) | Read terminal selection without a grid snapshot | P1 | Second batch | None | Implemented and verified |
| [AP-07](#ap-07) | Reduce file-tree preparation work | P2, relevant part of S3 | Second batch | AP-02; AP-01 first for shared inspector code | Implemented and verified |
| [AP-08](#ap-08) | Share measured and drawn inspector typography | M1 | Small cleanup | AP-07 | Implemented and verified |
| [AP-09](#ap-09) | Remove the obsolete preview truncation field | M2 | Small cleanup | AP-03 and AP-05 | Implemented and verified |
| [AP-10](#ap-10) | Correct source comments; assess only necessary readability changes | S1, remaining S3; A2/A3 disposition | Small cleanup | AP-01 through AP-09 | Implemented and verified |
| [AP-11](#ap-11) | Reconcile current documentation and verification claims | S2, packaging caveats | Documentation | AP-01 through AP-10 | Implemented and verified |
| [AP-12](#ap-12) | Run integration, lifecycle, and platform acceptance | M3 and all acceptance evidence | Final | AP-01 through AP-11 | Automated complete; release build fixed; Linux/manual pending |

Recommended serial order: **AP-01 -> AP-02 -> AP-03 -> AP-04 -> AP-05 -> AP-06 -> AP-07 -> AP-08 -> AP-09 -> AP-10 -> AP-11 -> AP-12**.

AP-04 and AP-06 are logically independent of the UI fixes. They can be moved earlier without changing their scope. Shared-file edits still need ordered integration; do not assign overlapping ownership of `document.rs`, `inspector.rs`, or `app.rs` without an explicit integration owner.

<a id="ap-01"></a>
## AP-01: Dispatch Markdown links directly and show failures

### Context and evidence

At the baseline, `TerminalWindow::open_link` stores failures as `ExternalTarget::Link`. The only renderer of `external_notices`, in `files_inspector`, skips that variant. The link handler in `document_element` also defers dispatch through `link_queue`, which is drained during rendering.

`WeakEntity` is usable in the toolkit's `Send + Sync` callback. The revised audit confirmed the relevant auto-trait properties with the compiler; the previous source comment claiming otherwise is not a reason to retain the queue. Recheck the installed API when implementing.

### Inspect first

- `native/src/document.rs`: `document_element`, the link callback, and the queue drain.
- `native/src/inspector.rs`: `TerminalWindow::open_link`, `open_externally`, and the external-notice renderer.
- `native/src/app.rs`: `ExternalTarget`, `link_queue`, initialization, and the existing failure banner around baseline lines 2555-2600.
- `core/src/opener.rs`: the URL refusal and exit-status contract.
- `native/src/inspector_tests.rs` and the existing headless window fixtures.

### Implementation steps

1. Capture a weak window entity in the TextView link callback and dispatch `open_link` from that callback, using the `Window` and `App` arguments it already receives.
2. Keep the actual opener invocation on the background executor. Removing render-side dispatch is not permission to run `Command::status()` on the UI thread.
3. Send link failures to the existing visible failure banner. Reuse its dismiss behavior instead of creating a new notification component.
4. Remove `link_queue`, its initialization, its drain, and the comments that justify it with an incorrect `Send` restriction.
5. Remove the now-unused link-notice path if no caller remains. Narrow `ExternalTarget` or its use only as far as this deletion requires; do not redesign all error maps.
6. Preserve path validation and the file-row opener notices. A link error must not alter the selected checkout, expand a folder, or close the current document.
7. Ensure weak callbacks and completions do not keep a closed window alive or panic after its release.

### Required regression checks

- Click a rendered link with a rejected scheme; verify a visible error and that Dismiss removes it.
- Repeat with Files not selected or the inspector hidden; the error must still be visible in the window banner.
- Verify the callback dispatches without a separate, unrelated render being the action processor.
- Verify opening a link does not also activate a file-tree row or change document ownership.
- Cover the dropped-window completion path using the existing weak-entity lifecycle pattern where practical.
- Use rejected URLs for the UI failure test: validation returns before any process spawn. For a successful opener result, reuse the existing test approach or a minimal test seam; do not launch a real browser.

### Acceptance

- [x] Rejected links produce a dismissible, visible error through the existing banner.
- [x] Link action dispatch no longer depends on draining a queue during render.
- [x] The production link queue and unused link-notice storage path are removed.
- [x] Blocking process work remains off the UI thread.
- [x] Existing file-opening and URL-validation tests remain green.

### Evidence

Implemented as described above. `document::link_handler` is the production closure the text view is handed: it captures `WeakEntity<TerminalWindow>` and dispatches `open_link` from the click's own `Window` and `App`. `TerminalWindow::link_queue` and its drain are gone, `ExternalTarget` is gone, and `external_notices` is keyed by `(checkout, path)` for file rows only. `open_link` still runs the opener on `background_executor()` and puts the refusal in the window's existing `failure` banner, which is drawn whatever the inspector is showing and is dismissed by the same `dismiss-message` control as every other message.

Three cases in `app::sidebar_tests::inspector_tests` (`native/src/inspector_tests.rs`):

- `a_refused_link_is_said_in_the_window_and_dismissed` — a real pointer click on a rendered `[not a page](file:///etc/passwd)` link; asserts the refusal lands with no frame drawn after the click, that `dismiss-message` is on screen and carries the opener's own words, that the document, the tree and `external_notices` are untouched, and that Dismiss clears it.
- `a_refused_link_is_said_whatever_the_inspector_is_showing` — the same click with `inspector_tab = Changes` and then with `panels_visible = false`.
- `a_link_clicked_after_its_window_is_released_opens_nothing` — the production closure called after the window entity is released.

Reverting `open_link` to the notice path and the handler to a render-drained queue fails the first two, on "the click opened the link without anything else having to draw". The third holds either way: it covers the new weak dispatch, not the removed queue.

Commands, both `rustc` and `cargo` at 1.97.1 through the pinned toolchain on `PATH`:

| Command | Result |
| --- | --- |
| `cargo test --locked -p muster-native -- refused_link after_its_window_is_released` | 3 passed, 0 failed, 295 filtered out; exit 0 |
| `cargo test --locked --workspace` | 102 core inline + 4 core integration + 296 native passed, 2 native ignored; exit 0 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --all --check` | exit 0 |

`core::opener::tests` (4 cases) are unchanged and pass as part of the workspace run; the success path of the opener is still covered there with `/usr/bin/true` as the program, so no browser was launched to make any of this pass.

Not done here, and left to AP-11: the §2 evidence table still describes the pre-AP-01 baseline counts.

Suggested focused checks: the affected `document::tests::`, `app::sidebar_tests::inspector_tests::`, and core `opener::tests::` cases. Record the actual new test names and nonzero matched counts.

<a id="ap-02"></a>
## AP-02: Separate checkout change presence from line totals

### Context and evidence

`TerminalWindow::select_checkout_row` in `native/src/diff.rs` treats `additions > 0 || deletions > 0` as the presence of changes. It also treats a missing total as false. Neither inference is valid. A checkout with one binary change can have `+0/-0`, while `GitStatus.files` contains that change.

The relevant comparison is against the default branch's merge base, not merely unstaged changes or `HEAD`. Committed changes in a feature worktree must remain visible.

### Inspect first

- `native/src/diff.rs`: `select_checkout_row`, `open_changes`, and the clean/dirty navigation contract.
- `native/src/inspector.rs`: `request_git_status`, its coalescing/token behavior, and status completion.
- `native/src/app.rs`: count sweeps, `refresh_diff_totals`, `apply_diff_totals`, and membership invalidation.
- `native/src/navigation.rs`: navigation revision and selection transitions.
- `core/src/navigation/changes.rs`: `GitStatus.files` and the merge-base comparison.
- `core/src/navigation/diffs.rs`: what `DiffTotals` counts and deliberately does not count.
- `native/src/sidebar_tests.rs` and `native/src/app_tests.rs`: checkout-row activation tests.

### Implementation steps

1. Preserve the intended interaction: a checkout with changes opens its change set; a known-clean checkout shows its terminal; an explicit terminal click always selects that terminal.
2. Reuse the existing Git change-list result as the authority for change presence where possible. Keep line totals as presentation metadata.
3. Represent the unknown and failed states explicitly in the decision. Do not replace the current condition with another `is_some_and` that silently maps an unavailable change list to clean.
4. If a fresh status read is required, use the existing bounded/coalesced request path rather than spawning Git from a render or one process per row.
5. Guard any delayed navigation decision against newer user intent. Checking only the checkout id is insufficient if the person has since opened a document or explicitly selected a terminal in that checkout.
6. Preserve request tokens, membership validation, missing/archive handling, and the current Git read budgets.
7. Do not rely solely on `Checkout.changed_files` unless the inspected code proves it describes the same merge-base change set. A dirty-working-tree count alone misses committed branch changes.
8. Keep line-count display independent of the decision to show a diff. Do not invent line counts for binary files.

### Required regression matrix

| Situation | Required result |
| --- | --- |
| One changed binary file, no text changes | Open Changes |
| New empty file | Open Changes |
| Tracked mode-only change on a filesystem that supports it | Open Changes |
| Committed binary change ahead of the merge base, clean working tree | Open Changes |
| Known-clean checkout | Show its terminal |
| Status absent when clicked | Resolve without assuming clean |
| Status lookup fails | Surface failure/unknown state, not a clean result |
| Person selects another checkout before completion | Do not navigate back |
| Person explicitly selects a terminal or document before completion | Do not steal the surface or keyboard |
| Click the row versus its name/control | Use the same decision path |
| Plain directory, missing checkout, or archived checkout | Preserve existing behavior |

### Acceptance

- [x] Zero line totals no longer imply a clean checkout.
- [x] Unknown and failed status do not become clean.
- [x] The change-presence decision uses the intended merge-base semantics.
- [x] Delayed results cannot override newer view-selection intent.
- [x] No synchronous Git call or per-render Git request is introduced.
- [x] Existing sidebar selection, process ownership, and missing/archive tests pass.

### Evidence

Implemented as described above. `TerminalWindow::change_presence` in
`native/src/diff.rs` is the whole decision, and it reads `git_status` — the
`GitStatus.files` list `core::navigation::status` builds from
`git status --porcelain=v2` plus `git diff --name-status <merge-base> HEAD` —
rather than `diff_totals`, so a binary, an empty file or a mode-only change is a
change. `Checkout.changed_files` was inspected and not used:
`core/src/navigation/discovery.rs:125` writes `changed_files: 0` for every
checkout, so it describes nothing.

The states are explicit: `Present`, `Absent`, `Unknown` (no read has answered)
and `Failed` (a read answered that it could not tell). Unknown and Failed both
arm `TerminalWindow::pending_row_choice` and ask for the status through the
existing coalesced `request_git_status` — no new Git call and no render path.
`Failed` is decided **before** the held list is read, and that ordering is the
correction this section records: a failed read leaves the previous `GitStatus` in
`git_status`, so a checkout whose last successful answer was clean was
`Absent`, and one whose last answer was dirty was `Present` — a read that failed
in between was answered from a list it had already overtaken. Both are `Failed`
now, which is the only answer a read that failed can support. The cached list is
untouched, so the panel still draws it under the failure and Retry still rereads
it.
`Absent` for a plain directory is a known answer (`is_git_checkout`) rather than
an inferred one, so a plain row does not wait for a read that would never come.
`settle_row_choice` is called from the one place a status read completes: it
opens the change set, or sets the existing `focus_pending` so the next frame
gives the keyboard to the terminal, or states the failure in the window's own
`failure` banner. The guard is `TerminalWindow::view_intent`, a counter bumped in
`clear_document` — every gesture that replaces what the window shows (another
row, a terminal, a crumb, a file in the tree, a new shell, closing a preview)
comes through it — so a late answer that no longer carries the newest intent is
dropped instead of navigating.

Regression checks, all in `native/src/sidebar_tests.rs`, against real Git
repositories and real pointer dispatch:

- `a_row_with_no_line_counts_still_opens_its_change_set` — a binary file, a new
  empty file and a staged mode-only change in one checkout. Asserts
  `diff_totals == {additions: 0, deletions: 0}` *before* the click, that the
  click opens the change set with the counts still zero, that the status lists
  three files, and that the binary file's `additions` is `None` rather than an
  invented zero. Three matrix rows at once, because they are one production
  path.
- `a_committed_change_ahead_of_the_merge_base_opens_the_change_set` — a
  worktree with one committed binary file and a clean working tree:
  `ahead_count` is 1, the row opens the change set, the counts are zero.
- `a_known_clean_checkout_shows_its_terminal` — a live shell in a Git checkout:
  the first click waits for the read and then focuses the terminal; the second is
  answered from the list the window already holds and starts no read.
- `a_click_before_the_change_list_is_read_waits_for_it` — asserts the click
  armed a decision and took none, then that the read's answer opens the change
  set.
- `a_failed_change_read_is_said_rather_than_called_clean` — `.git` removed, so
  `validated_checkout_root` refuses: `git_status_errors` holds the failure, no
  change set opened, the keyboard was not taken, and `dismiss-message` is on
  screen.
- `a_clean_list_cached_before_a_failed_read_is_not_a_clean_checkout` — the
  order reversed: a real read that says clean, then `.git` removed and a second
  read that fails, so a clean list is still in `git_status` under a recorded
  failure. The click arms a decision instead of taking the held answer, the
  shell does not get the keyboard where a clean answer would have given it, no
  change set opens, and the reread's failure is in `failure` and on screen as
  `dismiss-message`.
- `a_cached_change_list_is_not_opened_after_the_read_behind_it_failed` — the
  same order with the opposite held answer: a listed `picture.png`, then a
  failed read. The click opens no change set out of a list no read has
  confirmed, and the failure is said instead.
- `a_late_read_does_not_navigate_back_to_the_checkout_left_behind` — two dirty
  checkouts and two clicks in one turn; once every read has landed, the open
  change set is the second checkout's.
- `a_late_read_does_not_take_the_terminal_the_person_asked_for` and
  `a_late_read_does_not_take_the_document_the_person_asked_for` — a row click
  followed, in the same turn, by a real terminal-row click or a real
  `file-control:` click; the change set stays closed, the terminal keeps the
  keyboard, and the document is what is read.
- `a_plain_directory_row_still_shows_its_terminal` — a plain directory's row
  focuses its terminal and arms nothing.
- `the_row_and_the_figures_on_it_make_the_same_decision` — the figures drawn on
  the row (`diff-<id>`, an `on_mouse_down` target of its own) reach the same
  decision, which `a_worktree_row_and_the_label_on_it_do_the_same_thing` covers
  for the label button. That test used to insert `diff_totals` to make the row
  look dirty; it now inserts a `GitStatus` with one file and leaves the counts at
  zero, so it fails on the old condition and passes on the new one.

Checks, with `rustc 1.97.1 (8bab26f4f 2026-07-14)` and `cargo 1.97.1
(c980f4866 2026-06-30)` both selected through the `PATH` setup in section 3:

| Command | Result |
| --- | --- |
| `cargo test --locked -p muster-native -- row_with_no_line_counts click_before_the_change_list committed_change_ahead_of_the_merge_base known_clean_checkout failed_change_read clean_list_cached_before_a_failed_read cached_change_list_is_not_opened late_read plain_directory_row worktree_row_and_the_label row_and_the_figures` | 13 passed, 0 failed, 297 filtered out; exit 0 (1.74 s; the two new cases also run four times in a row without a flake) |
| `cargo test --locked --workspace` | core 102 passed and 4 passed; native 308 passed, 0 failed, 2 ignored; exit 0 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --all --check` | exit 0 |

Three halves of the decision were proved to bite by reverting each in place and
rerunning the same cases:

| Reverted | Failing cases |
| --- | --- |
| `change_presence` back to `diff_totals` with `is_some_and(additions > 0 \|\| deletions > 0)` | 7 of the original 11: `a_row_with_no_line_counts_still_opens_its_change_set`, `a_click_before_the_change_list_is_read_waits_for_it`, `a_committed_change_ahead_of_the_merge_base_opens_the_change_set`, `a_failed_change_read_is_said_rather_than_called_clean`, `a_late_read_does_not_navigate_back_to_the_checkout_left_behind`, `a_known_clean_checkout_shows_its_terminal`, `a_worktree_row_and_the_label_on_it_do_the_same_thing` |
| `change_presence` back to reading `git_status` before `git_status_errors` | both cached cases: `a_cached_change_list_is_not_opened_after_the_read_behind_it_failed` ("a read that failed still opened the change set it left behind") and `a_clean_list_cached_before_a_failed_read_is_not_a_clean_checkout` ("the click read the cached clean list as the newest answer and took it") |
| the `choice.intent != self.view_intent` guard in `settle_row_choice` | 2 of the 3 `late_read` cases: the terminal and the document ones |

`a_late_read_does_not_navigate_back_to_the_checkout_left_behind` holds with the
guard removed, because the single pending slot is already replaced by the newer
click; the guard is what the other two cases need, and those are the ones that
fail without it. `a_plain_directory_row_still_shows_its_terminal`,
`the_row_and_the_figures_on_it_make_the_same_decision` and the two `late_read`
cases pass on the baseline shape too: they pin behavior that must not change
rather than the defect itself.

`native` now holds 308 default tests and the two ignored performance checks
(298 before AP-02, ten added by AP-02 and two by the failure-precedence
correction recorded above); `core` is unchanged at 106. No count in
`AGENTS.md` was touched: reconciling the documented numbers is AP-11.

Limitations: the decision reads the status the window already holds, so a change
made seconds earlier is answered from the last read — the same staleness the
previous `diff_totals` decision had against the last sweep. A failure is not
stale in that way: it outranks the held list, so a checkout whose Git stopped
answering is asked again on the next row click rather than believed either way.
The background cadence in `app.rs` and the Changes tab are what make the list
current, and no per-click refresh was added. `core` is untouched.

<a id="ap-03"></a>
## AP-03: Prepare local Markdown media through a safe background read

### Context and evidence

`DocumentMedia::picture` currently calls `core::files::external_path`, stores a `PathBuf`, and gives it to GPUI's image source. The locked GPUI implementation later performs `fs::read` on that pathname. A successful validation therefore does not bind the later read to the validated object.

There is a second issue on the same path: `DocumentMedia::parse` performs filesystem validation. GPUI Base 0.7.3 can parse full Markdown replacements of at most 4 KiB synchronously, so the callback is not guaranteed to be a worker callback.

Both findings should be fixed together, not by two incompatible resource-loading designs.

### Inspect first

- `native/src/document/media.rs`: `DocumentMedia`, `picture`, `parse`, `render_inline`, `checkout_relative`, and fallback labels.
- `native/src/document.rs`: `DocumentView`, `document_state`, `document_body`, and the lifetime of the rendered state.
- `native/src/inspector.rs`: `request_document`, background execution, document tokens, and completion invalidation.
- `core/src/files.rs`: `root`, `relative`, `open_root`, `open_directory`, `open_file`, and `read_preview`'s bounded-read pattern.
- The locked toolkit's byte-backed image source and inline-resource invalidation APIs. Verify signatures before coding; do not invent an image constructor or manually decode formats the toolkit already handles.
- Existing media path tests and `a_rendered_document_draws_its_picture_and_says_what_it_cannot`.

### Required execution boundary

```text
Document/Markdown processing identifies a local resource
  -> worker performs checkout validation and a bounded descriptor-based read
  -> completion carries owned bytes or an equivalent immutable prepared resource
  -> native state accepts only a result for the current document request
  -> GPUI decodes/displays that prepared resource without reopening the pathname
```

The arrows describe ownership and thread boundaries, not a requirement for new layers or modules.

### Implementation steps

1. Reuse the existing toolkit parser/plugin mechanism to identify image nodes. Do not add a handwritten Markdown parser, regex parser, second highlighter, or unrelated parser dependency.
2. Keep URL normalization pure: relative directory resolution, percent decoding, fragment/query handling, and refusal of paths that escape the checkout must not require filesystem access.
3. Add only the binary-read capability actually missing from `core::files`. Reuse the existing descriptor walk and regular-file checks; do not create a second path-validation policy.
4. Read from the successfully opened descriptor. Return owned bytes/prepared content, not a supposedly safe pathname for a later consumer to reopen.
5. Reuse the existing bounded-read pattern, including a read bound that still rejects a file that grows after the metadata check. Refuse oversized resources explicitly; never return a truncated image as complete.
6. Choose and document finite encoded-resource and retained-document bounds as part of the local implementation. Prefer existing applicable limits. A 720px display constraint is not an encoded-byte or decoded-memory bound. Do not add user preferences or a generic configuration layer for this task.
7. Schedule filesystem discovery/reading through the existing background executor. Neither plugin parse callbacks, render callbacks, layout, nor paint may canonicalize, stat, open, or read a local image.
8. Keep prepared resources owned by the current document/view. Repeated renders and repeated references to the same image must not start an unbounded number of detached reads or copy complete buffers unnecessarily.
9. Use existing document identity/request-token rules to reject stale completions after another document opens, the checkout changes, or the view closes. A task that finishes late must not resurrect the old view or retain it strongly.
10. Hand GPUI a byte-backed source supported by the locked dependency. Do not validate a path twice and call that a TOCTOU fix. A second `stat`, `canonicalize`, or shorter delay still leaves a check/use boundary.
11. Preserve the currently supported local image formats, source offsets used for identity, selection text, alt text, missing-image fallback, front matter, and Markdown/Source toggle behavior.
12. Preserve the existing explicit behavior for data/network images and unsupported video/audio. Tests must not make external network requests. Raw HTML media is not silently added to this task's claimed coverage.

### Required security and lifecycle tests

- Valid image under the checkout, including a nested document and a normalized sibling path that remains inside the root.
- Absolute path, root-escaping traversal, `.git`, forged checkout, symlink, FIFO, socket, and non-regular final entry are refused consistently with the existing filesystem contract.
- Replace a file or ancestor with a symlink at the validation/read boundary. The read must either refuse or remain attached to the validated in-checkout object; it must not read the outside marker.
- Replace or remove the original pathname after the owned bytes are prepared. Rendering must use the snapshot, not reread the replacement.
- Reject an encoded file over the selected bound and one that grows past it during the read. Assert explicit fallback/error rather than partial bytes.
- For a small document below the toolkit's synchronous parse threshold, prove that the parse/render path does not perform the local filesystem read. Prefer a deterministic thread/job/read assertion using existing test seams, not an elapsed-time threshold.
- Loading success, decode failure, missing resource, and unsupported media retain meaningful fallback text.
- Verify a successfully loaded fixture actually reaches the asset/decode/paint outcome available through the toolkit's test support; an element id alone is not evidence that image pixels were decoded.
- Switch documents/checkouts or close the view while loading; stale completion is ignored and owned resources are released.
- Repeated rendering of unchanged content does not repeat the filesystem read.

### Acceptance

- [x] No local media pathname is validated and then reopened by the image consumer.
- [x] No local filesystem operation remains in Markdown parse/render callbacks.
- [x] The reader is descriptor-based, regular-file-only, and bounded.
- [x] The chosen resource limits and refusal behavior are documented and tested.
- [x] Stale completions cannot update or retain an obsolete document/window.
- [x] Existing local media, fallback, scrolling, and Source/Rendered behavior is preserved.
- [x] Core boundary tests and native media/lifecycle regressions pass.

This is the only task expected to need a new narrowly scoped core capability. It is not authorization to replace `external_path` for all external applications or design a general media subsystem.

### Evidence

Implemented as described above, with the boundary drawn where the plan puts it.
`core::files` gained exactly one capability, `read_resource`, which returns owned
bytes rather than a name: `open_regular` (the checkout identity checks, the
`O_NOFOLLOW` descriptor walk, the regular-file decision, the length) is now the
one place both readers go through, `read_whole` is the one bounded read, and
`read_preview` was folded onto them rather than left with a second copy. The
refusal wording for a name that is not a regular file is now shared
(`only regular files can be read`); nothing asserted the old wording.

`DocumentMedia::parse` no longer touches the filesystem at all. It resolves the
name the URL spells — `..` walked, `%20` decoded, a scheme, a backslash and a
climb past the root refused, all pure — records that the name is wanted, and puts
`{name, Weak<MediaStore>}` in the node. `DocumentMedia::prepare`, called from
`document_element`, drains the names the last parse asked for — as many as this
document has free read slots for, which is all of them for the picture a document
is written around — and starts one worker read each; `read_resource` plus a header
decision on that worker produce an `Arc<gpui::Image>`, and `render_inline` hands
GPUI `ImageSource::Image(..)` — the locked toolkit's byte-backed source, decoded by
its own asset system on its own worker. No pathname reaches the image consumer,
and no filesystem call is left in a parse, render, layout or paint callback.

Ownership is the document's. `DocumentView` owns a `MediaStore`; the plugin, the
nodes a parse leaves behind and the read in flight all hold it weakly, so closing
or replacing a document releases its bytes at once, and a read that lands late
upgrades nothing, updates nothing and asks for no frame. Nothing is scheduled
per render: a name leaves `Wanted` the moment its read starts, so one picture
named once is read once however many frames are drawn — and a name that is waiting
for a read slot is asked again rather than read again.

Limits, chosen here and documented at their definitions:

| bound | value | why |
| --- | --- | --- |
| `core::files::MAX_RESOURCE_BYTES` | 16 MiB per resource | a screenshot a document is written around is not a text file, and this is the same order of magnitude `core` already holds for diff content (`MAX_DIFF_BYTES`) |
| `document::media::MAX_DOCUMENT_MEDIA_BYTES` | 4 resources = 64 MiB retained per document | the retained set must not grow with the length of the file; past it a picture is said in words like any other that cannot be shown |
| `document::media::MAX_DOCUMENT_MEDIA_READS` | 4 reads open per document | the same number as that share, so a document never has more encoded bytes on the way to it than it may hold afterwards: 4 × 16 MiB = 64 MiB in flight. See "the in-flight bound" below |
| `MAX_IMAGE_EDGE` | 720 px | unchanged, and not a byte bound: it is a drawing bound |

A resource over either byte bound is refused outright — never truncated — and a
file that grows between the size it claimed and the read is the same refusal, as
`read_whole` asks the limit of the read as well as of the file.

Regressions added. `core/src/files.rs` (3): `a_resource_is_read_whole_from_the_descriptor_or_not_at_all`
(nested resource read whole including NUL and invalid UTF-8; a sparse file exactly
at the limit comes back whole and one byte over is refused; absolute, escaping,
`.git`, link, socket, FIFO, directory, empty and missing names refused; a forged
id and someone else's path refused), `a_resource_read_stays_attached_to_the_file_it_validated`
and `a_file_that_grows_between_the_size_and_the_read_is_refused`, both described
below because they are the two the review asked to be stronger.

`native/src/document/media.rs` (3): `the_bytes_decide_the_format_not_the_name`,
`a_document_holds_a_bounded_number_of_picture_bytes` and
`a_document_reads_only_as_many_pictures_as_it_has_slots_open`.

`native/src/inspector_tests.rs` (4, real checkouts and real pointer dispatch):
`a_rendered_document_draws_its_picture_and_says_what_it_cannot` (extended) now
asserts that the frame which parsed the document prepared no picture and drew no
element — this scheduler runs no worker until the test says so — then that the
picture is drawn, that the document started exactly **one** read, and that the
toolkit decoded the prepared bytes: `Image::get_render_image` returns an image
whose frame is four bytes of BGRA. That is the decode outcome, not an element id.
`a_picture_the_checkout_will_not_hand_over_is_said_in_words` walks seven refusals
in one document (escaping, absolute, `.git`, a link out, text bytes under a
`.png`, a sparse file one byte over the bound, and a file that is not there) and
asserts neither the element nor a byte came back for any of them.
`a_picture_that_does_not_decode_is_said_in_words` covers the one failure left
after a successful read: a PNG header followed by something that is not a PNG
falls back to the alt text where the picture would have been.
`a_picture_read_after_its_document_is_closed_is_dropped` closes the document
through the window's own Close control with the read in flight, asserts the store
is gone, that nothing is drawn and the pane is closed, and that reopening reads
again into a store of its own. `a_picture_drawn_is_the_bytes_that_were_read`
uses a nested document reaching a sibling directory, and after the picture is
drawn replaces the file behind its name with something that is not a picture: the
same `Arc` is still held, the element is still drawn and the decoded pixel is
still the one that was read.

#### The two regressions the review sent back

Both were passing and both were weaker than the property they claim. The swap test
exchanged the names and *then* called `read_resource`, so it only ever reached the
refusal — a name exchanged before the walk is refused, which is not the race the
plan asks about. The growing-file test drove `read_whole` with a `Cursor` five bytes
long against a limit of four: a reader that reports more than its limit is not a
file that grew, so it proved the arithmetic and not the boundary.

Both now stand where the production reader stands, using the two calls
`read_preview` and `read_resource` are made of, through a local `open_relative`
helper that is those same lines without the bound check:

- `a_resource_read_stays_attached_to_the_file_it_validated` checks both sides of
  the boundary, because a single-threaded test can only reach one answer to a race
  by standing on the other side of it. Before the walk, the final entry exchanged
  for a link and the ancestor exchanged for a link are each refused by the whole
  production read. Across the boundary, `open_regular` opens and classifies
  `sub/picture.png` and hands back the descriptor; the file is then removed and
  replaced by a link to the outside marker, and its ancestor is renamed out and
  replaced by a link to the outside directory — with `fs::read_to_string` on the
  name asserted to return `outside marker`, so the fixture is proved to be an
  escape rather than assumed to be one. The read then goes through the handle and
  returns `inside`.
- `a_file_that_grows_between_the_size_and_the_read_is_refused` opens a real
  1 MiB file through `open_regular` and reads it whole at `MAX_PREVIEW_BYTES` (at
  the bound comes back whole), then opens it again, appends a byte to the file
  behind the descriptor so the recorded length is stale, asserts the file is
  `MAX_PREVIEW_BYTES + 1` on disk, and reads through the handle at the same bound:
  the same `too_large` wording `read_preview` uses. The production length checks in
  `read_preview` and `read_resource` are untouched and still pinned, the first by
  `preview_returns_a_whole_file_or_nothing` and the second by the sparse
  at-limit/over-limit pair in `a_resource_is_read_whole_from_the_descriptor_or_not_at_all`.

Both were then broken on purpose and rerun, each failing on its own assertion:

| Broken | Failing case and what it said |
| --- | --- |
| the read-side bound in `read_whole` removed | `a_file_that_grows_between_the_size_and_the_read_is_refused` — the read past the bound raised no refusal at all |
| the raced file read by its name instead of through the handle | `a_resource_read_stays_attached_to_the_file_it_validated` — left `outside marker`, right `inside` |

Checks, with `rustc 1.97.1 (8bab26f4f 2026-07-14)` and `cargo 1.97.1
(c980f4866 2026-06-30)` both selected through the `PATH` setup in section 3:

| Command | Result |
| --- | --- |
| `cargo test --locked -p muster-core -- files::` | 13 passed, 0 failed, 92 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- picture document::media` | 11 passed, 0 failed, 305 filtered out; exit 0 |
| `cargo test --locked --workspace` | core 105 passed and 4 passed; native 314 passed, 0 failed, 2 ignored; exit 0 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --all --check` | exit 0 |

The review follow-up changed no production code: two `core` test bodies, the test
module's imports, and one local helper. Its own four runs were

| Command | Result |
| --- | --- |
| `cargo test --locked -p muster-core -- files::` | 13 passed, 0 failed, 92 filtered out; exit 0 |
| `cargo test --locked -p muster-core` | 105 inline passed and 4 instance-lock passed, 0 failed; exit 0 |
| `cargo clippy --locked -p muster-core --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --all --check` | exit 0 |

Both strengthenings rewrite tests that already existed, so the counts do not move:
the workspace row above was not re-run, because nothing outside two `core` test
bodies changed.

`core` holds 105 inline tests (102 before, three added here) and `native` 315
default tests plus the two ignored performance checks (308 before, seven added
here). No count in `AGENTS.md` was touched: reconciling the documented numbers is
AP-11.

The in-flight bound described below changed production code inside
`native/src/document/media.rs` and no other file, so its own runs are the focused
ones and the workspace row above was not repeated for it:

| Command | Result |
| --- | --- |
| `cargo test --locked -p muster-native -- document::media picture` | 12 passed, 0 failed, 305 filtered out; exit 0 |
| `cargo fmt --all --check` | exit 0 |
| `cargo clippy --locked -p muster-native --all-targets -- -D warnings` | exit 0 |

`native` moves 314 → 315 by one test, the regression the bound added; the two
it could have broken are in the same 12.

Three halves of the change were reverted in place and the same cases rerun:

| Reverted | Failing case |
| --- | --- |
| `with_fallback` on the image element | `a_picture_that_does_not_decode_is_said_in_words` — "a picture that will not decode is said in words where it was going to be" |
| `take_wanted` not clearing `Wanted` | `a_rendered_document_draws_its_picture_and_says_what_it_cannot` — reads 2, expected 1 |
| the `MAX_RESOURCE_BYTES` check on the way in | `a_resource_is_read_whole_from_the_descriptor_or_not_at_all` — the file one byte over the bound comes back whole |

The second of those was a real defect the tests found while being written: with
the state transition left out, a name that no read ever answered for was asked
again on every frame, so the document re-read every picture it could not have,
forever. Before the read counter existed the same revert made the suite spin at
100% CPU with no output (`ps` confirmed) rather than fail, which is why the count
is in the store.

Two things do not bite, and are recorded rather than dressed up. Reverting
`read_resource` to the pre-fix shape — validate with `external_path`, then
`fs::read` the returned pathname — still passes every core case. Both refusals the
strengthened race test makes before the walk are made by the walk, which refuses a
link at every step in either shape, and the read it makes across the boundary is
`read_whole` on the handle `open_regular` returned: the two calls the fixed reader
is made of, which the pre-fix shape does not have. That is what the seam is for
rather than a hole in it. What closes the window against a concurrent actor is that
`read_resource` holds no pathname at all after `open_regular` returns, and the
strengthened case now stands on the fixed side of that line and shows the handle
still reading the file it validated, with the outside marker answering to the name
it no longer has. And no test can distinguish a `parse` that
validates a path from one that does not: both produce the same reading. The
deterministic evidence is the one above — nothing is prepared on the frame that
parsed, and the bytes appear only after a worker tick — plus the shape of the two
callbacks, which is name resolution and a store lookup and nothing else.

Limitations. The decoded pixel buffer is the toolkit decoder's: the bounds here
are the encoded size of one resource and the retained size of one document, so a
16 MiB PNG that expands enormously is bounded by what `image` does with it, not by
this slice. SVG is decided by name, because markup has no fixed first byte; the
cost is that a `.png` holding SVG text is now said in words where the old loader
rendered it. Pictures are prepared per document, so reopening the same document
reads it again — there is no cross-document cache. A document's bytes are held
for as long as its view is on screen, and released with it.

### The in-flight bound

The retained bound above was met while the in-flight one was not part of it, and
that was a real gap rather than a documentation nicety. `prepare` drained every
name the last parse asked for in one frame and started a read for each, so the
number of reads open at once was set by how many pictures a document names — and a
document is itself bounded only at `MAX_PREVIEW_BYTES`, which still leaves a file
of `![](x)` on its own at roughly 170 000 names. Nothing the retained share
counted was counting the buffers those reads were holding.

`MediaStore` now has slots as well as a budget. `Pictures` counts `in_flight`,
and `take_wanted` takes only `MAX_DOCUMENT_MEDIA_READS - in_flight` names and
leaves the rest `Wanted`; `finish` gives a slot back before it looks at what the
read brought, so **both** answers release one, and a name that is not a picture,
a file that is not there and a document with no room left all free the reads
behind them instead of holding the document up. The completion still calls
`cx.notify()`, which is the frame that takes the next wanted names; a name left
waiting costs one entry in `by_name` and nothing else. Ownership is unchanged: the
store is still the document's, still held weakly by the plugin, the nodes and the
read in flight, so a document that is closed with slots open drops its store and
every read with it.

The concrete transient bound is therefore: at most
`MAX_DOCUMENT_MEDIA_READS × MAX_RESOURCE_BYTES` = 4 × 16 MiB = **64 MiB of
encoded bytes being read at once per document**, on top of at most
`MAX_DOCUMENT_MEDIA_BYTES` = 64 MiB of encoded bytes retained — 128 MiB of
encoded bytes in the worst case, and constant in the length of the document.
That is a bound on *encoded* bytes. The decoded pixel buffers belong to the
toolkit's asset system and are outside it, as they were outside the retained
bound; nothing here claims the process holds 128 MiB, and nothing here claims
that `MAX_IMAGE_EDGE` is a memory bound — a 720 px drawing constraint is neither.
A decoded 16 MiB PNG still expands by whatever factor `image` expands it by.

The regression is at the store, where the property is decided and nothing has to
be scheduled: `a_document_reads_only_as_many_pictures_as_it_has_slots_open` names
twenty pictures (`5 × MAX_DOCUMENT_MEDIA_READS`), then drives the frames by hand
— a `take_wanted` per frame, one completion per frame, alternating between a
picture and the refusal a name that is not there gives. It asserts that a frame
never starts more reads than the slots that are free, that a document with every
slot free fills all four, that all twenty names are asked exactly once with no
duplicate, that `reads` counts them all, and that nothing is left waiting once
the last read has given its slot back. It is deterministic: no clock, no window,
no worker.

Both halves were then broken on purpose and rerun, each failing on its own
assertion:

| Broken | Failing case and what it said |
| --- | --- |
| `take_wanted` taking everything again (`room = usize::MAX`) | `a_document_reads_only_as_many_pictures_as_it_has_slots_open` — a frame started reads with slots already open |
| a refused read keeping its slot (`finish` decrementing only on `Ok`) | the same case — "a document with every slot free fills them all", left 1, right 4 |

The two properties the new bound could have broken were rerun unchanged in the
same command rather than asserted here:
`a_document_holds_a_bounded_number_of_picture_bytes` still refuses the picture
that would pass the share (it now reaches the budget through `finish`, which is
the only door into a picture), and
`a_picture_read_after_its_document_is_closed_is_dropped` still finds the store
gone, nothing drawn and nothing kept after a close with a read open.

<a id="ap-04"></a>
## AP-04: Correct image-format metadata in Git diffs

### Context and evidence

At baseline, `image_format` falls back to the filename whenever signature matching fails, even when bytes exist. Its WebP pattern also places `WEBP` at the end of the supplied slice instead of at bytes 8-11.

### Inspect first

- `core/src/navigation/changes.rs`: `image_format`, `Snapshot`, `snapshot_file`, and `file_diff_before_git`.
- `core/src/navigation/tests.rs`: the binary-change and image-change tests around baseline lines 1207-1286.
- `native/src/diff.rs`: `binary_notice` and the branches that decide whether to display it.
- The corresponding binary-notice tests in the native crate.

### Implementation steps

1. Preserve the distinction the existing `Snapshot` enum already provides: missing file versus present file with a header, including an empty header.
2. Pass that distinction to format detection using the smallest clear representation, such as an optional header. Do not infer absence from an empty byte slice.
3. When the file is present, let its bytes determine the answer. Unrecognized bytes return no image format, regardless of extension.
4. Keep extension fallback only for the deliberately supported missing-file case. Do not add a new read of historical Git objects unless later requirements demand it.
5. Match WebP as `RIFF`, the four-byte size field, then `WEBP` at the fixed offset. Do not accept a generic trailing `WEBP` suffix.
6. Keep detection header-sized, with no new decoder or dependency in `core`.
7. Preserve the UI separation: text files use text diffs; only the appropriate no-text/binary presentation uses the image notice.

### Required regression matrix

| Input | Expected metadata/presentation |
| --- | --- |
| PNG bytes in a file without an extension | PNG |
| JPEG bytes in a `.png` file | JPEG |
| Arbitrary binary bytes in `.png` | No image; generic binary notice |
| Git LFS pointer in `.png` | No image metadata; remains a text diff |
| Existing empty `.png` | No image metadata; not treated as a missing file |
| Removed file with a recognized image extension | Preserve the documented missing-file extension fallback |
| WebP header with chunk bytes after `WEBP`, no extension | WebP |
| `RIFF`-prefixed non-WebP data ending in `WEBP` | No image |
| Short/truncated signature | No panic and no false signature match |

### Acceptance

- [x] Absence and an empty present file are distinct.
- [x] Present non-images cannot be classified solely by extension.
- [x] WebP is recognized at the correct fixed offset.
- [x] Public `file_diff` regressions cover the path, not just a duplicate helper implementation.
- [x] Native notice tests cover the resulting fallback and existing added/removed/changed wording.

### Evidence

`core::navigation::changes::image_format` now takes `Option<&[u8]>` and answers in
two clearly separated steps instead of one. `None` — and only `None` — is the
missing-file case, and it is the only one that reads the name. A present file is
answered by its bytes alone, and bytes that name nothing produce `None`. The call
site carries the distinction the `Snapshot` enum already made: `file_diff_before_git`
holds `Option<Vec<u8>>`, `Snapshot::File(read)` stores `Some(read)` even when the
read is empty, and `Snapshot::Missing` leaves it `None`. An empty header therefore
stops being indistinguishable from absence, which was the whole of the bug: an
empty `.png` used to be named by its extension exactly as a removed one is.

The WebP arm is now `[b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..]`
— `RIFF`, the four-byte size field, the form at bytes 8-11 — instead of a `..`
that put `WEBP` at the end of the supplied header. Nothing else moved: the other
five signatures, the extension table, `HEADER_BYTES = 16`, and the one header read
`Snapshot::File` already performs are unchanged, and no historical Git object is
read to answer a removed file.

Two cases in `core/src/navigation::tests` exercise it through the public
`file_diff`, each with its own temp repository, and cover the matrix:

- `an_image_change_names_the_picture_rather_than_the_encoding` (extended) — a
  committed-untracked `added.png`, JPEG bytes in `picture.png`, **a PNG signature
  in an extensionless `logo`**, **a RIFF/`WEBP`/chunk header in an extensionless
  `screenshot`**, and the removed `picture.png` that keeps the name-only answer.
- `a_present_file_is_named_by_its_bytes_and_never_by_its_extension` (new) —
  arbitrary binary bytes in `not-a-picture.png`, a Git LFS pointer in `pointer.png`
  (no image metadata and a text diff that still prints the pointer), an existing
  empty `empty.png` that is diffable and therefore not treated as missing,
  `RIFF`-prefixed `junkWEBP` in `wave.riff`, and a truncated PNG signature in
  `truncated.png`.

In `native`, `diff::tests::a_file_with_no_picture_says_the_same_thing_whatever_changed`
(new) pins the fallback across `??`, `A`, `D`, `M`, `R` and `MM`: with no image the
notice is the generic binary sentence whatever the status letter was, which is what
makes the added/removed/changed wording reachable only for a picture.
`a_change_with_no_text_says_which_one_it_is` is unchanged and still pins that
wording for images.

With the pre-fix `image_format` restored temporarily (the old `..`-suffix WebP arm
and one unconditional extension fallback, reached through the same `Option` seam)
every row of the matrix fails, and each fails for its own reason:

| Row | Pre-fix result | Expected |
| --- | --- | --- |
| `screenshot` (RIFF + `WEBP` at 8-11 + chunk) | `None` | `WebP` |
| `not-a-picture.png` (arbitrary binary) | `Some(Png)` | `None` |
| `pointer.png` (Git LFS pointer text) | `Some(Png)` | `None` |
| `empty.png` (present, no bytes) | `Some(Png)` | `None` |
| `wave.riff` (`RIFF` + `junkWEBP`) | `Some(WebP)` | `None` |
| `truncated.png` (6 of 8 signature bytes) | `Some(Png)` | `None` |

The two directionally opposite WebP failures are the point of the arm: the offset
error both missed the format and invented it.

Commands, both `rustc` and `cargo` at 1.97.1 through the pinned toolchain on `PATH`:

| Command | Result |
| --- | --- |
| `cargo test --locked -p muster-core -- an_image_change_names_the_picture_rather_than_the_encoding a_present_file_is_named_by_its_bytes_and_never_by_its_extension a_binary_change_is_listed_and_counted_by_nothing` | 3 passed, 0 failed, 103 filtered out; exit 0 |
| `cargo test --locked -p muster-core navigation::` | 56 passed, 0 failed, 50 filtered out; exit 0 |
| `cargo test --locked -p muster-core` | 106 inline + 4 integration passed, 0 failed; exit 0 |
| `cargo test --locked -p muster-native diff::tests::` | 2 passed, 0 failed, 316 filtered out; exit 0 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --all --check` | exit 0 |

The full `cargo test --locked --workspace` run, the ignored performance cases and
the macOS bundle smoke are left to AP-12; the native crate was exercised here only
through its two `diff::tests::` cases, which is where this change's native half
lives. `core::files` and `native/src/document/media.rs` are untouched by AP-04 and
their AP-03 tests were not rerun for that reason.

Suggested focused checks: `navigation::tests::` image rows and `diff::tests::`.

<a id="ap-05"></a>
## AP-05: Make Markdown focus follow the visible reading

### Context and evidence

At baseline, `document_element` focuses the editor on creation even when the rendered Markdown view is displayed. The toggle then focuses the editor when `view.rendered` becomes true. The direction is inconsistent with which element is mounted.

The audit established that inconsistency, not that every keyboard shortcut is broken. Implementation must measure the actual keyboard behavior instead of preserving the old report's exaggerated symptom.

### Inspect first

- `native/src/document.rs`: `DocumentView`, initial view creation, the reading toggle, and `document_body`.
- The toolkit's focus support for `EditorState` and `TextViewState`.
- `native/src/inspector_tests.rs`: the production Markdown toggle test.
- `native/src/sidebar_tests.rs`: existing checks that late terminal creation does not take focus from a preview.

### Implementation steps

1. Define focus for the two displayed states using the toolkit's existing focus handles. Prefer the visible TextView's focus handle for Rendered if it supports the required interaction; do not create another keyboard manager.
2. On initial open, focus only the reading actually displayed.
3. On Rendered -> Source, focus the editor after the transition, without requiring a pointer click in its body.
4. On Source -> Rendered, stop focusing the hidden editor and place focus on the visible reading.
5. Preserve the retained editor and preview, their source text, selection/scroll behavior, and read-only contract. A focus fix must not reload the file or reconstruct both entities on every toggle.
6. Ensure an older asynchronous open or terminal completion does not take focus from a later user selection. Reuse existing document/navigation invalidation.
7. Keep closing the document and explicit terminal activation consistent with current focus behavior.

### Required regression checks

- Open Markdown initially rendered and assert the intended visible focus target.
- Toggle to Source and perform a meaningful keyboard operation, such as select/copy, without clicking the editor first.
- Toggle back to Rendered and perform an operation supported by that view; assert the hidden editor is not the focus target.
- Open a non-Markdown document and confirm its editor receives focus.
- Verify toggling does not enable edits or change file contents.
- Retain the existing scrolling test for both readings and the late-terminal focus regression.
- Where headless focus differs from real OS interaction, record a manual smoke check rather than declaring the entire keyboard path verified.

### Acceptance

- [x] Initial focus and both transitions target the visible reading.
- [x] Tests assert focus and actual keyboard behavior, not only element visibility.
- [x] No extra click is required to use Source after toggling.
- [x] Existing read-only, scrolling, lifecycle, and terminal-focus behavior is preserved.

### Evidence

`DocumentView::focus_visible` is the one place that says which reading holds the
keyboard: the `TextViewState` handle when `is_rendered()`, otherwise the
`EditorState` handle. Both are handles the toolkit already owns and tracks — no
keyboard manager was added. `document_element`'s new-document branch calls it
instead of focusing the editor directly, and the header's toggle calls it after
flipping `rendered`, so both transitions land on the reading that is about to be
mounted. The editor behind a rendered document is no longer focused, and a
reading that goes away no longer keeps the keyboard.

Nothing else moved: both entities are still built once in `DocumentView::new`
and retained, the file is not re-read on a toggle, and no async path gained a
focus call — the only focus in the document is in the render for a brand-new
view and in the toggle's own click, both already keyed to the current
`document_request` identity.

`app::sidebar_tests::inspector_tests::the_keyboard_follows_the_reading_that_is_on_screen`
(`native/src/inspector_tests.rs`) drives the production window through a real
`README.md`:

- It opens rendered and asserts the reading's handle holds the focus and the
  editor's does not.
- Toggling to Source and pressing `secondary-a` then `secondary-c` **without a
  click in the editor's body** selects and copies the whole source; the reading
  that went away has let the focus go.
- Toggling back asserts the reading has the focus and the now-unmounted editor
  does not, and the same two presses replace a sentinel on the clipboard with
  the reading's own text.
- A blurred control at the end presses the same two keys with no focused handle
  and leaves the sentinel in place, which is what makes the two copies above
  evidence of a focused handle rather than of a window that copies on demand.
- Neither entity id changed across the two toggles, the text is byte-identical
  afterwards, and `presentation().is_readonly()` still holds.
- Opening `note.txt` gives the focus to its editor and grows no second reading.

Commands, both `rustc` and `cargo` at 1.97.1 through the pinned toolchain on `PATH`:

| Command | Result |
| --- | --- |
| `cargo test --locked -p muster-native -- the_keyboard_follows_the_reading_that_is_on_screen` | 1 passed, 0 failed, 318 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- document` | 24 passed, 0 failed, 295 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- focus` | 8 passed, 0 failed, 311 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- keyboard` | 11 passed, 0 failed, 308 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- terminal` | 135 passed, 0 failed, 184 filtered out; exit 0 |
| `cargo fmt --all --check` | exit 0 |
| `cargo clippy --locked -p muster-native --all-targets -- -D warnings` | exit 0 |

The 24 `document`-named cases include `document::tests::a_document_taller_than_the_pane_scrolls_in_both_readings`,
the existing production toggle test, the AP-03 media cases and
`app::titlebar_tests::closing_a_document_restores_the_retained_terminal`, so
scrolling, read-only document behaviour, media ownership and the late-read
guards were all re-run. `a_new_terminal_takes_the_preview_and_a_later_preview_keeps_the_keyboard`
and `a_late_read_does_not_take_the_document_the_person_asked_for` both passed: a
terminal or read that lands after a selection does not take the keyboard.

**Reverted-check.** Restoring the old initial focus fails the first assertion
(`native/src/inspector_tests.rs:1763`, "a Markdown document opened with its
reading on screen and the keyboard somewhere else"). Restoring only the old
toggle direction, with the initial open already fixed, fails at
`native/src/inspector_tests.rs:1781` ("the source is on screen and the keyboard
is not"). Both halves of the fix carry the test.

**What is not verified here.** The headless window records which handle holds the
focus and dispatches a keystroke to it; it does not run AppKit's real key
delivery. On macOS the menu bar answers `⌘C`/`⌘V` before the key reaches the
window, so the real copy arrives as `input::Copy`/`input::Paste` and reaches the
same handlers as an action on the focused node — which is focus-dependent in the
same way, but was not exercised by these tests. The following manual smoke check
is still owed on a real window: open a Markdown file, press `⌘C` in the reading,
toggle to Source and press `⌘C` with no click in the editor, toggle back and
press `⌘C` again. Visual focus rings, macOS text-caret blink, trackpad
selection and arrow-key scrolling in the reading were not exercised.

The full workspace gate (`cargo test --locked --workspace`, workspace clippy, the
audit tests and the bundle smoke) was deferred to AP-12 as instructed; the
native crate's full test count was not re-measured against a workspace run.

<a id="ap-06"></a>
## AP-06: Query terminal selection without copying the grid

### Context and evidence

`TerminalView::snapshot_range` calls `painting_snapshot().selection_range`. That constructs `rows` through `TerminalCore::visible_rows`, including every cell, even though the caller only needs the range. The drag handler calls it during selection updates.

### Inspect first

- `native/src/terminal/view.rs`: `snapshot_range`, mouse-down, mouse-move, and selection painting.
- `native/src/terminal/session.rs`: `snapshot_inner` and its viewport-coordinate conversion.
- `native/src/terminal/core.rs`: `selection_range`, `display_offset`, and selection behavior.
- `native/src/perf.rs`: existing measurements and lifecycle helpers.

### Implementation steps

1. Introduce the smallest session-level range query that reads the existing core selection under the existing lock without constructing visible rows or copying selected text.
2. Preserve the exact viewport conversion currently performed in `snapshot_inner`: display offset, a start above the viewport, and selections ending above it all matter.
3. Share that conversion with the full snapshot path where practical. Do not create two subtly different definitions of the same selection range.
4. Replace `snapshot_range`'s full-grid call with the lightweight query, or remove the wrapper if the direct call is equally clear.
5. Keep grid snapshots for the places that actually paint the grid. Do not redesign snapshot ownership or the PTY parser.
6. Extend the existing ignored performance check to measure the new query beside `painting_snapshot`, using the same sizes and loop counts.

### Required regression checks

- No selection returns `None`.
- Visible simple and multiline selections match the corresponding full snapshot range.
- Selection partly above the viewport keeps the existing clamped start semantics.
- Selection ending above the viewport returns the existing absent result.
- Scrolling and reverse selection preserve the previous coordinate contract.
- Mouse-drag selection and copying remain correct.
- The lightweight query's implementation does not call `visible_rows`, `painting_snapshot`, or selected-text extraction.

### Acceptance

- [x] Selection-range reads no longer copy the grid.
- [x] Range results match the existing snapshot contract across the edge cases.
- [x] Existing selection, paste, and terminal lifecycle tests remain green.
- [x] Before/after measurements use the same machine, toolchain, profile, and fixture sizes.
- [x] No fixed microsecond threshold, FPS claim, or claim of the application's hottest path is introduced without appropriate evidence.

Focused checks should cover the affected terminal session/view tests and the explicit `perf::` cases. Release measurements are useful if available; do not compare them directly with a test-profile baseline as though only the implementation changed.

### Evidence

The viewport conversion that used to live inside `snapshot_inner` is now one
free function, `viewport_selection` (`native/src/terminal/session.rs`), and the
frame path calls it as `viewport_selection(&core)`. `TerminalSession::selection_range`
calls the same function with the same short lock the snapshot already takes, and
returns the range: `core.selection_range()` and `core.display_offset()`, nothing
else. `TerminalView::snapshot_range` is gone; `on_mouse_down` and `on_mouse_move`
call `self.session.selection_range()` directly. `painting_snapshot()` is untouched
and still builds every cell for the frame that draws it, and `snapshot()` still
extracts selected text. Nothing else in the drag path changed: mouse reporting,
click counting, word and line kinds, and the release-time copy are as they were.

Three retained behavioral cases in `terminal::session::tests`
(`native/src/terminal/session.rs`), all driving the production session:

- `the_selection_range_query_agrees_with_the_frame_it_feeds` — no selection is
  `None` from both callers; a simple one-row selection is
  `Some(((0, 0), (0, 4)))`; a drag from `(0, 3)` to `(2, 3)` and the same drag
  from `(2, 3)` back to `(0, 3)` both give `Some(((0, 3), (2, 3)))`, equal to
  `snapshot().selection_range` and to `painting_snapshot().selection_range`, with
  `selection_text()` still `"ha beta\ngamma delta\nepsi"` for both directions.
- `a_selection_whose_start_is_above_the_viewport_is_clamped_to_its_first_row` —
  a selection anchored at display offset 10 reads `((3, 4), (15, 8))` there; after
  `set_display_offset(0)` its first row is above the viewport and it reads
  `((0, 0), (5, 8))`, the same start the frame paints and the same value the
  snapshot reports.
- `a_selection_that_ended_above_the_viewport_is_absent` — at offset 20 a
  selection reads `((0, 2), (2, 6))`; at offset 0 both ends are above the
  viewport and all three callers return `None`.
**Historical source-pin check (removed during AP-10 minimality review).**
The fourth case, `the_selection_query_and_the_drag_path_never_build_the_grid`,
read `session.rs` and `view.rs` with `include_str!`, checked forbidden call names
in the query/conversion, and checked direct lightweight-query calls and absence
of `painting_snapshot` in both mouse handlers. It enforced source shape, not
measured performance, and no longer exists.

**Historical reverted-check, before removal of the source pin.** Restoring `selection_range` to
`self.painting_snapshot().selection_range` fails the fourth case with "selection_range
copies the grid: it names painting_snapshot". Restoring only the mouse-move call
fails it with "on_mouse_move does not ask the lightweight query".

Historical AP-06 commands (before AP-10 removed the source pin), both `rustc`
and `cargo` at 1.97.1 through the pinned toolchain on `PATH`:

| Command | Result |
| --- | --- |
| `cargo test --locked -p muster-native -- selection` | 20 passed, 0 failed, 1 ignored (a perf case), 302 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- terminal::` | 109 passed, 0 failed, 214 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- terminal` | 139 passed, 0 failed, 184 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- paste` | 10 passed, 0 failed, 313 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- paste_` | 8 passed, 0 failed, 315 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- clipboard` | 3 passed, 0 failed, 320 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- scroll` | 11 passed, 0 failed, 312 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- history` | 2 passed, 0 failed, 321 filtered out; exit 0 |
| `cargo test --locked -p muster-native perf:: -- --ignored --nocapture --test-threads=1` | 2 passed, 0 failed; exit 0 |
| `cargo fmt --all --check` | exit 0 |
| `cargo clippy --locked -p muster-native --all-targets -- -D warnings` | exit 0 |

The 109 `terminal::` cases and the 139 `terminal`-named cases are the same set
with a wider filter; they include the existing drag regressions
`terminal::core::tests::a_drag_selection_returns_the_selected_grid_text_forward_and_backward`,
`word_and_line_selection_expand_and_stay_granular_during_drag`,
`drag_anchor_stays_on_content_when_viewport_or_output_scrolls`,
`terminal::view::tests::word_line_and_shift_selection_copy_on_release`,
`the_selection_is_a_band_over_the_cell_rather_than_a_fill`,
`selection_works_under_window_chrome`, `ctrl_c_still_interrupts_even_with_a_selection`,
the paste and lifecycle cases, and the pre-existing
`lightweight_metadata_and_selection_do_not_extract_text_for_painting`.

**Measurements.** `perf::perf_snapshot_title_and_selection` now also times
`session.selection_range()`, with the same `ITERATIONS` (2 000) and the same
`SIZES` (`(80, 24)` and `(300, 100)`) as the figures it sits beside. Test
profile (`cargo test --locked -p muster-native perf:: -- --ignored --nocapture
--test-threads=1`, no `--release`), one machine, one toolchain, back to back. The
"before" row is the same binary with `selection_range` reverted to the snapshot
path; the "after" row is the final tree. Microseconds per call:

| size | query | before (old path) | after |
| --- | --- | --- | --- |
| 80x24 | `session.selection_range()` | 22.924 | 0.015 |
| 300x100 | `session.selection_range()` | 313.829 | 0.015 |

In the same runs `painting_snapshot()` measured 20.931 and 309.109 after,
against the §2 baseline of ~22.190 and ~322.949 — the same order, so the frame
path is unchanged and the old drag path was, per call, the cost of a frame.
These are per-call costs of three operations in a test build on one machine.
They are not frame latency, not release timings, not a frame rate, and no
threshold is asserted anywhere; `perf.rs` still asserts nothing.

**Not done here.** No release-profile measurement: `target/release/deps` in this
worktree holds a corrupted `libserde_derive` proc-macro dylib ("mis-aligned
LINKEDIT string pool", dated before this task), so a `--release` build of the
workspace fails to link for reasons unrelated to this change. Fixing that build
directory is not this task's work, and a release number would not be comparable
to the test-profile baseline in §2 anyway. (AP-12 later established that the
dylib was not damaged build-directory output: the cause was the release
profile's strip step, and it is fixed. No release measurement was taken here
either way.) Drag selection on a real window was
not exercised manually; the headless cases above drive the production session and
the production handlers. The workspace-wide gate (`cargo test --locked
--workspace`, workspace clippy, audit tests, bundle smoke) stays deferred to
AP-12 as instructed, so the new native test count has not been re-measured
against a workspace run.

**AP-10 minimality review.** Removed the test-only `function_body` parser and
source-string assertion case (60 lines); retained all three behavioral cases,
the scrollback fixture, existing selection/copy coverage, and the performance
measurement. The historical runs and reverted-check above describe the earlier
patch, not a test still present. Final static inspection confirms
`selection_range` takes the existing core lock and calls `viewport_selection`,
which reads only the core range and display offset; both mouse handlers call
that query directly. No grid rows or selected text are copied on that path.
This is static evidence, not an automated source pin; the before/after numbers
above remain historical measurements, not newly measured timings. AP-10 records
the final nonzero session/selection reruns.

<a id="ap-07"></a>
## AP-07: Reduce file-tree preparation work without changing its geometry

### Context and evidence

`files_inspector` limits the elements it builds with `tree_window`, but first reconstructs status data, clones collections, flattens every expanded row, and calculates the maximum width. Rendering 64 elements does not make all of that preparation constant-time.

### Inspect first

- `native/src/inspector.rs`: `TreeSource`, `flatten`, `row_width`, `tree_window`, `sticky_ancestors`, and the Files render path.
- Status application in `request_git_status`, including AP-02's integrated changes.
- `native/src/app.rs`: `measured_width`, the width cache, and per-window cache ownership.
- `native/src/inspector_tests.rs`: scrolling, subtree collapse, expansion, sticky rows, and long-name tests.

### Implementation steps

1. Establish which preparation actually executes per parent render and which data changes only on a listing/status event. Record the dataset used for any measurement.
2. Replace whole-collection copies with borrows where the data is consumed during synchronous element construction. Do not move borrowed data into callbacks requiring `'static` ownership.
3. Remove the redundant checkout filter only where lookups already use the complete `(checkout, path)` key and tests prove another checkout cannot contaminate this tree.
4. Preserve an efficient status lookup. The `statuses` map is an index, not merely redundant storage; do not delete it and replace each row lookup with a scan of all changed files.
5. If indexing remains material, build/update it with the accepted Git status rather than rebuilding it during every render. Keep its invalidation with that status owner, including failure, removal, refresh, and checkout changes.
6. Preserve the measured-width cache's fixed-font assumptions. Do not add a global cache, preference system, or eviction framework to solve an unmeasured future workload.
7. After these small changes, measure again. A retained flattened-tree cache is optional only if remaining measured work justifies the extra invalidation surface.
8. If such a cache is needed, enumerate every invalidation event before implementation: listing completion, expand/collapse, loading/error transition, status update, checkout/root change, and removal. Do not retain stale rows to make a benchmark faster.
9. Extract a preparation function only if it isolates a real decision or traversal and improves testability. Do not split declarative element-building solely to hit a line-count target.

### Required regression checks

- Opening a nested directory preserves other expansions.
- Collapsing forgets the complete subtree but not a sibling such as `src-tauri`.
- Loading and retry/error notes remain in the correct position.
- Status changes, including clearing status and switching checkouts, immediately reach the correct rows.
- Sticky folders retain their own hitboxes and scroll targets.
- Every row remains fixed-height and `flex_none`.
- Files still scroll horizontally for long names; Changes retains its current panel-width/truncation behavior.
- If a new retained index/cache is introduced, tests cover its update and invalidation rather than only its initial population.

### Measurement and acceptance

Use a reproducible in-memory or temporary fixture with many expanded rows and repeat renders without data changes. Prefer the existing test/performance infrastructure. Record preparation cost separately from layout/paint and from constructing the fixture.

- [x] Unnecessary copies of the expansion/loading/error collections are removed where borrowing is sufficient.
- [x] Status lookup remains indexed rather than becoming an O(rows x changed-files) scan.
- [x] Any added cache has one clear owner and tested invalidation.
- [x] Existing tree geometry and interaction regressions pass.
- [x] The report distinguishes measured gains from static reductions in allocations/work.

### Follow-up correction evidence (not full AP-07 acceptance)

Against `69ae8baf55e31923a5b7f00488f9f43ec7980993` plus the existing uncommitted
AP-01–07 patch, restored `flatten`'s collapsed-folder guard before consulting
cached children, loading or errors. Membership pruning in `refresh_diff_totals`
now retains Git status and its index under the same checkout-membership predicate,
along with error/request state. Pruning precedes the running-sweep early return;
archived/missing checkouts still in navigation keep their existing status policy.
The existing multiline retain was for `diff_totals`; no production Git-status
remove/retain/clear existed in the inspected patch.

Added `collapsed_folders_hide_cached_children_loading_and_errors` and
`membership_removal_prunes_status_and_index_even_during_a_sweep` to the inspector
suite. With both rustc and Cargo 1.97.1 pinned through `PATH`:

- `cargo test --locked -p muster-native -- inspector_tests::`: 42 passed.
- `cargo test --locked -p muster-native -- removal`: 2 passed.
- `cargo fmt --all --check`: exit 0.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: exit 0.

That correction alone did not record benchmark results or complete AP-07. The
validation below completes AP-07; AP-08 remains unimplemented.

### Final validation and handoff

**Task:** AP-07. **Result:** implemented and verified in the existing patch;
this completion changes only AP-07 documentation and its register entry.
**Source commit and dependency commits:** `69ae8baf55e31923a5b7f00488f9f43ec7980993`
plus the uncommitted AP-01–07 implementation, including AP-01/AP-02 dependencies
and the collapsed-folder/membership-pruning correction above. There are no new
dependency commits. **Patch reference:** the current unstaged working-tree patch;
no staging or commit was performed.

**Observed cause and correctness review:** every Files parent render rebuilt the
status index, copied expansion/loading/error collections, cloned each flattened
entry and allocated owned width-cache lookup keys even on hits. The current
`files_inspector` calls production `prepare_tree` with borrowed window collections;
`TreeRow` borrows entries and notes. Full `(checkout, path)` lookups preserve
checkout isolation without a filtered error-map copy. Only visible element
callbacks acquire the owned data they need. Preparation still traverses all
expanded rows and computes their maximum width; it is not constant-time and
there is no retained flattened-tree cache.

`TerminalWindow` owns `status_index`, built by `accept_git_status` alongside the
accepted status, not during rendering. Lookup remains hashed by path. Refresh
failure preserves the last accepted status and index; successful replacement
removes paths absent from the new answer. Request tokens reject stale results.
Membership pruning removes status, index, errors and requests together before the
running-sweep early return. Checkout switching selects that checkout's index.
The existing per-window width cache is now keyed first by font-size bits, then
by text, so hits borrow `&str` without allocating a key; its fixed-font contract
is unchanged. No global cache or additional invalidation framework was added.

**Implementation files already changed by AP-07:** `native/src/inspector.rs`
(borrowed rows, preparation extraction and accepted-status index),
`native/src/app.rs` (per-window index/width-cache ownership and membership
pruning), `native/src/inspector_tests.rs` (production-path regressions), and
`native/src/perf.rs` (existing ignored preparation fixture). This validation
retains no additional Rust changes or baseline implementation.

**Regression checks added/extended in the existing patch:** these exact names
are under `app::sidebar_tests::inspector_tests::`:

- `a_second_status_read_replaces_what_the_rows_wear`
- `a_failure_in_another_checkout_is_not_a_note_in_this_tree`
- `a_status_read_for_one_checkout_does_not_reach_another_checkouts_rows`
- `collapsed_folders_hide_cached_children_loading_and_errors`
- `membership_removal_prunes_status_and_index_even_during_a_sweep`

The 42-test inspector run also passes the existing nested-expansion, complete
subtree-collapse, loading/retry, sticky-hitbox/scroll, fixed-height, horizontal
Files scrolling and truncated Changes-row coverage, including
`opening_a_nested_folder_keeps_the_folders_above_it_open`,
`collapsing_a_folder_forgets_everything_under_it`,
`a_folder_that_is_still_loading_says_so_in_place`,
`retry_after_a_failed_read_reads_the_file_again`,
`a_stacked_folder_keeps_its_own_click`,
`the_folder_stack_appears_over_a_scrolled_tree`,
`rows_keep_the_height_the_tree_scrolls_by`,
`a_name_too_long_for_the_panel_is_scrolled_to_not_cut`, and
`a_change_row_fits_the_panel_and_a_long_name_gives_up_its_room`.

**Measurements:** both tools were pinned by prepending
`dirname "$(rustup which --toolchain 1.97.1 rustc)"` to this shell's `PATH`:
`rustc 1.97.1 (8bab26f4f 2026-07-14)` and
`cargo 1.97.1 (c980f4866 2026-06-30)`. The existing ignored
`perf::perf_files_tree_preparation` runs 2,000 preparations per case in the
optimized test profile. Fixtures are in memory, all folders expanded, with two
loading and two failed folders substituting notes for their children. The small
case has 10 folders × 10 files, 74 flattened rows and 40 changed-file records;
the large case has 40 × 24, 908 rows and 320 changed-file records. Widths are
warmed once before timing; construction, layout, paint and element construction
are excluded from preparation timing.

| Fixture | Current construction (once) | Current preparation total / each | Original preparation total / each |
| --- | --- | --- | --- |
| 74 rows | 74.083 µs | 25.788 ms / 12.894 µs | 57.981 ms / 28.990 µs |
| 908 rows | 237.791 µs | 264.279 ms / 132.139 µs | 489.463 ms / 244.731 µs |

The comparison was feasible without replacing working production files. A
temporary test-only module in `perf.rs` extracted the actual `Ancestor`, owned
`TreeRow`, `TreeSource`, `flatten`, `row_width`, `file_marker`, text-size constants
and original `app::measured_width` directly from `git show 69ae8ba`. Its preparation
body was extracted from the original `files_inspector`, from status-map creation
through the maximum-width fold. Adaptations only supplied the same fixture's
collections, accepted Git status, window and old width-cache type in place of
`self`; the unrelated selected-file lookup was omitted, and the expansion copy
was kept observable with `black_box`. It used the same fonts/theme/window setup,
fixtures, row-count assertions, warmed widths and 2,000-iteration timer. It was
not a hand-written replacement algorithm. The temporary module was removed by
byte-for-byte restoration of `perf.rs` before final checks.

These single-run observations show lower preparation cost on these fixtures,
not a portable speedup, release timing or end-to-end frame-latency claim. Static
removal of collection/entry copies, render-time index construction and allocated
width-hit keys is independently established by code review; allocation counts
were not measured. Remaining row-vector, folder-chain, folder-key and formatted
count work still exists. No additional tree cache is justified by this scope.

**Commands, counts and exits:** all used the pinned `PATH` above.

| Command | Actual result |
| --- | --- |
| `cargo test --locked -p muster-native perf::perf_files_tree_preparation -- --ignored --exact --nocapture` | 1 passed, 328 filtered out; exit 0; current measurements above |
| `cargo test --locked -p muster-native perf::ap07_original::original_files_tree_preparation -- --ignored --exact --nocapture` (temporary) | 1 passed, 329 filtered out; exit 0; original measurements above |
| `cargo test --locked -p muster-native -- inspector_tests::` (after restoration) | 42 passed, 287 filtered out; exit 0 |
| `cargo test --locked -p muster-native -- removal` (after restoration) | 2 passed, 327 filtered out; exit 0 |
| `cargo fmt --all --check` (after restoration) | exit 0 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` (after restoration) | exit 0 |

The two removal matches are
`app::sidebar_tests::inspector_tests::membership_removal_prunes_status_and_index_even_during_a_sweep`
and `app::sidebar_tests::archive_does_not_wait_for_a_git_removal_preview` (the
former overlaps the 42-test run, so these are not 44 distinct tests). Two initial
temporary-baseline compile attempts exited 101 while adapting the GPUI window
handle setup; they executed no tests and are not counted as verification. The
successful temporary module emitted unused-field warnings for original row data
not drawn by the fixture; no such module remains. Cargo also reports the existing
future-incompatibility notice for `block v0.1.6`.

**Checks not run and remaining limitations:** no release build/performance run
was attempted because `target/release` has the reported pre-existing corrupted
proc-macro artifacts; this task neither repairs nor relies on that target. Full
workspace tests and AP-12 integration/platform gates were not rerun for this
validation-only completion; focused inspector/removal tests and workspace Clippy
were run instead. No manual UI checks were performed; geometry/interaction
claims here are automated headless regressions, not manual observations.
AP-08 onward, AP-01–06 evidence, `NOTAS.md` and the rest of the user's patch are
unchanged.

<a id="ap-08"></a>
## AP-08: Keep inspector measurement and typography in agreement

### Context and evidence

`ROW_TEXT_PX` and `STATUS_TEXT_PX` currently control measurement while matching numeric literals control drawing. The values agree today; this is a small maintenance risk, not a demonstrated visual defect.

### Inspect first

- `native/src/inspector.rs`: `ROW_TEXT_PX`, `STATUS_TEXT_PX`, `row_width`, `status_chip`, `count_chips`, and the panel's inherited text size.
- Existing inspector geometry tests.

### Implementation steps

1. Use the existing named sizes in the drawing sites whose text `row_width` measures.
2. Preserve the actual sizes, columns, spacing, and colors. This task changes their source of truth, not their appearance.
3. Do not propagate inspector constants to titlebar/sidebar components merely because those components currently use the same number.
4. If duplicated geometry literals must be named to keep measurement and drawing together, do so locally and only for the actual shared geometry. Do not turn this into a global design-token migration.
5. Keep Files and Changes as separate current layout rules. The earlier audit's claim that one width helper still served both layouts is obsolete at the baseline.

### Acceptance

- [x] Measured text and its drawing read the same size constants.
- [x] No intentional font size, spacing, color, or interaction changes.
- [x] Existing long-name, fixed-height, status-column, and counts tests pass.
- [x] No unrelated chrome-wide abstraction or reformatting appears in the diff.

### Evidence

Implemented on HEAD `69ae8ba`, retaining the reviewed, uncommitted AP-01–07
prerequisite patch (AP-07 is the direct dependency; no separate dependency
commits). Only three drawing expressions in `native/src/inspector.rs` changed:
`status_chip` uses `STATUS_TEXT_PX`, while `count_chips` and the panel's inherited
text size use `ROW_TEXT_PX`, already used by `row_width`. Values remain 10px and
13px respectively. Tab typography, geometry, colors, interactions and the separate
Files/Changes layout rules are untouched. No new abstraction or tests were needed
for literal-to-existing-constant substitutions; existing production UI regressions
cover geometry, status/count identity and refresh, and long names in both lists.

Verification used this shell-local setup (no persistent configuration changes):

```bash
set -eu
TOOLCHAIN_BIN="$(dirname "$(rustup which --toolchain 1.97.1 rustc)")"
export PATH="$TOOLCHAIN_BIN:$PATH"
rustc --version
cargo --version
cargo test --locked -p muster-native -- inspector_tests::rows_keep_the_height_the_tree_scrolls_by inspector_tests::rows_agreeing_about_git_keep_distinct_identities inspector_tests::a_second_status_read_replaces_what_the_rows_wear inspector_tests::the_file_and_change_lists_line_up_on_legacy_columns inspector_tests::a_name_too_long_for_the_panel_is_scrolled_to_not_cut inspector_tests::a_change_row_fits_the_panel_and_a_long_name_gives_up_its_room
cargo fmt --all --check
git diff --check
```

Both compiler and Cargo reported 1.97.1. All six named native library tests passed:
0 failed, 0 ignored, 323 filtered out; exit 0. The binary and doc-test targets each
ran zero tests and are not counted as verification. Formatting and diff checks
also exited 0. Cargo reported the existing `block v0.1.6` future-incompatibility
warning. No manual checks or measurements were performed; unchanged appearance is
supported by identical constant values and the focused headless checks, not a
manual visual comparison. No workspace tests, Clippy, performance or platform gates
were run: broad integration remains AP-12. No new behavioral regression is claimed.
Only AP-08's register, acceptance and evidence are updated here; AP-09 is not
started. The prior patch, `NOTAS.md`, other user changes and the plan's untracked
status are preserved. No staging or commit was performed.

<a id="ap-09"></a>
## AP-09: Remove the obsolete `FilePreview::truncated` field

### Context and evidence

`core/src/files.rs` documents and returns `truncated: false` for every preview. Oversized files are errors, and the current native UI does not use the field to decide behavior. Keeping a field only for older readers conflicts with this repository's pre-1.0 policy.

### Implementation steps

1. Search current production consumers, fixtures, and tests again; do not assume the baseline's usage count still holds after AP-03/AP-05.
2. Remove the field, constructor assignments, obsolete compatibility comment, and tests that only assert its constant value.
3. Preserve tests of complete text, oversized-file refusal, growing-file refusal, UTF-8 refusal, binary refusal, and empty files.
4. If a current consumer now needs to distinguish partial content, resolve the conflict against the full-file-or-error contract instead of restoring truncation.
5. Do not change the navigation JSON schema, add Serde defaults, write a migration, or bump a persistence schema for this runtime-only type cleanup.

### Acceptance

- [x] No production or test consumer refers to the removed field.
- [x] Previews remain complete-or-error, not silently partial.
- [x] Core file tests and native document tests pass.
- [x] No persistent-data compatibility code is introduced.

### Evidence

AP-09 only, on HEAD `69ae8ba`, with AP-01 through AP-08 present as an
uncommitted dependency patch. Searched current Rust definitions, constructors and
consumers before removal: the runtime field was always false and no production
consumer read it. Removed its compatibility comment, field, constructor assignment
and constant-only assertion from `core/src/files.rs`; removed the four fixture
assignments in `native/src/titlebar_tests.rs` and `native/src/sidebar_tests.rs`.
Legacy's separate type is frozen and unchanged. No schema, Serde default,
compatibility reader or migration was introduced.

No new regression logic is needed for this type deletion. Existing complete text,
empty file, exact-limit, oversized, binary, invalid UTF-8, growing-file and
filesystem-boundary tests remain intact apart from the constant-only assertion.
Post-edit searches found no runtime field consumer or assignment remaining.

Commands used the following shell setup (no persistent configuration change):

```bash
set -euo pipefail
TOOLCHAIN=1.97.1
TOOLCHAIN_BIN="$(dirname "$(rustup which --toolchain "$TOOLCHAIN" rustc)")"
export PATH="$TOOLCHAIN_BIN:$PATH"
rustc --version
cargo --version
cargo test --locked -p muster-core files::
cargo test --locked -p muster-native document
cargo fmt --all --check
git diff --check
```

Both reported 1.97.1. All commands exited 0: core file tests **13 passed**
(93 filtered); native document-matching tests **24 passed** (305 filtered),
including all three changed titlebar fixtures. An additional command with the
same pinned setup,
`cargo test --locked -p muster-native a_new_terminal_takes_the_preview_and_a_later_preview_keeps_the_keyboard`,
ran **1 passed** (328 filtered), exercising the changed sidebar fixture.
The core integration binary and native main binary matched zero cases; they are
not claimed as coverage. Formatting and diff whitespace checks passed. Native
Cargo reported the existing `block v0.1.6` future-incompatibility warning.

No manual checks or measurements were performed. Workspace tests, Clippy,
performance and platform acceptance were intentionally not run: final broad gates
belong to AP-12. AP-10 is not started. Patch reference: uncommitted working tree;
no staging or commit. Prior AP-01 through AP-08 changes, `NOTAS.md`, other user
changes and this plan's untracked status are preserved. Only AP-09 status and
evidence are changed in this document.

<a id="ap-10"></a>
## AP-10: Correct source explanations and resolve readability findings narrowly

### Inspect and correct

1. `native/src/lib.rs`, the `terminal_parser` module comment: remove the obsolete claim that the shipping application does not drive the parser. Explain that these are direct parser-level security regressions independent of PTY/window timing.
2. `native/src/terminal/core.rs`, `CursorSnapshot` documentation: describe the clamping that the implementation actually performs. Do not add a column clamp unless a separate, concrete cursor bug is demonstrated.
3. `native/src/inspector.rs`, `status_chip`: remove the leftover line claiming the function also draws counts.
4. `native/src/inspector.rs`, the neighboring `scroll`/`scrolled` comments: retain one accurate explanation of the previous-frame offset/layout observation.
5. Reconcile comments changed by AP-01 and AP-03. No explanation may claim an entity handle cannot satisfy the callback or that returning a pathname preserves descriptor identity for a later read.
6. Check `files_inspector` after AP-07. Extract additional logic only if it separates an actual decision or improves a runnable test. Long declarative rendering alone is not a reason.

### Explicit architecture dispositions

- **Large `TerminalWindow`: accepted for now.** Field count alone is not an implementation defect. Do not extract `FileTree`, `GitState`, or `DiffState` as a ceremonial response to the first audit.
- **Requested document, loaded preview, and retained widget state: accepted as distinct phases.** Preserve central invalidation and tokens. Do not call them conflicting sources of truth without demonstrating a broken transition.
- **Theme and grammar installation: accepted coupling for this pass.** A small private helper inside the existing module is allowed if it meaningfully separates initialization, but a new module is not required.
- **Direct parser tests: protect.** Their separate harness is deliberate security coverage; keep it.
- **`required_selection`: protect.** It distinguishes a missing field from an explicit null, and an existing persistence regression depends on that behavior.

### Acceptance

- [x] The identified false/repeated comments are corrected.
- [x] Comments explain current guarantees and limitations rather than an implementation that no longer exists.
- [x] Parser regression tests and required-field persistence behavior remain intact.
- [x] Any readability extraction has a stated benefit beyond reducing line count (none needed).
- [x] The handoff explicitly records the accepted-no-change architecture items.

For comment-only changes, `git diff --check` and the applicable formatting check are sufficient; rerun affected tests if any code was moved or changed.

### Evidence and handoff

- **Task / result:** AP-10 implemented and verified. AP-11 not started.
- **Source / dependencies:** HEAD `69ae8ba`; AP-01 through AP-09 are the
  existing uncommitted patch, inspected and preserved except for the explicitly
  requested AP-06 test-infrastructure removal below. No dependency commits created.
- **Cause / files:** `native/src/lib.rs` falsely said the shipping app did not
  drive the parser; its comment now describes direct security regressions independent
  of PTY/window timing. `native/src/terminal/core.rs` now documents row-only
  viewport clamping and the unchanged parser column, without changing cursor code.
  `native/src/inspector.rs` drops the stale counts claim and replaces duplicated
  scroll commentary with one previous-frame offset/bounds explanation, without
  asserting that a frame of lag cannot be seen.
- **AP-01/AP-03 reconciliation:** existing `document.rs`/`app.rs` comments correctly
  describe weak-handle direct link dispatch; `document/media.rs` describes worker
  reads through the validated descriptor and rendering returned bytes, not reopening
  a validated pathname. No further changes needed there.
- **Readability / accepted no change:** AP-07 already separates tree preparation
  (`prepare_tree`) from declarative drawing in `files_inspector`; no additional
  extraction improves a decision or runnable test. The large `TerminalWindow`,
  distinct requested-document/loaded-preview/retained-widget phases with central
  invalidation and tokens, and theme/grammar installation coupling remain accepted.
  Direct parser security tests and `required_selection`'s absent-versus-null
  persistence distinction are expressly retained, unchanged.
- **Minimality review:** removed only AP-06's 60-line bespoke `function_body`
  source parser and source-shape test from `native/src/terminal/session.rs`.
  String/indentation matching was brittle new infrastructure, unnecessary alongside
  production behavioral regressions, static call-path inspection, and the existing
  measurements. No runtime code changed. AP-06 evidence above preserves the
  historical test/revert record and identifies the final absence of a source pin.
- **Regression checks:** none added; three AP-06 behavioral cases and existing
  drag/copy, session lifetime, parser security and persistence checks retained.

All commands below exited **0**, with both tools pinned on the shell's `PATH`:
`rustc 1.97.1 (8bab26f4f 2026-07-14)` and
`cargo 1.97.1 (c980f4866 2026-06-30)`.

| Command | Exact library-test result |
| --- | --- |
| `cargo test --locked -p muster-native -- terminal::session::tests::` | 20 passed, 0 failed, 0 ignored, 308 filtered out |
| `cargo test --locked -p muster-native -- selection` | 19 passed, 0 failed, 1 ignored performance case, 308 filtered out |
| `cargo test --locked -p muster-native -- terminal_parser::` | 4 passed, 0 failed, 0 ignored, 324 filtered out |
| `cargo test --locked -p muster-core -- schema_requires_explicit_selection_and_refuses_unknown_fields` | 1 passed, 0 failed, 0 ignored, 105 filtered out |
| `cargo fmt --all --check` | passed |
| `git diff --check` | passed |

The ancillary binary/doc targets matched zero tests; they are not counted as
verification. Cargo reported the existing future-incompatibility warning for
`block v0.1.6`; no dependency changes made.

- **Manual checks / measurements:** static inspection only; no GUI exercise or
  new timings. Existing AP-06 measurements retained with their original caveats.
- **Not run / limitations:** workspace tests, clippy, audit, bundle/platform and
  full lifecycle acceptance are deferred to AP-12; no production code changed
  here to justify those broad gates. No new automated source-shape enforcement.
- **Patch reference:** unstaged working-tree delta over `69ae8ba`; no staging or
  commit. `NOTAS.md`, other user files and the plan's untracked status preserved.

<a id="ap-11"></a>
## AP-11: Reconcile current documentation, measurements, and packaging claims

### Latest NOTAS integration verification (native v0.1.0)

This supersedes earlier suite counts, not historical results below. With Rust and
Cargo 1.97.1, `cargo test --locked --workspace` passed **476 native, 225 core and
4 instance-lock tests**; 3 native performance measurements remain ignored.
`cargo clippy --locked --workspace --all-targets -- -D warnings`,
`cargo fmt --all --check` and `git diff --check` passed. The release/audit script
suite contains 23 tests; the live dependency audit has no blocking advisories after
updating `rustls` to 0.23.46. OpenCode protocol tests use a mock local service;
a real OpenCode session and a real external Prettier installation are not manual
acceptance evidence. The OpenCode integration's own rules — an unreadable service
protecting a worktree, a queue offered only on a freshly read idle session, one
observation answering the whole queue, a follow pass that is single-flight — are
pinned by the tests in `core/src/opencode/`, `native/src/agent_watch.rs`,
`native/src/follow.rs` and `native/src/sidebar_tests.rs`; none of them was
exercised against a real OpenCode service. Linux runtime and graphical/platform
acceptance remain open.
`scripts/release.sh release` passed and produced `dist/Muster.app` (0.1.0), including
local plist and ad-hoc signature verification. A clean optimized build needs
`strip = "none"`: the pinned compiler's debuginfo-stripping pass otherwise produces
misaligned Mach-O proc-macro libraries. This is not a cache-corruption workaround.
The bundle was not launched, distribution-signed or notarized.
Review parity regressions cover the two-row toolbar at 600 px, destination
selection without dispatch, range/text preservation, saved code capture, sticky
drift during prose edits, Escape cancellation, and Settings/export folder agreement
for a registered worktree. The same release script passed after these fixes.

### Inspect first

- `docs/native-rust-migration/README.md`, especially the dated current-implementation summary.
- `docs/native-rust-migration/opening-reading-and-scrolling.md`.
- `docs/native-rust-migration/typography-and-highlighting.md`.
- `docs/native-rust-migration/dark-colors.md` where capture/grammar claims are affected.
- Applicable test-count and invariant statements in `AGENTS.md`.
- `scripts/bundle-macos.sh`, especially its ad-hoc signing comment.
- Completion evidence from AP-01 through AP-10.

### Implementation steps

1. Separate historical pass records from statements of current behavior. Do not rewrite history as though the old implementation had never existed; label it and provide an unambiguous current summary.
2. Update link dispatch/error presentation, safe local media ownership, focus behavior, image metadata, and checkout selection to match the completed code and tests.
3. Correct stale statements that Changes always scrolls sideways if its current contract is panel-width rows with shortened names. Preserve Files' actual horizontal-scroll behavior.
4. Resolve the grammar inventory with an explicit counting rule. Languages selected by `syntax_language`, registry entries, Cargo features, grammar packages, and direct dependencies are not interchangeable counts.
5. Do not double-count a package merely because it becomes a direct dependency. At the baseline the path selector returns 35 distinct non-plaintext language names; recount the current implementation rather than copying that number forward.
6. Describe the actual strength of grammar checks: path mapping, capture normalization, standalone highlighter output, per-block Vue assertions, and rendered UI checks are different kinds of evidence.
7. Update any active test counts from the final suite, or replace unnecessary repeated counts with a reference to the canonical location. Mark historical counts as historical.
8. State that the bundle script assembles and verifies an ad-hoc-signed local bundle. Correct comments implying that this establishes Gatekeeper trust, Developer ID identity, or notarization.
9. Preserve the documented upstream limitation on injected-language highlighting in diffs. Do not claim it was fixed by the document-view changes.
10. Record only measurements actually run, with profile, sizes, toolchain, and scope. Snapshot timing does not establish graphical frame latency.
11. Keep changes within these audit-related statements. Do not turn this assignment into a rewrite of all migration research.

### Acceptance

- [x] Current behavior and historical notes are distinguishable.
- [x] Every claim about a fixed behavior names an appropriate test or explicitly identifies manual/static evidence.
- [x] Inventory counts have a reproducible definition and no dependency double-counting.
- [x] No stale queue, pathname-safety, focus, or layout promises remain in the affected current summaries.
- [x] Signing claims distinguish local signature validity from distribution trust.
- [x] Links and code references point to existing files/symbols.

### Evidence and required handoff

- **Task / result:** AP-11 implemented. AP-12 remains not started; its final gates
  belong to the next integration owner, not this documentation pass.
- **Source / dependencies:** HEAD `69ae8ba`, existing uncommitted AP-01–AP-10
  patch reviewed; no dependency commits or new commit.
- **Observed cause:** dated summaries and successive pass narratives mixed old
  queue/media/layout contracts and repeated obsolete counts with current claims;
  grammar/package accounting conflated different inventories, and the bundle
  comment incorrectly equated ad-hoc integrity with Gatekeeper trust.
- **Files / purpose:** `README.md` separates its dated snapshot from the current
  audit patch; `opening-reading-and-scrolling.md` gives the current link/media/
  focus/metadata/checkout/layout contract with regression references and encoded
  versus decoded-memory caveats. `typography-and-highlighting.md` defines the
  35 distinct non-plaintext selector outputs, separates mapping/capture/standalone
  style/per-block Vue/headless evidence, corrects layout and labels historical
  security results. `dark-colors.md` links those evidence limits. `AGENTS.md`
  references this canonical run instead of repeating counts and pins both tools.
  `scripts/bundle-macos.sh` changes only the signing comment: local ad-hoc
  validity is neither Developer ID nor notarization nor Gatekeeper trust.
- **Regression checks added:** none; documentation and a shell comment only.

Current workspace run (test profile, optimized + debuginfo), with the section 3
PATH setup selecting both `rustc 1.97.1 (8bab26f4f 2026-07-14)` and
`cargo 1.97.1 (c980f4866 2026-06-30)`:

| Command/check | Result |
| --- | --- |
| `cargo test --locked --workspace` | exit 0; core inline **106 passed**, core integration **4 passed**, native **325 passed, 3 ignored**, no failures or filtered tests. Binary and both doc targets: 0 tests, not counted as coverage. |
| `bash -n scripts/bundle-macos.sh` | exit 0; syntax only, not bundle assembly/signing verification |
| `git diff --check` | exit 0 |
| Ad-hoc Python static selector extraction | Printed **35** distinct non-plaintext RHS/return strings in `syntax_language`, agreeing with the typography inventory; combined initial link-check script exited 1 as explained below |
| Ad-hoc Python local Markdown link-file check for the four edited migration docs | exit 0 after excluding inline-code examples; initial naive check exited 1 on the literal `![](clip.mp4)` example, not a documentation link |

The three ignored native cases are measurements in `native/src/perf.rs`; they
were not executed. Cargo reported the existing `block v0.1.6`
future-incompatibility warning. Earlier AP counts remain historical observations;
this run supersedes active repeated suite totals, not earlier measurements.

- **Manual checks / measurements:** static source/test and link-target inspection
  only; no GUI exercise, new timings, image pixel observation or memory measure.
  Prior AP-06/AP-07 measurements retain their original fixture/profile/toolchain
  caveats; snapshot timing is not graphical frame latency.
- **Not run / exact reason:** clippy, fmt, auditor unit tests/live lookup, ignored
  performance cases, release measurements, actual bundle smoke, Linux and manual
  lifecycle/rendering/opener checks are deferred to AP-12: no runtime logic changed
  and this assignment is documentation only. A passing macOS workspace suite does
  not establish Linux, release readiness, decoded-memory bounds or manual parity.
- **Patch reference:** unstaged AP-11 changes over `69ae8ba`; no stage/commit.
  User `NOTAS.md`, the untracked plan and prior AP patches preserved.

<a id="ap-12"></a>
## AP-12: Integration, lifecycle, and platform acceptance

This task validates the combined result. It is not permission to repair unrelated failures or begin deferred architecture work.

### Automated gates

With the pinned compiler setup from section 3:

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
node --test scripts/audit-rust.test.mjs
cargo test --locked -p muster-native perf:: -- --ignored --nocapture --test-threads=1
```

Run release performance measurements when available using the existing `scripts/perf-native.sh` entry point or its documented Cargo command. Keep test-profile and release results separate. Do not make timing thresholds machine-independent test assertions.

Also retain the existing CI dependency-audit gate. Passing the auditor's unit tests is not evidence of a current vulnerability lookup; report the real audit separately if it is run. Do not change dependency versions just to make this plan look more comprehensive.

### End-to-end behavior checklist

The checked items below mean existing production-path automated regressions passed
in the combined macOS workspace suite, not a real-app visual/manual observation.
AP-01–AP-11 evidence above supplies the detailed cases and limitations.

- [x] A rejected rendered link displays a dismissible error without a real browser being required by the test suite.
- [x] The same error remains visible with the inspector hidden or Changes selected.
- [x] Binary-only, empty-file, mode-only, and committed branch changes select Changes correctly.
- [x] Unknown/failed status is not presented as clean.
- [x] Late status/document/media/terminal results cannot override a newer surface selection or focus target.
- [x] Markdown opens with the correct focus; both toggle directions support keyboard use without another body click.
- [x] Source remains read-only, and both readings retain usable scrolling (automated coverage only).
- [x] Local images use the safe prepared resource; replacement symlinks cannot redirect the later consumer.
- [x] Image success, decode failure, refusal, and missing-resource outcomes remain meaningful (headless/resource checks, not observed pixels).
- [x] Drag selection, scrollback selection, and copying preserve their existing coordinate/text semantics.
- [x] Tree expansion/collapse, status refresh, retries, sticky-folder interaction, fixed row height, and long-name layout remain correct.
- [x] State persistence and terminal/process cleanup regressions still pass.

### Platform and manual checks

1. **macOS:** build the actual native binary when feasible, assemble it with the existing bundle script, validate its plist and signature, and manually exercise the affected readings and keyboard transitions. A bundle assembled with a disposable CLI is a structural smoke test, not proof the native app launches.
2. **Linux:** use the existing Linux CI job or an available Linux environment for the build and core POSIX tests. Record which new native behavior tests ran there. A successful macOS suite is not a Linux result.
3. **External application opening:** perform a real opener smoke only as an intentional manual action in an appropriate environment, not as a surprise automated test side effect.
4. **Actual rendering:** record whether image pixels, font/layout behavior, and keyboard interaction were observed in the real app or only via headless checks.
5. **Unavailable environments:** identify the exact checks not run and why. Do not mark platform-specific evidence as successful based on another platform, and do not claim release readiness while required release checks remain outstanding.

### Final acceptance

- [x] AP-01 through AP-11 have implementation/verification evidence or a reviewed already-satisfied disposition.
- [x] All five required automated gates pass; the separate release failure is identified below.
- [x] Added regression tests target the production paths and exercise the relevant failure cases.
- [x] No personal state, live user terminal, external browser, or public network dependency was used inadvertently by tests.
- [x] No unapproved migration, compatibility path, dependency, generated-file edit, or vendor change was introduced.
- [x] AP-12 changes only this acceptance record; existing AP patches and user files remain untouched.
- [x] The final report lists completed tasks, actual test counts, measurements, manual observations, and remaining platform limitations without overstating coverage.
- [ ] Release performance, Linux acceptance, and real-app manual rendering/lifecycle/keyboard checks completed.

### AP-12 evidence and handoff

- **Task/result:** AP-12 automated acceptance complete; platform/manual acceptance
  partially blocked or not run, not release-ready acceptance.
- **Source/dependencies:** HEAD `69ae8baf55e31923a5b7f00488f9f43ec7980993`,
  existing uncommitted AP-01–AP-11 patch; no new commit or staging.
- **Scope:** only this untracked plan was edited by AP-12. `NOTAS.md`, all prior
  tracked/untracked user work, dependencies, generated files and vendor tree were
  preserved. No implementation failure required a fix; no new regression added.
- **Toolchain:** section 3 PATH setup confirmed `rustc 1.97.1 (8bab26f4f 2026-07-14)`
  AND `cargo 1.97.1 (c980f4866 2026-06-30)` before the gates. Apple M1 Pro,
  32 GiB, Darwin 27.0.0 arm64 (machine details printed by the perf entry point).

Commands were run separately without output pipelines, retaining actual exits:

| Command | Observed result |
| --- | --- |
| `cargo fmt --all --check` | exit 0 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo test --locked --workspace` | exit 0; **106 core inline + 4 core integration + 325 native passed**, **3 native ignored**, 0 failed/filtered; binary and two doc targets had 0 tests, not coverage |
| `node --test scripts/audit-rust.test.mjs` | exit 0; **18 passed**, 0 failed/skipped |
| `cargo test --locked -p muster-native perf:: -- --ignored --nocapture --test-threads=1` | exit 0; **3 passed**, 325 native filtered; binary target 0 tests |
| `cargo build --locked -p muster-native` | exit 0; actual native dev binary, optimized + debuginfo |
| `bash scripts/bundle-macos.sh "$PWD/target/debug/muster-native" <disposable-output>` | exit 0; actual binary bundled at `/tmp/muster-ap12-bundle.7GJULY/Muster.app`, version 0.0.0 |
| `plutil -lint <bundle>/Contents/Info.plist` | exit 0; OK |
| `codesign --verify --deep --strict --verbose=2 <bundle>` | exit 0; valid on disk, designated requirement satisfied |
| `codesign -dv --verbose=2 <bundle>` / `file <bundle>/Contents/MacOS/muster` | exit 0; `dev.muster.workspace`, arm64 Mach-O, ad-hoc + runtime, no TeamIdentifier |
| `bash scripts/perf-native.sh` | **exit 101**, release compilation failed before measurements or the follow-up unmounted-view command |

The initial attempted inline Bash gate wrapper was rejected by the tool policy
(exit 126) before execution; gates were then executed individually. No permission
or configuration changes were made. Cargo continues to report the existing
`block v0.1.6` future-incompatibility warning, not a failed gate.

**Production-path evidence:** the suite includes rejected rendered-link clicks,
closed-window weak dispatch, status/navigation races, descriptor/resource reads,
media completion invalidation, and `the_keyboard_follows_the_reading_that_is_on_screen`
(real headless select/copy dispatch in both directions, read-only assertion).
Existing tree, selection/scrollback, state-writer and PTY shutdown suites passed
unchanged. Headless layout/interaction and resource decoding are not GPU pixel
or physical keyboard observations. AP-01–AP-11 statuses remain implemented and
verified; none were reopened or expanded.

**Measurements:** test profile (optimized + debuginfo), same pinned tools and
machine above, 2,000 iterations per repeated operation; microseconds per call:

| Operation | 80x24 | 300x100 |
| --- | ---: | ---: |
| `session.title()` | 0.031 | 0.031 |
| `session.selection_text()` | 7.176 | 41.642 |
| `session.selection_range()` | 0.015 | 0.015 |
| `session.snapshot()` | 28.351 | 350.604 |
| `session.painting_snapshot()` | 20.971 | 308.668 |

Idle 300x100: title **0.031 us**, snapshot **308.669 us**; one
`close().wait()` **15,373.125 us**. Warm-width tree preparation:
10 folders × 10 files, **74 prepared rows**, **12.723 us/call**;
40 × 24, **908 rows**, **131.365 us/call**. Loading/error placeholders
replace some children, so row counts are not the nominal file product.
Fixture construction separately **73.750 us / 249.833 us** (one per size).
These are local CPU preparation/snapshot costs, not paint/frame latency,
release figures, memory bounds, or portable performance thresholds.

**Release blocker — diagnosed and fixed.** The earlier reading of
`target/release/deps/libserde_derive-d86fbf3cb6548c8b.dylib` failing `dlopen`
with `mis-aligned LINKEDIT string pool, fileOffset=0x002CE8E4`, and E0463 for
`zerofrom_derive`/`zerocopy_derive`, as pre-existing damage in the build
directory, was wrong. `cargo clean --release` reproduces it, and so does
re-running the exact `serde_derive` rustc command by hand.

The cause is the profile, not the cache: Cargo's default `strip = "debuginfo"`
for `release` is not a linker flag — rustc rewrites the finished Mach-O itself,
and on rustc 1.97.1 / aarch64-apple-darwin / macOS 27 that rewrite leaves a
dylib's `LC_SYMTAB` string pool at an offset that is not a multiple of eight.
dyld rejects the file, so rustc cannot load any proc macro and reports E0463
for whichever one it reaches first; deleting one dylib only moves the error to
the next. Verified from the artifacts: the release dylib has
`stroff = 0x2ce8e4` (`& 7 == 4`) against the dev dylib's `0x330c98`
(`& 7 == 0`); the linker command line is byte-identical with and without the
strip step; and `/usr/bin/strip -S` applied to the same file yields an aligned,
loadable dylib (`0x2ce8e8`). Release emits no debuginfo to strip, so the root
`Cargo.toml` sets `[profile.release] strip = "none"`, `scripts/release.test.mjs`
pins it, and `cargo build --locked --release -p muster` completes. **No release
performance measurements taken**: the ignored checks in `scripts/perf-native.sh`
still need a run.

**Not run / remaining acceptance:**

- Real-app launch, image pixels, font rendering, physical keyboard/toggle/scroll
  interaction and manual lifecycle exercise: **NOT RUN**. No isolated interactive
  GUI session was established; intentionally did not launch the personal app or
  risk its workspace state. The bundle smoke used the actual debug app binary,
  not a CLI substitute, but tested assembly/signature only, not startup.
- Linux build/POSIX/new native behavior coverage: **NOT RUN**; this execution
  environment is macOS and no Linux runner was used. Existing CI remains intact;
  no macOS result is being relabeled Linux evidence.
- Real external opener: **NOT RUN**; no intentional isolated manual opener
  environment was established. Rejected URLs and existing controlled-program
  opener tests passed without a real browser or external application launch.
- Live vulnerability/network audit: **NOT RUN**, not requested for this pass.
  The 18 auditor unit tests validate auditor behavior and the pinned vendor hash,
  not current vulnerability status; the existing CI dependency-audit gate remains.
- Release performance: **NOT RUN**. The blocker above was the release profile's
  strip step, not a damaged build directory, and it is fixed, so the ignored
  checks in `scripts/perf-native.sh` can now run; no release timings have been
  measured and none are claimed.

No application was launched, no personal workspace state was intentionally read
or written, and no external opener was invoked by this acceptance pass. The
bundle was left only in the named disposable directory. Full AP-12 platform
acceptance remains pending those explicit gaps.

## 5. Findings intentionally not turned into implementation work

These guardrails are part of the plan. An agent must not resurrect a withdrawn recommendation as an extra task.

| Earlier claim or proposed change | Current disposition |
| --- | --- |
| LFS pointers necessarily show an image notice | Incorrect as stated: the reproduced pointer uses a text diff. AP-04 fixes metadata and tests the actual presentation branch. |
| The grid snapshot is the application's most expensive hot path | Not established. AP-06 removes demonstrated unnecessary work and measures its own result. |
| Duplicate inspector size literals are a High-severity visual defect | Not established: baseline values agree. AP-08 is a small consistency cleanup. |
| `TerminalWindow` must be split because it has many fields | Not approved without a concrete invariant/testability benefit. |
| Three document fields are necessarily three conflicting sources of truth | Not established; they represent request/result/view phases. Preserve their invalidation. |
| Direct parser tests should be removed or folded into the production wrapper | Rejected. Keep independent security regression coverage. |
| `required_selection` is a wrapper with no behavior | Incorrect. Missing versus explicit null is tested behavior. |
| A second path check closes the local-media race | Incorrect. AP-03 must bind the read to a descriptor/prepared resource. |
| A missing explicit frame request proves current link clicks are lost | Not demonstrated. AP-01 removes the unnecessary render dependency without claiming that symptom. |
| Every language lacks registration/highlighting coverage | Incorrect. Extend existing focused tests; do not create a second inventory framework. |
| Every cursor coordinate needs a new clamp | Not established. Correct the inaccurate documentation unless a separate failure is reproduced. |
| No TODO/FIXME means the software is finished | Not a valid engineering inference or acceptance criterion. |
| The opener's wait or lossy conversion is automatically a current UI bug | Not demonstrated by the revised audit. Do not redesign process launching or pathname APIs without a concrete current caller/failure. |
| The audit proves there are no vulnerabilities or that Linux/release rendering works | Incorrect. Those require their own current checks. |

## 6. Delegation template

Use this template with the task-specific section and completed prerequisite reports. Do not supply only a title and expect the next agent to infer the safety boundaries.

```text
Implement AP-XX from docs/native-rust-migration/audit-action-plan.md.

Read the common assignment rules, the complete AP-XX section, its acceptance
criteria, and the prerequisite handoffs. Verify the named symbols against the
current checkout. Baseline audit commit: 7265e48; implementation may have moved.

Scope: only AP-XX and directly required tests/documentation. Do not implement
other tasks, revive withdrawn findings, edit Legacy/vendor/generated data, or
change persistent agent settings. Honor the agreed runtime model/effort only
through supported controls; report a delegation-control limitation honestly.

Prerequisite commits: [fill in].
Owned files / integration constraints: [fill in from the task].
Current source commit: [fill in].

Use the smallest complete solution that preserves validation, resource lifetime,
request invalidation, read-only behavior, accessibility, and platform contracts.
If already satisfied, prove it instead of rewriting it. Add a regression for
non-trivial behavior and run focused checks with a nonzero matched test count.

Stop when AP-XX is complete. Return the required handoff, including exact checks,
results, limitations, and the commit/patch reference. Do not start the next task.
```

## 7. Completion report outline

The integration owner should finish with one consolidated report, not a concatenation of unverified agent claims:

1. Source commit(s), task status table, and files changed.
2. Correctness: which user-visible failures and reading-boundary issues were fixed, with regression references.
3. Maintainability: which obsolete or duplicated contracts were simplified, and what was deliberately left alone.
4. Performance: static reductions and measured results reported separately, including profile and fixture sizes.
5. Architecture/design: local boundary improvements, preserved ownership, and accepted-no-change decisions.
6. Style/readability: corrected source explanations and current-versus-historical documentation.
7. Verification: exact command outcomes, test counts, platform/manual checks, and unresolved limitations.

This document is complete as a plan when its assignments are clear and delegable. The implementation is complete only when the task acceptance evidence exists; neither the plan's length nor a clean test baseline substitutes for that evidence.

## Final A08 safety corrections — 2026-10-17

This section supersedes the earlier A08 statements that deduplication was exact and that forgetting delivered reviews could always recover store capacity. Historical audit entries remain as records of what was believed and verified then; they are not the current contract.

- Persistent review claims now pair the exact note id with a SHA-256 digest of the canonical snapshot (`sha2` 0.10.9, already present in the locked graph). This makes collisions computationally impractical, not impossible; the id does not prevent a same-id edited note from colliding. The non-security FNV fingerprint remains only for line-drift bookkeeping.
- Ordinary **Forget notes** removes settled note payloads while retaining claims. The same Settings confirmation also exposes **Forget + reset**, a deliberate last-resort action that removes settled rounds and their claims. The dialog warns that unchanged delivered notes may be sent again. Queued, dispatching, and unknown rounds are retained. This enables real recovery after compacted tombstones alone fill the file; ordinary cleanup still preserves deduplication.
- The new `reset_duplicate_scope_recovers_a_full_store_but_keeps_unresolved_rounds` test fills the store with settled tombstones, verifies a refused write leaves disk byte-identical, resets settled history, then verifies a send succeeds and unresolved rounds remain. `forgetting_delivered_reviews_keeps_them_from_being_sent_again` continues to pin normal dedup behavior.
- The incompatible rounds schema remains 3 and both workspace crates are now `0.2.0`, as required for this persistent-shape change; historical `0.1.0` report values describe earlier state only.
- `read_formatter_stream` now receives stdout identity explicitly rather than inferring it from a size limit. Each reader still consumes at most `limit + 1` bytes; process cleanup and join behavior are unchanged. A missing-pipe early return is not reachable with the command's explicit `Stdio::piped()` setup, so no speculative lifecycle change was added.

Verification for the A08 corrections alone, on the pinned Rust/Cargo 1.97.1: `cargo test --locked -p muster-core` passed 170 unit tests and 4 instance-lock tests; `cargo test --locked -p muster` passed 383 tests with 3 ignored. `cargo fmt --all --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings` passed. No release build, real-window interaction pass, or manual UI check was run for it.

## Final correction: a document transition asks only when a destination is pending — 2026-10-17

`continue_document_transition` decided a draft's fate without checking that anything was being navigated to. `format_document` calls it unconditionally when the formatter finishes, so ordinary formatting of a dirty document — and a formatter that failed, with only the error text changing — raised "Save changes to this document?" although the window was already where the person asked to be. `refresh_document_draft` also ran first, so a keystroke-free call still settled the draft flag. The helper now returns early when `pending_document_transition` is `None`. Every destination is requested through `request_document_transition`, which fills that field first, so the decision still happens for a navigation, a quit and a window close; only the destination-less callers are affected.

Two tests in `native/src/inspector_tests.rs` pin it: `a_document_transition_with_no_destination_asks_nothing` calls the helper on a dirty document with nothing pending and checks that no prompt appears and the text, draft and target are untouched, and `a_finished_format_with_no_destination_asks_nothing` runs the real `format_document` completion over a dirty `.ts` document and checks the same thing. The second test asserts the decision rather than the formatter's outcome, so it holds whether or not a Prettier is installed. Both fail on the unfixed helper with `assertion failed: !cx.has_pending_prompt()`.

Verification on the pinned Rust/Cargo 1.97.1, by run:

- This run: `cargo test --locked -p muster --lib` passed 385 tests with 3 ignored (383 before this correction, plus the two above); the focused `navigation_requested`/`dirty_document`/`transition`/`format` subset passed 17 of 17, including `navigation_requested_during_formatting_is_deferred_until_idle` and `dirty_document_save_conflict_keeps_draft_and_pending_destination`; `cargo fmt --all --check` and `cargo clippy --locked -p muster --all-targets -- -D warnings` are clean.
- The parent's independent run, not repeated here: the workspace test run passed 383 native and 170 core tests plus the 4 instance-lock tests, the Node suites passed 23, `cargo fmt --all --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings` were clean, the release application build succeeded, and the 3 ignored release performance checks passed. Those counts predate this correction's two tests.

Still not run: any real-window interaction pass, manual VoiceOver or IME checking, a Linux build, or macOS bundle inspection.

## Final correction: every tooltip trigger waits for itself — 2026-10-17

The toolkit's managed tooltip is one overlay per window with a grace period: `request_show` puts the next trigger on screen at once whenever a tooltip is still visible or was visible within the grace period. Moving down the Files tree therefore painted the path of every row the pointer crossed, and the wait that is supposed to keep a passing pointer from naming everything was paid once per session instead of once per row. The overlay's state is not reachable from the application (`WindowState` and its overlay handle are `pub(crate)` in `gpui-component`), so the timing policy now lives here: `native/src/tooltip.rs` owns one `gpui_base::TooltipOverlay` per window, and `Tooltips::rest` clears it before asking for the next trigger, which puts the show delay back in the trigger's own hands. The popup, its placement, its 150 ms arrival and the `TooltipDefaults` delay are the toolkit's own.

A trigger is a canvas laid over the control plus a window mouse listener registered as it is painted, not `Button::on_hover`: that is inert on a disabled button, and several triggers are disabled while the tooltip is the answer (the save and cancel actions while an operation runs, a missing worktree row, the session row while it is closing). Triggers are identified by their bounds, so the order in which two of them answer the same move cannot leave a tooltip up that belongs to neither. A press, and a pointer that leaves the window, dismiss what was up, as the managed tooltip did.

`every_trigger_waits_its_own_time` in `native/src/tooltip_tests.rs` rests on a sidebar row and then moves to the one below it, asserting the popup is absent one millisecond short of the delay, present at it, absent again immediately after the move and present only after the next delay. `a_file_row_names_its_path_after_the_pointer_rests_on_it` does the same over two file-tree rows, the surface the report came from, and `a_disabled_row_still_names_itself` rests on a missing-worktree row — a disabled button, whose own hover never fires — and expects the same wait and the same answer. Dropping the `hide` before the request — the one line that restores the switch — fails the first at "the next trigger inherited the last one's tooltip".

Verification on the pinned Rust/Cargo 1.97.1, by run: `cargo test --locked --workspace` passed 388 native tests with 3 ignored (385 before this correction, plus the three above), 170 core unit tests and the 4 instance-lock tests; `cargo fmt --all --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings` are clean. The teeth check above is a deliberate temporary removal, not a claim about a release build. Still not run: any real-window interaction pass, manual VoiceOver or IME checking, a Linux build, or macOS bundle inspection.

## Final correction: the preferences have one home and no ladder — 2026-10-17

Legacy's Settings dialog generated every row from a schema in `src/domain/settings.ts`, so the panel it draws is a rendering of 19 fields with their defaults, ranges and closed sets; nothing else in that dialog was a preference. Those fields now have a home in this app: `core/src/settings.rs` is the shape, the clamping and `~/.muster/settings.json`, and the panel is drawn from it.

Three of the nineteen did not come across, and each omission is a decision rather than a gap. `ui.theme`, `ui.contentBackground` and `ui.zoom` are the ones `AGENTS.md` forbids: this app has one appearance, dark, chosen in `main.rs` before the first window opens, `native/src/theme.rs` is the only place that may name a chrome colour, and nothing here scales as a whole — a stored field for any of them would be a second authority for a decision that has one, and a preference a person can set and watch do nothing. `reviews.storage` is not here either, because `core::navigation` already persists it: two homes for one preference is one preference that can disagree with itself, and moving it would mean refusing every existing workspace file over a stored shape that changed for a cosmetic reason. `terminal.followWorktree` went with it, because this app has no follow-worktree behaviour to follow; a mode nothing acts on is configuration, not a preference. What is left is 14 fields, all of them about how a thing is drawn.

The file is JSON rather than Legacy's YAML `config.yml`, because that file is not read by this app and nothing reads it any more: the two stores this crate already keeps in `~/.muster` are JSON, and adding a YAML parser to read a document no build writes would be keeping legacy's code rather than its meaning. It has **no schema version**, which is the one place in this crate without a ladder, and the reason is that the other two stores version their shape because each holds someone's work — checkouts somebody registered, rounds that owe a service a decision. A preference holds no work: re-picking a font size costs nothing and no field can be half-read into a wrong state, since each is either a number inside a published range or a closed set of spellings. So every field defaults, an unknown key is ignored, a size out of range is clamped, and the ranges are published as `SizeLimits` so a control cannot offer a value the file would refuse. A misspelled enum is the exception and is refused by name, because an unknown spelling is not a setting this build could act on.

`save` clamps before it encodes, which is what keeps the file readable: JSON has no infinity, so an unclamped one would be written as `null` and the next read would refuse the file this app had just written. A file that does not parse is never written over — it is the only copy of somebody's preferences, and replacing a typo with the defaults they never asked for loses the rest of it too. Everything else is the house pattern the other two `~/.muster` stores use: bounded, non-symlink `O_NOFOLLOW` read, private directory, `0600` temporary file, atomic rename, directory sync.

Seven tests: the round trip, a file that sets one key and keeps the defaults for the rest, an unknown key ignored, a size clamped at both ends, an unusable number refused by the parse while an in-memory one becomes the default, an enum this build does not have named in the error, a broken file preserved by a refused save, and a symlink refused while a saved file is `0600`.

Verification on the pinned Rust/Cargo 1.97.1, by run: `cargo test --locked --workspace` passed 388 native tests with 3 ignored, 177 core unit tests (170 before this change, plus the seven above) and the 4 instance-lock tests; `cargo fmt --all --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings` are clean. Two claims were found false by the tests and corrected rather than kept: `1e999` in the file is refused by the JSON parser instead of arriving as infinity, and a misspelled enum's error names the variant it did not have, not the field. Still not run: any real-window interaction pass, manual VoiceOver or IME checking, a Linux build, or macOS bundle inspection. The panel that draws this schema, and the live application of each field in the terminal and the editor, are not part of this change.

## Final correction: the preferences have a panel — 2026-10-17

`native/src/settings.rs` draws `core::settings::AppSettings` as a dialog: four sections, a row per preference, a footer of Reset / Cancel / Apply. The schema is the whole dialog — a preference added to `core::settings` is a row and a closure, not a branch in a template — which is the property Legacy's `SETTINGS_SECTIONS` had and this one keeps.

The panel is a draft and nothing reaches the file until Apply, because that is what makes Cancel possible at all and what keeps a size somebody is still typing from being the size anything is drawn at. The draft is built whole on every open, so a half-typed font size left behind last time is not what the next open shows.

The four sizes are the one place the draft is not the whole truth. A toggle and a select report their change and can be held in the model; a number field is a text field holding a person mid-thought, so those four live in their fields and are read when the draft is compared or written. The range is applied on the way in and the clamped number is written back into the field, so the panel closes on numbers that are the ones on file. A first pass had the model hold sizes too, and the test that set a size through the model caught it: the value was accepted and then ignored, which would have been a preference no keyboard can reach.

A file that does not parse opens the panel on the defaults **with the failure on screen**, because whoever typed it is who can fix it; Apply then refuses, which is `core::settings`' own promise that a broken file is never written over. `every_preference_has_a_row_and_nothing_else_does` also asserts the three fields this build has no answer for are absent — the theme, the editor background and the interface scale — so the panel cannot quietly grow a stored preference that nothing reads.

The Settings action in the title bar opened a dropdown carrying review storage, **Clear cached review exports…** and **Forget delivered reviews…**. It now opens the panel, and those three moved into its Review notes section: one surface, which is what `AGENTS.md`'s **Settings → Forget delivered reviews…** had been claiming since the rounds store landed. The storage itself is a draft field even though `core::navigation` persists it, because it is chosen here like everything else and must not take effect before Apply; Apply writes the preferences file first and the location second, so a failure to write the file leaves the panel open with the choices still in it. The panel is last in the window's dialog chain, so a confirmation raised from inside it is the dialog on screen rather than the panel behind it.

What Apply deliberately does not do is repaint a live terminal: nothing here can change a running grid's font, cursor or scrollback yet, and the panel says nothing about it rather than implying otherwise.

Seven tests in `native/src/settings_tests.rs`, each against a temporary file so running them cannot touch a person's own settings: a changed draft writes nothing; a thrown-away draft leaves the file as it was and the next open shows the defaults; Apply writes it, closes the panel and the next open reads it back; a size of 400 is clamped to 20 and the field is told; a broken file opens the panel with the failure shown and survives an Apply untouched; Reset puts the defaults back in the fields and not only in the model; and every preference has a row while the three this build has no answer for do not.

Verification on the pinned Rust/Cargo 1.97.1, by run: `cargo test --locked --workspace` passed 395 native tests with 3 ignored (388 before this change, plus the seven above), 177 core unit tests and the 4 instance-lock tests; `cargo fmt --all --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings` are clean. Two claims in the first draft of the tests were false and were corrected against the toolkit source rather than kept: `Select`'s words arrive as `SharedString` and do not compare against `&str` directly, and the dialog body is a scroll region, so a row below the fold exists without being visible — which is why the row list asserts presence and only the rows at the top assert visibility. Still not run: any real-window interaction pass, manual VoiceOver or IME checking, a Linux build, or macOS bundle inspection. Applying a terminal or editor preference to what is already on screen is not part of this change.
