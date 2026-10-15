# Native interface and workflow parity specification

**Parent report:** [Migration feasibility and strategy](README.md).  
**Terminal implementation:** [Terminal-first specification](terminal-first.md).  
**Status:** behavior inventory and proposed implementation map; no UI redesign authorized.  
**Baseline:** Muster v0.21.1 at `4412f63bd385bdb3b9eb12b829858578a2009f81`.

## 1. The preservation contract

The user permits visual adaptation of controls and modals, but not a change to the application's general structure or purpose. Native widgets can look different while the same actions remain in the same contextual workflow.

Use this priority order when judging parity:

1. Correct operation, ownership and safety.
2. Existing information architecture and navigation.
3. Existing action placement and context.
4. Keyboard/focus/accessibility behavior.
5. Selection, persistence, async-state and reading-position semantics.
6. Visual resemblance, density and familiar state cues.
7. Fine styling details.

A native generic IDE template does not satisfy items 2–3 simply because it contains a file tree and terminal. Neither does copying tty7's repository sidebar or persistent-session workflow.

### Explicitly not part of this port

- A background terminal daemon or sessions surviving application quit.
- SSH/remote workspaces, mobile gateway, terminal command composer or new CLI.
- A new agent abstraction or replacing the existing OpenCode integration with tty7 agent detection.
- A new docking UX or arbitrary rearrangement of all panels.
- New Windows support merely because GPUI supports it.
- New schema migrations, compatibility layers or releases during research.

## 2. Current structural layout

Observed in `src/App.vue` and `src/components/MainPane.vue`:

```text
┌───────────────────────────────────────────────────────────────────┐
│ native window controls / breadcrumb menus / empty drag region      │
│                                      focus↔split action / Settings │
├───────────────┬────────────────────────────────┬───────────────────┤
│ Repository    │ Main surface                   │ Inspector         │
│ groups        │                                │ Files / Changes   │
│   checkout    │ focus: terminal OR document/diff│                   │
│     terminal  │                                │                   │
│     terminal  │ split: terminal + preview      │ drawer when needed│
│   worktree    │                                │                   │
│     terminal  │                                │                   │
└───────────────┴────────────────────────────────┴───────────────────┘
     confirmations / worktree dialog / settings / transient feedback
```

The important layout modes:

- **Focus:** central panel shows a terminal, a document, or a diff. Opening a document hides the terminal but does not terminate it.
- **Split:** terminal remains visible beside a document/diff preview. The inspector becomes a right-side overlay/drawer, including hover-strip behavior.
- **Side panels hidden:** the current window shortcut hides the side panels without changing the selected checkout/terminal or discarding state.

### Dimensions and persistence

`src/domain/ui-state.ts` and Rust persistence agree on:

| Dimension | Default | Limits |
| --- | --- | --- |
| Sidebar width | 240 | 240–500. |
| Inspector width | 280 | 200–480. |
| Preview width | 360 | 260–900. |
| Focus central minimum | Main App panel uses 420 | Inspector drawer threshold additionally considers a 430 main allowance. |
| Split terminal/preview | Terminal at least 320; preview at least 260 | Resize must not collapse either into an unusable surface. |
| Window | 1280×800 | Tauri config minimum 900×600. |

Do not reinterpret saved widths as physical screen pixels. App zoom and native display scale need an explicit mapping. Preserve stored application layout values when their meaning remains unchanged.

The current split mode is **terminal plus preview**. A Rust domain model also supports terminal tabs/binary split trees, but the inspected `SessionPane.vue` template displays one active terminal at a time; it does not render a general terminal split tree. Do not advertise terminal-pane splitting as an already-existing visible feature or add a new dock UI under the name of parity.

## 3. Component migration map

| Current source | Native building block | Product-specific logic that remains |
| --- | --- | --- |
| `App.vue` | Retained workspace/root entity and resizable layout | Startup, active context, request generations, focus/split/drawer policy, close workflow. |
| `TitlebarMenu.vue` | Popup/menu/search controls plus native title bar | Hierarchical breadcrumb choices, exclusive open menu, path shortening. |
| `Sidebar.vue` | Sidebar shell or custom native rows using Kit primitives | Repo/worktree/session hierarchy, state inference, rename, move and drag/drop. |
| `MainPane.vue` | Native layout/view switching | Terminal retained behind preview, focus/split semantics and width constraints. |
| `SessionPane.vue` | Session registry + active retained terminal entity | Creation token, stable pending identity, persisted ordering, moves and cleanup. |
| `TerminalSession.vue` | Native terminal controller/element | Entire terminal specification; not a Kit editor. |
| `InspectorPane.vue` | Tabs, Tree/virtual lists, native scroll handles | Lazy reads, saved expansion/selection, grouping, counts, sticky ancestors, open actions. |
| `DocumentPane.vue` | Editor, TextView/Markdown, image surface | Drafts, safe save, language/format controls, media routing, navigation revisions. |
| `FileDiff.vue` | Diff/annotations, with bounded large-file path | Review ranges, notes, send controls, scroll and paged backend integration. |
| `ReviewComposer.vue` | Textarea/inputs/buttons | Anchor range, draft text, validation and safe action semantics. |
| `ReviewNoteList.vue` | Lists/buttons/status labels | Draft/sent/outdated/resolved note state and review rounds. |
| `WorktreeDialog.vue` | Dialog/forms/checkboxes | Defaults, branch/path validation, remove preview, destructive acknowledgements, archive vs delete. |
| `SettingsDialog.vue` | Settings/form/navigation components | Current sections, keys, Apply/Cancel, validation, version/credits and storage actions. |
| `ConfirmDialog.vue` | AlertDialog/Dialog | Question identity, cancel semantics, focus return and failure persistence. |
| `ToastStack.vue` | Notifications | Current transient error/info semantics and contextual reporting. |
| `OverlayScrollbar.vue` | Native scroll handles/scrollbars | Theme and preference behavior; no hidden competing scroll region. |
| `FileIcon.vue`, local UI wrappers | Kit/Lucide icons and controls | File-type mapping, state icons and accessible labels. |

