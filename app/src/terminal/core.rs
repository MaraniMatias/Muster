use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::{Colors, COUNT as COLOR_COUNT};
// Not a test-only path, despite living in a module called `test`: the vendored
// crate leaves `pub mod test` ungated, and it is the crate's own `Dimensions`
// implementation, so there is no reason for this crate to grow a second one.
// `src/terminal_parser.rs` already reaches for the same type.
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Osc52, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, NamedColor, Processor};

use super::graphics::{GraphicSnapshot, Graphics, MAX_PACKET};
use super::graphics_handler::GraphicsHandler;
use super::modes::TerminalModes;
use super::opencode_status::{self, OpenCodeReport};
use super::pi_status::{self, PiProgramStatus, PiTaskState};
use super::sync_updates::{SyncUpdates, MAX_SYNC_BYTES};

/// Longest window title kept, in characters.
///
/// A title is written by whatever program happens to be running and goes straight
/// into a window's chrome, so it is untrusted text with a fixed-size destination.
/// Bounding it in characters rather than bytes keeps the cap meaningful for the
/// emoji and CJK titles that dominate real shells, and keeps the string the UI
/// thread reads a fixed size no matter what the child asks for.
pub const MAX_TITLE_CHARS: usize = 80;

/// Longest OSC 52 clipboard payload kept, in bytes.
///
/// `vte`'s OSC buffer is an ordinary `Vec` when the crate is built with `std`, so
/// an OSC string carries no length limit of its own and is already decoded into a
/// `String` by the time it reaches here. This is where the payload is refused
/// instead of copied: a clipboard write is a line or two of text, and 64 KiB is
/// already far past anything real while staying cheap to compare.
pub const MAX_CLIPBOARD_PAYLOAD_BYTES: usize = 64 * 1024;

/// Longest OSC payload this scanner holds while it waits for a terminator, in
/// bytes.
///
/// Every OSC is accumulated, not only OSC 133, so the cap has to clear the
/// largest one a real program sends -- a clipboard write. Past it the payload is
/// refused whole rather than truncated: half a marker is not a marker, and
/// reading `133;D;1` off the front of a flood would report an exit status nobody
/// sent.
const MAX_OSC_PAYLOAD_BYTES: usize = 64 * 1024;

/// A cell as the view needs it.
///
/// No alacritty type crosses this boundary. A view holding a parser `Color` would
/// be holding a type whose names resolve against the parser's palette, and the
/// palette that matters is the host's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellSnapshot {
    pub c: char,
    /// Codepoints attached to the base cell, including combining marks.
    pub zerowidth: Vec<char>,
    /// Parser-owned column span: zero for spacers, one or two otherwise.
    pub width: u8,
    pub fg: ColorSnapshot,
    pub bg: ColorSnapshot,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inverse: bool,
}

/// The three cases a palette has to answer for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSnapshot {
    /// The theme's own colour: its foreground, background, cursor or bright
    /// foreground. Left to the host, because only the host knows the theme.
    Default,
    /// One of the 256 xterm palette entries, which the host may override.
    Indexed(u8),
    /// An exact colour the program asked for.
    Rgb(u8, u8, u8),
}

/// One visible row, `cols` cells wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSnapshot {
    pub cells: Vec<CellSnapshot>,
}

/// A prompt boundary captured when OSC 133;B is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptAnchor {
    /// Absolute grid line number (negative lines are scrollback).
    pub line: i32,
    /// First editable column.
    pub column: usize,
    /// Scroll position captured with the anchor.
    pub display_offset: usize,
}
/// Where the cursor is, in viewport coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorSnapshot {
    pub row: usize,
    pub col: usize,
    /// False when the program hid the cursor, or when the view is scrolled back
    /// far enough that the cursor is not on screen. `row` is clamped to the
    /// viewport bounds; `col` is the parser's cursor column, unchanged. Only
    /// draw the cursor when `visible` is true.
    pub visible: bool,
    pub shape: CursorShapeSnapshot,
    pub blinking: bool,
    /// OSC 12 override, if the program supplied one.
    pub color: Option<(u8, u8, u8)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShapeSnapshot {
    Block,
    Underline,
    Beam,
    HollowBlock,
    Hidden,
}

/// Local selection granularity; the parser expands words and wrapped lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    Simple,
    Word,
    Line,
}

/// Something the parser reported that the window has to act on.
///
/// No `Clone`/`PartialEq`: each reply carries its own closure, and two closures
/// are never meaningfully equal. `Debug` is written out for the same reason.
pub enum TerminalEvent {
    /// A reply the child asked us to send.
    ///
    /// Never dropped. A dropped reply desynchronises the child permanently: a
    /// program waiting on a keyboard-mode or size answer waits on nothing, and
    /// nothing the host does later will make it ask again.
    PtyWrite(String),
    /// Sanitised and bounded. Never a control character, never longer than
    /// [`MAX_TITLE_CHARS`].
    TitleChanged(String),
    /// OSC 52. Bounded by [`MAX_CLIPBOARD_PAYLOAD_BYTES`], or not produced at all.
    ///
    /// Stored, never applied automatically: a program asking to *write* the
    /// clipboard is a weaker claim than a person pressing a key, and the host is
    /// the one that decides whether to honour it.
    ClipboardStore(String),
    /// A program asked to read the clipboard. Surface it; never satisfy it
    /// silently, and never satisfy it here.
    ClipboardRequested,
    /// A program asked for a palette entry by index.
    ///
    /// Carries the reply rather than the answer because only the host knows its
    /// palette, and carries a reply rather than nothing because a query that is
    /// never answered blocks the program that asked it.
    ColorRequested {
        index: usize,
        /// `respond` turns a resolved `(r, g, b)` into the exact bytes the child
        /// is waiting on. The parser always has a reply to give; what the host
        /// lacks is an override, and answering out of its own palette is the point.
        respond: Box<dyn FnOnce(u8, u8, u8) -> String + Send>,
    },
    /// A program asked the text area's size in pixels.
    ///
    /// The character count is known here; the cell size is the view's, so the
    /// answer is handed back through `respond` the same way.
    TextAreaSizeRequested {
        /// `respond` takes `(cols, rows, cell_width, cell_height)`.
        respond: Box<dyn FnOnce(u16, u16, u16, u16) -> String + Send>,
    },
    /// Pi asked whether the host understands program status.
    ///
    /// The handshake, and it is the only way Pi is told that it may report:
    /// without the answer, Pi keeps its state to itself.
    PiStatusQuery,
    /// Pi reported the state of the task in front of it.
    PiTaskState(PiTaskState),
    /// OpenCode reported the state of the task in front of it.
    ///
    /// The same escape sequence Pi uses, and a different contract: the report
    /// also names the session it is about, which is what says which of the
    /// service's sessions this terminal is showing.
    OpenCodeReport(OpenCodeReport),
    /// Pi's progress indicator: something is running, or is not.
    PiProgress(bool),
    /// A command started in the shell.
    ///
    /// The boundary a report from before it cannot cross: what Pi said was
    /// about the command that was running, and a new command is a new process
    /// in front of the shell.
    CommandStarted,
    /// A command finished in the shell.
    CommandFinished,
    Bell,
}

impl std::fmt::Debug for TerminalEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The replies are closures, and what matters in a log is which query
        // arrived rather than the bytes it turned into.
        match self {
            TerminalEvent::PtyWrite(reply) => write!(f, "PtyWrite({reply:?})"),
            TerminalEvent::TitleChanged(title) => write!(f, "TitleChanged({title:?})"),
            TerminalEvent::ClipboardStore(text) => write!(f, "ClipboardStore({text:?})"),
            TerminalEvent::ClipboardRequested => write!(f, "ClipboardRequested"),
            TerminalEvent::ColorRequested { index, .. } => write!(f, "ColorRequested({index})"),
            TerminalEvent::TextAreaSizeRequested { .. } => write!(f, "TextAreaSizeRequested"),
            TerminalEvent::PiStatusQuery => write!(f, "PiStatusQuery"),
            TerminalEvent::PiTaskState(state) => write!(f, "PiTaskState({state:?})"),
            TerminalEvent::OpenCodeReport(report) => write!(f, "OpenCodeReport({report:?})"),
            TerminalEvent::PiProgress(working) => write!(f, "PiProgress({working})"),
            TerminalEvent::CommandStarted => write!(f, "CommandStarted"),
            TerminalEvent::CommandFinished => write!(f, "CommandFinished"),
            TerminalEvent::Bell => write!(f, "Bell"),
        }
    }
}

/// `EventListener::send_event` takes `&self`, so the terminal cannot hand its
/// events back by value. It parks them and `take_events` drains the buffer on the
/// same reader thread that called `feed`, which is also the only thread that
/// touches the core at all.
struct ParkedEvents {
    events: Arc<Mutex<Vec<Event>>>,
}

