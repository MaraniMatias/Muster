# Changelog

One section per released version, newest first. The release workflow prepends the section matching
the version it is publishing to the notes GitHub generates from the commits, so a section here is
the part a reader should read and the generated list is the part they can skip. Keep the newest work
under `Unreleased` and rename that heading to the version when you tag it; a version with no section
here still gets its generated notes.

## Unreleased

- **The terminal's history is the shell's own history again.** Muster starts the
  shell in a directory of its own so it can read the person's startup files, and
  zsh picks the file it keeps the command history in from that directory before
  it reads a single startup file. The history was therefore a file of the app's
  own: empty on every launch, and thrown away when the app closed, so the up
  arrow had nothing to walk through. The shell now keeps its history where
  every other terminal on the machine keeps it, and the up arrow walks the same
  commands it walks in one.

- **A terminal's row follows the worktree it moved to.** A terminal whose shell
  changed directory into another worktree stayed filed under the one it was
  opened in, so the panel said where it was started rather than where it was
  working. The follow pass reads the agent's work from the OpenCode service,
  which answers while the pass is on its way, and the pass threw away its
  whole answer whenever that answer was not the one it started with. The two
  questions are decided separately now: what the service says about an agent's
  work no longer throws away where a shell is sitting, and a title that
  changed in the meantime does not either.

- **The worktree's warning says which commits are not merged.** Deleting a
  worktree warns about the commits its branch holds that the default branch
  does not, and the warning called them "unmerged commits" -- a word for the
  state of a merge rather than for the commits themselves -- and the second
  place it appeared did not say which branch they were not merged into. It now
  says how many commits are not merged into the default branch, and names that
  branch.

## 1.0.0

- **Muster 1.0.0 is the first stable release.** The native build is the whole
  application now: the workspace window with its workdirs and their terminals,
  the VT terminal, the file tree and the document editor, and the review
  composer with its notes on added and removed lines — exported as Markdown,
  sent to an OpenCode session, or handed to a new Pi session.

- **It ships for macOS and for Linux.** macOS 15 and later, on Apple Silicon
  and on Intel, as an app inside a ZIP; Ubuntu 24.04, on x86_64 and on arm64,
  as a package that installs the app, its icon and its desktop entry.

## 1.0.0-rc.18

- **The Files and Changes panel can be moved under the sidebar.** The window
  had two panels beside each other — the workdirs and their terminals on the
  left, Files and Changes on the right — and the panel on the right took room
  the body could have used. A button in that panel's own header now moves it
  under the sidebar, so the window has one column: the workdirs and their
  terminals above, the file tree and the change set below it, with a divider
  between the two that can be dragged. The same button brings the panel back to
  its own column, and the workspace file keeps the shape the window was left in.

## 1.0.0-rc.17

- **An SVG file is read as a drawing, and its markup is one click away.** A
  drawing opened in the document pane was source and nothing else, because the
  app had no reading for it. An SVG file now opens as the drawing it is — the
  markup the file holds, drawn by the toolkit — and the header offers the source
  beside it, the way a Markdown file offers its rendered reading. The two are
  one document rather than two reads of the file, and the source is the same
  text with the file's own markup highlighted.

- **An SVG file is read with its markup.** A drawing opened in the document pane
  was plain text, because the toolkit ships no SVG grammar and nothing else in
  the app could read one. SVG is XML, so the pinned upstream XML grammar reads it
  and is registered under SVG's own name: the picker says **SVG**, and the
  file's elements, attributes, values and comments wear the colours they mean.

- **An OpenCode row says what its own terminal is doing.** The TUI reports
  through OSC 7501, the same escape sequence Pi uses, and the report says what
  *this* terminal is doing and which session it is showing -- the one thing the
  service cannot say, because the service has no way to know which of its
  sessions a terminal is showing. The row draws that now: the spinner while a
  turn runs and the question while it waits are the terminal's own claim, and a
  terminal at OpenCode's home screen says so instead of borrowing the state of
  a session that belongs to another terminal. The agent's name and its colour
  still come from the service, for the session the report names.

- **An OpenCode row wears the colour of its own agent.** The panel read the
  service's pending lists for eight of the locations the service named and
  rotated through the rest, so a session in a checkout with more than a handful
  of worktrees around it was *unknown* most of the time: the row said its state
  was unavailable and wore the warning ink instead of the colour the agent
  paints itself with. Every location is asked about now, bounded by the
  observation's deadline rather than by a count of locations, so the row says
  what its session is doing — idle, working, waiting for an answer — and wears
  the agent's colour while it says it. The row's name and its tooltip carry the
  agent's name and the time since the service last touched the session, so
  what the row shows is what it announces.

## 1.0.0-rc.16

- **The filesystem root is not a workdir.** The panel showed a workdir named
  `/`. An application opened from the desktop starts in the root of the
  filesystem, and that directory was offered as a workdir and kept in the
  workspace file, so it came back on every launch and came back after it had
  been removed. The root holds every other directory rather than being one of
  them, so it is not opened, not restored and not written to the file. Opening
  it by hand is refused, and says why.