Search for the Kit component first. Custom views are justified for the terminal and Muster's dense navigation rows, not for recreating every button/menu from scratch.

## 4. Header and breadcrumbs

### Existing behavior to preserve

- Workdir/repository breadcrumb at the left.
- Worktree/branch breadcrumb only for a Git repo; detached state remains meaningful.
- Terminal breadcrumb is a menu when a terminal is shown.
- Document/diff breadcrumb names the current path; it is not a terminal menu over a file.
- One open breadcrumb menu at a time.
- Workdir/worktree choices have search where currently present.
- Paths shorten according to available room, keeping useful identity and a full tooltip/accessibility label.
- Empty header area is draggable; breadcrumb and settings actions remain clickable.
- Layout toggle and settings action remain at the far end.
- macOS traffic-light room is preserved; other targets have appropriate window controls.
- Double-click behavior belongs to one native window path, not two handlers that maximize then restore.

### Native strategy

Use native titlebar/window support and Kit menus; build the breadcrumb row with ordinary horizontal layout. Keep labels as context, not oversized always-raised buttons. Replace DOM measurement with native text measurement; do not mechanically port a hidden HTML path probe.

Menu keyboard behavior must support arrows, Enter, Escape, outside click and focus return. A context menu and a breadcrumb popup cannot both remain active accidentally.

### Not ported with the bar

The bar in `native/src/app.rs` carries the window chrome, the three crumbs and the panel toggle. Four pieces of the reference header are **not** migrated, because the surfaces behind them do not exist in the native build yet and a header row for them would be chrome for nothing:

- The settings action: there is no native settings surface to open.
- The focus/split layout toggle: the native window has one layout, not two.
- The document/diff crumb and its file glyph: there is no native inspector to open a file from.
- The maximized-state tracking the reference kept to label its own restore button: the native controls ask the window.

They arrive with the surface each one opens, not before.

### Checks

- Opening one crumb closes the previous one.
- Selecting a checkout/terminal updates the same active context in header, sidebar and main view.
- A file open in focus mode has a file crumb and no misleading terminal menu.
- Resizing/zooming truncates a long path without covering the layout/settings buttons.
- Dragging empty space moves the window; dragging/clicking a crumb does not.

## 5. Sidebar: behavior before appearance

`Sidebar.vue` is not a simple static tree. It combines domain information, OS terminal status, service agent state, label fitting, pointer gestures and action menus. Reuse Kit's navigation primitives while owning this row model.

### 5.1 Hierarchy

Preserve:

- One repository/plain-directory group.
- One primary checkout and zero or more worktree rows under each Git group.
- Terminal rows nested under the owning checkout.
- Home workdir special handling: it cannot be removed like a normal group.
- Missing directory state in the relevant row, not just a toast.
- Repository collapse and checkout collapse as separate local view sets.
- Archived worktrees outside the normal visible list, recoverable from the repo action.

Collapse state currently is local presentation state; do not silently add a new persistent preference just because Kit supports serializable trees.

### 5.2 Selection semantics

This is a particularly important non-obvious contract:

- Picking a checkout row **with displayed change counts** opens its whole change-set diff.
- Picking a checkout without changes restores its saved main view and does not automatically spawn a terminal.
- Picking a terminal selects that terminal's checkout and focuses the terminal.
- In focus mode, terminal selection brings the terminal into the main view.
- In split mode, terminal selection must not unnecessarily replace the current preview.
- A branch/checkout owns the selected accent only when terminal context is visible and it has not delegated selection to its terminal row.
- An active terminal can be visually dimmed when a file/diff occupies the main surface.
- Inspector selection and sidebar context must not create two competing claims about which content is visible.

Port `activateCheckoutTerminal` and `activateTerminalSession` behavior, not a generic “every row selects a node” model.

### 5.3 Terminal row name/state

The row presents:

- The most useful session/process/title name.
- Detail identifying a known agent when available.
- A state glyph and color.
- Elapsed activity only when a real agent session was identified.
- A text/accessibility description for states that color alone cannot convey.

Agent matching must remain conservative:

1. OS runtime confirms the foreground application is the intended agent app.
2. The terminal's own title matches exactly one listed service session.
3. No match or several matches produces an explicit unknown/ambiguous state.
4. A title alone never authorizes a review target or proves agent identity.
5. Pending-reply state and a running turn remain distinct.

For plain terminals, last-command failure is independent of shell exit and foreground activity. A failed command can color the row while the shell remains live; a new command clears that failure. An unidentifiable agent is not automatically painted as a healthy running process.

Kit's SidebarMenuItem does not provide these semantics by itself. Create one native row projection from the current workspace/status/agent models; derive visible and accessible state from the same projection.

