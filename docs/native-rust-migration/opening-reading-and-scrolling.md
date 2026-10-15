# Opening things elsewhere, reading Markdown, and the panels that scroll

The record of one pass over five reported defects: a non-text file could not be
opened at all, a Markdown file was only ever its source, a change with no text
said nothing about what changed, and neither inspector panel could be read to
the end of a long name.

The scope was behaviour and presentation. No stored shape, IPC contract or
preference key changed, so nothing here needed the `v0.*.*` migration rule.

## What was wrong, and what actually caused it

**A non-text file had nowhere to go.** `core::files::read_preview` refuses a
binary, a non-UTF-8 or an oversized file, and the document pane showed that
refusal with a Retry button — which is the right answer to a read and the wrong
one to a *reader*, because a `.png`, an `.mp4` or a `.psd` is not a file that
fails to open. Legacy handed those to the platform from a ctrl-click
(`legacy/src-tauri/src/services/files.rs:555`), and there was no port of it.

**A Markdown file was its source.** The highlighter was never the problem — a
probe against the installed `muster-content` theme resolves headings, list
markers, links, emphasis, and the fence's own language through Markdown's
injections. What was missing is that Legacy *rendered* `.md` files
(`legacy/src/components/DocumentPane.vue:242`) and this window had no second
reading of a document at all.

**"Binary file; text diff is unavailable."** The notice was Legacy's and it is
right about the encoding and silent about the change: a picture added, a picture
removed and a picture replaced are three different things to somebody reviewing
a branch. Git's own status letter was already on the row, and the bytes were
already in the private snapshot this diff reads — so both answers were being
thrown away.

**The panels could not be read to the end.** Names were shortened to the panel's
width with an ellipsis and there was no sideways scroll, where Legacy's rows are
`max-content` inside an `overflow: auto` scroller. Measured on a 280px panel: a
file name cut at 212px with nothing to scroll to; in the change list, a group
heading 24px wide and a long path running 667px past a 279px panel.

**The folder stack answered for the row under it.** Drawn over the rows it
names, with no hitbox of its own: clicking a stacked folder's name selected
`src/nested/f12.txt`, a file whose own row sat at y = −207, above the top of the
tree.

## What changed

| File | Change |
| --- | --- |
| `core/src/files.rs` | `external_path` resolves a checkout-relative name to the canonical path another program may be handed, through the same `O_NOFOLLOW` walk a preview uses. A link is refused rather than resolved. |
| `core/src/opener.rs` | `open_path` and `open_url`: one argument, no shell, only `http`/`https` for a link, and the exit status read rather than assumed. The opener is a parameter so the tests can run a process whose ending they decide. |
| `native/src/inspector.rs` | A tree row answers for itself: ctrl (cmd on macOS) hands it to the platform and never also selects or expands it. What the opener said is drawn under the tree, with a Dismiss. Rows carry the width of the widest one and the tree scrolls sideways. `collapse_directory` is one gesture from a row or from a stacked row. |
| `native/src/document.rs` | A Markdown file is drawn by the toolkit's `TextView` — headings, lists, tables, fences coloured by the installed theme, front matter through `FrontmatterPlugin` — with its source one header click away. A link clicked in the rendered view goes to the platform through the same opener, refused if it is not a web address. A refused read offers "Open externally". |
| `native/src/document/media.rs` | The pictures a document names, and the line that stands where one is not drawn: the alt text the author wrote, or `Video: assets/demo.mp4`. |
| `core/src/navigation/changes.rs` | `FileDiff::image`: the format, read from the bytes Git refused to print, with the name as the answer only when there are no bytes left to ask. |
| `native/src/diff.rs` | The notice names the change: "Image added · png.", "Image removed · jpeg.", "Image changed · webp.", and the encoding's own sentence for a file that is not a picture. |
| `native/src/app.rs` | `measured_width`: a text width asked once per name and size. |

## How it is verified

`cargo test --locked --workspace`, `cargo clippy --locked --workspace
--all-targets -- -D warnings` and `cargo fmt --all --check`. `core` holds 106
tests and `native` 276. New ones, and what each is for:

- `an_external_path_is_the_canonical_one_inside_the_checkout` and
  `a_fifo_is_not_something_to_open` — the chain that decides what may be opened:
  `..`, an absolute path, `.git`, a link out of the checkout and a forged
  checkout are all refused before anything is spawned, and a FIFO is not handed
  to a desktop application because opening one parks forever.
- `only_an_http_or_https_link_is_a_thing_this_will_open`,
  `a_link_is_read_by_what_the_process_answered`,
  `a_file_is_opened_by_path_and_refuses_what_would_be_read_as_a_flag` — the
  opener, against `/usr/bin/true`, `/usr/bin/false` and a name nothing answers
  for, including a path with a space in it.
- `a_modifier_is_what_separates_opening_a_row_from_acting_on_it` — which
  modifier asks for the other reading, from a real click event rather than a
  key name.
- `a_markdown_document_opens_rendered_and_reads_as_source_on_request` — the
  rendered reading, the toggle, and the source still whole underneath.
- `a_rendered_document_draws_its_picture_and_says_what_it_cannot` — a picture
  inside the checkout is found in the window where the document names it, and
  the reading's own text carries `Video: assets/demo.mp4` and
  `Image: ../outside.png` for the two a document names that are not pictures.
- `a_name_is_written_against_the_documents_own_directory`,
  `a_name_that_reaches_out_of_the_checkout_is_no_name`,
  `the_alt_text_is_the_line_when_there_is_one` and
  `a_picture_the_toolkit_draws_is_left_to_the_toolkit` — the resolution of a
  name against the document's directory, what it refuses (`..` past the root,
  `/etc/passwd`, a scheme, a Windows spelling, a broken escape), the line that
  stands in for a picture, and the two URLs the toolkit draws better than this
  can.
- `an_image_change_names_the_picture_rather_than_the_encoding` — the bytes
  decide over the name, and the name answers only when there are no bytes.
- `a_change_with_no_text_says_which_one_it_is` — the sentence, for all three
  directions and for a file that is not a picture.
- `a_name_too_long_for_the_panel_is_scrolled_to_not_cut` and
  `a_change_row_fills_the_panel_and_a_long_one_is_scrolled_to` — the scroll is
  asserted as the scroll handle's own maximum offset, not as pixels: a name
  wider than the panel reaches `max_offset().x > 0`, a tree that fits reaches
  `0`, and every row of the change list is the width of the widest one.
- `a_stacked_folder_keeps_its_own_click` — the defect, measured the way it was
  found.

## What is still different

- **The ctrl-click's spawn is not exercised by a test.** The row's other answer
  opens a real application, and a headless run would open one on the machine
  running the suite. What the gesture *is*, and what the path resolution
  refuses, are both tested without one.
- **A rendered Markdown file draws its pictures and says everything else.** The
  toolkit's own image path resolves a URL against the process rather than
  against the document, so this claims the image nodes and resolves them through
  `core::files::external_path`; a picture is drawn from the canonical path and
  capped at 720px on each edge, because the toolkit measures an inline element
  against `MaxContent` and an element cannot be scaled once measured. A `data:`
  picture and an `http(s)` picture are left to the toolkit, which decodes and
  fetches both; raw HTML `<img>` and `<video>` are not claimed, because the
  toolkit's HTML parser does not offer its `<img>` to a Markdown extension.
- **A video is named, not played.** There is no video surface in the window and
  no decoder wired to one, so a `![](clip.mp4)` is `Video: clip.mp4` or its own
  alt text.
- **A picture in a diff is named, not shown.** `core::navigation::changes`
  carries no image bytes by design — "binary and refused patches are empty" —
  so a two-up before/after would mean changing that contract, not adding a view.
- **The Markdown header toggle is one control**, where Legacy had a view/edit
  mode. The document pane is read-only here, so the two readings are the whole
  of what it can offer.
- **XML, INI and Dockerfile still open as plain text.** Vue, GraphQL and
  Protobuf have grammars; those three would each need a query this crate would
  have to carry, because the toolkit ships no grammar and no feature for them.