- **The workdir's menu no longer moves the workdir.** **Move up** and **Move
  down** are gone from it, so the menu holds only what the workdir can be asked
  for: a new worktree, an archived worktree to restore, and its removal from
  the panel. The panel's order is still the drag's.

## 1.0.0-rc.15

- **The workdirs can be put in the order you want them in.** The panel drew
  the workdirs in the order they were opened in, and there was no way to say
  otherwise. A workdir is now picked up by its heading and put down in another
  place, and the heading under the pointer says where it will go: the upper
  half of a heading is the place above that workdir and the lower half is the
  place below it, with a line drawn where the workdir will be. The order is
  the workspace file's order, so it is the order the next launch draws. The
  home workdir is where the panel starts: it does not move, and nothing is put
  above it. The same move is in the workdir's menu, as **Move up** and **Move
  down**, so it is reached with the keyboard as well as with the pointer.

## 1.0.0-rc.14

- **A `.feature` file is read with Gherkin's own words.** A Cucumber feature
  file is prose with a few words of structure, and it opened as plain text. The
  word that opens a header and the word a step begins with wear the keywords'
  colour, the title a header carries wears a title's, a tag and a comment and a
  quoted string wear theirs, and the prose in between stays prose.

- **The document's header copies the file's path.** The name in the header is a
  name inside the checkout, and a path somebody pastes into a terminal or a
  message is the file's own path on the disk. An icon beside the name puts it on
  the clipboard, the way the diff's title does for a change.

- **The delete action stands on its own in the worktree dialog's footer.** The
  destructive action is the one button in that footer that cannot be taken
  back, so it no longer sits among the two that keep the worktree: it goes to
  the far end and the safe actions keep the other.

- **The preferences are in four sections, and the panel is a card with a
  sidebar.** Interface, Terminal, Editor and Review notes are the four
  sections, and the panel draws a sidebar of them on the left and the rows of
  the one on screen on the right: a card of a fixed size, where the sidebar
  does not change with the section and a section with more rows than fit
  scrolls inside the card rather than growing it. A section is a button, so it
  is reached with the keyboard as well as with the pointer, and the section on
  screen wears its own surface and ink rather than an accent bar. The section
  on screen is window state, like the draft, and is never written to the file.

- **A control wears an edge you can see.** The toolkit paints a field's
  border, an ordinary button's and an unchecked checkbox's box with one ink,
  and at the value the overlays were closed with that ink was the surface the
  control is drawn on: a checkbox nobody can see is a checkbox nobody can find.
  A control's own edge is a step stronger than the overlay hairline now, and it
  is visible on every surface the app draws a control on.

- **The shell starts at the size the grid is drawn at.** A terminal was opened
  at a size it was never going to be shown at, and then corrected once the
  window had measured the pane: the shell was asked for another size, and a
  shell that is asked for another size redraws its prompt from the cursor,
  which leaves the first row of the old prompt on screen. The PTY is now opened
  without its child, and the child starts once the size it will keep is known:
  the grid and the PTY are given that size before the fork, so the prompt is
  drawn once, in the grid it is shown in. A terminal that is not the one on
  screen is never measured, so the window tells it the size of the pane it
  shares. The scrollbar reads its preference now: Hidden draws no thumb, While
  scrolling draws one where there is somewhere to scroll to, and Always draws
  one even when there is nothing to scroll.

## 1.0.0-rc.13

- **A row is the font size and not the face's line box.** The grid's row was
  the face's own ascent and descent multiplied by the line height ratio, and a
  face's line box is already taller than the em it is drawn in -- Fira Code's
  is 1.23 of it -- so every row was a quarter taller than the glyph in it. The
  glyph was painted at the foot of the cell as well, which left a gap above
  every row a block element filled, so a bar of blocks read as a row of
  separate tiles. A row is the font size times the ratio now, and the line box
  starts at the top of the cell, so a block fills the row it is in.

- **The file beside the terminal is set like the code it sits next to.** The
  document's rows were drawn at the toolkit's one and a half times the size,
  which is a quarter taller than the grid's, so a file read beside a terminal
  was set apart from it. The document is drawn at the grid's own ratio now.

- **The grid and the file beside it have a size each.** The panel's terminal
  size is the size the grid is drawn at and its editor size is the size the
  document and the diff are drawn at, so a preference is never a number
  nothing draws at. Both are read when the app starts, so a size that changes
  needs a restart, and each row says so.

## 1.0.0-rc.12

- **A terminal's row follows the work it is doing.** The panel filed a terminal
  under the worktree it was opened in and left it there when the work behind it
  moved: a shell that `cd`s into a sibling worktree, or an agent session that
  starts working in one. The row follows both now -- the directory the shell is
  sitting in, and the place an agent is working. Legacy followed the agent's work
  out of the box; the shell's own directory is the other half of the same question,
  and this build follows it too. What moves is the row: the process, its scrollback
  and its window stay exactly where they are, and nothing here types a `cd` into a
  shell. Settings says which of the two is followed, and `off` still means neither.