### 5.4 Action placement

- Repository group actions stay in its more/context menu.
- Frequent “new worktree” remains available without putting it dangerously next to removal.
- Restore archived worktrees shows the relevant count.
- Remove-from-panel is distinguishable from deleting a worktree from disk.
- Checkout fold control does not also select the checkout.
- Close controls do not trigger row selection as a side effect.
- Terminal rename, close and move remain in terminal context.
- Native control decoration may change, but moving these actions into a disconnected global dashboard would not preserve the workflow.

### 5.5 Rename

- Enter commits; Escape cancels.
- Focus the editing field without sending its keys to the terminal.
- Use service validation: trim; reject empty, over-60-character or control-containing names.
- A backend rejection leaves the row with its existing name.
- An async rename result must target the same session ID even if navigation changed.

### 5.6 Reorder and cross-checkout move

- A drop in the same checkout is reorder, not a backend ownership transfer or `cd`.
- Sidebar order comes from terminal layout/session order, not incidental DB row order.
- A cross-checkout move preserves the PTY process and output.
- Destination insertion index reflects the pointer/menu choice.
- Invalid/missing destinations are refused.
- Source and target layouts are updated consistently with backend ownership.
- Manual move may write a guarded `cd` to an idle shell; never feed it to a foreground build/TUI.
- “Row moved” and “directory-change bytes were written” are separate results. Accepted bytes do not prove `cd` executed successfully.
- A failed directory change reports that the terminal moved but its cwd did not change.

Drag semantics to carry forward:

- Drag threshold and pointer capture.
- Insertion indicator and meaningful ghost label.
- Auto-scroll near list edges.
- Cancel with Escape/pointer cancel/lost capture.
- Suppress the post-drag click.
- A click/rename is not accidentally promoted into a drag.

Reuse native drag/drop where it matches these semantics. Do not depend on a browser `DataTransfer` port or on Kit Dock dragging that changes a different model.

### 5.7 Automatic movement

`terminal-movement-controller.ts` is a substantial controller, not visual code. It tracks manual operations, queued relocations, observation timestamps, inactive terminals and navigation revision.

Preserve:

- Follow modes `off`, `cd`, `agent`, `both`.
- Exactly one terminal match before following agent relocation.
- No stale/duplicate event overriding a newer manual move.
- Manual directory-change settling/timeout behavior.
- Serialized operations per terminal.
- Automatic movement follows the window only if the relevant terminal is still the current visible context.
- Newer user navigation wins over an old async workspace snapshot.
- Exited/closed terminals cannot be moved by late observations.

Do not use a native reactive observer that instantly moves rows whenever cwd changes without these guards. Translate the controller tests first, then bind it to native views.

## 6. Terminal pane and layouts

### Current pane behavior

- Terminals are lazily created by an explicit request or the empty-state action.
- A shell request uses a token and checkout pair so one request becomes one terminal even when checkout UI state arrives later.
- A pending view has a stable key separate from its eventual backend session ID.
- The visible terminal is resolved from the same selected-session policy as the sidebar.
- All retained terminal views stay alive when another checkout is selected.
- Missing checkout shows a useful empty/error state without spawning.
- No checkout shows Open directory; an empty available checkout shows New terminal.
- Creation failure removes pending state and reports the actual failure.

### Native state ownership

Maintain a registry of retained session entities plus explicit pending creation entries. Backend ID is the stable identity after creation; pending request identity must not be discarded/recreated when that ID arrives.

Preserve current terminal layout validation/reconciliation. The Rust domain already knows tabs, session order and binary split nodes. Keep its current JSON if stored meaning does not change; do not store Kit Dock's entity IDs or opaque panel JSON in the same field.

General terminal splitting is not an implicit new deliverable. If that UX is requested later, use the existing domain model deliberately and add separate acceptance criteria.

### Native panel sizing

Use resizable layout primitives for focus/split preview structure. Avoid full Dock unless current requirements actually need free panel docking. A sidebar and inspector at fixed logical locations are simpler and closer to the existing interface.

## 7. Files and changes inspector

### Delivered slice and remaining legacy work

The native Files/Changes slice is **not full parity**. The following audit is
scoped to the inspector, central previews and the sidebar routes that open them;
the full-port completion rule in section 16 still applies to the rest of the app.

