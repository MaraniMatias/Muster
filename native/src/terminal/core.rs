use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
// Not a test-only path, despite living in a module called `test`: the vendored
// crate leaves `pub mod test` ungated, and it is the crate's own `Dimensions`
// implementation, so there is no reason for this crate to grow a second one.
// `native/src/terminal_parser.rs` already reaches for the same type.
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor};

use super::modes::TerminalModes;

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
    /// far enough that the cursor is not on screen. `row` and `col` are clamped
    /// rather than wrapped in that case, so a stale position never reads as a
    /// real one.
    pub visible: bool,
    pub shape: CursorShapeSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShapeSnapshot {
    Block,
    Underline,
    Beam,
    HollowBlock,
    Hidden,
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
    Osc,
    OscEscape,
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
}

impl TerminalCore {
    /// `scrollback_lines` is Muster's scrollback setting, the same 10,000 lines
    /// the parser's own `Config::default()` happens to carry. It is set
    /// explicitly rather than left to that default so the number is a decision of
    /// this crate and not of whichever parser gets vendored next.
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self {
        let config = Config {
            scrolling_history: scrollback_lines,
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
            osc: Vec::new(),
            osc_oversized: false,
            pending: Vec::new(),
            title: None,
            prompt_anchor: None,
            command_exit: None,
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
        let mut offset = 0;
        let mut state = self.string_scan;
        for (index, &byte) in bytes.iter().enumerate() {
            let previous = state;
            state = match (state, byte) {
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
                (StringScan::Escape, 0x1b) => StringScan::Escape,
                (StringScan::Escape, _) => StringScan::Ground,

                (StringScan::Osc, 0x07) => StringScan::Ground,
                (StringScan::Osc, 0x1b) => StringScan::OscEscape,
                (StringScan::OscEscape, b'\\') => StringScan::Ground,
                (StringScan::OscEscape, 0x1b) => StringScan::OscEscape,
                (StringScan::OscEscape, _) => StringScan::Osc,
                (StringScan::Osc, _) => {
                    self.scan_osc(byte);
                    StringScan::Osc
                }
                (StringScan::Ground, _) => StringScan::Ground,
            };
            let terminated = (matches!(previous, StringScan::Osc) && byte == 0x07)
                || (matches!(previous, StringScan::OscEscape) && byte == b'\\');
            if terminated {
                state = StringScan::Ground;
                // The grid is read after the parser has consumed exactly up to
                // the terminator and before anything after it, which is the
                // position the marker was written at.
                self.parser.advance(&mut self.term, &bytes[offset..=index]);
                let marker = std::mem::take(&mut self.osc);
                if !self.osc_oversized {
                    self.apply_shell_marker(&marker);
                }
                self.handle_events_at_position();
                offset = index + 1;
            }
        }
        if offset < bytes.len() {
            self.parser.advance(&mut self.term, &bytes[offset..]);
            self.handle_events_at_position();
        }
        self.string_scan = state;
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
            "D" => self.command_exit = rest.parse().ok(),
            "C" => {
                self.command_exit = None;
                self.prompt_anchor = None;
            }
            "B" => {
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
            Event::ColorRequest(index, format) => Some(TerminalEvent::ColorRequested {
                index,
                respond: Box::new(move |r, g, b| {
                    format(alacritty_terminal::vte::ansi::Rgb { r, g, b })
                }),
            }),
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
        // Only preserve an anchor while its line's display mapping is unchanged.
        // Any scroll/reflow we cannot prove invalidates it rather than guessing.
        (anchor.display_offset == grid.display_offset()
            && (-(grid.total_lines() as i32)..grid.screen_lines() as i32).contains(&anchor.line))
        .then_some(anchor)
    }

    pub fn move_prompt_caret(&mut self, row: usize, column: usize) -> Option<usize> {
        let anchor = self.prompt_anchor()?;
        let grid = self.term.grid();
        let anchor_row = anchor.line + grid.display_offset() as i32;
        let cursor_row = grid.cursor.point.line.0 + grid.display_offset() as i32;
        // Movement is intentionally limited to the same physical line. The PTY
        // does not expose Readline/ZLE's logical line mapping, so wrapped or
        // multiline input is ambiguous and must remain inert.
        if row as i32 != anchor_row || cursor_row != anchor_row || row != cursor_row as usize {
            return None;
        }
        let start = anchor.column;
        let end = grid.cursor.point.column.0;
        // Before the prompt is not editable, and on the caret is nowhere to go.
        // Past the caret is allowed in both directions: the shell's own line is
        // longer than its cursor whenever the caret sits in the middle of it,
        // and refusing that is refusing half the line.
        if column < start || column == end {
            return None;
        }
        Some(column.abs_diff(end))
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
    pub fn visible_rows(&self) -> Vec<RowSnapshot> {
        let grid = self.term.grid();
        // Viewport row 0 sits `display_offset` lines above the top of the screen.
        // This is the same mapping the parser's own damage iterator uses, so the
        // snapshot and any future damage tracking agree on what row 0 is.
        let offset = grid.display_offset() as i32;

        (0..grid.screen_lines())
            .map(|row| RowSnapshot {
                cells: (0..grid.columns())
                    .map(|col| cell_snapshot(&grid[Line(row as i32 - offset)][Column(col)]))
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

    /// Select the inclusive grid range described by viewport cells.
    pub fn select(&mut self, start: (usize, usize), end: (usize, usize)) {
        use alacritty_terminal::index::{Point, Side};
        use alacritty_terminal::selection::{Selection, SelectionType};

        let grid = self.term.grid();
        let clamp = |(row, col): (usize, usize)| {
            Point::new(
                Line(
                    row.min(grid.screen_lines().saturating_sub(1)) as i32
                        - grid.display_offset() as i32,
                ),
                Column(col.min(grid.columns().saturating_sub(1))),
            )
        };
        let start = clamp(start);
        let end = clamp(end);
        let (anchor, anchor_side, head, head_side) = if start <= end {
            (start, Side::Left, end, Side::Right)
        } else {
            (start, Side::Right, end, Side::Left)
        };
        let mut selection = Selection::new(SelectionType::Simple, anchor, anchor_side);
        selection.update(head, head_side);
        self.term.selection = Some(selection);
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
fn cell_snapshot(cell: &Cell) -> CellSnapshot {
    CellSnapshot {
        c: cell.c,
        fg: color_snapshot(cell.fg),
        bg: color_snapshot(cell.bg),
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
fn color_snapshot(color: Color) -> ColorSnapshot {
    match color {
        Color::Spec(rgb) => ColorSnapshot::Rgb(rgb.r, rgb.g, rgb.b),
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

            ColorSnapshot::Indexed(index as u8)
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
                assert_eq!(core.command_exit(), None, "nothing yet, split {split}");
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
            "an unterminated OSC is bounded, kept {}",
            unflooded.osc.len()
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
    }

    /// A click inside the editable prompt moves the caret; a click on the
    /// prompt's own prefix, on the caret, or on output above it does not.
    #[test]
    fn a_click_on_the_prompt_measures_the_move_and_nothing_else_does() {
        let mut core = core();
        core.feed(b"ab\x1b]133;B\x07cd");
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

        assert!(
            !core.take_events().iter().any(|event| matches!(event, TerminalEvent::ClipboardStore(_))),
            "a payload of {decoded_len} bytes is over the {MAX_CLIPBOARD_PAYLOAD_BYTES} byte limit and must be refused"
        );
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