- **A path in the terminal opens in the editor.** Ctrl+click -- cmd+click on
  macOS -- on a path the terminal printed opens that file in the preview, and on
  an address it opens the browser. The name is read the way the shell reads it:
  a bare name is the file where the shell is sitting, because an `ls` prints
  `LICENSE` with no directory on it. Only a file the worktree holds is opened,
  so a path that leaves it opens nothing. A plain click is still a click that
  selects nothing: terminal output is not the person's own text, and nothing in
  a build log opens a browser on its own.

- **The diff's title copies the path it names.** A change on screen is named by
  its path, and that path is what somebody pastes into a terminal, a ticket or
  a message. An icon beside it puts it on the clipboard, the way the document
  toolbar did: the copy is of the very path the title shows, and a whole change
  set names no file, so it has nothing to copy.

- **A note's lines stay washed on the diff.** The wash that says which lines a
  comment is about belonged to the draft: saving the note dropped it, so the
  diff went back to saying nothing about those lines while the note's card
  named them in a header. The wash belongs to the note now. It is kept when the
  note is applied, another note takes the mark over, a dropped draft gives it
  back to the note that was applied last, and a deleted note takes its lines
  with it. The toolkit's selection is a text selection, so anything that clears
  the window's selection takes the wash with it: the window puts it back while
  the diff has nothing selected, and a selection of the person's own is left
  alone.

- **The characters a modifier spells reach the terminal as text.** A Mac layout
  spells a symbol with Option -- ⌥2 is @ on a Spanish layout and ⌥E is ~, and
  ⌥⇧H is ◊ on a US one -- and the terminal only knew the Option+Shift spelling
  of that: Option on its own was the meta key, so the character was never sent.
  The character the platform produced is the input now, and the terminal's meta
  prefix does not turn it into a command the shell never asked for. Option on a
  key that carries no character of its own stays the meta key, which is what a
  shell reads as ESC. Ctrl with Option is the layout's third level -- that is
  how Windows and Linux report AltGr -- and the character spelled there is a
  character and not a control code.

- **The terminal and the file beside it are drawn at the same size.** The grid
  was 16 pixels and the editor and the diff were 13, so the file pane read as
  set in a smaller type than the terminal it sat beside. The grid is 13 pixels
  now -- the size Ghostty and iTerm2 use on macOS -- and the editor and the
  diff read that same size from it, so a file and a change are read at the size
  of the terminal next to them.

## 1.0.0-rc.11

- **Every release carries a build for every supported platform.** The release
  pipeline built two artifacts: a macOS bundle on an Apple Silicon runner and an
  Ubuntu package on an x86_64 one. An Intel Mac and an ARM64 Linux had no
  build at all, so a candidate could not be installed on either. There are four
  now -- macOS on Apple Silicon and on Intel, Ubuntu on x86_64 and on arm64 --
  and each one is built on a runner of its own architecture, so the binary
  inside a package is the one that runner's toolchain made. A package that
  names one architecture and carries the other's installs and then refuses to
  start, so the release check refuses to publish one.

## 1.0.0-rc.10

- **On Linux the window manager draws the frame.** The app asked for
  client-side decorations, and the band that reserved was the toolkit's own:
  twenty pixels on every side, for the shadow those decorations draw. That band
  was a margin around the app -- the window's controls sat twenty pixels away
  from the corner the pointer goes to, and the band was gone when the window
  was maximized, so the app looked different maximized than it does not. The
  controls those decorations draw did not answer the click either, so minimize,
  maximize and close were three buttons that did nothing. The window manager
  draws the frame now: the app fills the window, and the window's controls are
  the ones the manager draws, which are the ones that work. The app's own bar
  sits under the manager's caption, and it is still the bar that carries the
  crumbs and the window's actions.

## 1.0.0-rc.9

- **The app fills the window on Linux.** The window's own frame kept a
  twenty-pixel band for a shadow the window manager draws, and that band was a
  margin around the app: the window's controls sat away from the corner the
  pointer goes to, and the band was gone when the window was maximized, so the
  app looked different maximized than it does not. The frame reserves nothing
  now, and the app fills the window either way.

- **A directory can be opened where the picker cannot.** The native picker is
  the desktop portal on Linux, which is a service the desktop session may not
  run: without it, adding a directory was a button that did nothing. The path
  is asked for instead when the picker cannot open, and the package now
  depends on the portal so that a fresh install has one.

## 1.0.0-rc.8

- **The chrome wears the toolkit's own controls.** Buttons, checkboxes and
  fields are drawn by the toolkit rather than by a hand-rolled helper, so the
  shape, the states and the focus are one implementation instead of two that
  drift apart. A control is rounded 4px, and the small overlays the toolkit
  draws itself -- a menu, a popover, a picker's panel -- wear the same corner;
  the large surfaces over the window -- a dialog, a notification, a command
  palette -- are 8px.