| Surface | Delivered natively | Still required from the live legacy UI |
| --- | --- | --- |
| Files | Bounded lazy listings, virtual rows, expansion/selection, subtree errors and operation-specific retry; file/folder icons with file-type ink, path tooltips, dimmed dotfiles, filtered Git markers and selected tree/tab states. | Ignored metadata, supported symlink representation, sticky ancestors/reveal, modifier external-open, horizontal/scroll stability and tree/tab keyboard parity. |
| Changes | Git merge-base status and per-file optional counts, directory grouping, count badge, individual and All changes routes. Unknown counts do not hide other files or become zero. | Independent selected-change/scroll state per checkout, virtualized large change lists, and richer branch/head/ahead/rename context. |
| Navigation | Dirty checkout rows/label/count targets share one All changes action; explicit terminal selection and preview Close retain the existing PTY. A newly requested terminal replaces the current preview without a later shell completion stealing a subsequently opened preview. | Restore each clean checkout's prior view, not always its terminal; preserve a preview beside session selection in split layout. |
| Documents | Whole UTF-8 text up to 1 MiB in a retained read-only toolkit Editor, with source line numbers, selection/copy, horizontal scrolling and syntax highlighting. Empty files and trailing newlines remain exact. | Editor/draft Save/Cancel with expected-content conflict checks; formatting, additional grammars/settings and changed-line gutters; Markdown/links, images/SVG, review-document origins and path copy. |
| Diffs | One retained toolkit unified viewer for individual or All changes: real old/new coordinates, selection/copy, syntax and inline emphasis, horizontal scrolling, unchanged-context folding and a coherent virtualized viewport. Complete patches over 1,000 lines are displayed under the hard caps; revision-keyed parsed state, 32 MiB raw patch cache, four running reads and at most 32 queued reads. Binary/link/oversize explanations remain explicit. Working inputs are descriptor-opened and privately snapshotted before Git reads them. | Per-checkout reading-position restoration and remaining hunk/context controls. There is no pagination beyond the whole-patch safety caps; refusal is not full legacy large-file parity. |
| Refresh | Git status/diff polling with either inspector tab active, manual Changes refresh and stale-result rejection. | Repository/shared-Git watchers with debounce and recovery; refresh Files listings and open documents after file activity, including ignored-file edits. |
| Layout/state | Resizable inspector and panel visibility; existing sidebar persistence. | Inspector/preview width and focus/split layout persistence, narrow-window drawer/hover strip, per-checkout tabs, views, expansions and independent reading positions. Native workspace persistence does not currently store these. |
| Review | Not delivered. | Line/range notes, anchor reconciliation, note lifecycle, rounds, Markdown export, OpenCode target/send/queue/acknowledgement and persistence. |

Audit sources: `legacy/src/components/{InspectorPane,DocumentPane,FileDiff,MainPane}.vue`,
`legacy/src/App.vue` (`showView`, `activateCheckoutTerminal`),
`legacy/src/domain/ui-state.ts`, `legacy/src/presentation/{git-watchers,diff-loader,review-notes}.ts`
and `legacy/src/components/use-large-diff.ts`, compared with
`native/src/{inspector,document,diff,app}.rs` and `core/src/{files,navigation/changes}.rs`.
The legacy Files per-file count fields are dormant, not a live feature to recreate;
its filtered Git status markers are live. Backend viewed-file APIs with no UI caller
are likewise not port requirements. Inline video remains explicitly deferred under
section 8.6; this does not defer image/SVG previews or explicit external-open.
Enabled syntax grammars are JSON, JavaScript/JSX, TypeScript/TSX, Rust, Markdown,
HTML and CSS; unsupported languages remain readable plain text. Syntax/content
colours are installed separately from the shared chrome palette. Source previews
remain capped at 1 MiB. Diffs refuse whole patches above 16 MiB, 100,000 lines,
10,000 hunks or 64 KiB per line; no truncated patch is presented as complete.

Visual, screen-reader and Linux verification remain separate acceptance checks;
headless unit/integration tests are not evidence for those gates.

### 7.1 Tabs and scope

- Files is available for a plain or Git workdir.
- Changes exists only for an available Git checkout.
- Current tab, selected paths, expansion and independent scroll positions are stored per checkout.
- Tab keyboard navigation includes current Home/End behavior.
- The right inspector remains files/changes; do not move the review composer or agent-session list there just because a standard toolkit sidebar can hold them.

### 7.2 File tree

Current implementation has lazy directory reads, flattened/virtualized expanded rows, intrinsic-width calculation and optional sticky ancestor rows.

Native implementation should:

- Load only requested directories through the existing file service.
- Keep loading/error/empty states associated with the correct subtree.
- Reject stale directory results after switching checkout.
- Reuse backend limits and hidden/ignored/symlink metadata.
- Keep tree scroll position and horizontal width stable while rows are virtualized.
- Preserve file/folder icon mapping and the filtered Git status markers currently shown. Per-file counts belong to Changes; the legacy Files count fields are dormant.
- Preserve folder reveal/expand behavior distinct from opening a file.
- Preserve platform-modifier external-open behavior.
- Keep focus usable when collapsing a subtree containing the focused row.
- Implement sticky ancestor behavior if Kit Tree does not supply the same interaction.

Do not read the entire checkout recursively on each paint or use a toolkit tree's eager file-loading shortcut to bypass existing containment policy.

### 7.3 Changes list

- Changed-file count in the tab.
- All-changes action at the top.
- Path grouping and per-file status/counts.
- Independent scroll/selection from Files.
- Selecting a change opens the relevant document/diff view using the current domain mapping.
- Pure rename, deleted, binary, symlink and oversized-file states remain meaningful.

Git is still the source of change state. Toolkit Diff parsing does not replace status/watch services.

### 7.4 Drawer behavior

The current App makes inspector a drawer when space is insufficient; split layout uses a hover strip and close/open scheduling. Carry the behavior, not the Vue Teleport mechanism.

Native overlay/hit testing must ensure:

- Drawer paints above the terminal/preview.
- Hover/entry/leave timers do not flicker while moving between strip and drawer.
- It does not change stored inspector width while temporarily collapsed.
- Keyboard users can reach its actions without relying solely on hover.
- Clicking underlying content does not simultaneously trigger a drawer item.

## 8. Documents and editor

### 8.1 Scope of current document surface

`DocumentPane.vue` is more than a read-only code preview. It includes:

