# Native application audit

## Scope and evidence

- Audit date: 2026-10-22.
- Baseline: `b256863` (`main` at worktree creation).
- Remediation branch: `fix/audit-native-app`.
- Worktree: `.worktrees/fix-audit-native-app`.
- Host: macOS, Apple Silicon; `rustc` and `cargo` 1.97.1 verified.
- Method: targeted source and dependency inspection, existing checks, and a bounded parser reproduction. This is a risk-based audit, not an exhaustive proof of security or terminal compatibility.
- References below describe the baseline. Fixes and verification are recorded separately so the original findings remain reviewable.

Priority: **P1** means address before shipping (untrusted-input resource exhaustion, data-integrity or destructive-action bypass); **P2** means a reproducible correctness or performance defect.

| ID | Area | Priority | Finding | Initial status |
| --- | --- | --- | --- | --- |
| SEC-01 | Security / trust boundaries | P1 | OSC limits do not bound the underlying VT parser | Open |
| DATA-01 | Work integrity / state | P1 | Window close bypasses uncertain durability and clean in-flight work | Open |
| TERM-01 | Terminal / process lifecycle | P1 | App Quit bypasses the running-terminal confirmation | Open |
| TERM-02 | Terminal protocol correctness | P2 | String scanners do not recover consistently from cancellation / ESC | Open |
| PERF-01 | Performance / resources | P2 | Full width-cache eviction causes repeated shaping on large trees | Open |
| UX-01 | Native UX / keyboard access | P2 | Grammar-menu keyboard scrolling uses the wrong offset sign | Open |

## 1. Security and trust boundaries

### Reviewed protections

- `core/src/files.rs`: descriptor-relative checkout traversal, refusal of symlinks and special files, bounded reads, and pre-publication target revalidation.
- `core/src/opencode.rs:462-541`: literal loopback HTTP endpoint validation, no redirects and no environment proxy for authenticated requests.
- `core/src/navigation/instance.rs`: private owned directory and OS-held instance lock.
- `core/src/pi.rs`: private prompt files, ownership handover and expiry handling.
- `app/src/terminal/paste.rs`: explicit approval for non-bracketed multiline paste and sanitization of control characters.
- `app/src/terminal/graphics.rs`: direct transfers only, decoded-image and resident quotas. Production clipboard host writes are disabled in `app/src/session_owner.rs`.

### SEC-01 — The OSC cap does not protect the parser's allocation

**Evidence:** `app/src/terminal/core.rs:377-477, 484-522`; `vendor/alacritty_terminal-0.26.0/Cargo.toml:81-87`; locked `vte` 0.15.0 `src/lib.rs:55-65, 407-433, 544-551`.

`scan_osc` stops retaining the application's copy after 64 KiB, but `feed_inner` still sends the entire raw input to `Processor::advance`. The enabled `vte/std` implementation stores OSC bytes in an uncapped `Vec<u8>`. An unterminated OSC emitted by any child, including remote output shown by that child, can therefore keep growing the process despite the apparent application limit. Sanitizing a title or rejecting an OSC after parsing is too late.

**Executed reproduction:** a standalone Rust probe linked to the already-built locked `vte` artifact fed `ESC ] 0 ;`, 512 chunks of 4096 bytes and BEL. The parser dispatched **2,097,153 payload bytes**, demonstrating accumulation far beyond the application's 65,536-byte cap without attempting an OOM. The existing `an_unterminated_or_oversized_osc_is_bounded_and_reads_as_nothing` test only checks `TerminalCore::osc`, not the second buffer.

**Required correction:** enforce the limit before oversized OSC payloads reach the underlying parser, discard an oversized sequence whole, and recover for subsequent text and sequences. Do not modify hash-pinned vendor files or weaken valid bounded OSC handling.

**Completion checks:** regression coverage for large terminated and unterminated OSC across chunk boundaries; oversized title / clipboard effects must not be published; demonstrate that the upstream parser is not fed an unbounded OSC; normal text and the next valid OSC still work.

## 2. Work integrity and state consistency

### Reviewed protections

- `core/src/files.rs:516-613, 916-1011`: compare-before-replace and explicit published-but-not-durable result.
- `app/src/document.rs:1129-1340`: deferred document transitions, retention of newer edits, durability retry and stale-result guard.
- `app/src/state_writer.rs`: latest-value slot, final flush, failure reporting and explicit retry behavior.
- `core/src/navigation/persistence.rs`: strict stored shape, validation, bounded reads and refusal to overwrite an unreadable file.
- `core/src/opencode/rounds.rs:372-426`: durable dispatch intent before network effects, no automatic retry of an uncertain send.

