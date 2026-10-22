//! The half of the terminal that does not know it will be drawn.
//!
//! `TerminalCore` lives on the PTY reader thread. It owns the VT parser and the
//! grid, takes raw bytes from the reused backend's `OutputSink`, and answers the
//! window with a snapshot of what a view of this size can actually show. Nothing
//! in this module draws or uses a window/application context. Inline images are
//! decoded into immutable raster data on the reader and shared with the painter;
//! the view gets to be redrawn and this part gets the bytes exactly once.
//!
//! The other direction -- keys and pastes in, terminal modes out -- is here
//! too: `keys` and `paste` turn an input event into the exact bytes a child
//! expects, and `write_queue` is the bounded, serialized path those bytes take
//! to the PTY. Neither half names the parser, and `TerminalCore` does not name
//! the encoders: the parser reports, the encoders answer, and neither has to
//! know the other exists.
//!
//! `session` is the join of the two halves: it owns the reused PTY backend and
//! the threads around it, so parsing runs on the reader and the view only ever
//! sees a snapshot, a wake, and state it can act on.

mod core;
mod graphics;
mod graphics_handler;
mod input;
mod keys;
mod links;
mod modes;
mod paste;
mod pi_status;
mod session;
mod sync_updates;
mod view;
mod write_queue;

pub use self::core::{
    CellSnapshot, ColorSnapshot, CursorShapeSnapshot, CursorSnapshot, PromptAnchor, RowSnapshot,
    SelectionKind, TerminalCore, TerminalEvent, MAX_CLIPBOARD_PAYLOAD_BYTES, MAX_TITLE_CHARS,
};
pub use self::graphics::GraphicSnapshot;
pub use self::input::{FocusState, Input, MouseReport};
pub use self::keys::{encode_key, Key, KeyPress};
pub use self::links::LinkTarget;
pub use self::modes::TerminalModes;
pub use self::paste::{encode_paste, PasteError, MAX_PASTE_BYTES};
pub use self::pi_status::{pi_task_note, PiTaskState};
pub use self::session::{
    login_shell, shutdown, CellMetrics, CloseHandle, CloseReport, Foreground, GridRange,
    SessionError, SessionStatus, ShellState, StreamState, TerminalHost, TerminalSession,
    TerminalSessionConfig, TerminalSnapshot, TerminalWake, DEFAULT_DRAIN_DEADLINE,
    DEFAULT_SCROLLBACK_LINES, DEFAULT_STATUS_INTERVAL,
};
/// The grid's own row ratio, which the document is drawn at too.
pub(crate) use self::view::LINE_HEIGHT;
pub use self::view::{xterm_256, CellSize, InputRefused, Palette, Placement, TerminalView};
pub use self::write_queue::{EnqueueError, WriteQueue, MAX_QUEUED_BYTES, MAX_QUEUED_REQUESTS};

/// The key context the grid answers keys in, and the two Tab bindings that
/// belong to it.
///
/// A toolkit dispatches a key binding *before* it hands the key to any
/// element, and the window root's own `tab` binding -- focus traversal --
/// stops the key there. A terminal that waits for `on_key_down` therefore
/// never sees a Tab, and a shell is left with no completion at all. This is
/// the same shape the toolkit's own text field uses: a key context on the
/// element, and bindings registered for that context, which sort deeper than
/// the root's and so are answered first.
pub const TAB_CONTEXT: &str = "MusterTerminal";

gpui_kit::actions!(muster_terminal, [TerminalTab, TerminalShiftTab]);

/// Answer Tab for the terminal, before the window root answers it for itself.
///
/// Called once, with the rest of the toolkit's wiring, before any window
/// exists: a binding registered after a window's first frame would be one
/// keystroke too late.
pub fn install_keys(cx: &mut gpui_kit::App) {
    cx.bind_keys([
        gpui_kit::KeyBinding::new("tab", TerminalTab, Some(TAB_CONTEXT)),
        gpui_kit::KeyBinding::new("shift-tab", TerminalShiftTab, Some(TAB_CONTEXT)),
    ]);
}