- Code/view/diff-related domain routing.
- Native-backed file loading and review-document origins.
- Editable drafts with Save/Cancel.
- Language selection/highlighting and editor settings.
- Prettier formatting into a draft.
- Changed-line decorations.
- Markdown rendering and internal/external links.
- File path copying.
- Media preview routing.
- Scroll/read-position restoration and watcher-aware refresh.

### 8.2 Editor integration

Use Kit `EditorState`/`Editor` for source editing. Retain state per currently-owned draft/document and subscribe to change events. Enable required language grammars deliberately; bundle only the languages needed for current coverage unless a measured build tradeoff favors all grammars.

Map:

| Current need | Native handling |
| --- | --- |
| Editor font size/ligatures/cursor | Theme plus editor state/options. |
| Spaces vs tabs and indentation size | Editor TabSize/rules using current settings. |
| Search and undo/redo | Editor facilities, verify actual key bindings. |
| Read-only source when editing is disallowed | Explicit editor readonly state. |
| Changed lines/gutter | Editor decoration API, tied to current Git revision. |
| Language choice | App-owned language menu and compatible grammar selection. |
| Save/Cancel toolbar | Same contextual actions, not automatic writes on every change. |

Do not add LSP servers merely because Kit supports LSP. Current feature requirements determine whether one is needed.

### 8.3 Safe saves

Keep `services::files::write` and expected-content validation. The frontend currently:

1. Captures original/expected disk content.
2. Saves the captured draft.
3. Retains a newer draft typed while that save was in flight.
4. Updates that newer draft's expected-content base to the saved bytes.
5. Applies UI changes only if file identity still matches.

Port this behavior exactly. A toolkit `save` example using `std::fs::write` is not a valid replacement. Protect checkout/review origin, symlink boundaries and backend file-size limits.

Do not claim an existing unsaved-window-close confirmation without verifying one; the observed contract here is draft retention during navigation and safe explicit saves. Native cutover must make its actual dirty-draft close policy explicit rather than assuming framework hooks supply it.

### 8.4 Formatting decision

The existing Format action uses project-aware Prettier config and a worker, produces a draft, and does not immediately write to disk. It rejects stale results if content, language, file, editor generation or formatting revision changed while formatting ran.

A native port must preserve that asynchronous safety even if the formatting engine changes.

Approved policy: external Node/Prettier is allowed, with a preference for a suitable native Rust formatter. The following tradeoffs guide the later editor implementation:

| Policy | Advantage | Limitation |
| --- | --- | --- |
| Invoke installed project Prettier from Rust | Reuses the formatter family; no JS UI | Requires external Node/Prettier, which the current bundled formatter does not require; versions/plugins/config trust may differ; not a zero-JavaScript execution environment. |
| Native language-specific formatters | Rust-native application/runtime boundary | Different language/config/output coverage; needs explicit support matrix. |
| Reimplement all Prettier semantics | No external JavaScript | Excessive scope; not recommended. |

Do not run arbitrary project configuration code silently if the existing reader only supports a safe configuration subset. Preserve trust boundaries and explicit user actions. Formatter process execution is new service work and must have time/output limits.

Resolve formatter availability/version/support during the editor phase; the terminal MVP is unaffected. Prefer a suitable native Rust solution if verified to meet required coverage. TODO / technical debt if Node/Prettier is used: evaluate native replacement while preserving the agreed languages/config semantics. Do not automatically execute project plugins or JS configuration to imitate the current bundled formatter's constrained behavior. Record supported languages/config options and how missing tools are handled.

### 8.5 Markdown

Reuse native TextView/Markdown before implementing a parser/render tree from scratch. Compare actual results against current tests for:

- Headings, paragraphs, lists and task lists.
- Tables and code fences with language colors.
- Inline code, emphasis and copy behavior.
- Relative file links, anchors and external URLs.
- Review export documents with their distinct safe origin.
- Scroll restoration after refresh and view switching.

No arbitrary script execution or webview HTML fallback. Treat optional raw HTML support as a controlled native-renderer capability, not browser equivalence. Source preview may remain available when native Markdown cannot display a construct safely.

### 8.6 Media: exact baseline

`src/domain/media.ts` recognizes:

- Images: png, jpg/jpeg, gif, webp, avif, ico, bmp.
- SVG separately.
- Video: mp4, webm, mov, ogv.

Audio playback is not established by this inspected routing and is **not added** as migration scope.

For images/SVG, use native supported decoding/rendering with backend containment and limits. Verify supported formats, animation behavior and SVG external-resource policy; a matching filename extension is not validation.

**Approved video scope adjustment:** inline playback is deferred. In the later document/file surface, selecting a video shows a preview-unavailable state. Do not embed a player or automatically launch an external application. Preserve the existing explicit modifier-click external-open action from the file panel so the user's OS-selected player can open it. Use the actual platform gesture already implemented; do not reinterpret macOS Control-click/context-menu behavior without checking current handling.

M1 defers all media. Inline video is explicitly accepted as later work, not a terminal-MVP or full-port blocker under this scope. Native image/SVG support remains required for the document phase. Any future inline video path needs its own platform/codec verification; no webview fallback.

## 9. Diff, review notes and review rounds

### 9.1 Native Diff is the first reuse candidate

The inspected Kit Diff component offers:

- Git/unified patch parsing supplied by the application.
- Multi-file virtualized display.
- Original and modified source coordinates.
- Unified and split diff presentation.
- Text/source-line selection and copying.
- File/hunk collapsing and supplied-context expansion.
- Custom file-header slots.
- Stable-ID line/file annotations and annotation-start callbacks.
- Background syntax/inline-emphasis preparation.