- **Keyboard focus is a line, not a ring.** The 3px outer ring is off: the rows
  in this window clip their content, so the ring was cut off exactly where a
  keyboard user needs to see it. A focused control shows the toolkit's thin
  focus on its own edge now, and no control suppresses it.

- **A button says what it is.** The destructive action is `danger`, the commit
  action is `primary`, and the quiet ones are `ghost`. A disabled control is
  the toolkit's disabled control, so it stays visibly disabled without turning
  a destructive action into a misleading neutral one.

- **The worktree removal dialog reads its buttons in the order the decision
  works.** `Delete worktree` comes first and stands apart from `Cancel` and
  `Archive`: it is the one action there that cannot be taken back.

## 1.0.0-rc.7

- Prepare the next native testing candidate for Ubuntu 24.04 amd64 and macOS
  arm64. No application behavior changes from rc.6.

## 1.0.0-rc.6

- Render inline images from Pi and OpenCode through the Kitty graphics protocol,
  including chunked PNG/RGB/RGBA uploads, compressed pixels, cropped placements
  and Unicode placeholders. Images follow terminal output and are released from
  the GPU atlas when no longer visible. Pi receives an explicit image capability
  unless the user already configured one; the terminal still identifies as Muster.

- **Apply does not freeze the panel.** The preferences file is written on a
  background task, so the panel keeps answering while it is written: the button
  says `Saving…`, the fields are disabled, and a second Apply is refused. A
  write that fails keeps the draft and says what went wrong, and a write that
  finishes after its draft was replaced cannot close the draft that took its
  place.

- **A review that cannot be claimed is still there to claim.** The review round
  store bounds the wait for its lock: an ordinary change gives up after a
  quarter of a second and changes nothing, and a send waits for the client's own
  deadline -- at most five seconds -- rather than for as long as another
  window's service call takes. A cancelled or expired client refuses the send
  before it claims the notes it would have carried.

- **A formatter that keeps a pipe open cannot pin the editor.** Prettier runs
  with one deadline for its three pipes and reads them without blocking, so a
  descendant that holds one cannot keep the reader past that deadline. The
  child is given its own process group, and the whole group is killed when the
  format is abandoned.

- **The release pipeline reads the bundle's own floor.** The finalization job
  reads the minimum macOS version out of the packaged arm64 binary's own Mach-O
  load commands and refuses a bundle that declares a different one, so the
  floor in `Info.plist`, the floor the build enforces and the floor the binary
  states are one decision that is checked rather than assumed. Every action in
  the release workflow is pinned to a commit, and each job asks for the least
  permission it needs.

## 1.0.0-rc.4

- **A Mac layout's symbols are typed, not prefixed.** ⌥⇧H is ◊ on a US
  layout, and the terminal now sends that character to the shell as text
  instead of prefixing it with the meta escape a shell reads as a command.
  Option on its own is still the meta key, which is what ESC in front of a
  key means.

- **Pi is a review destination, and its state is on the panel.** Pi reports
  through OSC 7501, and the terminal now reads it: a bounded payload, a
  handshake answered with the same question, and a report believed only while
  the operating system says Pi is what is in front of the shell. The sidebar
  row for a Pi terminal wears Pi's glyph, a spinner while a turn is running,
  and the words `working`, `task complete` or `last task failed`. A review can
  be sent to Pi as well: it is written to a private file under the app's own
  folder and handed to a new Pi terminal in the checkout as its first prompt.
  There is no queue, no session to choose and nothing marked as sent -- a send
  opens a terminal, and the terminal is the whole of what happened. A prompt
  that no Pi read -- a terminal that failed to start, a window that went away
  before the terminal opened -- is removed rather than left behind. The
  per-checkout review destination in `~/.muster/opencode-rounds.json` carries
  `pi` beside `markdown` and `opencode`, which is a change to that file's
  shape: it is made in place, and a file this build cannot read is refused by
  name.

- **The legacy tree is gone, and the native app is the whole app.** The old
  Tauri source, its schemas and the crate that held them were deleted; the
  workspace is now the app crate and the core crate, with a virtual manifest
  at the root. Nothing reads the old SQLite workspace any more: the native one
  is `~/.muster/native-workspace.json`, and a stored shape is the whole
  contract -- it is changed in place, and a file this build cannot read is
  refused by name.

- **The sidebar says what the pane is on.** A file preview and a diff become
  rows under their worktree, below its terminals and above **New terminal**,
  and the row that names what the pane is showing is the one that wears the
  selection. A file being edited and a diff holding notes are rows too, so
  unfinished work is visible without leaving the pane.

- **OpenCode is a review destination, and a send is safe to retry.** A review
  can be handed to a running OpenCode session; a busy session is asked about
  rather than interrupted, and a queued round is written down before anything
  is offered, so a round that waits is a round that provably was never taken.

## 1.0.0-rc.3

- Ship both the native macOS Apple Silicon ZIP and an Ubuntu 24.04 x86_64
  Debian package. Linux ARM64, AppImage and older distributions are not included.
- Validate formatting, clippy, workspace tests, the RustSec audit and release
  builds on both platforms before publishing. Install the Linux package in a
  clean runtime-only container and require the app to open a window under Xvfb.
