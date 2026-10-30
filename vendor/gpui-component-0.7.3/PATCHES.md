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

`scripts/patched-gpui-component.mjs` pins the entire vendored tree. Remove this
patch when adopting an upstream release that fixes the same geometry.