These are a close functional fit for the existing small/normal diff path. Keep current visible display choices; new supported modes are not automatically new user-facing buttons.

### 9.2 What remains Muster-owned

- Checkout and patch identity/revision.
- ReviewSide mapping and source line ranges.
- Draft composer content and range adjustment.
- Note lifecycle and content/code anchor.
- Sent/outdated/resolved/relocated state.
- Agent target selection.
- Export-to-Markdown destination.
- Send now vs queue.
- Review-round persistence and restart semantics.
- Clear/resolve/outdated confirmations.
- Error messages and repeated-action prevention.

Use stable review note IDs for native annotations. Do not keep notes as ephemeral DiffState children only; replacing the patch must not erase persisted review work.

### 9.3 Selection and anchors

Native Diff exposes original/modified sides and one-based source coordinates, which should map to current review ranges explicitly.

Preserve distinctions between:

- A code-text selection for copying.
- A source-line range for an annotation.
- A note range on an original vs modified side.
- A folded/unloaded range and a range absent from the actual patch.

Changing display mode, folding or virtualization must not renumber review anchors. Do not use visible list indices as persistent source coordinates.

### 9.4 Large diffs

Muster has a separate paged large-diff path in `use-large-diff.ts` and backend APIs. Kit virtualizes painting but `DiffFile::parse` still receives and stores the supplied patch.

Prototype with current large-file thresholds before choosing:

1. Native Diff for normal patches and an application-owned page-backed native row view for oversized patches.
2. A bounded adapter to existing Kit facilities if they expose sufficient incremental data semantics.
3. A deliberately measured full-patch path only where existing limits make it safe.

Prefer reuse, but do not discard paging on the basis of the word “virtualized.” Preserve backend caps and useful too-large/binary/symlink/deleted summaries.

A note cannot be declared outdated just because its line has not been loaded. Current FileDiff explicitly distinguishes whole-file knowledge from partial pages. Translate that logic before implementing automatic anchor cleanup.

### 9.5 Dispatch semantics

The current App builds a review message from chosen notes and records a review round before contacting the agent. The queued version stores the exact accepted prompt for a later flush. A restarted app should recognize an interrupted send rather than blindly duplicate it.

Native actions must still:

- Validate checkout availability and correct review target.
- Preserve selected note IDs/content order.
- Prevent duplicate sends while one is active.
- Keep send/queue and Markdown export distinct.
- Report disconnected or unavailable agent state honestly.
- Preserve service round/ack/relocation handling instead of interpreting UI animation as an acknowledgement.

The Rust agent service already supports injected event delivery. Rebind it to native observers. The inspected `main.rs` states that it connects to the user's existing OpenCode service; do not introduce automatic per-checkout server spawning based on stale comments or tty7's architecture.

## 10. Worktree lifecycle dialogs

Visual restyling is permitted; destructive semantics are not.

### Create

Preserve:

- Existing source/default-branch selection semantics.
- Task name, suggested branch and editable location.
- Branch suggestion stops overriding the user once manually edited.
- Existing service/path validation and collision behavior.
- Busy state preventing duplicate creation.
- Result updates workspace and offers/requests a shell in the created checkout.

Use the current service; GPUI has no reason to reimplement Git worktree commands.

### Remove/archive

Keep three concepts distinct:

1. Remove a workdir from the panel: no files deleted.
2. Archive a worktree: keep branch/commits/files, hide it from normal panel.
3. Delete a worktree: remove its directory/Git metadata under the existing safety checks.

The existing dialog previews dirty/unmerged state, listed sessions and missing checkout metadata, with explicit acknowledgements and optional branch deletion. Preserve those checks, not merely a generic “Are you sure?” dialog.

- Dirty/uncommitted loss must be explicit.
- Stopping sessions must be explicit.
- Deleting a branch/unmerged commits must be distinguishable from keeping it.
- Archive restrictions remain service-driven.
- Missing worktree cleanup must not pretend it removed a real directory.
- A failure keeps the dialog useful with the actual error; it is not hidden behind a success toast.

## 11. Preferences and global state

### Current key groups

`config.rs` defines:

| Group | Keys to preserve |
| --- | --- |
| UI | `fontSize`, `zoom`, `theme`, `contentBackground`, `treeStickyScroll`. |
| Terminal | `fontSize`, `ligatures`, `cursorBlink`, `cursorStyle`, `scrollbar`, `shellIntegration`, `followWorktree`, `selectionCopy`. |
| Editor | `fontSize`, `ligatures`, `cursorBlink`, `indentation.useSpaces`, `indentation.size`. |
| Reviews | `storage`. |

Keep `~/.muster/config.yml`, current normalization and currently-written key meanings. A native control's internal name is not permission to rename a preference.

### Apply timing

- Font/theme/zoom/cursor/selection preferences should reach live views where current behavior does.
- Shell integration affects terminals created after the change; do not retrofit a startup hook into an already-running shell.
- Changing palette or ligatures must not recreate the PTY.
- Follow-worktree mode changes controller policy, not process lifetime.
- Apply/Cancel must remain distinct and errors must not falsely report a saved configuration.
- Review storage actions preserve directory ownership and deletion scope.

Use Kit settings/form controls, but store draft dialog state separately from committed app settings. Avoid writing config on every slider event unless that is the deliberately verified current behavior.