- Publish the exact checksummed artifacts built from the tagged source commit,
  with matching versions and architectures. Candidate releases remain prereleases
  and do not replace the latest stable release.
- Correct Linux clipboard tests to use Ctrl+Shift+V and give the Git shared-budget
  test realistic CI scheduling margins without changing production read limits.

## 1.0.0-rc.2

- **The native app compiles and links on Linux.** The `linux-build` job installs
  every system library the native crate links against -- Vulkan for wgpu, X11 and
  xcb for the window, XKB and its X11 half for keys, freetype for text -- plus
  `libfontconfig1-dev`, which is the one that fails as a build-script panic on a
  missing `fontconfig.pc` rather than as a link error anybody can read. That job
  is the reason the workflow carries one: macOS never compiles the Linux branches
  of the code, so without it nothing here would notice them rotting until
  somebody tried to build on Linux.

- **Closing a busy terminal asks, and the test that pins it waits for the
  answer it is about.** The prompt is the last status poll's reading of the
  foreground process group, and the test clicked close without waiting for that
  reading. On a loaded runner the poll had not seen the foreground process yet,
  the terminal read as idle, and a close that correctly asked nothing failed a
  test that asserted a prompt. It waits for the busy answer now -- the premise
  the assertion rests on -- rather than assuming it.

## 1.0.0-rc.1

- **Ctrl+Shift+<key> now reaches the program in the terminal.** xterm.js 6 answers the keys it has an
  escape for and drops the rest: `Ctrl+Shift+A` sent no bytes at all, where `Ctrl+Shift+F5` went out
  as `ESC [ 15 ; 6 ~` with the shift in it. The letter is what went missing, because it is the one
  key xterm turns into a control character and the shift it cannot put on a control character is
  dropped with it — so a program bound to `Ctrl+Shift+<key>` never heard it pressed and the shortcut
  did nothing. The terminal now answers that chord itself, in the encoding the program chose: one
  that pushed the keyboard protocol gets `ESC [ 97 ; 6 u` and the whole chord, and one that never
  announced anything is left with xterm's own answer rather than one invented here. This is the
  same path `Shift+Enter` already took, and it means the shortcut only exists behind a reader that
  asked to be told about keys at all — no terminal can send a chord no program has agreed to
  distinguish.

- **There is one stored shape, and the file carries no version.** Both state
  files in `~/.muster` -- the native workspace and the review round store --
  carry no `schemaVersion` at all: every field is required, no field is unknown,
  and the shape itself is the whole contract. A shape is changed in place, a file
  this build cannot read is refused by name and never written over, and nothing
  left behind reads an older file: the navigation store no longer parses a second
  time to accept a file that carries no version at all, and a stamped file is
  refused like any other one this build does not know.

- **A document can be edited, and a save says what it wrote.** The document pane
  is its editor: type in it, and a bar appears along the bottom with Cancel and
  Save — `Esc` and `⌘S` are the same two controls. Cancel puts the text the file
  was read with back; Save writes what the editor holds, through the same walk
  every read of a checkout is answered by: a path outside the checkout, a link, a
  file somebody else wrote since it was opened, or a file that is read-only are
  all refused in as many words rather than overwritten, and what is written is a
  replacement rather than a rewrite, so a reader sees the old text or the new one.
  A Markdown document opens as its reading and is edited as its source, because
  the reading is a parse of the text as it was read.

- **A file with changes offers them from its own document.** When Git has said
  this file is in the change set, the document's header carries a **Changes**
  button with the counts beside it, and it opens that one file's diff rather than
  the whole set. A file Git has not answered about offers nothing, because a
  button that appears and disappears with a read is worse than one that waits.

- **A picture, a video and a file's name stand apart from the text.** The toolkit
  measures an inline element as a box of its own, so the air above and below a
  picture — and around the words that stand in for what cannot be drawn — is
  inside that box. A document's SVG is drawn like any other picture: it is the
  one format this window takes by name rather than by its bytes.

- **The app has its icon back on macOS.** An executable carries no icon there: the
  icon is what the bundle around it says it is. `scripts/bundle-macos.sh` builds
  `Muster.app` with the same bundle identifier, name, copyright and artwork the
  Tauri build shipped, so the OS files the native shell as the same application
  rather than as a second one that happens to look like it. Two fields differ on
  purpose: the bundle declares macOS 15 as its floor, which is what the toolkit
  this app is built on needs rather than what the old webview build needed, and
  the build now enforces that floor instead of only promising it, so an API newer
  than 15.0 is a build error rather than a crash on someone else's Mac. The
  version is whatever `native/Cargo.toml` declares.

- **There is a menu bar on macOS.** The app menu carries About, Services, Hide,
  Hide Others and Quit, and the Edit submenu carries Undo through Select All —
  the same two submenus the Tauri build had. Edit is not decoration there: those
  are the key equivalents `⌘C`, `⌘V`, `⌘X`, `⌘A` and `⌘Z` are answered by, and on
  macOS they are answered before the key reaches the window, so the terminal
  answers them as the toolkit's copy and paste actions as well as in its own key
  handling. Either way the grid copies its selection and pastes into the child.

