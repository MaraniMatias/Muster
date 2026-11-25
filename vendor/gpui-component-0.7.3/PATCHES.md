# Vendored `gpui-component` 0.7.3

Sources are the published crates.io `gpui-component-0.7.3.crate`, SHA-256
`465a363235cd4062ef5f0953fc096fc764ed6f53dfbd9f409f669f93ea3b1ff9`.
The Apache-2.0 license is retained in `LICENSE-APACHE`. The local Cargo cache's
`.cargo-ok` marker is omitted; all upstream sources and tests are retained.

## Diff annotation hit target

In `src/diff/mod.rs`, `add_annotation_button` originally placed a square of
`row_height` at the start of the change-indicator lane. At the app's default
font size that square extended 13 pixels into source text. Its mouse-down
handler stopped propagation there, preventing text selection from starting on
the first characters of a line, even when the button was transparent.

The button now ends at the source-text boundary. It keeps its row-height target
when gutters provide enough room; with hidden line numbers its width is limited
to the available gutter. Both sign/bar indicators and the narrower hidden
indicator lane are accounted for. No application feature or public API changes.

`diff_code_can_be_selected_and_copied_while_a_note_is_marked` in
`app/src/inspector_tests.rs` checks the button bounds, selects from the first
character, copies through the window's keyboard action, and verifies that the
note's range can still be restored. Existing composer tests exercise the `+`.

## Contextual diff syntax (DIFF-01)

`DiffFile::with_syntax_source` accepts complete old/new sources solely for syntax
preparation. `presentation::syntax_spans` parses each complete side once and maps
captures to the patch's existing source line numbers, including unchanged hunk
context. Supplied text must agree with a row before its captures can be used.
Hunks, counts, copy coordinates and review anchors are not changed. Without full
source, the existing independent-hunk fallback remains; it never joins gaps.

`SyntaxHighlighter::capture_names` now uses the editor's bounded injection parser
instead of suppressing injections. The diff no longer repeatedly parses fragments
when complete source is supplied; injection queries are shared within each source
parse by the existing parser. The document/editor update path is unchanged.

The app supplies bounded UTF-8 sources from the same immutable merge-base and
private working-file snapshot that produced its patch, not from a second checkout
read. Missing, oversized or non-UTF-8 sources leave the patch available without
full-source highlighting. Retained sources count toward the app's diff byte cap.

`diff_injections_use_full_sources_without_moving_review_anchors` in
`app/src/inspector_tests.rs` checks both sides of sparse Vue and Markdown diffs
against document styles and explicit keyword/string/comment/template/CSS tokens.
The opening tags/fences are outside the hunks; a context comment is inside.
`diff_full_source_spans_keep_crlf_tabs_and_unicode_coordinates` in the same app
test module covers CRLF, tabs, Unicode, mismatched sources and the
line-length cap. `DiffState::syntax_highlights_for_test`, gated by `test-support`,
exposes the actual prepared painter spans to the app's integration test.

`scripts/patched-gpui-component.mjs` pins the entire vendored tree. Remove these
patches when adopting an upstream release that fixes the same defects.

## A sticky file header (DIFF-02)

`Diff::sticky_header` pins the header of the file at the top of the viewport
over the rows that scroll under it, so a patch longer than the pane keeps
saying which file the lines under it belong to. It is off by default and the
header row is drawn as it was before when it is off.

The pinned copy is a second element, so it is named apart from the row in the
list: two elements with one id are one ambiguous target. The public header
slots keep the arity they had — `render_header` and its three siblings take
`(&DiffFile, &mut Window, &mut App)` — and a narrowly scoped slot,
`render_pinned_header`, renders the pinned copy. An application that names the
two apart sets that slot; the pinned copy falls back to `render_header` when it
is not set.

The copy carries the header row and not the annotations under it: a notice or a
note belongs to the row in the list, and drawing it twice would say it twice.

The next file's header is on its way up to the top of the pane, and the pinned
header is lifted by the room the next one takes: the next header is never
masked by it, and the pinned header never takes the next header's click. The
lift is measured from the list's own viewport, and the diff body clips what is
lifted out of it.

The room is the pinned copy's own height and not the next header's row: that
row carries the notices and the notes under it, which the copy does not draw,
and a copy lifted by that row is lifted past the next header and leaves a gap
under it. The copy's height is measured, because the list cannot say it: the
copy is a second element that the list never draws. The header that is pinned
is found by a binary search over the header rows rather than by a scan, and
the rows are in the order of the files, so it is the header of the file the
row at the top of the viewport belongs to.

`the_header_of_the_file_being_read_stays_at_the_top` and
`the_pinned_header_steps_aside_for_the_next_header` in `app/src/app_tests.rs`
open a file, scroll down its patch, and check that the pinned header is drawn
where the header was and that it steps aside for the next header.

## A patch that arrives keeps the reader's place (DIFF-03)

`DiffState::set_files` replaced the rows with a full list reset, which dropped
the scroll position: the patch of a file that was opened landed under the
reader and the diff went back to its first row. The rows are now spliced, and
the reader's place is put back: the row that was at the top of the viewport,
with the reader's place inside it, is restored by that row and not by the
source line it showed. A source line is the fallback for a row that a rebuild
changed, and the offset into the row is kept in both cases, so the reader is
not moved a row's height either.

The reader's place is named by the file's own path and by what the row is
inside that file, and not by the place of either in the list: the place of a
file in the list is not the file, and a patch that reorders its files or drops
one of them renumbers the files that are left. A row that shows source lines
is named by the source line it shows, and that line is asked for before the
row's own place in the file: a code row's place is a pair index, which a
newer patch of the same file renumbers when it supplies a different window of
it, while the source line is the row's own coordinates.

`DiffState::scroll_top_position_for_test`, gated by `test-support`, names the
source line at the top of the viewport, which is what an integration test
needs to say where the reader is.

`opening_a_file_does_not_move_the_reader` in `app/src/app_tests.rs` opens a
file with the reader on the next file's header and checks that the header is
still where it was, and that the diff did not go back to the top of the list.
`a_patch_that_reorders_its_files_keeps_the_readers_place` and
`a_newer_patch_keeps_the_reader_on_the_same_source_line` in the same file
check that a patch that reorders its files, and a newer patch of the same
file, keep the reader on the line they were reading.