### Theme mapping

Replace CSS custom-property lookup with one native palette/theme mapping derived from `src/muster.css` and current theme behavior. Preserve semantic roles: content background, sidebar, text levels, selection, warning/danger, ANSI colors and agent-specific tint.

Exact colors can adapt within the user's tolerance, but state meanings must not invert. A waiting agent, missing directory and failed terminal command cannot all become the same unlabeled decorative dot.

### Layout persistence is separate from preferences

App/checkout layout remains in SQLite; persistent user settings remain in YAML. Do not move everything into a toolkit config file or store a new schema version inside each JSON blob.

Current checkout UI state includes selected document, main-view mode, all-files diff flag, inspector tab/paths, expanded directories and independent scroll positions. Retain those current shapes unless a verified implementation need changes them; if changed before v1, follow the schema-reset rule, not migrations.

## 12. Focus, accessibility and feedback

### Focus ownership

- Window-wide actions use platform-appropriate modifier routing.
- Focused terminal owns terminal keys.
- Rename/input/editor controls own their text editing keys.
- Menus/dialogs trap or manage focus according to their semantic role.
- Closing a popup returns to its trigger; terminal selection restores the terminal.
- Errors should not silently move focus away from a command being typed.
- Pointer-only hover drawers must also be accessible through keyboard context.

Current shortcut facts live in `src/domain/shortcuts.ts` and are tested against App behavior. Preserve the published set before adding upstream defaults.

### Semantic baseline

Retain equivalents of:

- Sidebar/inspector/main view accessible names.
- Tablist/tab/tabpanel relations and current tab state.
- Expanded/collapsed repo and folder state.
- Current/selected checkout/session semantics.
- Missing, unknown, busy, failed and asking states in text, not only color.
- Dialog titles/messages/destructive button meanings.
- Resize separators with orientation/current bounds and accessible interaction.
- Busy/loading/status announcements that do not flood assistive tools.

Kit's native accessibility foundation helps standard controls. Verify the custom terminal element and dense sidebar rows separately with platform assistive technology.

### Feedback placement

Preserve the current distinction between:

- Terminal input/status error tied to that session.
- Missing checkout shown in its row/pane.
- A destructive dialog failure kept in the relevant question/workflow.
- General transient errors/info as toasts.
- Healed invalid terminal layouts that need not alarm the user as if an operation failed.

Do not put every error in one permanent bar that reduces terminal height, or turn every transient error into an irreversible modal. Reuse the current controller reporting rules.

## 13. Model/controller port order

Translate behavior in this order before attaching all views:

1. Rust domain reuse and current persisted layout validation.
2. Workspace selection and main-document routing.
3. Retained terminal registry, request-token and creation state.
4. Sidebar row projection from workspace/status/agent observations.
5. Movement/reorder controller and navigation revisions.
6. Inspector lazy tree/changes projection and saved-state restoration.
7. Document draft/save/refresh controller.
8. Diff loading, source-coordinate selection and review annotations.
9. Review dispatch/round UI and native agent-event observations.
10. Settings draft/commit and app-close persistence controller.

Keep pure model/controller tests independent of GPUI where possible. Use native component tests for input/focus/rendered feedback, not to simulate every Git/file operation through a window.

## 14. Parity acceptance scenarios

These scenarios define completion more precisely than “all panels have been ported.” They are future runnable/manual checks, not results claimed by this research.

| ID | Scenario | Required invariant |
| --- | --- | --- |
| P01 | Open plain folder then Git repo | Correct group types, available tabs and no unnecessary terminal spawn. |
| P02 | Select checkout with/without changes | Changed row opens all-changes diff; unchanged restores saved context. |
| P03 | Select terminal in focus and split modes | Focus changes correctly; split preview is not unintentionally replaced. |
| P04 | Open document while terminal runs | Same PTY/grid alive; switching back shows current output. |
| P05 | Toggle side panels and resize/zoom | No context loss; widths/constraints/persistence remain sensible. |
| P06 | Long duplicate repo/session labels | Useful fitted identity, full tooltip/accessibility and correct target actions. |
| P07 | Fold repo and checkout independently | No accidental selection; local fold model preserved. |
| P08 | Missing/home checkout actions | Missing state clear; Home cannot be removed; no accidental disk deletion. |
| P09 | Rename accepted/rejected/cancelled | Correct service validation; terminal receives none of rename keystrokes. |
| P10 | Reorder within checkout | Same PTY; sidebar order matches saved layout immediately. |
| P11 | Move idle vs busy terminal | No respawn; guarded `cd` only when safe; distinct failure feedback. |
| P12 | Move while relocation/navigation arrives | Newer manual/navigation intent wins; no stale automatic selection. |
| P13 | Agent title unknown/ambiguous/unique | No guessed identity; visible/accessibility states agree. |
| P14 | Terminal command fails but shell survives | Sidebar failure independent of process exit; next command clears it. |
| P15 | Rapid checkout/tree switching | Old async subtree/status never appears in new context. |
| P16 | Huge expanded tree/changes list | Virtualization without jittering horizontal width or lost focus. |
| P17 | Sticky ancestors and inspector drawer | Existing ancestry/context accessible by pointer and keyboard. |
| P18 | Edit A, switch to B, return to A | A's draft retained; expected disk base correct. |
| P19 | Save while typing or file changes externally | Newer draft survives; conflicting disk write refused. |
| P20 | Format while editing/navigating | Stale result discarded; formatting stays a draft. |
| P21 | Markdown internal/review/external links | Correct origin/containment and external URL policy. |
| P22 | Images/SVG and deferred video | Native image/SVG support; video shows preview unavailable, explicit file-panel external-open uses OS association, no automatic playback/webview. |
| P23 | Diff note on original/modified lines | Correct stable source coordinates after folding/refresh. |
| P24 | Paged diff with unloaded note anchor | Unknown is not incorrectly marked absent/outdated. |
| P25 | Send/queue/export selected review notes | Exact persisted round/prompt; no duplicates or wrong target. |
| P26 | Agent disconnected/reconnected | Honest state and current events; no unintended server ownership. |
| P27 | Create worktree after editing branch suggestion | User branch preserved; duplicate submit blocked. |
| P28 | Remove/archive with dirty/unmerged/session state | Same explicit acknowledgements; no hidden destructive side effects. |
| P29 | Settings apply/cancel/failure/live updates | File and running views agree; shell hooks affect only new terminals. |
| P30 | Native close/Quit with foreground work | Cancel leaves app/processes intact; accept performs bounded safe cleanup. |
| P31 | Restart native app | Same current schema/layout/config policy; no restored dead PTYs. |
| P32 | Second app launch | Excluded before DB cleanup; existing window activation policy verified. |
| P33 | Native keyboard/menu/IME use | Terminal controls preserved; text fields/dialogs own correct keys. |
| P34 | VoiceOver/Orca review | Same operational actions reachable; custom terminal accessible strategy works. |
| P35 | Supported Linux/macOS packaging | Runnable bundles, notices, identity, audit and release contract intact. |