### DATA-01 — Window close checks only dirty text

**Evidence:** `app/src/app.rs:1856-1869`; `app/src/document.rs:114-116, 1163-1168, 1296-1322`; existing navigation-only test `uncertain_publication_blocks_navigation_until_durability_retry` in `app/src/inspector_tests.rs`.

`request_window_close` enters the document-transition guard only when `DocumentView::is_dirty()` is true. That method ignores `durability_uncertain`, `saving` and `formatting`. A published save normally leaves the editor equal to its updated baseline, so a directory-sync failure can leave `dirty == false` and `durability_uncertain == true`. Clicking the window close button then proceeds to teardown instead of preserving the retry intent. Clean text also does not prove that an asynchronous save or format has finished.

**Reproduction:** open a clean document, put it into the published-but-not-durable state used by the existing test, then invoke `request_window_close` instead of navigating. The baseline takes the teardown path, whereas ordinary navigation remains blocked.

**Required correction:** route window close through the same document-transition protection even when the text is clean. Preserve the destination while a save / format is running and while durability needs retry. Do not introduce persistence for live document layouts or a migration.

**Completion checks:** a clean uncertain document cannot close; retry can complete the pending close; clean in-flight save / format does not close early; dirty Save / Discard / Cancel behavior remains intact.

## 3. Terminal correctness and process lifecycle

### Reviewed protections

- `core/src/pty/mod.rs`: bounded writes, foreground-group cleanup and shutdown serialization, with PTY integration tests.
- `app/src/session_owner.rs`: ownership before UI publication, rejection of late spawns and writer flush on shutdown.
- `app/src/terminal/session.rs`: bounded input queue, coalesced wakes, background polling, ordered protocol replies and asynchronous close.
- `app/src/terminal/graphics.rs` and `sync_updates.rs`: bounded transfers and synchronized-update deadlines.

### TERM-01 — App Quit skips the destructive-close decision

**Evidence:** `app/src/app.rs:1852-1854, 1873-1891, 3739-3751, 3778-3784`; `app/src/menus.rs` Quit dispatch.

The window close route asks before stopping a foreground process, an unknown shell status or a pending terminal. The app-level Quit route instead queues `cx.quit()` directly after document handling. The quit hook then calls `owner.stop()`. Consequently the menu / Command-Q can terminate a running foreground job without the warning shown by the window's close button. The existing quit cleanup test proves shutdown, not user approval.

**Reproduction:** with a running foreground command and no dirty document, use the app Quit action and compare it with window close.

**Required correction:** share the existing running / unknown / pending-terminal confirmation policy between app Quit and window close, retaining the intended final action. Cancellation must stop neither processes nor the application. Keep the final synchronous quit cleanup hook as a last safety net.

**Completion checks:** foreground, unknown and pending cases require approval through both entry points; Cancel leaves them live; approval continues the requested close / quit; document decisions still precede termination.

### TERM-02 — The side scanners disagree with VT string termination

**Evidence:** `app/src/terminal/core.rs:402-445, 447-469`; `app/src/terminal/sync_updates.rs:61-66`; locked `vte` 0.15.0 `src/lib.rs:407-423`.

The native VT parser leaves OSC on CAN / SUB and on ESC. `StringScan::Osc` instead appends CAN / SUB, and `OscEscape` falls back into OSC for non-ST escapes. `SyncUpdates` handles direct cancellation but has different recovery after `StringEscape`. Once the scanners disagree, valid following shell / Pi / graphics or synchronization sequences can be swallowed or classified as string payload even though the grid parser has returned to ordinary VT parsing.

**Reproduction input:** an interrupted OSC such as `ESC ] 0 ; partial CAN`, immediately followed by a valid `ESC ] 133 ; D ; 7 BEL`. The wrapper must recover and record the latter command exit independently. Also exercise cancellation immediately after ESC and a following CSI synchronization marker.

**Required correction:** align recovery with the actual parser for the supported string types, without interpreting nested-looking bytes that are still genuine payload. SEC-01 and this finding share the raw string boundary and should be solved coherently.

**Completion checks:** CAN, SUB and ESC recovery across every input split, followed by valid text / OSC / APC / synchronized-update sequences; existing graphics and synchronized-update tests remain green.