impl EventListener for ParkedEvents {
    fn send_event(&self, event: Event) {
        self.events
            .lock()
            .expect("terminal event buffer poisoned")
            .push(event);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StringScan {
    Ground,
    Escape,
    EscapeIntermediate,
    Osc,
    Apc,
    ApcEscape,
    Ignored,
    IgnoredEscape,
}

/// The parser, the grid, and the parser's own view of what it has been told.
///
/// `Send` and not `Sync`, deliberately: this is owned by one thread, the PTY
/// reader, and every method here takes `&mut self` or `&self` on that thread's
/// terms. The window never holds one.
pub struct TerminalCore {
    term: Term<ParkedEvents>,
    events: Arc<Mutex<Vec<Event>>>,
    /// One parser for the whole session.
    ///
    /// `vte`'s parser is stateful between calls: it holds a partial UTF-8
    /// sequence and a half-read escape sequence across the boundary, which is
    /// the only reason a character split across two reads still lands as one
    /// cell. A parser built per chunk would drop that state and the grid would
    /// drift from the byte stream permanently.
    parser: Processor,
    string_scan: StringScan,
    #[cfg(test)]
    parser_scan_test: StringScan,
    #[cfg(test)]
    parser_osc_bytes_test: usize,
    #[cfg(test)]
    parser_osc_max_test: usize,
    /// The payload of the OSC sequence being scanned, across reads.
    osc: Vec<u8>,
    /// That payload outgrew [`MAX_OSC_PAYLOAD_BYTES`] and was refused, so what is
    /// left of it is not a payload at all. One flag per payload: the next
    /// `\x1b]` starts a fresh one.
    osc_oversized: bool,
    /// Everything the parser reported since the window last asked. Held here
    /// rather than translated on demand so a report is never made twice, and so
    /// nothing is reported before the window is ready to act on it.
    pending: Vec<TerminalEvent>,
    /// The last title the program asked for, already sanitised and bounded.
    title: Option<String>,
    prompt_anchor: Option<PromptAnchor>,
    command_exit: Option<i32>,
    graphics: Graphics,
    apc: Vec<u8>,
    apc_oversized: bool,
    sync: SyncUpdates,
}

impl TerminalCore {
    /// `scrollback_lines` is Muster's scrollback setting, the same 10,000 lines
    /// the parser's own `Config::default()` happens to carry. It is set
    /// explicitly rather than left to that default so the number is a decision of
    /// this crate and not of whichever parser gets vendored next.
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self {
        let config = Config {
            scrolling_history: scrollback_lines,
            default_cursor_style: CursorStyle {
                shape: CursorShape::Block,
                blinking: true,
            },
            // Shift+Enter is only a distinct key to a program that has been told
            // it may disambiguate one, and this is what tells it.
            kitty_keyboard: true,
            // The crate's default, `OnlyCopy`, drops OSC 52 *reads* in the
            // handler, before any event exists: a program asking to read the
            // clipboard would be answered with silence, forever. `CopyPaste`
            // lets the request arrive as an event, where the host sees it and may
            // refuse. Silence is not a refusal the host ever gets to make.
            osc52: Osc52::CopyPaste,
            ..Config::default()
        };

        let size = TermSize::new(cols as usize, rows as usize);
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener = ParkedEvents {
            events: Arc::clone(&events),
        };

        Self {
            term: Term::new(config, &size, listener),
            events,
            parser: Processor::new(),
            string_scan: StringScan::Ground,
            #[cfg(test)]
            parser_scan_test: StringScan::Ground,
            #[cfg(test)]
            parser_osc_bytes_test: 0,
            #[cfg(test)]
            parser_osc_max_test: 0,
            osc: Vec::new(),
            osc_oversized: false,
            pending: Vec::new(),
            title: None,
            prompt_anchor: None,
            command_exit: None,
            graphics: Graphics::new(rows),
            apc: Vec::new(),
            apc_oversized: false,
            sync: SyncUpdates::default(),
        }
    }

    /// Raw PTY bytes. Owns the parser, so a sequence split across two calls
    /// parses exactly as it would have in one.
    ///
    /// There is no error to return and nothing to log: every byte a terminal can
    /// receive has a defined meaning, including the ones that mean nothing at
    /// all, and a call that cannot fail is a call the reader thread can make
    /// without a branch.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.flush_sync_timeout(std::time::Instant::now());
        let mut offset = 0;
        for (index, &byte) in bytes.iter().enumerate() {
            let marker = self.sync.scan(byte);
            if self.sync.started.is_some() {
                self.sync.bytes.push(byte);
                if marker == Some(false) || self.sync.bytes.len() == MAX_SYNC_BYTES {
                    self.flush_sync();
                    offset = index + 1;
                } else if marker == Some(true) {
                    self.sync.started = Some(std::time::Instant::now());
                }
            } else if marker == Some(true) {
                self.feed_inner(&bytes[offset..=index]);
                self.sync.started = Some(std::time::Instant::now());
                offset = index + 1;
            }
        }
        if self.sync.started.is_none() && offset < bytes.len() {
            self.feed_inner(&bytes[offset..]);
        }
    }

    pub(crate) fn flush_sync_timeout(&mut self, now: std::time::Instant) -> bool {
        if !self.sync.expired(now) {
            return false;
        }
        self.flush_sync();
        true
    }

    pub(crate) fn flush_sync(&mut self) {
        self.sync.started = None;
        let bytes = std::mem::take(&mut self.sync.bytes);
        self.feed_inner(&bytes);
    }

    fn feed_inner(&mut self, bytes: &[u8]) {
        let mut offset = 0;
        let mut state = self.string_scan;
        for (index, &byte) in bytes.iter().enumerate() {
            let previous = state;
            // After a string's ESC, vte processes the next byte as native Escape.
            // Reuse that classification for non-ST APC and ignored-string recovery.
            let transition = match (state, byte) {
                (StringScan::ApcEscape | StringScan::IgnoredEscape, b'\\' | 0x18 | 0x1a) => state,
                (StringScan::ApcEscape | StringScan::IgnoredEscape, _) => StringScan::Escape,
                _ => state,
            };
            if state == StringScan::ApcEscape && byte != b'\\' {
                self.apc_oversized = true;
                self.graphics.abort(self.is_alt_screen());
            }
            state = match (transition, byte) {
                (StringScan::Ground, 0x1b) => StringScan::Escape,
                (StringScan::Escape, b']') => {
                    // The payload is accumulated here rather than sliced out of
                    // this chunk: a marker can arrive split across two reads,
                    // and a terminal that drops the half it already saw reports
                    // no prompt at all.
                    self.osc.clear();
                    self.osc_oversized = false;
                    StringScan::Osc
                }
                (StringScan::Escape, b'_') => {
                    self.apc.clear();
                    self.apc_oversized = false;
                    StringScan::Apc
                }
                (StringScan::Escape, b'P' | b'^' | b'X') => StringScan::Ignored,
                (StringScan::Escape, 0x18 | 0x1a) => StringScan::Ground,
                (StringScan::Escape, 0x1b) => StringScan::Escape,
                (StringScan::Escape, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff) => {
                    StringScan::Escape
                }
                (StringScan::Escape, 0x20..=0x2f) => StringScan::EscapeIntermediate,
                (StringScan::Escape, _) => StringScan::Ground,
                (StringScan::EscapeIntermediate, 0x18 | 0x1a) => StringScan::Ground,
                (StringScan::EscapeIntermediate, 0x1b) => StringScan::Escape,
                (
                    StringScan::EscapeIntermediate,
                    0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff | 0x20..=0x2f,
                ) => StringScan::EscapeIntermediate,
                (StringScan::EscapeIntermediate, _) => StringScan::Ground,

                (StringScan::Osc, 0x07 | 0x18 | 0x1a) => StringScan::Ground,
                // vte dispatches OSC and leaves the string state on ESC itself.
                // Its next byte is parsed from Escape, whether or not it is ST.
                (StringScan::Osc, 0x1b) => StringScan::Escape,
                (StringScan::Osc, _) => StringScan::Osc,
                (StringScan::Apc, 0x1b) => StringScan::ApcEscape,
                (StringScan::Apc, 0x18 | 0x1a) => {
                    self.apc.clear();
                    self.graphics.abort(self.is_alt_screen());
                    StringScan::Ground
                }
                (StringScan::Apc, _) => {
                    if !self.apc_oversized {
                        if self.apc.len() == MAX_PACKET {
                            self.apc.clear();
                            self.apc_oversized = true;
                            self.graphics.abort(self.is_alt_screen());
                        } else {
                            self.apc.push(byte);
                        }
                    }
                    StringScan::Apc
                }
                (StringScan::ApcEscape, b'\\') => StringScan::Ground,
                (StringScan::ApcEscape, _) => StringScan::Ground,
                (StringScan::Ignored, 0x1b) => StringScan::IgnoredEscape,
                (StringScan::IgnoredEscape, b'\\') | (StringScan::Ignored, 0x18 | 0x1a) => {
                    StringScan::Ground
                }
                (StringScan::IgnoredEscape, 0x18 | 0x1a) => StringScan::Ground,
                (StringScan::IgnoredEscape, _) => StringScan::Ground,
                (StringScan::Ignored, _) => StringScan::Ignored,
                (StringScan::Ground, _) => StringScan::Ground,
            };
            let terminated =
                matches!(previous, StringScan::Osc) && matches!(byte, 0x07 | 0x18 | 0x1a | 0x1b);
            let graphic_terminated = matches!(previous, StringScan::ApcEscape) && byte == b'\\';
            if matches!(previous, StringScan::Osc) && matches!(state, StringScan::Osc) {
                let was_oversized = self.osc_oversized;
                self.scan_osc(byte);
                if !was_oversized && self.osc_oversized {
                    // The parser has already received at most the accepted prefix.
                    // Drop its owned OSC buffer without dispatching that prefix;
                    // subsequent bytes are discarded until the actual terminator.
                    self.advance(&bytes[offset..index]);
                    self.parser = Processor::new();
                    #[cfg(test)]
                    {
                        self.parser_scan_test = StringScan::Ground;
                        self.parser_osc_bytes_test = 0;
                    }
                    offset = index + 1;
                }
            }
            if terminated || graphic_terminated {
                let oversized_osc_terminated =
                    matches!(previous, StringScan::Osc) && self.osc_oversized;
                if !matches!(previous, StringScan::Osc) {
                    state = StringScan::Ground;
                }
                // The grid is read after the parser has consumed exactly up to
                // the terminator and before anything after it, which is the
                // position the marker was written at.
                if oversized_osc_terminated {
                    match (previous, byte) {
                        (StringScan::Osc, 0x1b) => self.advance(&[0x1b]),
                        (StringScan::Osc, 0x18 | 0x1a) => self.advance(&[byte]),
                        _ => {}
                    }
                } else {
                    self.advance(&bytes[offset..=index]);
                }
                if oversized_osc_terminated {
                    self.osc_oversized = false;
                }
                if graphic_terminated {
                    if !self.apc_oversized {
                        if let Some(reply) = self.graphics.apply(&self.apc, &mut self.term) {
                            self.pending.push(TerminalEvent::PtyWrite(reply));
                        }
                    }
                    self.apc.clear();
                } else {
                    let marker = std::mem::take(&mut self.osc);
                    if !self.osc_oversized {
                        self.apply_osc(&marker);
                    }
                }
                self.handle_events_at_position();
                offset = index + 1;
            }
        }
        if offset < bytes.len() && !(matches!(state, StringScan::Osc) && self.osc_oversized) {
            self.advance(&bytes[offset..]);
            self.handle_events_at_position();
        }
        self.string_scan = state;
    }

    /// A full-damage operation can scroll or replace rows. There is no parser
    /// marker API to follow our separate prompt anchor through that operation,
    /// so retire it rather than allow a click to edit unrelated content. Merely
    /// scrolling the viewport is safe and is not part of this input operation.
    fn advance(&mut self, bytes: &[u8]) {
        #[cfg(test)]
        self.observe_parser_osc_bytes(bytes);
        self.term.reset_damage();
        self.parser.advance(
            &mut GraphicsHandler {
                term: &mut self.term,
                graphics: &mut self.graphics,
            },
            bytes,
        );
        // Raw synchronized updates are held by `feed`, so the processor must
        // not defer the text before a graphics command to a second timeline.
        if self.parser.sync_timeout().sync_timeout().is_some() {
            self.parser.stop_sync(&mut GraphicsHandler {
                term: &mut self.term,
                graphics: &mut self.graphics,
            });
        }
        self.graphics
            .prune(self.is_alt_screen(), self.history_size());
        if matches!(self.term.damage(), TermDamage::Full) || self.is_alt_screen() {
            self.prompt_anchor = None;
        }
    }

    #[cfg(test)]
    fn observe_parser_osc_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let previous = self.parser_scan_test;
            self.parser_scan_test = match (previous, byte) {
                (StringScan::Ground, 0x1b) => StringScan::Escape,
                (StringScan::Escape, b']') => {
                    self.parser_osc_bytes_test = 0;
                    StringScan::Osc
                }
                (StringScan::Escape, 0x18 | 0x1a) => StringScan::Ground,
                (StringScan::Escape, 0x1b) => StringScan::Escape,
                (StringScan::Escape, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff) => {
                    StringScan::Escape
                }
                (StringScan::Escape, 0x20..=0x2f) => StringScan::EscapeIntermediate,
                (StringScan::Escape, _) => StringScan::Ground,
                (StringScan::EscapeIntermediate, 0x18 | 0x1a) => StringScan::Ground,
                (StringScan::EscapeIntermediate, 0x1b) => StringScan::Escape,
                (
                    StringScan::EscapeIntermediate,
                    0x20..=0x2f | 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff,
                ) => StringScan::EscapeIntermediate,
                (StringScan::EscapeIntermediate, _) => StringScan::Ground,
                (StringScan::Osc, 0x07 | 0x18 | 0x1a) => StringScan::Ground,
                (StringScan::Osc, 0x1b) => StringScan::Escape,
                (StringScan::Osc, _) => StringScan::Osc,
                (state, _) => state,
            };
            if previous == StringScan::Osc && self.parser_scan_test == StringScan::Osc {
                self.parser_osc_bytes_test += 1;
                self.parser_osc_max_test = self.parser_osc_max_test.max(self.parser_osc_bytes_test);
            }
        }
    }

    /// One byte of the OSC payload being scanned, kept only while it fits.
    ///
    /// The refusal is sticky: a payload that outgrew the cap is never refilled,
    /// because a fresh buffer halfway through an oversized sequence is exactly
    /// how a truncated `133;D;1` would end up read as a status.
    fn scan_osc(&mut self, byte: u8) {
        if self.osc_oversized {
            return;
        }
        if self.osc.len() >= MAX_OSC_PAYLOAD_BYTES {
            self.osc.clear();
            self.osc_oversized = true;
            return;
        }
        self.osc.push(byte);
    }

    /// What one OSC payload means, applied at the position it was written.
    ///
    /// Three sequences are read here and everything else is left alone: the
    /// shell's own markers, Pi's program status, and the progress indicator
    /// every terminal understands.
    fn apply_osc(&mut self, payload: &[u8]) {
        if payload.starts_with(b"133;") {
            self.apply_shell_marker(payload);
        } else if let Some(data) = payload.strip_prefix(b"7501;") {
            self.apply_program_status(data);
        } else if let Some(data) = payload.strip_prefix(b"9;") {
            self.apply_progress(data);
        }
    }

    /// What one OSC 133 payload means, applied at the position it was written.
    ///
    /// `B` is the prompt the person can still edit, `C` says a command is
    /// running and `D` carries its exit status. Anything else is somebody
    /// else's sequence and is left alone.
    fn apply_shell_marker(&mut self, payload: &[u8]) {
        let Some(marker) = payload.strip_prefix(b"133;") else {
            return;
        };
        let text = String::from_utf8_lossy(marker);
        // Split on the marker code itself: `C` and `D` both arrive with and
        // without parameters, and a shell that sends the bare form must not be
        // read as somebody else's sequence.
        let (code, rest) = text.split_once(';').unwrap_or((text.as_ref(), ""));
        match code {
            "D" => {
                self.command_exit = rest.parse().ok();
                self.pending.push(TerminalEvent::CommandFinished);
            }
            "C" => {
                self.command_exit = None;
                self.prompt_anchor = None;
                self.pending.push(TerminalEvent::CommandStarted);
            }
            "B" if !self.is_alt_screen() => {
                let grid = self.term.grid();
                self.prompt_anchor = Some(PromptAnchor {
                    line: grid.cursor.point.line.0,
                    column: grid.cursor.point.column.0,
                    display_offset: grid.display_offset(),
                });
            }
            _ => {}
        }
    }

    /// What one OSC 7501 payload means.
    ///
    /// The payload is refused whole when it is malformed, so a broken report
    /// never becomes half a state.
    fn apply_program_status(&mut self, data: &[u8]) {
        let Ok(text) = std::str::from_utf8(data) else {
            return;
        };
        match pi_status::parse_program_status(text) {
            Some(PiProgramStatus::Query) => self.pending.push(TerminalEvent::PiStatusQuery),
            Some(PiProgramStatus::State(state)) => {
                self.pending.push(TerminalEvent::PiTaskState(state))
            }
            // Pi's shape and OpenCode's are two contracts over one escape
            // sequence, and a payload that is not Pi's may still be OpenCode's.
            None => {
                if let Some(report) = opencode_status::parse_opencode_status(text) {
                    self.pending.push(TerminalEvent::OpenCodeReport(report));
                }
            }
        }
    }

    /// What one OSC 9 payload means, when it is the progress indicator.
    fn apply_progress(&mut self, data: &[u8]) {
        let Ok(text) = std::str::from_utf8(data) else {
            return;
        };
        if let Some(progress) = pi_status::parse_progress(text) {
            self.pending.push(TerminalEvent::PiProgress(
                progress == pi_status::PiProgress::Working,
            ));
        }
    }

    fn handle_events_at_position(&mut self) {
        let parked =
            std::mem::take(&mut *self.events.lock().expect("terminal event buffer poisoned"));
        for event in parked {
            if let Event::Title(raw) = &event {
                if raw.strip_prefix("133;").is_some() {
                    continue;
                }
            }
            if let Some(event) = self.translate(event) {
                self.pending.push(event);
            }
        }
    }

    /// Resize the grid. The scrollback survives; the scroll offset does too,
    /// clamped by the grid to whatever the shorter history can still show.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.prompt_anchor = None;
        self.graphics.resize(rows as usize);
        self.term
            .resize(TermSize::new(cols as usize, rows as usize));
    }

    /// Everything the parser reported since the last call.
    ///
    /// Protocol replies are carried through one for one. Signals that only mean
    /// "redraw" are coalesced away, because a caller repaints once it has read
    /// this list either way and carrying them would cost an allocation per chunk.
    pub fn take_events(&mut self) -> Vec<TerminalEvent> {
        std::mem::take(&mut self.pending)
    }

    /// One parser event as one view event, or nothing.
    ///
    /// `None` is a decision, not a leftover. See the grouped arms below for which
    /// events it covers and why dropping each is the right answer here.
    fn translate(&mut self, event: Event) -> Option<TerminalEvent> {
        match event {
            Event::PtyWrite(reply) => Some(TerminalEvent::PtyWrite(reply)),

            // Both stored and reported: `title` answers "what is the title now"
            // for a view that asks, and the event answers "it changed" for one
            // that has to be told. A title that sanitises down to nothing is a
            // reset as far as the view is concerned.
            Event::Title(raw) => {
                self.title = sanitise_title(&raw);
                self.title.clone().map(TerminalEvent::TitleChanged)
            }
            Event::ResetTitle => {
                self.title = None;
                None
            }

            // Refused above the cap rather than truncated: a clipboard write is
            // all-or-nothing, and half a program payload is worse than none,
            // because it looks like the write succeeded.
            Event::ClipboardStore(_, payload) => (payload.len() <= MAX_CLIPBOARD_PAYLOAD_BYTES)
                .then_some(TerminalEvent::ClipboardStore(payload)),

            // The formatter that would answer this read goes with the event. The
            // child is not answered here and must not be: whether this program's
            // clipboard is the user's to hand over is the host's decision.
            Event::ClipboardLoad(..) => Some(TerminalEvent::ClipboardRequested),

            Event::Bell => Some(TerminalEvent::Bell),

            // Redraw hints, and nothing else.
            Event::MouseCursorDirty | Event::Wakeup | Event::CursorBlinkingChange => None,

            // Queries the child is blocked on. Neither is answered here and neither
            // is dropped: the palette and the cell metrics are the host's, so both
            // are handed on with the reply attached, and a program that asked once
            // gets exactly one chance to be unblocked.
            Event::ColorRequest(index, format) => {
                if index < COLOR_COUNT {
                    if let Some(color) = self.term.colors()[index] {
                        return Some(TerminalEvent::PtyWrite(format(color)));
                    }
                }
                Some(TerminalEvent::ColorRequested {
                    index,
                    respond: Box::new(move |r, g, b| {
                        format(alacritty_terminal::vte::ansi::Rgb { r, g, b })
                    }),
                })
            }
            Event::TextAreaSizeRequest(format) => Some(TerminalEvent::TextAreaSizeRequested {
                respond: Box::new(move |cols, rows, cell_width, cell_height| {
                    format(WindowSize {
                        num_lines: rows,
                        num_cols: cols,
                        cell_width,
                        cell_height,
                    })
                }),
            }),

            // Raised by the tty layer, or by a caller that decides the terminal is
            // over. Nothing here is either.
            Event::Exit | Event::ChildExit(..) => None,
        }
    }

    /// The current title, sanitised and bounded, or `None` if there is none.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn prompt_anchor(&self) -> Option<PromptAnchor> {
        let anchor = self.prompt_anchor?;
        let grid = self.term.grid();
        // Display scrolling does not change a grid line's identity. Input that
        // can move or replace rows is guarded separately in `advance`.
        (-(grid.history_size() as i32)..grid.screen_lines() as i32)
            .contains(&anchor.line)
            .then_some(anchor)
    }

    pub fn move_prompt_caret(&mut self, row: usize, column: usize) -> Option<usize> {
        self.prompt_caret_move(row, column).map(|(count, _)| count)
    }

    /// Arrow count and direction (`true` for right) from one authoritative read.
    /// Soft-wrapped rows are one edit line; hard newlines, tabs and combining
    /// clusters are refused because shell editors can count them differently.
    pub fn prompt_caret_move(&self, row: usize, column: usize) -> Option<(usize, bool)> {
        if self.is_alt_screen() || self.is_mouse_tracking() {
            return None;
        }
        let anchor = self.prompt_anchor()?;
        let grid = self.term.grid();
        if row >= grid.screen_lines() || column >= grid.columns() {
            return None;
        }
        let target = Point::new(
            Line(row as i32 - grid.display_offset() as i32),
            Column(column),
        );
        let start = Point::new(Line(anchor.line), Column(anchor.column));
        let cursor = grid.cursor.point;
        if target < start || cursor < start {
            return None;
        }

        // Find the logical line, following only rows explicitly soft-wrapped by
        // the parser. A separate continuation prompt is not guessed to be input.
        let mut last_line = start.line;
        while last_line < grid.bottommost_line()
            && grid[last_line][grid.last_column()]
                .flags
                .contains(Flags::WRAPLINE)
        {
            last_line += 1;
        }
        if target.line > last_line || cursor.line > last_line {
            return None;
        }
        let columns = grid.columns();
        let ordinal =
            |point: Point| (point.line - start.line).0 as usize * columns + point.column.0;
        let cursor_offset = ordinal(cursor) + usize::from(grid.cursor.input_needs_wrap);
        let mut target_offset = ordinal(target);
        if grid[target.line][target.column]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
        {
            target_offset = target_offset.saturating_sub(1);
        }

        // The caret certifies occupied cells up to itself. Text to its right
        // certifies the remaining input, but empty padding past that is inert.
        let mut end = cursor_offset;
        for line in start.line.0..=last_line.0 {
            let first = if line == start.line.0 {
                start.column.0
            } else {
                0
            };
            for col in first..columns {
                let cell = &grid[Line(line)][Column(col)];
                if cell.c == '\t' {
                    // A tab and its fill columns are not individual edit keys.
                    return None;
                }
                if cell.c != ' ' && !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                        2
                    } else {
                        1
                    };
                    end = end.max(ordinal(Point::new(Line(line), Column(col))) + width);
                }
            }
        }
        if target_offset > end || target_offset == cursor_offset {
            return None;
        }

        let (from, to) = if target_offset < cursor_offset {
            (target_offset, cursor_offset)
        } else {
            (cursor_offset, target_offset)
        };
        let mut count = 0;
        for offset in from..to {
            let line = start.line + (offset / columns) as i32;
            let col = Column(offset % columns);
            let cell = &grid[line][col];
            if cell.zerowidth().is_some_and(|chars| !chars.is_empty()) {
                return None;
            }
            if !cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                count += 1;
            }
        }
        (count > 0).then_some((count, target_offset > cursor_offset))
    }

    pub fn invalidate_prompt_anchor(&mut self) {
        self.prompt_anchor = None;
    }

    /// The exit status the last *finished* command reported over OSC 133 `D`.
    ///
    /// `None` covers three states the parser cannot tell apart and a reader must
    /// not guess between: no command has finished, a new one started (`C`), and
    /// the status was absent or unparseable. Only what the child printed is
    /// recorded here -- never the shell's own exit status, and never anything
    /// about a foreground program.
    pub fn command_exit(&self) -> Option<i32> {
        self.command_exit
    }

    /// Only the rows a view of this size can show, honouring the scroll offset.
    ///
    /// Every row is `cols` cells wide. A view indexes by column, and trimming the
    /// tail of a row would mean every one of them had to handle a row shorter
    /// than the screen to save an allocation nobody is short of.
    pub(crate) fn graphic_snapshots(&self) -> Vec<GraphicSnapshot> {
        self.graphics.snapshot(&self.term)
    }

    pub(crate) fn set_graphics_cell_metrics(&mut self, width: u16, height: u16) {
        self.graphics.cell = [width.max(1), height.max(1)];
    }

    pub fn visible_rows(&self) -> Vec<RowSnapshot> {
        let grid = self.term.grid();
        // Viewport row 0 sits `display_offset` lines above the top of the screen.
        // This is the same mapping the parser's own damage iterator uses, so the
        // snapshot and any future damage tracking agree on what row 0 is.
        let offset = grid.display_offset() as i32;

        (0..grid.screen_lines())
            .map(|row| RowSnapshot {
                cells: (0..grid.columns())
                    .map(|col| {
                        let mut cell = cell_snapshot(
                            &grid[Line(row as i32 - offset)][Column(col)],
                            self.term.colors(),
                        );
                        // Graphics placeholders are metadata, not font glyphs.
                        if cell.c == super::graphics::PLACEHOLDER {
                            cell.c = ' ';
                            cell.zerowidth.clear();
                        }
                        cell
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn cursor(&self) -> CursorSnapshot {
        let grid = self.term.grid();
        let row = grid.cursor.point.line.0 + grid.display_offset() as i32;
        let shape = self.term.cursor_style().shape;

        // Three independent ways to have no cursor to draw: the program hid it,
        // the program switched it off, and the view scrolled back past it.
        let visible = self.term.mode().contains(TermMode::SHOW_CURSOR)
            && shape != CursorShape::Hidden
            && (0..grid.screen_lines() as i32).contains(&row);

        CursorSnapshot {
            // Clamped rather than wrapped: the row means nothing whenever
            // `visible` is false, and a wrapped one would read as a real position
            // somewhere else on the screen.
            row: row.max(0).min(grid.screen_lines().saturating_sub(1) as i32) as usize,
            col: grid.cursor.point.column.0,
            visible,
            blinking: self.term.cursor_style().blinking,
            color: self.term.colors()[NamedColor::Cursor].map(|color| (color.r, color.g, color.b)),
            shape: match shape {
                CursorShape::Block => CursorShapeSnapshot::Block,
                CursorShape::Underline => CursorShapeSnapshot::Underline,
                CursorShape::Beam => CursorShapeSnapshot::Beam,
                CursorShape::HollowBlock => CursorShapeSnapshot::HollowBlock,
                CursorShape::Hidden => CursorShapeSnapshot::Hidden,
            },
        }
    }

    /// How many lines the view is scrolled back from the live bottom.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    pub fn background(&self) -> Option<(u8, u8, u8)> {
        self.term.colors()[NamedColor::Background].map(|color| (color.r, color.g, color.b))
    }

    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    pub fn scroll_by(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn set_display_offset(&mut self, offset: usize) {
        // The grid only scrolls by a delta, so an absolute offset is a delta from
        // wherever it already sits. `Scroll::Delta` clamps the result to the
        // history, which is what stops a view asking for an offset past the top.
        let from = i32::try_from(self.term.grid().display_offset()).unwrap_or(i32::MAX);
        // A `usize` offset is never negative, so this subtraction cannot overflow.
        let to = i32::try_from(offset).unwrap_or(i32::MAX);

        self.term.scroll_display(Scroll::Delta(to - from));
    }

    /// The alternate screen is a separate buffer with its own cursor and no
    /// scrollback, so while it is up there is no history to scroll: the scroll
    /// offset a view sets belongs to a screen nobody is looking at.
    pub fn is_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// True when a mouse-reporting mode is on, so the view knows not to also
    /// start a text selection for the same gesture. Both meaning "this click is
    /// the program's" is how a drag ends up in the scrollback by accident.
    pub fn is_mouse_tracking(&self) -> bool {
        self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// True when a paste has to be bracketed, because the program asked to be
    /// told which kind of paste it is getting.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Every mode the input path reads, as plain booleans.
    ///
    /// Read on the reader thread like every other parser question, and handed
    /// over by value: the encoders in this module decide what a key press means
    /// from this snapshot, and they must decide it from a mode the child
    /// actually pushed rather than from a flag the host set. Notably it carries
    /// no record of the child having *asked* about the keyboard protocol --
    /// `CSI ? u` is a question, and only `CSI > 1 u` is an answer this crate is
    /// allowed to act on.
    pub fn modes(&self) -> TerminalModes {
        let mode = self.term.mode();

        TerminalModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            disambiguate_esc_codes: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
            report_event_types: mode.contains(TermMode::REPORT_EVENT_TYPES),
            newline_is_return: mode.contains(TermMode::LINE_FEED_NEW_LINE),
            mouse_click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            mouse_drag: mode.contains(TermMode::MOUSE_DRAG),
            mouse_motion: mode.contains(TermMode::MOUSE_MOTION),
            mouse_sgr: mode.contains(TermMode::SGR_MOUSE),
            mouse_utf8: mode.contains(TermMode::UTF8_MOUSE),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
        }
    }

    pub fn cols(&self) -> u16 {
        // The grid was only ever sized from a `u16` and `Term::resize` only ever
        // takes one, so this cannot truncate.
        self.term.grid().columns() as u16
    }

    pub fn rows(&self) -> u16 {
        self.term.grid().screen_lines() as u16
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn selection_range(&self) -> Option<alacritty_terminal::selection::SelectionRange> {
        self.term.selection.as_ref()?.to_range(&self.term)
    }

    fn selection_point(&self, (row, col): (usize, usize)) -> Point {
        let grid = self.term.grid();
        Point::new(
            Line(
                row.min(grid.screen_lines().saturating_sub(1)) as i32
                    - grid.display_offset() as i32,
            ),
            Column(col.min(grid.columns().saturating_sub(1))),
        )
    }

    /// Capture an anchor in grid coordinates once, not once per drag frame.
    /// The parser rotates this selection with output, independently of the view.
    pub fn start_selection(&mut self, point: (usize, usize), kind: SelectionKind) {
        let point = self.selection_point(point);
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        let mut selection = Selection::new(ty, point, Side::Left);
        if kind != SelectionKind::Simple {
            selection.update(point, Side::Right);
        }
        self.term.selection = Some(selection);
    }

    pub fn update_selection(&mut self, end: (usize, usize)) {
        let end = self.selection_point(end);
        if let Some(selection) = &mut self.term.selection {
            selection.update(end, Side::Right);
            selection.include_all();
        }
    }

    /// Select the inclusive grid range described by viewport cells.
    pub fn select(&mut self, start: (usize, usize), end: (usize, usize)) {
        self.start_selection(start, SelectionKind::Simple);
        self.update_selection(end);
    }
}

/// Strip what a title must never carry, then bound what is left.
///
/// DEL is the control character worth naming: every other C0 byte ends the OSC
/// string in the parser before it can be payload, but DEL is ordinary data, so it
/// does reach here and would otherwise ride into a window's chrome. Titles come
/// from whatever program is running, which is the same trust level as its output.
fn sanitise_title(raw: &str) -> Option<String> {
    // Capped before trimming rather than after: the parser's OSC buffer is not
    // bounded, so this is also the point where the copy out of it is bounded. A
    // title that is nothing but whitespace is no title at all.
    let capped: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE_CHARS)
        .collect();
    let title = capped.trim();

    (!title.is_empty()).then(|| title.to_owned())
}

/// Copy one grid cell into the shape the view reads.
fn cell_snapshot(cell: &Cell, colors: &Colors) -> CellSnapshot {
    CellSnapshot {
        c: cell.c,
        zerowidth: cell.zerowidth().unwrap_or_default().to_vec(),
        width: if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            0
        } else if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        },
        fg: color_snapshot(cell.fg, colors),
        bg: color_snapshot(cell.bg, colors),
        bold: cell.flags.contains(Flags::BOLD),
        dim: cell.flags.contains(Flags::DIM),
        italic: cell.flags.contains(Flags::ITALIC),
        // Four ways to underline a cell and one flag to ask for; a view that
        // only knew about `UNDERLINE` would drop three of them.
        underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
        strikethrough: cell.flags.contains(Flags::STRIKEOUT),
        inverse: cell.flags.contains(Flags::INVERSE),
    }
}

/// Map a parser colour onto the three cases a palette has to answer for.
fn color_snapshot(color: Color, colors: &Colors) -> ColorSnapshot {
    let override_color = match color {
        Color::Spec(rgb) => Some(rgb),
        Color::Indexed(index) => colors[index as usize],
        Color::Named(named) => colors[named].or_else(|| {
            matches!(
                named,
                NamedColor::BrightForeground | NamedColor::DimForeground
            )
            .then(|| colors[NamedColor::Foreground])
            .flatten()
        }),
    };
    if let Some(rgb) = override_color {
        return ColorSnapshot::Rgb(rgb.r, rgb.g, rgb.b);
    }
    match color {
        Color::Spec(_) => unreachable!("literal RGB was resolved above"),
        Color::Indexed(index) => ColorSnapshot::Indexed(index),
        Color::Named(named) => {
            // `NamedColor` is documented as castable to an index precisely so it
            // can address a colour list, which is what its discriminant order
            // encodes. It is not a `u8`: the theme's own colours sit past 255.
            let index: usize = match named {
                // The first sixteen are the xterm palette, in order.
                color if color as usize <= 15 => color as usize,

                // The dim variants are those same sixteen a second time: the
                // parser records "dim" by naming a dimmer colour. The cell
                // already carries `dim`, so folding these onto the index they dim
                // lets the view apply one rule instead of two.
                NamedColor::DimBlack => NamedColor::Black as usize,
                NamedColor::DimRed => NamedColor::Red as usize,
                NamedColor::DimGreen => NamedColor::Green as usize,
                NamedColor::DimYellow => NamedColor::Yellow as usize,
                NamedColor::DimBlue => NamedColor::Blue as usize,
                NamedColor::DimMagenta => NamedColor::Magenta as usize,
                NamedColor::DimCyan => NamedColor::Cyan as usize,
                NamedColor::DimWhite => NamedColor::White as usize,

                // Foreground, background, cursor, bright foreground, dim
                // foreground: all of these mean "whatever the theme says", which
                // is what `Default` asks the host for.
                _ => return ColorSnapshot::Default,
            };

            colors[index].map_or(ColorSnapshot::Indexed(index as u8), |rgb| {
                ColorSnapshot::Rgb(rgb.r, rgb.g, rgb.b)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMNS: u16 = 80;
    const SCREEN_LINES: u16 = 24;

    /// A core sized like the parser tests above.
    fn core() -> TerminalCore {
        // The core is owned by the PTY reader thread, so a core that stopped being
        // `Send` would push every call site back behind a lock. Nothing else here
        // checks it, and it is cheaper to check than to find out later.
        fn assert_send<T: Send>() {}
        assert_send::<TerminalCore>();

        TerminalCore::new(COLUMNS, SCREEN_LINES, 10_000)
    }

    /// The first visible row of a core, as plain chars.
    fn first_row(core: &TerminalCore) -> Vec<char> {
        core.visible_rows()
            .first()
            .expect("a 24 row core has a first row")
            .cells
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    #[test]
    fn a_multibyte_character_split_across_feeds_lands_as_one_cell() {
        // A parser rebuilt per chunk forgets the bytes it could not decode yet and
        // writes a replacement character instead, and nothing later repairs it.
        for (c, split_at) in [('é', 1_usize), ('😀', 2)] {
            let mut encoded = [0u8; 4];
            let bytes = c.encode_utf8(&mut encoded).as_bytes();
            assert!(
                split_at < bytes.len(),
                "the split has to land inside the sequence"
            );

            let mut whole = core();
            whole.feed(bytes);

            let mut split = core();
            split.feed(&bytes[..split_at]);
            split.feed(&bytes[split_at..]);

            assert_eq!(
                whole.visible_rows(),
                split.visible_rows(),
                "`{c}` fed whole and in two chunks"
            );

            let row = first_row(&whole);
            assert!(
                row.contains(&c),
                "`{c}` should have landed in one cell, got {row:?}"
            );
        }
    }

    #[test]
    fn an_escape_sequence_split_across_feeds_is_parsed_once() {
        let mut whole = core();
        whole.feed(b"\x1b[31mred\x1b[0m");

        // Cut between the digits of the colour parameter: a parser that started
        // over on each chunk would either drop the sequence or apply it twice.
        let mut split = core();
        split.feed(b"\x1b[3");
        split.feed(b"1mred\x1b[0m");

        assert_eq!(whole.visible_rows(), split.visible_rows());

        let rows = split.visible_rows();
        let red = &rows[0].cells[..3];
        assert_eq!(red.iter().map(|cell| cell.c).collect::<String>(), "red");
        assert!(
            red.iter().all(|cell| cell.fg == ColorSnapshot::Indexed(1)),
            "all three cells should be the indexed colour for red, got {:?}",
            red.iter().map(|cell| cell.fg).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_title_split_across_feeds_still_sets_the_title() {
        // BEL is one payload byte; ST is two, and the split can land between them,
        // which leaves the parser mid-way through the terminator it is holding.
        for sequence in [
            b"\x1b]0;split title\x07".as_slice(),
            b"\x1b]0;split title\x1b\\".as_slice(),
        ] {
            let mut whole = core();
            whole.feed(sequence);

            assert_eq!(
                whole.title(),
                Some("split title"),
                "the title fed in one call"
            );

            for at in [7, sequence.len() - 1] {
                let mut split = core();
                split.feed(&sequence[..at]);
                split.feed(&sequence[at..]);

                assert_eq!(split.title(), Some("split title"), "split at byte {at}");
                assert!(
                    split.take_events()
                        .iter()
                        .any(|event| matches!(event, TerminalEvent::TitleChanged(title) if title == "split title")),
                    "splitting at byte {at} should still report the title change"
                );
            }
        }
    }

    #[test]
    fn osc133_bel_and_st_are_forwarded_without_losing_split_input() {
        for ending in [b"\x07".as_slice(), b"\x1b\\".as_slice()] {
            let marker = [b"\x1b]133;B".as_slice(), ending].concat();
            for split in 1..marker.len() {
                let mut terminal = TerminalCore::new(80, 24, 20);
                terminal.feed(&marker[..split]);
                terminal.feed(&marker[split..]);
                terminal.feed(b"visible");
                assert!(terminal.visible_rows()[0]
                    .cells
                    .iter()
                    .any(|cell| cell.c == 'v'));
            }
        }
    }

    #[test]
    fn a_program_status_split_across_reads_is_read_once() {
        for ending in [b"\x07".as_slice(), b"\x1b\\".as_slice()] {
            let status = [b"\x1b]7501;state=working".as_slice(), ending].concat();
            for split in 1..status.len() {
                let mut terminal = TerminalCore::new(80, 24, 20);
                terminal.feed(&status[..split]);
                terminal.feed(&status[split..]);
                let states = terminal
                    .take_events()
                    .iter()
                    .filter(|event| {
                        matches!(event, TerminalEvent::PiTaskState(PiTaskState::Working))
                    })
                    .count();
                assert_eq!(
                    states, 1,
                    "a status split at byte {split} is read once, not twice and not never"
                );
            }
        }
    }

    #[test]
    fn an_opencode_report_names_its_state_and_its_session() {
        let mut terminal = TerminalCore::new(80, 24, 20);
        terminal
            .feed(b"\x1b]7501;state=working:id=ses_e912b168dffeXW3oWEfvXIw4U0:app=opencode\x07");
        let report = terminal
            .take_events()
            .into_iter()
            .find_map(|event| match event {
                TerminalEvent::OpenCodeReport(report) => Some(report),
                _ => None,
            })
            .expect("the report is read");
        assert_eq!(report.state, opencode_status::OpenCodeState::Working);
        assert_eq!(
            report.session.as_deref(),
            Some("ses_e912b168dffeXW3oWEfvXIw4U0")
        );
    }

    /// The bytes OpenCode's TUI actually writes, captured from a live run.
    #[test]
    fn a_real_opencode_report_is_read() {
        let mut terminal = TerminalCore::new(80, 24, 20);
        terminal.feed(b"\x1b]7501;state=idle:app=opencode\x07");
        terminal.feed(b"\x1b]7501;state=working:app=opencode:title=TmV3IHNlc3Npb24gLSAyMDI2LTEwLTI0VDE2OjUzOjI5LjU4N1o=\x07");
        terminal.feed(b"\x1b]7501;state=working:id=ses_e912b168dffeXW3oWEfvXIw4U0:app=opencode:title=TmV3IHNlc3Npb24gLSAyMDI2LTEwLTI0VDE2OjUzOjI5LjU4N1o=\x07");
        terminal.feed(b"\x1b]7501;state=clear:id=ses_e912b168dffeXW3oWEfvXIw4U0\x07");
        let reports = terminal
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                TerminalEvent::OpenCodeReport(report) => Some(report),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(reports.len(), 4, "every report is read");
        assert_eq!(reports[0].state, opencode_status::OpenCodeState::Idle);
        assert_eq!(reports[0].session, None);
        assert_eq!(reports[2].state, opencode_status::OpenCodeState::Working);
        assert_eq!(
            reports[2].session.as_deref(),
            Some("ses_e912b168dffeXW3oWEfvXIw4U0")
        );
        assert_eq!(reports[3].state, opencode_status::OpenCodeState::Clear);
    }

    #[test]
    fn a_program_status_query_and_a_progress_indicator_are_read() {
        let mut terminal = TerminalCore::new(80, 24, 20);
        terminal.feed(b"\x1b]7501;?\x07");
        terminal.feed(b"\x1b]9;4;3\x07");
        terminal.feed(b"\x1b]9;4;0\x07");
        let events = terminal.take_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, TerminalEvent::PiStatusQuery)),
            "the handshake is read"
        );
        let progress = events
            .iter()
            .filter_map(|event| match event {
                TerminalEvent::PiProgress(working) => Some(*working),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(progress, vec![true, false], "both progress values are read");
    }

    #[test]
    fn a_malformed_program_status_is_not_a_state() {
        let mut terminal = TerminalCore::new(80, 24, 20);
        terminal.feed(b"\x1b]7501;state=working:colour=red\x07");
        terminal.feed(b"\x1b]7501;state=typing\x07");
        assert!(
            terminal
                .take_events()
                .iter()
                .all(|event| !matches!(event, TerminalEvent::PiTaskState(_))),
            "half a status is not a status"
        );
    }

    #[test]
    fn a_command_boundary_is_reported() {
        let mut terminal = TerminalCore::new(80, 24, 20);
        terminal.feed(b"\x1b]133;C\x07");
        terminal.feed(b"\x1b]133;D;0\x07");
        let events = terminal.take_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, TerminalEvent::CommandStarted)),
            "a command that started is reported"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, TerminalEvent::CommandFinished)),
            "a command that finished is reported"
        );
    }

    #[test]
    fn a_drag_selection_returns_the_selected_grid_text_forward_and_backward() {
        let mut core = TerminalCore::new(8, 2, 10);
        core.feed(b"hello\r\nworld");
        core.select((0, 1), (0, 3));
        assert_eq!(core.selection_text().as_deref(), Some("ell"));
        core.select((0, 3), (0, 1));
        assert_eq!(core.selection_text().as_deref(), Some("ell"));
        core.select((0, 4), (1, 1));
        assert_eq!(core.selection_text().as_deref(), Some("o\nwo"));
        core.select((1, 1), (0, 4));
        assert_eq!(core.selection_text().as_deref(), Some("o\nwo"));
    }

    #[test]
    fn snapshots_keep_combining_codepoints_and_parser_widths() {
        let mut core = TerminalCore::new(8, 2, 10);
        core.feed("e\u{301}界".as_bytes());
        let row = &core.visible_rows()[0].cells;
        assert_eq!(row[0].c, 'e');
        assert_eq!(row[0].zerowidth, ['\u{301}']);
        assert_eq!(row[0].width, 1);
        assert_eq!(row[1].c, '界');
        assert_eq!(row[1].width, 2);
        assert_eq!(row[2].width, 0);

        let mut wrapped = TerminalCore::new(4, 2, 10);
        wrapped.feed("abc界".as_bytes());
        let rows = wrapped.visible_rows();
        assert_eq!(rows[0].cells[3].width, 0, "leading spacer is not a glyph");
        assert_eq!(rows[1].cells[0].width, 2);
        assert_eq!(rows[1].cells[1].width, 0);
    }

    #[test]
    fn word_and_line_selection_expand_and_stay_granular_during_drag() {
        let mut core = TerminalCore::new(16, 2, 10);
        core.feed(b"alpha beta");
        core.start_selection((0, 2), SelectionKind::Word);
        assert_eq!(core.selection_text().as_deref(), Some("alpha"));
        core.update_selection((0, 7));
        assert_eq!(core.selection_text().as_deref(), Some("alpha beta"));
        core.update_selection((0, 0));
        assert_eq!(core.selection_text().as_deref(), Some("alpha"));

        let mut wrapped = TerminalCore::new(5, 3, 10);
        wrapped.feed(b"abcdefgh\r\nnext");
        wrapped.start_selection((1, 1), SelectionKind::Line);
        assert_eq!(wrapped.selection_text().as_deref(), Some("abcdefgh\n"));
    }

    #[test]
    fn drag_anchor_stays_on_content_when_viewport_or_output_scrolls() {
        let mut core = TerminalCore::new(8, 2, 10);
        core.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(core.history_size(), 1);
        core.start_selection((0, 0), SelectionKind::Simple);
        core.scroll_by(1);
        core.update_selection((0, 0));
        assert_eq!(core.selection_text().as_deref(), Some("one\nt"));

        core.scroll_to_bottom();
        core.start_selection((0, 0), SelectionKind::Simple);
        core.update_selection((0, 2));
        assert_eq!(core.selection_text().as_deref(), Some("two"));
        core.feed(b"\r\nfour");
        core.scroll_by(1);
        core.update_selection((0, 2));
        assert_eq!(core.selection_text().as_deref(), Some("two"));
    }

    #[test]
    fn dynamic_colors_are_used_for_cells_cursor_queries_and_reset() {
        let mut core = core();
        core.feed(b"\x1b]4;1;#112233\x07\x1b]10;#445566\x07\x1b]11;#778899\x07\x1b]12;#abcdef\x07");
        core.feed(b"\x1b[31mx\x1b[0my");
        let row = &core.visible_rows()[0].cells;
        assert_eq!(row[0].fg, ColorSnapshot::Rgb(0x11, 0x22, 0x33));
        assert_eq!(row[1].fg, ColorSnapshot::Rgb(0x44, 0x55, 0x66));
        assert_eq!(row[1].bg, ColorSnapshot::Rgb(0x77, 0x88, 0x99));
        assert_eq!(core.background(), Some((0x77, 0x88, 0x99)));
        assert_eq!(core.cursor().color, Some((0xab, 0xcd, 0xef)));
        core.take_events();
        core.feed(b"\x1b]4;1;?\x07\x1b]11;?\x07");
        let replies: Vec<_> = core
            .take_events()
            .into_iter()
            .filter_map(|event| {
                if let TerminalEvent::PtyWrite(reply) = event {
                    Some(reply)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            replies,
            [
                "\x1b]4;1;rgb:1111/2222/3333\x07",
                "\x1b]11;rgb:7777/8888/9999\x07",
            ]
        );
        core.feed(b"\x1b]104;1\x07\x1b]110\x07\x1b]111\x07\x1b]112\x07");
        assert_eq!(
            core.visible_rows()[0].cells[0].fg,
            ColorSnapshot::Indexed(1)
        );
        assert_eq!(core.visible_rows()[0].cells[1].fg, ColorSnapshot::Default);
        assert_eq!(core.background(), None);
        assert_eq!(core.cursor().color, None);
        core.feed(b"\x1b]11;?\x07");
        assert!(core.take_events().iter().any(|event| matches!(event,
            TerminalEvent::ColorRequested { index, .. } if *index == NamedColor::Background as usize)));
    }

    #[test]
    fn cursor_blink_and_shape_follow_program_requests() {
        let mut core = core();
        assert!(core.cursor().blinking);
        core.feed(b"\x1b[2 q");
        assert!(!core.cursor().blinking);
        assert_eq!(core.cursor().shape, CursorShapeSnapshot::Block);
        core.feed(b"\x1b[5 q");
        assert!(core.cursor().blinking);
        assert_eq!(core.cursor().shape, CursorShapeSnapshot::Beam);
        core.feed(b"\x1b[0 q");
        assert!(core.cursor().blinking);
        assert_eq!(core.cursor().shape, CursorShapeSnapshot::Block);
        core.feed(b"\x1b[?25l");
        assert!(!core.cursor().visible);
    }

    #[test]
    fn snapshots_expose_negotiated_mouse_and_focus_modes() {
        let mut core = core();
        core.feed(b"\x1b[?1000h\x1b[?1006h\x1b[?1004h");
        let modes = core.modes();
        assert!(modes.mouse_click && modes.mouse_sgr && modes.focus_reporting);
        assert!(!modes.mouse_drag && !modes.mouse_motion);
        core.feed(b"\x1b[?1002h");
        assert!(core.modes().mouse_drag);
        assert!(!core.modes().mouse_click);
        core.feed(b"\x1b[?1003h\x1b[?1005h\x1b[?1007l");
        assert!(core.modes().mouse_motion && core.modes().mouse_utf8);
        assert!(!core.modes().mouse_sgr && !core.modes().alternate_scroll);
    }

    #[test]
    fn prompt_click_counts_wide_characters_once_and_handles_wraps() {
        let mut core = TerminalCore::new(8, 3, 10);
        core.feed("> \x1b]133;B\x07ab界cdef".as_bytes());
        assert_eq!(core.prompt_caret_move(0, 4), Some((5, false)));
        assert_eq!(
            core.prompt_caret_move(0, 5),
            Some((5, false)),
            "spacer maps to glyph"
        );
        core.feed(b"\x1b[1;3H");
        assert_eq!(core.prompt_caret_move(1, 1), Some((6, true)));
        assert_eq!(
            core.prompt_caret_move(1, 2),
            Some((7, true)),
            "line end is editable"
        );
        assert_eq!(
            core.prompt_caret_move(1, 3),
            None,
            "padding beyond line end"
        );
        assert_eq!(core.prompt_caret_move(0, 1), None, "before prompt");
    }

    #[test]
    fn prompt_clicks_refuse_tabs_combining_cells_and_non_shell_screens() {
        let mut tab = core();
        tab.feed(b"> \x1b]133;B\x07a\tb");
        assert_eq!(tab.prompt_caret_move(0, 3), None);
        let mut combining = core();
        combining.feed("> \x1b]133;B\x07e\u{301}x".as_bytes());
        assert_eq!(combining.prompt_caret_move(0, 2), None);
        let mut terminal = core();
        terminal.feed(b"> \x1b]133;B\x07abc\x1b[?1000h");
        assert_eq!(terminal.prompt_caret_move(0, 2), None);
        terminal.feed(b"\x1b[?1000l\x1b[?1049h\x1b]133;B\x07abc");
        assert!(terminal.prompt_anchor().is_none());
        assert_eq!(terminal.prompt_caret_move(0, 2), None);
    }

    #[test]
    fn full_history_scroll_and_screen_replacement_retire_prompt_identity() {
        let mut core = TerminalCore::new(8, 2, 1);
        core.feed(b"old\r\nold\r\n> \x1b]133;B\x07abc");
        assert_eq!(core.history_size(), 1);
        assert!(core.prompt_anchor().is_some());
        core.feed(b"\r\n");
        assert_eq!(
            core.history_size(),
            1,
            "eviction hides growth but not scrolling"
        );
        assert!(core.prompt_anchor().is_none());
        core.feed(b"> \x1b]133;B\x07abc\x1b[2J");
        assert!(
            core.prompt_anchor().is_none(),
            "replaced content is not the same input"
        );
    }

    #[test]
    fn prompt_anchor_survives_viewport_scroll_but_not_output_scroll_or_resize() {
        let mut core = TerminalCore::new(8, 3, 2);
        core.feed(b"old\r\nold\r\n> \x1b]133;B\x07abc");
        core.scroll_by(1);
        assert!(core.prompt_anchor().is_some());
        core.scroll_to_bottom();
        assert_eq!(core.prompt_caret_move(2, 2), Some((3, false)));
        core.feed(b"\r\n");
        assert!(
            core.prompt_anchor().is_none(),
            "output scrolled the marker row"
        );
        core.feed(b"> \x1b]133;B\x07abc");
        assert!(core.prompt_anchor().is_some());
        core.resize(10, 3);
        assert!(
            core.prompt_anchor().is_none(),
            "reflow needs a fresh marker"
        );
    }

    /// `133;B` after `hello` anchors the prompt at column 5, the exact
    /// position the marker was written at.
    #[test]
    fn a_prompt_marker_anchors_the_cursor_where_it_was_written() {
        let mut core = core();
        core.feed(b"hello\x1b]133;B\x07");
        let anchor = core.prompt_anchor().expect("the marker recorded an anchor");
        assert_eq!(
            (anchor.line, anchor.column),
            (0, 5),
            "hello is five columns"
        );
    }

    /// The same marker split across two reads, and terminated by ST rather than
    /// BEL: both are how a real shell's output arrives.
    #[test]
    fn a_prompt_marker_split_across_reads_or_ended_by_st_still_anchors() {
        let mut bel = core();
        bel.feed(b"hi\x1b]133;");
        assert!(
            bel.prompt_anchor().is_none(),
            "half a marker is not a marker"
        );
        bel.feed(b"B\x07");
        assert_eq!(
            bel.prompt_anchor().map(|a| (a.line, a.column)),
            Some((0, 2)),
            "the marker split across reads still anchors"
        );

        let mut st = core();
        st.feed(b"hey\x1b]133;B\x1b\\");
        assert_eq!(
            st.prompt_anchor().map(|a| (a.line, a.column)),
            Some((0, 3)),
            "an ST-terminated marker anchors too"
        );
    }

    /// `133;C` means a command is running: the line is no longer editable.
    #[test]
    fn a_command_marker_invalidates_the_prompt_anchor() {
        let mut core = core();
        core.feed(b"typed\x1b]133;B\x07");
        assert!(core.prompt_anchor().is_some(), "the prompt is editable");
        core.feed(b"\x1b]133;C\x07");
        assert!(
            core.prompt_anchor().is_none(),
            "a running command is not a prompt"
        );
    }

    #[test]
    fn parser_byte_observer_tracks_osc_after_escape_termination() {
        let mut core = core();
        core.feed(b"\x1b]x\x1b]12345\x07");
        assert_eq!(core.parser_osc_max_test, 5);
    }

    #[test]
    fn osc_scanning_recovers_after_cancel_and_escape_at_every_input_split() {
        for ending in [0x18, 0x1a, 0x1b] {
            let mut interrupted = b"\x1b]133;D;1".to_vec();
            interrupted.push(ending);
            interrupted.extend_from_slice(b"\x1b]133;D;7\x07");
            for split in 1..interrupted.len() {
                let mut core = core();
                core.feed(&interrupted[..split]);
                core.feed(&interrupted[split..]);
                assert_eq!(
                    core.command_exit(),
                    Some(7),
                    "terminator {ending:#x}, split {split}"
                );
            }
        }
    }

    /// Somebody else's OSC is still theirs: the parser handles it, and nothing
    /// here mistakes it for a prompt.
    #[test]
    fn an_osc_that_is_not_a_marker_leaves_the_prompt_alone() {
        let mut core = core();
        core.feed(b"\x1b]1339;B\x07");
        assert!(core.prompt_anchor().is_none(), "1339 is not 133");
        core.feed(b"\x1b]52;c;aGVsbG8=\x07");
        assert!(
            core.prompt_anchor().is_none(),
            "a clipboard write is not a prompt"
        );
    }

    /// `133;D;<code>` is the only thing that records a command's status, and it
    /// records it the same way whether it arrives whole, ended by BEL, ended by
    /// ST, or split anywhere across reads.
    #[test]
    fn a_command_exit_marker_records_the_status_however_it_arrives() {
        for ending in [b"\x07".as_slice(), b"\x1b\\".as_slice()] {
            let marker = [b"\x1b]133;D;3".as_slice(), ending].concat();
            for split in 1..marker.len() {
                let mut core = core();
                core.feed(&marker[..split]);
                let ended_on_escape = ending == b"\x1b\\" && split == marker.len() - 1;
                assert_eq!(
                    core.command_exit(),
                    ended_on_escape.then_some(3),
                    "vte ends OSC on ESC itself, split {split}"
                );
                core.feed(&marker[split..]);
                assert_eq!(
                    core.command_exit(),
                    Some(3),
                    "split at {split}, ended by {ending:?}"
                );
            }
        }
    }

    /// A new command retires the last one's status, and success is a status like
    /// any other: the question a sidebar asks is "did it fail", so zero has to
    /// answer that, not be mistaken for a status that says nothing.
    #[test]
    fn a_new_command_and_a_success_each_clear_the_previous_failure() {
        let mut core = core();
        core.feed(b"\x1b]133;C\x07");
        core.feed(b"\x1b]133;D;1\x07");
        assert_eq!(core.command_exit(), Some(1), "the command failed");
        core.feed(b"\x1b]133;C\x07");
        assert_eq!(core.command_exit(), None, "the next command starts clean");
        core.feed(b"\x1b]133;D;0\x07");
        assert_eq!(core.command_exit(), Some(0), "success is still a status");
    }

    /// Somebody else's OSC is not a statement about the command, so it neither
    /// claims a status nor takes one away -- including `1339`, which shares the
    /// prefix, and `133;A`, which is this sequence's own prompt marker.
    #[test]
    fn an_osc_that_is_not_a_133_d_marker_leaves_the_exit_status_alone() {
        for other in [
            &b"\x1b]1339;D;1\x07"[..],
            b"\x1b]52;c;aGVsbG8=\x07",
            b"\x1b]133;A;1\x07",
            b"\x1b]0;a title\x07",
        ] {
            let mut core = core();
            core.feed(b"\x1b]133;D;1\x07");
            core.feed(other);
            assert_eq!(core.command_exit(), Some(1), "{other:?} said nothing");
        }
    }

    /// A `133;D` with no readable integer is a command that finished without
    /// saying how, and it takes the claim away rather than leaving a failure on
    /// screen from two commands ago. That is the safe direction: no claim is
    /// recoverable, a wrong claim is not.
    #[test]
    fn a_133_d_marker_with_no_readable_integer_leaves_no_status_claimed() {
        for malformed in [
            &b"\x1b]133;D\x07"[..],
            b"\x1b]133;D;\x07",
            b"\x1b]133;D;abc\x07",
            b"\x1b]133;D;1;2\x07",
            b"\x1b]133;D;\x1b\\",
        ] {
            let mut core = core();
            core.feed(b"\x1b]133;D;1\x07");
            core.feed(malformed);
            assert_eq!(core.command_exit(), None, "{malformed:?} claims nothing");
        }
    }

    /// A child that opens an OSC and never closes it must not be able to grow
    /// this terminal, and a sequence too large to hold must be refused whole
    /// rather than read off the front.
    #[test]
    fn an_unterminated_or_oversized_osc_is_bounded_and_reads_as_nothing() {
        let mut unflooded = core();
        let mut never_terminated = b"\x1b]".to_vec();
        never_terminated.extend(std::iter::repeat_n(b'x', MAX_OSC_PAYLOAD_BYTES * 8));
        unflooded.feed(&never_terminated);
        assert!(
            unflooded.osc.len() <= MAX_OSC_PAYLOAD_BYTES,
            "the wrapper retains no more than its OSC cap"
        );
        assert!(
            unflooded.parser_osc_max_test <= MAX_OSC_PAYLOAD_BYTES,
            "Processor::advance received at most the accepted OSC prefix"
        );

        let mut big = core();
        let mut oversized = b"\x1b]133;D;1".to_vec();
        oversized.extend(std::iter::repeat_n(b'x', MAX_OSC_PAYLOAD_BYTES));
        oversized.push(0x07);
        big.feed(&oversized);
        assert_eq!(
            big.command_exit(),
            None,
            "a marker truncated by an oversized payload is not a marker"
        );
        assert!(big.osc.len() <= MAX_OSC_PAYLOAD_BYTES);
        assert!(
            big.parser_osc_max_test <= MAX_OSC_PAYLOAD_BYTES,
            "the underlying parser did not retain the oversized tail"
        );

        let mut title = core();
        let mut oversized_title = b"\x1b]0;untrusted".to_vec();
        oversized_title.extend(std::iter::repeat_n(b'x', MAX_OSC_PAYLOAD_BYTES));
        oversized_title.extend_from_slice(b"\x07after");
        title.feed(&oversized_title);
        assert_eq!(
            title.title(),
            None,
            "the oversized title has no host effect"
        );
        assert_eq!(title.visible_rows()[0].cells[0].c, 'a');
        assert!(title.parser_osc_max_test <= MAX_OSC_PAYLOAD_BYTES);

        let mut clipboard = core();
        let mut oversized_clipboard = b"\x1b]52;c;".to_vec();
        oversized_clipboard.extend(std::iter::repeat_n(b'a', MAX_OSC_PAYLOAD_BYTES));
        oversized_clipboard.push(0x07);
        clipboard.feed(&oversized_clipboard);
        assert!(!clipboard
            .take_events()
            .iter()
            .any(|event| matches!(event, TerminalEvent::ClipboardStore(_))));
    }

    #[test]
    fn ignored_controls_after_string_escape_recover_real_osc_host_events() {
        let mut cases = Vec::new();
        for introducer in b"]_P^X".iter().copied() {
            let mut bytes = vec![0x1b, introducer];
            if introducer == b'_' {
                // Valid base64, but far beyond the graphics packet cap.
                bytes.extend(std::iter::repeat_n(b"QUFB".as_slice(), 16_385).flatten());
            } else {
                bytes.extend_from_slice(b"payload");
            }
            let escape = bytes.len();
            bytes.extend_from_slice(b"\x1b\0]52;c;QUFB\x07\x1b]0;recovered\x07\x1b]0;");
            bytes.extend(std::iter::repeat_n(b'x', MAX_OSC_PAYLOAD_BYTES + 1));
            bytes.extend_from_slice(b"\x07\x1b]0;last\x07");
            cases.push((introducer, bytes, escape));
        }

        for (introducer, bytes, escape) in cases {
            let control = escape + 1;
            let osc = control + 1;
            let cap = if introducer == b'_' { MAX_PACKET } else { 0 };
            let splits = [
                0,
                bytes.len(),
                escape.saturating_sub(1),
                escape,
                control,
                osc,
                (2 + cap).min(bytes.len()),
                (2 + cap + 1).min(bytes.len()),
            ];
            for split in splits {
                let mut terminal = core();
                terminal.feed(&bytes[..split]);
                terminal.feed(&bytes[split..]);
                let events = terminal.take_events();
                assert!(
                    events.iter().any(
                        |event| matches!(event, TerminalEvent::ClipboardStore(value) if value == "AAA")
                    ),
                    "introducer {introducer:#x}, split {split}: {events:?}"
                );
                let titles: Vec<_> = events
                    .iter()
                    .filter_map(|event| match event {
                        TerminalEvent::TitleChanged(title) => Some(title.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    titles,
                    ["recovered", "last"],
                    "the oversized title is refused, and parsing recovers: introducer {introducer:#x}, split {split}"
                );
                assert!(
                    terminal.parser_osc_max_test <= MAX_OSC_PAYLOAD_BYTES,
                    "OSC buffering stays bounded: introducer {introducer:#x}, split {split}"
                );
            }

            let mut oversized = vec![0x1b, introducer];
            if introducer == b'_' {
                oversized.extend(std::iter::repeat_n(b"QUFB".as_slice(), 16_385).flatten());
            } else {
                oversized.extend_from_slice(b"payload");
            }
            let escape = oversized.len();
            oversized.extend_from_slice(b"\x1b\0]0;");
            oversized.extend(std::iter::repeat_n(b'x', MAX_OSC_PAYLOAD_BYTES + 1));
            oversized.push(0x07);
            for split in [
                escape,
                escape + 1,
                escape + 2,
                escape + 3,
                2 + MAX_PACKET,
                escape + 5 + MAX_OSC_PAYLOAD_BYTES - 1,
                escape + 5 + MAX_OSC_PAYLOAD_BYTES,
                escape + 5 + MAX_OSC_PAYLOAD_BYTES + 1,
            ] {
                let split = split.min(oversized.len());
                let mut terminal = core();
                terminal.feed(&oversized[..split]);
                terminal.feed(&oversized[split..]);
                assert!(
                    terminal
                        .take_events()
                        .iter()
                        .all(|event| !matches!(event, TerminalEvent::TitleChanged(_))),
                    "oversized title has no host effect: introducer {introducer:#x}, split {split}"
                );
            }
        }
    }

    #[test]
    fn ignored_escape_controls_cannot_reopen_an_unbounded_osc() {
        let mut intermediate = core();
        intermediate.feed(b"\x1b(]52;c;QUFB\x07");
        assert!(!intermediate
            .take_events()
            .iter()
            .any(|event| matches!(event, TerminalEvent::ClipboardStore(_))));
        assert_eq!(intermediate.parser_osc_max_test, 0);

        for ignored in (0x00..=0x17)
            .chain([0x19])
            .chain(0x1c..=0x1f)
            .chain([0x7f, 0x80])
        {
            let mut bytes = vec![0x1b, ignored];
            bytes.extend_from_slice(b"]52;c;");
            bytes.extend(std::iter::repeat_n(b'A', MAX_OSC_PAYLOAD_BYTES + 1));
            bytes.push(0x07);

            for split in [
                0,
                bytes.len(),
                MAX_OSC_PAYLOAD_BYTES + 8,
                MAX_OSC_PAYLOAD_BYTES + 9,
            ] {
                let split = split.min(bytes.len());
                let mut core = core();
                core.feed(&bytes[..split]);
                core.feed(&bytes[split..]);
                assert!(
                    !core
                        .take_events()
                        .iter()
                        .any(|event| matches!(event, TerminalEvent::ClipboardStore(_))),
                    "ignored escape byte {ignored:#x} at split {split}"
                );
                assert!(core.parser_osc_max_test <= MAX_OSC_PAYLOAD_BYTES);
            }
        }
    }

    /// A click inside the editable prompt moves the caret; a click on the
    /// prompt's own prefix, on the caret, or on output above it does not.
    #[test]
    fn a_click_on_the_prompt_measures_the_move_and_nothing_else_does() {
        let mut core = core();
        // The caret is in the middle of a real edit buffer, not beyond its end.
        core.feed(b"ab\x1b]133;B\x07cdef\x1b[2D");
        assert!(
            core.prompt_anchor().is_some(),
            "the prompt is where the marker said it was"
        );
        assert_eq!(core.move_prompt_caret(0, 6), Some(2), "two to the right");
        assert_eq!(core.move_prompt_caret(0, 2), Some(2), "two to the left");
        assert_eq!(
            core.move_prompt_caret(0, 4),
            None,
            "the caret is already there"
        );
        assert_eq!(core.move_prompt_caret(0, 1), None, "the prompt's own text");
        assert_eq!(
            core.move_prompt_caret(1, 0),
            None,
            "another row is not this line"
        );
    }

    /// A running command has no editable line, so its output cannot be clicked
    /// into.
    #[test]
    fn no_click_moves_anything_without_a_prompt() {
        let mut core = core();
        core.feed(b"ab");
        assert_eq!(core.move_prompt_caret(0, 4), None, "no marker, no anchor");
    }

    #[test]
    fn a_hostile_title_is_sanitised_and_bounded() {
        let mut core = core();
        // 500 emoji is 2000 bytes, so a cap taken in bytes would have to cut the
        // sequence in half to be this small; DEL on either side is the control
        // character the parser does not swallow as OSC syntax.
        let hostile = format!("  \x7f{}\x7f  ", "😀".repeat(500));
        core.feed(format!("\x1b]0;{hostile}\x07").as_bytes());

        let title = core.title().expect("a non-empty title should survive");

        assert!(
            !title.chars().any(char::is_control),
            "no control character should survive: {title:?}"
        );
        assert_eq!(
            title,
            title.trim(),
            "the title should be trimmed: {title:?}"
        );
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        // A cap taken by byte index would leave half an emoji here. Every character
        // being a whole emoji is the same fact stated about the code points rather
        // than about the UTF-8 they happen to encode to.
        assert!(
            title.chars().all(|c| c == '😀'),
            "truncation should land on a code point boundary: {title:?}"
        );
    }

    #[test]
    fn an_oversized_clipboard_payload_is_refused_without_being_kept() {
        // The parser decodes the OSC into a `String` before this layer sees it, so
        // the only thing left to check is that the payload is dropped instead of
        // copied into an event the window would have to hold.
        let mut core = core();
        // `QUFB` is base64 for `AAA`, which is how a payload of any length is built
        // without pulling in an encoder: four ASCII characters in, three plain
        // ASCII bytes out, and the result decodes to valid UTF-8.
        let encoded = "QUFB".repeat(MAX_CLIPBOARD_PAYLOAD_BYTES / 3 + 2);
        let decoded_len = encoded.len() / 4 * 3;
        assert!(
            decoded_len > MAX_CLIPBOARD_PAYLOAD_BYTES,
            "the test payload has to exceed the limit"
        );

        // A payload this size is kept, so the refusal below is about the size and
        // not about OSC 52 having stopped working: without this the whole case
        // would also pass against a core that dropped every clipboard write.
        core.feed(b"\x1b]52;c;QUFB\x07");
        assert!(
            matches!(
                core.take_events().as_slice(),
                [TerminalEvent::ClipboardStore(text)] if text == "AAA",
            ),
            "a payload under the cap is kept, so the refusal below is about the size and not \
             about OSC 52 having stopped working: without this the whole case would also pass \
             against a core that dropped every clipboard write",
        );

        core.feed(format!("\x1b]52;c;{encoded}\x07").as_bytes());

        assert!(!core.take_events().iter().any(|event| matches!(event, TerminalEvent::ClipboardStore(_))),
            "a payload of {decoded_len} bytes is over the {MAX_CLIPBOARD_PAYLOAD_BYTES} byte limit and must be refused");
    }

    #[test]
    fn a_program_asking_to_read_the_clipboard_is_surfaced_not_answered() {
        let mut core = core();
        core.feed(b"\x1b]52;c;?\x07");

        let events = core.take_events();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, TerminalEvent::ClipboardRequested)),
            "the request should reach the host: {events:?}",
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, TerminalEvent::PtyWrite(_))),
            "nothing may be written back: {events:?}"
        );
    }

    #[test]
    fn only_a_pushed_keyboard_mode_counts_as_negotiation() {
        // The other half of `keys::a_program_that_only_queried_keyboard_support_did_not_agree_to_use_it`:
        // this is what decides whether the snapshot those encoders read says the
        // child agreed to the keyboard protocol.
        let mut core = core();

        // `CSI ? u` asks. The parser answers with the mode on top of its stack,
        // which is none -- and answering a question is not doing the thing the
        // question was about.
        core.feed(b"\x1b[?u");
        let events = core.take_events();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, TerminalEvent::PtyWrite(_))),
            "the query has to have been answered, or this test proves nothing about it: {events:?}"
        );
        assert!(
            !core.modes().disambiguate_esc_codes,
            "a query is not a negotiation: {:?}",
            core.modes()
        );

        // `CSI > 1 u` is the child asking for the disambiguating forms, and it
        // is the one thing that turns them on.
        core.feed(b"\x1b[>1u");

        assert!(
            core.modes().disambiguate_esc_codes,
            "after the push: {:?}",
            core.modes()
        );

        // `CSI > 2 u` replaces the mode rather than adding to it
        // (`push_keyboard_mode`, `src/term/mod.rs:1288`), so the snapshot a
        // view reads is the top of the stack and never an accumulation of it.
        core.feed(b"\x1b[>2u");
        let modes = core.modes();
        assert!(modes.report_event_types, "after the second push: {modes:?}");
        assert!(
            !modes.disambiguate_esc_codes,
            "the first push was replaced, not extended: {modes:?}"
        );

        // The other modes a snapshot carries come from the same place, so a
        // snapshot that read the wrong bit would read all of them wrong.
        core.feed(b"\x1b[?1h\x1b[?2004h\x1b[20h");

        let modes = core.modes();
        assert!(modes.app_cursor, "DECCKM: {modes:?}");
        assert!(modes.bracketed_paste, "DECSET 2004: {modes:?}");
        assert!(modes.newline_is_return, "LNM: {modes:?}");
    }
}
