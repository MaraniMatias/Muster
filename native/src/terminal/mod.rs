//! The half of the terminal that does not know it will be drawn.
//!
//! `TerminalCore` lives on the PTY reader thread. It owns the VT parser and the
//! grid, takes raw bytes from the reused backend's `OutputSink`, and answers the
//! window with a snapshot of what a view of this size can actually show. Nothing
//! in this module draws and nothing here names a UI framework, because the view
//! is the part that gets to be redrawn and this is the part that gets the bytes
//! exactly once.
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
mod input;
mod keys;
mod modes;
mod paste;
mod session;
mod view;
mod write_queue;

pub use self::core::{
    CellSnapshot, ColorSnapshot, CursorShapeSnapshot, CursorSnapshot, RowSnapshot, TerminalCore,
    TerminalEvent, MAX_CLIPBOARD_PAYLOAD_BYTES, MAX_TITLE_CHARS,
};
pub use self::input::{FocusState, Input};
pub use self::keys::{encode_key, Key, KeyPress};
pub use self::modes::TerminalModes;
pub use self::paste::{encode_paste, PasteError, MAX_PASTE_BYTES};
pub use self::session::{
    login_shell, shutdown, CellMetrics, CloseHandle, CloseReport, Foreground, SessionError,
    SessionStatus, ShellState, StreamState, TerminalHost, TerminalSession, TerminalSessionConfig,
    TerminalSnapshot, TerminalWake, DEFAULT_DRAIN_DEADLINE, DEFAULT_SCROLLBACK_LINES,
    DEFAULT_STATUS_INTERVAL,
};
pub use self::view::{xterm_256, CellSize, Palette, Placement, TerminalView};
pub use self::write_queue::{EnqueueError, WriteQueue, MAX_QUEUED_BYTES, MAX_QUEUED_REQUESTS};