## 4. Performance and resource consumption

### Reviewed protections

- `app/src/inspector.rs`: virtualized fixed-height tree rows, bounded diff concurrency, stale-read tokens and subtree release.
- `app/src/document/media.rs`: bounded concurrent resource reads, encoded-byte retention and weak ownership after document release.
- Terminal input queues, wake coalescing and image quotas prevent several common unbounded producer / consumer paths.
- `app/src/perf.rs` and `scripts/perf-native.sh` provide existing repeatable measurements, rather than asserting machine-specific frame timings.

### PERF-01 — The width cache repeatedly discards the working set

**Evidence:** `app/src/app.rs:134-172`; `app/src/inspector.rs:859-892`; `app/src/app_tests.rs:130-150`; `app/src/perf.rs:195-218, 288-332`.

Every tree preparation looks up widths for the entire expanded tree. At 8192 strings in one size, the next miss removes that size's whole cache. A repeated traversal over 8193 distinct names therefore continues cycling through misses rather than retaining a warm useful subset. Multiple individually valid folder listings can exceed that size. The current test requires full eviction, and the performance fixture repeats the same filenames across folders, so neither exposes the large-distinct-name case.

**Impact qualification:** the repeated-cache-miss mechanism follows directly from the source; no measured production frame-latency or memory-leak claim is made.

**Required correction:** retain a useful bounded working set without clearing every cached width on one overflow. Prefer a small local admission / eviction change over a new cache dependency or tree architecture.

**Completion checks:** deterministic coverage using more than 8192 unique names over repeated passes; already-admitted widths remain useful; both font-size and entry counts stay bounded; accurate row widths and horizontal scrolling are preserved.

## 5. Native UX, accessibility and platform fit

### Reviewed protections

- Inspector and sidebar row controls use toolkit buttons with names and selected / expanded state; confirmation buttons have explicit actions.
- `app/src/confirmation.rs` shares modal / focus handling across platforms.
- Theme roles separate chrome, terminal protocol colors and file-icon ink.
- `.github/workflows/ci.yml` has distinct macOS and Linux build / packaging checks, pinned actions and explicit permissions; the macOS deployment floor is shared with packaging.

### UX-01 — Grammar-menu keyboard selection can leave the viewport

**Evidence:** `app/src/document.rs:943-959, 1008-1022`; locked GPUI `ScrollHandle::set_offset` documentation in `gpui-pre` 0.3.9 `src/elements/div.rs:4397-4402`.

GPUI offsets become **negative** when scrolling down. `scroll_grammar_row_into_view` instead treats `offset.y` as a positive content coordinate and writes a positive offset when the highlighted row is below the viewport. Keyboard Down can therefore move selection beyond the visible list without revealing it; Up after an actual scroll also computes the wrong visible range. The existing grammar-menu tests cover searching and choosing, not traversing beyond the first viewport.

**Reproduction:** open an unfiltered grammar menu, press Down until the highlighted row lies below its 240 px viewport, then navigate back up after scrolling.

**Required correction:** calculate content-space visibility using the negated offset and write the corresponding negative displacement, or reuse a verified native scroll-to-item API. Preserve the horizontal offset, search behavior and focus restoration.

**Completion checks:** real headless-window keyboard navigation in both directions beyond the first viewport; the selected row remains within list bounds; choosing it still closes the menu and focuses the editor.

## Audit-time verification

- `cargo fmt --all --check`: passed.
- `node --test scripts/audit-rust.test.mjs scripts/release.test.mjs scripts/linux-release.test.mjs`: 41 passed, 2 platform-gated skips, 0 failed.
- `node scripts/audit-rust.mjs`: queried 887 locked package versions; no blocking advisory. Eight informational / low warnings remain: `instant`, `paste`, `rustls-pemfile`, `rustybuzz`, `ttf-parser`, `zbus_names`, `zvariant_derive`, `zvariant`. This is not a warning-free dependency graph; no dependency update is justified solely by these informational results in this remediation.
- Locked VT parser probe: confirmed accumulation of 2,097,153 OSC payload bytes.
- A workspace test run was started before remediation. No retained baseline verdict is claimed here; the independently verified post-remediation result is recorded below.

## Remediation and final verification

