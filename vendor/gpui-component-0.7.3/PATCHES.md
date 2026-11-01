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