- **About Muster says what it always said.** The icon, the name, the version, the
  copyright and the repository, in the order macOS shows them, in a panel of its
  own that closes with `⌘W`.

- **Where an exported review round lands is a preference.** Settings has a "Review notes" section
  with a selector for it: `Default` keeps them in `~/.muster/tmp/reviews`, and `Work directory`
  keeps them in `.muster/reviews` inside the checkout the review was made in — the repository or
  worktree in front of you. The folder the selector names is written out underneath it, with how
  many exported notes it holds and how much they weigh, and in the default folder there is a button
  that deletes them: it asks first, and it removes only the files an export wrote, so anything else
  you have put in that folder stays. A working directory's notes belong to the repository, so there
  is no button there and the command refuses one anyway.

- **The app is called Muster.** The name changed together with everything that carries it: the
  product, the release artifacts, the paths on disk and the data behind them. The bundle identifier
  went from `dev.marvis.workspace` to `dev.muster.workspace`, and that is what moves the app-data
  directory, the log directory, and the filenames of the Linux packages — `muster_<version>_.deb`
  and `Muster_<version>_.AppImage`.

- **Preferences move by hand; the workspace does not move at all.** Settings live in
  `~/.muster/config.yml` now and exported review rounds in `~/.muster/tmp/reviews/`, so copy
  across whatever is worth keeping — nothing reads the old folders. Reviews already exported under
  the `code-reviews` name this section used to carry stay there and are not moved or read: at
  `0.x` a path change is made in place, and the ones still worth having are worth copying by hand.
  The database is
  `muster.sqlite3` and it starts empty: no migration runs in either direction, so the repositories
  you registered, your review notes and your rounds stay in the old `marvis.sqlite3` on disk. That
  file is still an ordinary SQLite database if you need anything out of it; this build only refuses
  a schema stamped with a version it does not know, which is the rule that has always governed it.

- **An old `Marvis.app` stays installed next to `Muster.app`.** Two bundle identifiers are two
  applications to macOS: neither replaces the other, and each keeps its own dock tile and its own
  data. Removing the old one is a manual step.

- **Why it was done in one go.** The project is still `0.x`, so a change that moves paths is made in
  place instead of being carried by a deprecation window: nothing has to read what an older build
  wrote, and a compatibility layer kept "one release" would only be code nobody exercises.

- Preview images and videos directly; switch SVGs between an editable source and a live preview.

- **Settings now says what the keyboard does.** The chords the window answers were written down in
  the source and nowhere else, so learning them meant reading `App.vue`. There is now a Shortcuts
  section in Settings that writes them down: the side panels, a new terminal and the three zoom
  keys, then the few keys that mean something only where they are pressed — confirming a session
  name, `Esc`, ctrl/click on a terminal link, and the inspector's `Home`/`End`. Each chord is drawn
  the way the platform it is read on presses it, so a Mac shows `⌘/` and everything else shows
  `Ctrl+/` rather than both, because a chord printed the other way round is a key nobody has. It is
  a list and nothing more: no chord is configurable, and none of it is written to `config.yml`.

- **F2 no longer pretends to rename a session.** The sidebar rows carried `@keydown.f2`, which
  could only fire on a row that already had the focus, and a click does not focus a row: on macOS
  the webview does not move focus to a button it is clicked, so the binding was reachable only by
  tabbing to a row nobody tabs to. Double-click still renames. The binding is gone rather than left
  where it is a lie about a shortcut Settings now lists.

- **About says which build it is, and stops leading with the licences.** The section opened with an
  acknowledgement and four groups of credits — artwork, colours, the typeface and every library —
  which is longer than the rest of it and is the answer to a question nobody opened Settings with.
  The version is now the first line, read from the binary rather than written into the dialog, so it
  cannot name a release behind the app it is in; a build that cannot read its own version draws no
  version rather than one that is nearly right. The credits are still all there, folded into a
  "Credits and licences" row that says what is inside it. The notices themselves were never only
  there: the full licence texts ship as `THIRD-PARTY-NOTICES.txt` inside the bundle, which is where
  a notice has to travel to count as one.

- **A URL in the terminal can now be clicked open in the browser.** The terminal underlines the paths
  it can open in the preview, and it refused to underline an address because `https://…` names no file
  in the checkout — which is true, and left the one thing a terminal prints most often (`curl`, a
  deprecation warning, an npm error pointing at its own docs) as the one thing it could not follow.
  An address is now underlined next to those paths and ctrl/click opens it in the browser, the same
  key and the same key-only rule the paths have: a plain click still selects, because a terminal
  prints what other programs wrote and nothing in a build log should be able to open a browser by
  itself. Only `http` and `https` are underlined — `file:///…`, `vscode://…` and `mailto:` name an app
  or a file rather than a page, and are left alone here exactly as they already were in the preview.
  A path and an address on the same line are both offered, and an address costs no lookup on the disk
  the way a path does, so the underline appears as soon as the mouse arrives.