All six findings are remediated in this worktree; the audit observations and reproductions above remain the baseline record. Initial remediation was delegated to `coder` with `openai-codex/gpt-6-luna:high`, followed by parent diff review and independent checks. Review caught an introduced oversized-OSC-to-APC recovery regression; a failing reproduction was added before correcting it. The subsequent per-point agent/review cycle below found and corrected further incomplete handling before final acceptance.

| ID | Disposition and change | Regression coverage |
| --- | --- | --- |
| SEC-01 | **Fixed.** `feed_inner` stops feeding an OSC to `Processor` when its payload exceeds the cap, resets the parser before discarding the remainder, and resumes only at the real terminator. The oversized flag is cleared at that OSC's own terminator, not a later APC boundary. Oversized OSCs are refused whole. No hash-pinned vendor code changed. | `an_unterminated_or_oversized_osc_is_bounded_and_reads_as_nothing` tracks actual bytes passed through `Processor::advance`, checks the bound, verifies no title / clipboard effects, and confirms following text is parsed. `oversized_osc_bel_preserves_following_text_and_graphics_in_same_or_split_chunks` checks text, cursor, graphic placement, subsequent OSC, and representative cap / terminator splits. `parser_byte_observer_tracks_osc_after_escape_termination` pins test instrumentation to vte's ESC transition. |
| DATA-01 | **Fixed.** Window close now queues through document transitions when a document is dirty, saving, formatting, or durability-uncertain, retaining the requested close until work is safe to leave. | `a_clean_document_with_uncertain_durability_keeps_window_close_pending` verifies pending close and continuation after durability is resolved; `clean_in_flight_save_and_format_keep_close_pending_until_idle` verifies clean save and format both defer close. Existing save-retry and document-transition tests remain green. |
| TERM-01 | **Fixed.** App Quit and window close share the live / unknown / pending terminal confirmation policy while retaining their distinct approved action. Quit cleanup remains in the existing synchronous shutdown hook. | `app_quit_asks_before_stopping_pending_terminals_and_cancel_keeps_them` verifies the Quit action label and cancellation without quitting the test process. Existing close and quit cleanup tests remain green. The follow-up also exercises a real foreground process through both entry points and document-before-termination ordering. Unknown status and approved app Quit remain explicit coverage limits below. |
| TERM-02 | **Fixed.** OSC side scanning ends on BEL, CAN, SUB, or ESC as `vte` does; the synchronized-update scanner resumes from the post-ESC parser state. APC abort/recovery also follows ESC and cancellation boundaries. | `osc_scanning_recovers_after_cancel_and_escape_at_every_input_split`, `canceled_and_escaped_strings_recover_before_a_sync_marker`, `synchronization_markers_inside_string_payload_are_not_scanned`, and `a_canceled_or_escaped_apc_does_not_swallow_the_next_graphics_packet`. |
| PERF-01 | **Fixed.** Width-cache admission now declines new strings or font-size keys at their caps instead of discarding an entire useful cache. Existing entries remain warm and both cardinalities stay bounded. | `measured_width_cache_admits_a_bounded_warm_working_set` exercises 8193 distinct names over repeated passes and verifies both limits. |
| UX-01 | **Fixed.** Grammar-row visibility uses content coordinates and the correct negative offset. Reopening or filtering also resets the list together with the highlighted first row, so selection cannot be left off-screen. | `the_grammar_menu_is_a_search_over_one_list` covers Down/Up, selection and viewport bounds; `the_grammar_menu_opens_on_the_row_the_keyboard_points_at` and `the_grammar_menu_search_puts_the_list_back_at_the_first_row` pin the reopening and filtering cases. Existing choosing / focus-return tests pass. |

### Point-by-point follow-up

Agents used different providers in parallel: `openai-codex/gpt-6-luna:high` and `minimax/MiniMax-M3.1-Flash:max`, both with the `coder` role. File ownership was separated per wave; the parent reviewed every returned delivery, independently checked the relevant source and tests, and returned defects to the responsible agent rather than accepting the completion claim. No commit, merge or rebase was performed.