Capture baseline behavior before replacing the Vue implementation. Existing test names/fixtures are more reliable than memory or a screenshot of only one app state.

The terminal MVP is the first delivery and user feedback gate; these workspace scenarios are not prerequisites for its initial handoff. As each later workflow is ported, track its scenario ID, existing baseline test/fixture, native automated check, user manual result/environment and open failures. Use pending/pass/fail explicitly; neither a completed widget nor an unreported manual check proves parity. The user owns real UI/platform checks, while the implementer owns automated safety/model tests, runnable instructions, feedback-driven fixes and evidence updates. Do not introduce a separate test framework or tracking file before needed; a per-delivery completion report can hold these records.

## 15. Existing test/source inventory for translation

### Core navigation and layouts

- `src/App.test.ts`.
- `src/domain/main-document.test.ts`.
- `src/domain/ui-state.test.ts`.
- `src/domain/terminal-layout.test.ts`.
- `src/domain/shortcuts.test.ts`.
- `src/components/MainPane.test.ts`, `SessionPane.test.ts`, `Sidebar.test.ts`, `TitlebarMenu.test.ts`.
- `src/presentation/layout-persistence.test.ts`, `terminal-movement.test.ts`, `worktree-sync.test.ts`.

### Terminal

- `src/components/TerminalSession.test.ts`.
- `src/lib/terminal-renderer.test.ts`, `terminal-keys.test.ts`.
- `src/lib/terminal-shell-integration.test.ts`, `terminal-shell-integration-render.test.ts`.
- `src/lib/terminal-click-cursor.test.ts`, `terminal-file-links.test.ts`, `terminal-paths.test.ts`.
- Rust tests within `terminal/mod.rs`, `terminal/process.rs`, `services/terminal.rs`, `commands/terminal.rs`.

### Files, diffs and reviews

- `src/components/InspectorPane.test.ts`, `DocumentPane.test.ts`, `FileDiff.test.ts`.
- `src/components/use-large-diff.test.ts`.
- `src/domain/files.test.ts`, `media.ts`, `review.test.ts`.
- `src/presentation/diff-loader.test.ts`, `active-git-snapshot.test.ts`, `changed-lines.test.ts`, `review-notes.test.ts`, `agent-sessions.test.ts`.
- Rust file/review/review-round/agent/persistence service tests.

### Host and release

- Rust `main.rs` security/host/menu/logging tests; retarget meaningful invariants before removing source-inspection tests tied to Tauri.
- `scripts/audit-rust.test.mjs`, `artifacts.test.mjs`, `build-app.test.mjs`, `build-third-party-notices.test.mjs`.
- Release/artifact script tests and CI workflows.

This is a translation inventory, not an instruction to preserve a Node runtime in the final application. Web-specific tests disappear with the web implementation only when the underlying behavior has native coverage or is intentionally irrelevant.

## 16. Full-port completion rule

The complete migration is done only when:

- The terminal-first gate has passed on supported targets.
- Every required workflow above has a native implementation and verification evidence.
- Formatting implementation, supported current-platform coverage and accessibility are verified. External Node/Prettier is permitted with recorded native-replacement debt if used; inline video is explicitly deferred with preview-unavailable and explicit external-open behavior. Neither acceptance permits silently omitting other workflows.
- The native application's persisted shapes follow the repository's current-schema rule.
- Build, audit, licensing and bundles no longer depend on Tauri/Vue/xterm assets for the application.
- Obsolete web/Tauri host paths are removed in the cutover rather than kept as a supported legacy mode.

The resulting application should still be recognizable as Muster by what a user can do and where they do it. GPUI Kit supplies the controls; the application must preserve the carefully designed workflow.