- **Closing a terminal no longer leaves the sidebar pointing at nothing while the panel shows
  something.** The selected terminal is stored in the database, and everything that makes that stored
  selection stop naming a terminal clears it: closing the terminal that was selected, and opening
  another workdir. The row in the sidebar read that stored id on its own, so it marked nothing there,
  while the main panel — which kept the terminal it opened last — carried on showing one. Same panel,
  two answers: the workdir row wore the selection the terminal row used to, and the crumb in the
  titlebar kept naming a terminal the sidebar said was not there. There is now one rule for which
  terminal the window means — the selected one, or the newest the workdir has — and the sidebar row,
  the main panel, the titlebar menu and the crumb all read that same answer. The panel is unchanged
  (it already showed the newest); what changed is that the sidebar, the menu and the crumb now agree
  with it, and they also agree when switching workdirs.

- **A right-click no longer opens the system menu.** Marvis is a webview, and the webview answers a
  right-click with a menu of its own — Reload, Look Up, Inspect Element — which is how an unsaved
  draft in the editor used to go away without a word. That menu was kept where text is selected
  from, because it is where Cut, Copy and Paste arrive greyed out when there is nothing to act on.
  It is kept nowhere now: while a process is watching the mouse — a screen recorder, a clip tool, a
  remote-desktop client — that right-click opens a second menu next to the first, one with Insert
  Emoji in it, so both were drawn where a capture was running and only one was drawn where it was
  not. The webview is never told that something is watching the mouse, so there is no way to tell
  those two clicks apart. Cut, Copy, Paste and Select All are still in the Edit menu and still on
  ⌘X, ⌘C, ⌘V and ⌘A, and the right-click menus of the sidebar are the app's own.

## 0.13.2

- **A Git command no longer makes every checkout be read again.** Marvis watches each repository to
  know when the files of a checkout moved, and on Linux the watcher reports every _read_ of a
  watched file as an event of its own. Git reads `.git/HEAD` before it does anything, so any command
  run inside a watched repository — a `git status` in a terminal, an agent asking what changed — was
  answered as though a branch had moved: the numbers on every checkout were dropped and recomputed,
  and the sidebar refreshed, for a repository nothing had changed in. A read is no longer taken for a
  change, so only a real write says anything now.

- **The terminal cursor is a block you can change.** It is drawn in a color of its own: a lavender on
  the dark palette and the page's own ink on the light one, which is the ink a cell has to be
  filled with there for the caret to stand off a near-white surface. The glyph under the block is
  the surface, so a cell is drawn inverted, and the editor's caret is the same block in the same
  color. Shape (`Block`, `Bar`, `Underline`) and blinking are preferences in the terminal section of
  Settings, defaulting to a blinking block. `terminal.cursorStyle` in `~/.marvis/config.yml` says
  the same thing. A shape a program asks for through DECSCUSR still wins over the preference, as
  it always did.

- **Home is always the first workdir.** Marvis opens a shell there on launch, keeps it running when
  you switch workdirs, and lets you close it like any other terminal. The Home row cannot be removed;
  regular folders with the same name remain ordinary removable workdirs.

- **Settings has an About section that says what Marvis is made of.** At the bottom of the dialog:
  the Catppuccin file icons, the Lucide icons in the interface, Zed's One palettes, the bundled
  Fira Code, and the libraries the window is assembled from — each with the licence it travels
  under, and the repository at the end. Every one of those licences asks its notice to travel with
  the software, and until now the notices were only in the source: the Catppuccin copyright in the
  header of a generated file, the font's OFL beside files Vite never bundles, the Zed credit in a
  comment above a color block. Somebody with the app installed had none of them. The list is checked
  against `package.json`, so swapping a dependency out fails a test instead of leaving a credit
  naming something that no longer ships.

- **`Cmd`/`Ctrl` `+`, `-` and `0` scale the window.** Eight steps from 80% to 150%, and the scale is
  remembered between launches. Each change says where it landed — `Zoom 120% (Cmd 0 to reset)` —
  with the name of the key that was pressed, because the handler needs a modifier either way and so
  already knows which one it is. The terminal is redrawn at the scaled cell size rather than
  stretched, so it stays as sharp as the rest of the window. One consequence to know about: the
  window keeps its 900px minimum, so at 150% a small window has its right edge cut off rather than
  scrolling.

- **`exit` closes the terminal it was typed in.** Ending a shell with `exit` or an alias of it like
  `:q` now takes the pane with it, instead of leaving a frozen last frame that reads as a hung app
  and an entry in the sidebar that looks alive. Closing a terminal whose process is still running
  still asks first.

- **The terminal and editor share a bundled typeface, and its icons are a second family.** Neither
  surface depends on fonts installed on the machine; both use the same regular and bold faces, and
  a glyph from the private use range resolves to an icon family beside them. The terminal waits for
  all three before opening xterm, so the initial grid and glyph atlas are created with the bundled
  fonts. The icons used to be inside both text faces, which meant a face nobody reads was downloaded
  and parsed twice: 4.6MB of TTF for what is now 1.1MB of woff2. This also addresses the reported
  selection-rendering inconsistency; visual verification on the affected macOS setup is still needed.