| Point | Agent | Parent review and final disposition |
| --- | --- | --- |
| SEC-01 | Luna | **Returned twice, then accepted.** Initial review missed `ESC NUL ]` and other ignored escape bytes: native vte could enter OSC while the application scanner missed it. A subsequent fix still missed the same transition after APC/DCS/PM/SOS. Both bypasses were reproduced with real title / clipboard events before correction. Non-ST string escape recovery now reuses the native Escape classification instead of maintaining diverging copies. Regression tests cover ignored C0, DEL/high bytes, escape intermediates, affected string prefixes, cap boundaries and split feeds. |
| DATA-01 | MiniMax | **Accepted after test correction.** Production close guards were already correct. Added real save/retry completion and refusal coverage. Parent rejected formatter-success claims that could silently pass through a missing-tool refusal and waits shorter than the production deadline. Tests now use a private fixture formatter through a per-window test-only seam, assert its successful output and use bounded wall-clock waits independent of a removed window. No global PATH mutation or installed Prettier is required. |
| TERM-01 | MiniMax | **Accepted.** Real interactive PTY foreground-job tests verify that both Close and Quit ask, cancellation leaves the program running, and a dirty document is decided before terminal termination. Existing pending-terminal and window-close approval coverage remains. The final app Quit action is not invoked inside the test process. |
| TERM-02 | Luna | **Returned, then accepted.** Parent rejected a recovery test whose extra ESC masked the defect and a newly introduced Clippy failure. The test now uses the immediate continuation after ignored control bytes; scanners preserve native Escape / intermediate / CSI behavior, and the lint is fixed without suppression. Terminal-focused tests passed after the final scanner adjustments. |
| PERF-01 | Luna | **Accepted without additional changes.** Source inspection confirms `measured_width` returns the actual shaped width and only admits entries within its bounds. The 8193-name regression retains 8192 useful widths and caps font-size cardinality; another cache layer or performance refactor was unnecessary. No production frame-timing claim is made. |
| UX-01 | MiniMax | **Accepted after further correction.** The existing sign fix passed, but reopening and filtering reset selection without resetting the retained scroll position. Both defects were reproduced in production headless views, corrected at the shared reset helper, and covered by separate failing-before/passing-after keyboard/geometry tests. Parent reviewed the helper and both reset call sites. |

This follow-up added 17 app regression tests relative to the initial 558-test app pass. One intermediate agent run reported a terminal-image atlas test failure; its isolated rerun and subsequent complete app/workspace runs passed. Its cause was not established, so this is recorded as an intermittent observation rather than asserted to be a proven pre-existing issue.

### Final verification

- Toolchain: `rustc 1.97.1` and `cargo 1.97.1`; Cargo used `CARGO_TARGET_DIR=/Users/matiasmarani/Trabajo/Marvis/target` and `--locked`.
- Independent final `cargo test --locked --workspace`: passed — app 575 passed / 3 ignored; core 232 passed; instance-lock integration tests 4 passed; **811 passed, 0 failed** overall.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all --check`: passed.
- `git diff --check`: passed.
- Independent final parent verification: workspace tests, Clippy, formatting and diff checks passed again. `lsp_diagnostics path="*"` is clean in the parent session; transient stale diagnostics reported by the subagent were not reproduced.
- Cargo reports an existing future-incompatibility warning for the dependency `block` 0.1.6. This is not a source error introduced by these changes.
- No app-quit approval was exercised in a test because that would quit the test process. Real foreground-process confirmation/cancellation, document ordering, pending-terminal Quit cancellation and approved window close are covered. Unknown shell status / poll errors are inspected in the shared predicate but are not deterministically injected in these window tests. Approved app Quit still relies on the inspected final-action branch and existing shutdown-cleanup coverage.

### Remaining audit limits

- Linux desktop behavior and a clean Linux package installation cannot be executed on this macOS host; source / CI inspection is not a Linux runtime pass.
- No live Pi / OpenCode server was contacted; service-side behavior requires the existing mock / integration tests and, separately, a live smoke test.
- No manual VoiceOver / Orca session, display-scale visual review, prolonged RSS / CPU soak, exhaustive VT conformance suite or release-profile performance comparison was run.
- The narrow POSIX compare-before-rename race with a non-cooperating external writer remains an explicitly documented platform limitation, not an unconditional compare-and-swap guarantee.

## Commit, rebase and main verification

The remediation was committed and rebased without conflicts onto `main` at
`862b3dc`. Main advanced during verification, so the rebase and branch checks
were repeated against that commit. The main worktree's unrelated edits were
not changed; main checks ran in a separate clean worktree at `862b3dc`.

The branch was then rebased again, onto `2dd850e` (`v1.0.0-rc.15`). Main's
only changes since `862b3dc` were the version, the changelog and the README,
so the rebase onto `2dd850e` touched none of the remediated source, and the
branch's own checks were repeated against it.

- Rebasing the remediation onto main does **not** integrate it into main.
- Main's existing workspace tests passed: app 558, core 232, instance-lock 4
  (**794 passed**, 3 ignored). Clippy with warnings denied and formatting passed.
- Main's audit/release script tests passed with two platform-gated skips.
  The dependency audit queried 887 versions and reported no blocking advisory;
  the eight informational/low advisories listed above remain.
- Main nevertheless **does not satisfy this application's audit criteria**:
  all six findings remain in its source. `app/src/terminal/core.rs:377-477`
  still feeds oversized OSC to the native parser and retains divergent string
  recovery; `app/src/app.rs:1852-1869` still quits directly after document
  handling and guards window close only by dirty text; `app/src/app.rs:138-150`
  still evicts the width-cache working set; `app/src/document.rs:1008-1022`
  still uses the incorrect scroll-offset sign. Passing the older tests does
  not close these findings.
- After the final rebase, the remediation branch's workspace tests passed:
  app 584, core 232, instance-lock 4 (**820 passed**, 3 ignored).
  Clippy with warnings denied, formatting and diff whitespace checks passed.
  LSP supplied syntax-only validation; Cargo supplied compilation/type checking.
- No merge into main or push was performed. The runtime/platform coverage
  limits above still apply.

### Independent review on `2dd850e`

The branch was reviewed again on `2dd850e`. Every finding was checked against
the locked `vte` 0.15.0 and `gpui-pre` 0.3.9 sources rather than against the
audit's own summary of them, and each remediation was mutation-checked: the
fix that finding belongs to was reverted one at a time, and the regression
that pins it fails without it. No fix was changed, because none was found to
be incomplete.

| ID | Verdict | Evidence |
| --- | --- | --- |
| SEC-01 | Fixed, unchanged | `app/src/terminal/core.rs:474-490` stops feeding an OSC at the cap, resets the parser and discards the tail; `:529` keeps the tail out of the parser at a chunk's end; `:607-617` refuses the payload whole. The bound holds against the locked `vte`, whose `advance_osc_string` stores the payload in an uncapped `Vec` and dispatches on BEL, CAN, SUB and ESC. |
| DATA-01 | Fixed, unchanged | `app/src/app.rs:1872-1886` queues the close when the document is dirty, saving, formatting or durability-uncertain; `app/src/document.rs:1149-1177` waits for the work to be idle; `:1290-1294` and `:1505-1509` carry the queued close on the save's and the format's completion. A refused save keeps the destination queued by design (`app/src/inspector_tests.rs:4453`, `a_refused_save_keeps_the_draft_and_the_queued_close`). |
| TERM-01 | Fixed, unchanged | `app/src/app.rs:1864-1870` sends both entry points through one `request_close`; `:1890-1934` is one predicate and one question with two final actions; `:3892-3906` keeps the quit cleanup hook, and `app/src/session_owner.rs:157-179` keeps that stop synchronous. |
| TERM-02 | Fixed, unchanged | `app/src/terminal/core.rs:396-470` follows the locked `vte` transitions (Escape, EscapeIntermediate, OSC on BEL/CAN/SUB/ESC, APC and ignored strings) instead of copies of them; `app/src/terminal/sync_updates.rs:34-92` does the same for the synchronization scanner. |
| PERF-01 | Fixed, unchanged | `app/src/app.rs:138-168` declines an admission at the cap instead of discarding the working set. Only two font sizes are measured (`app/src/inspector.rs:82-88`), so the four-size cap is not reached and the decline cannot starve a live size. |
| UX-01 | Fixed, unchanged | `app/src/document.rs:1008-1024` computes visibility in content space and writes the negative offset the locked `gpui-pre` documents; `:1520-1521` and `:1964-1979` reset the list with the highlighted row when the menu opens or the search changes. |

Checks on `2dd850e`: `cargo test --locked --workspace` passed (app 584, core
232, instance-lock 4 — **820 passed**, 3 ignored); `cargo clippy --locked
--workspace --all-targets -- -D warnings` passed; `cargo fmt --all --check`
passed; `git diff --check` passed; `node --test scripts/audit-rust.test.mjs
scripts/release.test.mjs scripts/linux-release.test.mjs` passed (41 passed, 2
platform-gated skips). The terminal suite was run three times over with the
same result. The coverage limits above are unchanged: the unknown shell status
is a conservative policy rather than a defect, and the approved app Quit is
not observable in a headless test because the test platform's `quit` is a
no-op, so the final branch was read rather than exercised.