- **The terminal scrollbar is a choice.** It is hidden by default, which is what the terminal has
  always done; `Auto` shows it while you scroll and takes it away when you stop, and `Always` keeps
  it there to be dragged. All three are under the terminal breadcrumb. The thumb is drawn over the
  right edge rather than beside it, so it does not cost the grid a column.

- **A worktree somebody else created shows up in the panel.** More than one hand adds a worktree:
  Marvis's own dialog, an agent running `git worktree add` in a terminal, a script. Until now only
  the first was noticed while the app was open — a worktree added any other way appeared at the
  next launch, and its row was never watched once it did, so the numbers on it did not refresh.
  Marvis now watches the list of worktrees in the Git directory they share, so a worktree joining
  or leaving a repository is read again and the panel agrees with the disk. An archived worktree
  stays archived, and a worktree removed elsewhere becomes a row that says its directory is gone,
  which is what closing it from the panel has always done.

- **The file editor has a block caret, in the terminal's colors.** It was a thin blue bar while the
  terminal next to it drew a block, and the two were the same window. The caret is now a cell wide
  in the palette's bright white — the same two tokens xterm.js is handed for `cursor` and
  `cursorAccent` — so the editor and the terminal are painted from one palette in both themes. The
  cell covers the character under it rather than inverting it, which is what a block drawn as a
  rectangle can be; a selection still shows the characters it covers, because that band is what is
  painted over them. Blinking is a preference in the Editor section, the same toggle the terminal
  already had, and turning it off does not touch the open document: CodeMirror blinks the caret
  itself, so the answer is one declaration.

## 0.6.0

- **Markdown export is now the default review destination.** With no saved choice, sending review
  notes writes a non-overwriting Markdown file under `~/.marvis/tmp/code-reviews/` and opens it in
  Marvis for editing. This changes what existing users get from the review button: choose OpenCode
  once per checkout to keep sending rounds to an OpenCode session. Exporting does not create a
  round or mark notes sent. Saved open documents without an origin are dropped and the checkout
  view falls back to its default; review notes are unaffected.

## 0.3.0

- **A menu bar with the menus that earn their place.** Marvis used to open with the menu macOS
  gives every app that does not bring its own: a File, a View, a Window and a Help with nothing in
  it. What is left is the Marvis menu — About, Services, Hide, Hide Others, Quit — and Edit, which
  is not there by convention. macOS does not deliver `⌘C`, `⌘V`, `⌘X`, `⌘A` or `⌘Z` to the app at
  all: the Edit menu is what turns them into `copy:` and `paste:` and sends them into the window.
  Take it away and copying stops working in the search field, the editor and the terminal. Window
  went with the rest, so the traffic lights are how the window is minimized, zoomed and closed.

- **Archiving a worktree instead of losing it.** The cross on a worktree row now archives it: the
  row leaves the sidebar and nothing on disk moves — the branch, its commits and its files stay
  exactly where they are. The repo root grows a restore action that brings back every worktree
  that repository archived, in one click. Both actions ask first, in a dialog drawn like the rest
  of the app.

## 0.1.0

The first build anyone else could install: a `.dmg` for Apple Silicon and a `.deb` and `.AppImage`
for x86_64 Linux, published as a GitHub release.

- **One window for a repository.** The shell is a titlebar of breadcrumbs over a workdir sidebar, a
  details panel and a single main pane that switches between terminal, document and diff.
- **One agent per checkout.** The review loop runs end to end, each checkout gets its own bridge,
  and an agent mid-turn is asked before it is interrupted.
- **Documents and diffs.** Syntax highlighting with Shiki, the language taken from the file itself,
  front matter parsed once and read as YAML, and review notes and agent delivery inside the diff.
- **Terminals that earn their row.** One per checkout, the program in front of it named in the
  sidebar, nameable sessions, and a grid that keeps its columns inside the pane.
- **Losing nothing quietly.** One schema with no migrations, dead sessions no longer hoarded, and
  the four ways this app could drop work or a database closed.

## 0.2.0

- **Linux arm64.** `.deb` and `.AppImage` built on a native arm64 runner, so Raspberry Pi and ARM
  servers get a real binary instead of an emulated one.
- **macOS signed ad-hoc.** The bundle carries an ad-hoc signature with the hardened runtime, which is
  what keeps macOS from reporting a download from a release as damaged. It is not a Developer ID, so
  Gatekeeper still asks once.
- **Releases gated on their own commit.** A tag now runs the full CI against the tagged SHA before
  anything is built, the release lands as a draft with a `SHA256SUMS` beside the artifacts, and it is
  published only once every platform passed. The version is bumped, committed and tagged by
  `pnpm release`.
- **Builds are cached.** Neither workflow cached anything, so a release compiled the Rust graph
  three times over.
