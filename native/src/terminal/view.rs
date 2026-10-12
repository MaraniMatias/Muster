//! Drawing the grid, and the measurement that decides how big the grid is.
//!
//! Everything above this file -- the parser, the session, the encoders -- knows
//! about columns and rows and nothing at all about pixels. This is where a
//! column becomes a rectangle: one cell measured from the font that will draw
//! it, a grid that many cells to the window, and a paint loop that puts every
//! cell's character, colours and flags where the parser says they belong.
//!
//! Two rules the paint loop does not get to break:
//!
//! * The pen is the *grid*, never the shaped line. A terminal that lets the
//!   text system's own advances decide where a glyph lands tears the moment a
//!   fallback face has different metrics, and a grid is the only thing the
//!   child agreed to. Each cell is therefore shaped on its own and painted at
//!   `column * cell_width`, and the shaping result is free to be wider,
//!   narrower or two columns across without moving anything.
//! * A wide glyph is painted once. The parser has already given it two columns
//!   and left a placeholder in the second; drawing that placeholder is how CJK
//!   output ends up with a ghost character and every column after it half a
//!   cell out.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use gpui_kit::{
    canvas, div, point, px, quad, App, Bounds, Context, FocusHandle, FontId, FontStyle, FontWeight,
    Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels,
    Render, Rgba, SharedString, StrikethroughStyle, Styled, Task, TextAlign, TextStyle,
    UnderlineStyle, Window, WindowTextSystem,
};

use super::input::{self, FocusState};
use super::{
    CellMetrics, CellSnapshot, ColorSnapshot, CursorShapeSnapshot, GridRange, TerminalSession,
    TerminalSnapshot,
};

/// The terminal's font size and line spacing, matching the legacy terminal defaults.
const FONT_SIZE: Pixels = px(16.0);
const LINE_HEIGHT: f32 = 1.2;
const CURSOR_COLOR: (u8, u8, u8) = (0xc2, 0xc9, 0xf1);
static BUNDLED_FONT: OnceLock<Result<(), String>> = OnceLock::new();
static BOLD_FONT: OnceLock<Result<(), String>> = OnceLock::new();

fn load_bundled_font(
    window: &mut Window,
    loaded: &OnceLock<Result<(), String>>,
    bytes: &'static [u8],
) -> Result<(), String> {
    loaded
        .get_or_init(|| {
            window
                .text_system()
                .add_fonts(vec![std::borrow::Cow::Borrowed(bytes)])
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .copied()
        .map_err(Clone::clone)
}

/// How often the coalesced wake is sampled.
///
/// One frame at 60Hz. The wake channel holds a single token, so sampling twice
/// as fast could not turn one burst of output into a second repaint; it could
/// only add a wake-up per frame to a window that has nothing to show.
const WAKE_POLL: Duration = Duration::from_millis(16);

/// The thickness of bar and underline cursors, as a fraction of the cell extent.
const CURSOR_BAR: f32 = 0.08;

/// Never thinner than this, because a sub-pixel bar is a bar nobody can see.
const CURSOR_MIN_THICKNESS: Pixels = px(1.0);

/// Families to take a terminal grid from, best first.
///
/// Bundled Fira Code is converted to TrueType because GPUI's macOS text system
/// registers embedded fonts through Core Graphics, which does not accept WOFF2.
const MONOSPACE_FAMILIES: &[&str] = &[
    "Fira Code",
    "SF Mono",
    "Menlo",
    "Monaco",
    "JetBrains Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Cascadia Mono",
    "Cascadia Code",
    "Ubuntu Mono",
    "Consolas",
];

/// Blend factor used for dim terminal text.
const DIM_BLEND: f32 = 0.45;

// ---------------------------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------------------------

/// ANSI entries 0 to 15 in the legacy terminal's dark theme.
const ANSI_COLORS: [(u8, u8, u8); 16] = [
    (0x28, 0x2c, 0x34), // black
    (0xe0, 0x6c, 0x75), // red
    (0x98, 0xc3, 0x79), // green
    (0xe5, 0xc0, 0x7b), // yellow
    (0x61, 0xaf, 0xef), // blue
    (0xc6, 0x78, 0xdd), // magenta
    (0x56, 0xb6, 0xc2), // cyan
    (0xab, 0xb2, 0xbf), // white
    (0x63, 0x6d, 0x83), // bright black
    (0xea, 0x85, 0x8b), // bright red
    (0xaa, 0xd5, 0x81), // bright green
    (0xff, 0xd8, 0x85), // bright yellow
    (0x85, 0xc1, 0xff), // bright blue
    (0xd3, 0x98, 0xeb), // bright magenta
    (0x6e, 0xd5, 0xde), // bright cyan
    (0xfa, 0xfa, 0xfa), // bright white
];

/// One xterm-256 entry by index. Indices 0 to 15 use the legacy theme; the
/// remaining entries follow the xterm 6x6x6 cube and 24-step grey ramp.
pub fn xterm_256(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => ANSI_COLORS[index as usize],
        16..=231 => {
            let offset = index - 16;
            let level = |step: u8| if step == 0 { 0 } else { 55 + 40 * step };
            (
                level(offset / 36),
                level((offset % 36) / 6),
                level(offset % 6),
            )
        }
        _ => {
            let grey = 8 + 10 * (index - 232);
            (grey, grey, grey)
        }
    }
}

/// Every colour this terminal draws with.
///
/// The defaults and ANSI colors for the terminal palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// What a cell with no colour of its own is drawn in, matching the dark legacy theme.
    pub foreground: (u8, u8, u8),
    /// The same, behind the character.
    pub background: (u8, u8, u8),
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            foreground: (0xdc, 0xe0, 0xe5),
            background: (0x22, 0x24, 0x36),
        }
    }
}

impl Palette {
    /// The colour a cell's text is painted in.
    ///
    /// Resolve a cell color, except default foreground which depends on the current theme.
    pub fn foreground(&self, color: ColorSnapshot) -> Hsla {
        match color {
            ColorSnapshot::Default => opaque(self.foreground),
            ColorSnapshot::Indexed(index) => opaque(xterm_256(index)),
            ColorSnapshot::Rgb(r, g, b) => opaque((r, g, b)),
        }
    }

    /// The colour behind a cell's text.
    pub fn background(&self, color: ColorSnapshot) -> Hsla {
        match color {
            ColorSnapshot::Default => opaque(self.background),
            ColorSnapshot::Indexed(index) => opaque(xterm_256(index)),
            ColorSnapshot::Rgb(r, g, b) => opaque((r, g, b)),
        }
    }
}

fn opaque((r, g, b): (u8, u8, u8)) -> Hsla {
    Hsla::from(Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: 1.0,
    })
}

// ---------------------------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------------------------

/// One grid cell, measured from the font that will draw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellSize {
    pub width: Pixels,
    pub height: Pixels,
}

impl CellSize {
    /// Measure a cell by shaping one character in the terminal's face.
    ///
    /// The width is the shaped advance of `M` rather than any ratio of the font
    /// size: it is the number the text system itself will use, so a face whose
    /// advance is not a round fraction still produces a grid that lines up.
    /// The height is the face's own ascent plus descent for the same reason.
    ///
    /// `None` for metrics that came back empty. Every column count below divides
    /// by this, and a zero here is a division by zero rather than a small grid.
    pub fn measure(
        text_system: &WindowTextSystem,
        style: &TextStyle,
        font_size: Pixels,
    ) -> Option<Self> {
        let probe = text_system.shape_line("M".into(), font_size, &[style.to_run(1)], None);
        let measured = Self {
            width: probe.width(),
            // Whole pixels, preserving the legacy 1.2 line height. The font's
            // own ascent and descent may be fractional, so round only after spacing.
            height: ((probe.ascent + probe.descent) * LINE_HEIGHT).ceil(),
        };
        (measured.width > px(0.0) && measured.height > px(0.0)).then_some(measured)
    }

    /// How many whole cells of `extent` fit, rounded down.
    ///
    /// Down, not nearest: a grid with a half column in it has a column the
    /// child will never be told about and the window will paint over anyway.
    pub fn columns_in(&self, extent: Pixels) -> u16 {
        cells_in(extent, self.width)
    }

    /// [`Self::columns_in`], down the other axis.
    pub fn rows_in(&self, extent: Pixels) -> u16 {
        cells_in(extent, self.height)
    }
}

/// A measured cell in the whole pixels a child's size query is answered in.
///
/// Rounded rather than truncated: a child measuring its text area in pixels
/// wants the cell that will actually be drawn, and a column of 8.4 pixels is
/// eight pixels to everything on the far side of the PTY.
fn cell_px(value: Pixels) -> u16 {
    f32::from(value.round())
        .clamp(0.0, f32::from(u16::MAX))
        .trunc() as u16
}

fn cells_in(extent: Pixels, cell: Pixels) -> u16 {
    if cell <= px(0.0) || extent <= px(0.0) {
        return 0;
    }
    (extent / cell).floor().min(f32::from(u16::MAX)) as u16
}

fn compatible_run_end(cells: &[CellSnapshot], start: usize, wide: bool) -> usize {
    let Some(first) = cells.get(start) else {
        return start;
    };
    if wide || (first.c == ' ' && !first.underline && !first.strikethrough) {
        return start + 1;
    }
    let mut end = start + 1;
    while let Some(next) = cells.get(end) {
        if next.fg != first.fg
            || next.bg != first.bg
            || next.bold != first.bold
            || next.dim != first.dim
            || next.italic != first.italic
            || next.underline != first.underline
            || next.strikethrough != first.strikethrough
            || next.inverse != first.inverse
            || (next.c == ' ' && !next.underline && !next.strikethrough)
        {
            break;
        }
        end += 1;
    }
    end
}

fn grid_point(
    position: gpui_kit::Point<Pixels>,
    origin: gpui_kit::Point<Pixels>,
    cell: CellSize,
    grid: (u16, u16),
) -> (usize, usize) {
    let column = ((position.x - origin.x) / cell.width).floor().max(0.0) as usize;
    let row = ((position.y - origin.y) / cell.height).floor().max(0.0) as usize;
    (
        row.min(grid.1.saturating_sub(1) as usize),
        column.min(grid.0.saturating_sub(1) as usize),
    )
}

// ---------------------------------------------------------------------------------------------
// Row layout
// ---------------------------------------------------------------------------------------------

/// One grid column's worth of painting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The first column this occupies.
    pub column: usize,
    /// How many columns it occupies: two for a wide glyph, one otherwise.
    pub columns: usize,
    /// The character to draw.
    pub glyph: char,
}

/// Lay one row out against the grid.
///
/// A wide glyph's second column never appears here at all. That is the whole
/// point: the wide glyph's own placement already claims both columns, so the
/// parser's placeholder for the second one is neither painted nor allowed to
/// shift the columns after it, and `is_wide` is asked about exactly one
/// character per cell instead of about every placeholder too.
///
/// A pure function of the row, which is what makes the CJK case testable
/// without a window: the arithmetic is the same whether it is being asserted
/// here or run against a real font 60 times a second.
pub fn place_row(cells: &[CellSnapshot], is_wide: impl Fn(char) -> bool) -> Vec<Placement> {
    let mut placements = Vec::with_capacity(cells.len());
    let mut column = 0;
    while column < cells.len() {
        let glyph = cells[column].c;
        let columns = usize::from(is_wide(glyph)) + 1;
        placements.push(Placement {
            column,
            columns,
            glyph,
        });
        column += columns;
    }
    placements
}

// ---------------------------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------------------------

/// One terminal, drawn in one element.
///
/// Owns the session because everything a frame needs is on this side of it: a
/// snapshot, a wake, a key. Nothing here reaches back into the parser, and the
/// session does not know this type exists.
pub struct TerminalView {
    session: Arc<TerminalSession>,
    palette: Palette,
    focus: FocusHandle,
    /// Whether the window's own activation has been observed yet.
    ///
    /// A field rather than a subscription field because the observation needs a
    /// `&mut Window`, which `new` does not have.
    observing: bool,
    /// The measured cell, and the face it was measured in. `None` until the
    /// first frame has a text system to ask.
    cell: Option<CellSize>,
    style: TextStyle,
    font_id: Option<FontId>,
    /// Whether a left-button text drag is in progress.
    dragging: bool,
    click_origin: Option<(usize, usize)>,
    last_click: Option<(std::time::Instant, (usize, usize))>,
    modified_click: bool,
    left_mouse: Option<(f32, f32)>,
    dragged: bool,
    /// Selection currently shown in the last rendered frame.
    selection_range: Cell<Option<GridRange>>,
    selection_origin: Cell<(usize, usize)>,
    canvas_origin: Rc<Cell<gpui_kit::Point<Pixels>>>,

    /// The size the PTY has last been told about, shared with the paint pass.
    ///
    /// Shared rather than a field because the size is known in prepaint, inside
    /// a closure that cannot borrow the entity that is being painted. Re-
    /// entering it from there would be a second borrow of a `RefCell` that the
    /// paint phase is already holding.
    applied: Rc<Cell<(u16, u16)>>,
    /// Where the measured cell is published for the session's host callback.
    ///
    /// `cell_metrics` is asked from the reader thread, long after the render
    /// that measured the font has ended, and it is built before this view
    /// exists. A cell neither of them owns is the only thing that can answer
    /// both.
    metrics: Arc<Mutex<CellMetrics>>,
}

impl TerminalView {
    /// Take the session's wake and start watching it.
    pub fn new(
        session: TerminalSession,
        metrics: Arc<Mutex<CellMetrics>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = Arc::new(session);

        // Focus first, because every other input path in the window is
        // downstream of somebody having focused this element.
        let focus = cx.focus_handle();

        let wake_task = start_wake_task(Arc::clone(&session), cx);

        let view = Self {
            session,
            palette: Palette::default(),
            focus,
            observing: false,
            cell: None,
            style: TextStyle {
                font_size: FONT_SIZE.into(),
                line_height: gpui_kit::DefiniteLength::Fraction(LINE_HEIGHT),
                ..TextStyle::default()
            },
            font_id: None,
            applied: Rc::new(Cell::new((0, 0))),
            metrics,
            dragging: false,
            click_origin: None,
            last_click: None,
            modified_click: false,
            left_mouse: None,
            dragged: false,
            selection_range: Cell::new(None),
            selection_origin: Cell::new((0, 0)),
            canvas_origin: Rc::new(Cell::new(point(px(0.0), px(0.0)))),
        };
        // Detached rather than remembered: the task ends when the session
        // closes or the entity goes, and holding it as a field would keep the
        // entity alive for as long as the session is.
        if let Some(task) = wake_task {
            task.detach();
        }
        view
    }

    /// The session this draws. What the window closes through.
    pub fn session(&self) -> &Arc<TerminalSession> {
        &self.session
    }

    /// The size the PTY has last been told about, as the status line shows it.
    pub fn applied_size(&self) -> (u16, u16) {
        self.applied.get()
    }

    /// Begin the close sequence, without waiting for it.
    ///
    /// A no-op the second time: the session refuses new work once a close has
    /// begun, so a window that is closed twice starts one close and not two.
    pub fn close(&self) {
        if !self.session.is_closing() {
            self.session.close();
        }
    }

    /// Measure the font, once.
    ///
    /// In `render` rather than in `new` because a cell is a pixel measurement
    /// and there is no window in `new`. Doing it here also means the very first
    /// frame already has a grid, rather than one empty frame before it.
    fn measure(&mut self, window: &mut Window) {
        if self.cell.is_some() {
            return;
        }

        let regular_font_loaded = load_bundled_font(
            window,
            &BUNDLED_FONT,
            include_bytes!("../../assets/fonts/FiraCode-Regular.ttf"),
        )
        .is_ok();
        if !regular_font_loaded {
            log::error!(
                "muster-native: bundled terminal font could not load; using an installed fallback"
            );
        }
        let bold_font_loaded = load_bundled_font(
            window,
            &BOLD_FONT,
            include_bytes!("../../assets/fonts/FiraCode-Bold.ttf"),
        )
        .is_ok();
        if !bold_font_loaded {
            log::error!("muster-native: bundled bold terminal font could not load");
        }

        let installed = window.text_system().all_font_names();
        let family = if regular_font_loaded
            && installed
                .iter()
                .any(|name| name.eq_ignore_ascii_case("Fira Code"))
        {
            "Fira Code"
        } else {
            MONOSPACE_FAMILIES
                .iter()
                .skip(1)
                .find(|wanted| {
                    installed
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(wanted))
                })
                .copied()
                .unwrap_or("monospace")
        };
        if family != "Fira Code" {
            log::error!("muster-native: bundled Fira Code did not resolve; using {family}");
        }
        self.style.font_family = SharedString::from(family);
        self.style.font_weight = FontWeight::NORMAL;
        self.style.font_features = gpui_kit::FontFeatures(std::sync::Arc::new(vec![
            ("liga".to_owned(), 1),
            ("calt".to_owned(), 1),
        ]));
        let shaping_font_id = window.text_system().resolve_font(&self.style.font());

        let font_size = self.style.font_size.to_pixels(window.rem_size());
        match CellSize::measure(window.text_system(), &self.style, font_size) {
            Some(cell) => {
                self.font_id = Some(shaping_font_id);
                *self.metrics.lock().expect("measured cell poisoned") = CellMetrics {
                    width: cell_px(cell.width),
                    height: cell_px(cell.height),
                };
                self.cell = Some(cell);
            }
            None => log::warn!(
                "muster-native: the terminal font produced no metrics for {family}; \
                 the grid is not drawn until it does"
            ),
        }
    }

    /// The focus answer for this frame.
    fn focus_state(&self, window: &Window) -> FocusState {
        FocusState {
            focused: self.focus.is_focused(window),
            window_active: window.is_window_active(),
        }
    }

    /// One keystroke, sent or refused.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Copy is an application shortcut on macOS. Check it before `input::press`,
        // which intentionally rejects platform-modified keystrokes.
        let key = &event.keystroke.key;
        let modifiers = event.keystroke.modifiers;
        if !self.focus_state(window).accepts_input() {
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("c") {
            self.copy_selection(cx);
            cx.stop_propagation();
            return;
        }
        if !cfg!(target_os = "macos")
            && modifiers.control
            && modifiers.shift
            && key.eq_ignore_ascii_case("c")
        {
            self.copy_selection(cx);
            cx.stop_propagation();
            return;
        }
        let paste_shortcut = if cfg!(target_os = "macos") {
            modifiers.platform && key.eq_ignore_ascii_case("v")
        } else {
            modifiers.control && modifiers.shift && key.eq_ignore_ascii_case("v")
        };
        if paste_shortcut {
            self.paste_clipboard(cx);
            cx.stop_propagation();
            return;
        }

        // Only one source of printable text exists here, which is what keeps a
        // press from being sent twice. See `input` for why it is this one.
        let input::Input::Press(press) = input::press(&event.keystroke) else {
            return;
        };
        if key == "tab" {
            // Capture phase prevents GPUI's focus traversal binding from taking it.
            cx.stop_propagation();
        }
        if !modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.platform {
            self.session.clear_selection();
            self.selection_range.set(None);
            cx.stop_propagation();
            cx.notify();
        }
        if let Err(error) = self.session.send_key(&press) {
            log::warn!("muster-native: a keystroke was not sent: {error}");
            cx.notify();
        }
    }

    /// What a frame needs that is not the session: the palette, the face, and
    /// the resolved font.
    ///
    /// Cloned out of the view rather than borrowed because the canvas paint
    /// closure is `FnOnce + 'static`, and the only thing it could otherwise
    /// capture is the entity that is being painted right now.
    fn copy_selection(&self, cx: &mut Context<Self>) {
        if let Some(text) = self
            .session
            .snapshot()
            .selection
            .filter(|text| !text.is_empty())
        {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
        }
    }

    /// Read system clipboard text and queue a terminal-aware paste.
    fn paste_clipboard(&self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            if let Err(error) = self.session.paste(&text) {
                log::warn!("muster-native: clipboard paste was refused: {error}");
            }
        }
    }

    fn snapshot_range(&self) -> Option<GridRange> {
        self.session.snapshot().selection_range
    }

    fn move_prompt_caret(&self, count: usize, right: bool) {
        let press = super::keys::KeyPress {
            key: if right {
                super::keys::Key::Right
            } else {
                super::keys::Key::Left
            },
            shift: false,
            ctrl: false,
            alt: false,
            meta: false,
            text: None,
        };
        for _ in 0..count {
            if let Err(error) = self.session.send_key(&press) {
                log::warn!("muster-native: prompt caret movement failed: {error}");
                break;
            }
        }
    }

    fn frame(&self) -> Frame {
        Frame {
            palette: self.palette,
            style: self.style.clone(),
            font_id: self.font_id,
        }
    }
}

/// The per-frame drawing state, held by value.
#[derive(Debug, Clone)]
struct Frame {
    palette: Palette,
    style: TextStyle,
    font_id: Option<FontId>,
}

/// Everything one paint pass needs about the grid, gathered so the painter takes
/// one argument for the grid instead of five.
struct FrameInput<'a> {
    snapshot: &'a TerminalSnapshot,
    cell: CellSize,
    columns: u16,
    rows: u16,
    focus: FocusState,
    selection_range: Option<GridRange>,
}

struct TextRun<'a> {
    bounds: Bounds<Pixels>,
    cell: &'a CellSnapshot,
    text: &'a str,
    at_cursor: bool,
    selected: bool,
}

impl Frame {
    /// Draw one frame of the grid into `bounds`.
    fn paint(
        &self,
        bounds: Bounds<Pixels>,
        input: FrameInput<'_>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let FrameInput {
            snapshot,
            cell,
            columns,
            rows,
            focus,
            selection_range,
        } = input;
        let text_system = window.text_system().clone();
        let font_id = self.font_id;
        let font_size = self.style.font_size.to_pixels(window.rem_size());

        // A window too small for one whole cell draws its background and
        // nothing else. A cell cannot be drawn at a fraction of its own size,
        // and drawing half of one is how a terminal ends up with a column of
        // clipped glyphs at the edge of a resized window.
        if columns == 0 || rows == 0 {
            return;
        }

        let cursor = cursor_cell(snapshot, (columns, rows));
        let is_wide = |c: char| {
            let Some(font_id) = font_id else { return false };
            let advance = text_system.layout_width(font_id, font_size, c);
            advance > cell.width * 1.5
        };

        for (row_index, row) in snapshot.rows.iter().take(rows as usize).enumerate() {
            let y = bounds.origin.y + row_index as f32 * cell.height;
            let mut placements = place_row(&row.cells, is_wide).into_iter().peekable();
            while let Some(placement) = placements.next() {
                let Some(source) = row.cells.get(placement.column) else {
                    break;
                };
                let x = bounds.origin.x + placement.column as f32 * cell.width;
                let span = Bounds {
                    origin: point(x, y),
                    size: gpui_kit::size(placement.columns as f32 * cell.width, cell.height),
                };
                let at_cursor = cursor == Some((row_index, placement.column));
                let selected = selection_range.is_some_and(|(start, end)| {
                    let point = (row_index, placement.column);
                    start <= point && point <= end
                });

                let mut run = source.c.to_string();
                let mut run_columns = placement.columns;
                if !at_cursor && placement.columns == 1 {
                    let end =
                        compatible_run_end(&row.cells, placement.column, placement.columns == 2);
                    while let Some(next) = placements.peek() {
                        if next.column >= end
                            || next.columns != 1
                            || cursor == Some((row_index, next.column))
                            || selection_range.is_some_and(|(start, end)| {
                                let point = (row_index, next.column);
                                start <= point && point <= end
                            }) != selected
                        {
                            break;
                        }
                        let next = placements.next().expect("peeked placement exists");
                        run.push(next.glyph);
                        run_columns += next.columns;
                    }
                }
                let run_span = Bounds {
                    origin: span.origin,
                    size: gpui_kit::size(run_columns as f32 * cell.width, span.size.height),
                };
                self.paint_text_run(
                    TextRun {
                        bounds: run_span,
                        cell: source,
                        text: &run,
                        at_cursor,
                        selected,
                    },
                    snapshot,
                    focus,
                    window,
                    cx,
                );

                if at_cursor {
                    paint_cursor(span, snapshot.cursor.shape, focus, &self.palette, window);
                }
            }
        }
    }

    fn paint_text_run(
        &self,
        run: TextRun<'_>,
        snapshot: &TerminalSnapshot,
        focus: FocusState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let TextRun {
            bounds,
            cell,
            text,
            at_cursor,
            selected,
        } = run;
        let mut background = self.palette.background(cell.bg);
        let mut foreground = self.palette.foreground(cell.fg);
        if cell.inverse {
            std::mem::swap(&mut foreground, &mut background);
        }
        if selected {
            background = opaque((0x45, 0x52, 0x79));
            foreground = opaque((0xff, 0xff, 0xff));
        }
        if cell.dim && !selected {
            foreground = fade_toward(foreground, background);
        }
        if background != self.palette.background(ColorSnapshot::Default) {
            window.paint_quad(quad(
                bounds,
                px(0.0),
                background,
                px(0.0),
                gpui_kit::transparent_black(),
                Default::default(),
            ));
        }
        let cursor_colors = block_cursor_colors(
            foreground,
            background,
            at_cursor
                && snapshot.cursor.visible
                && snapshot.cursor.shape == CursorShapeSnapshot::Block,
            focus,
        );
        if cursor_colors.background != background {
            window.paint_quad(quad(
                bounds,
                px(0.0),
                cursor_colors.background,
                px(0.0),
                gpui_kit::transparent_black(),
                Default::default(),
            ));
        }
        let mut style = self.style.clone();
        style.color = cursor_colors.foreground;
        style.font_weight = if cell.bold && BOLD_FONT.get().is_some_and(Result::is_ok) {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        };
        style.font_style = if cell.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };
        style.underline = cell.underline.then(|| UnderlineStyle {
            thickness: px(1.0),
            ..UnderlineStyle::default()
        });
        style.strikethrough = cell.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.0),
            ..StrikethroughStyle::default()
        });
        let text = SharedString::from(text.to_owned());
        let run = style.to_run(text.len());
        let shaped = window.text_system().shape_line(
            text.clone(),
            style.font_size.to_pixels(window.rem_size()),
            &[run],
            None,
        );
        // Ligature advances can exceed the terminal's fixed grid columns. Paint
        // within the run's full shaped extent; the next run still starts at its
        // grid coordinate, so we neither clip nor accumulate shaping advances.
        let paint_height = shaped.ascent + shaped.descent;
        let paint_origin = point(
            bounds.origin.x,
            bounds.origin.y + bounds.size.height - paint_height,
        );
        if let Err(error) = shaped.paint(
            paint_origin,
            paint_height,
            TextAlign::Left,
            None,
            window,
            cx,
        ) {
            log::warn!("muster-native: a text run was not painted: {error}");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CellColors {
    foreground: Hsla,
    background: Hsla,
}

fn block_cursor_colors(
    foreground: Hsla,
    background: Hsla,
    at_cursor: bool,
    focus: FocusState,
) -> CellColors {
    if at_cursor && focus.accepts_input() {
        CellColors {
            foreground: background,
            background: foreground,
        }
    } else {
        CellColors {
            foreground,
            background,
        }
    }
}

/// The grid position of the cursor, or `None` when there is nothing to draw.
///
/// Three separate refusals, and they are separate for the same reason the core
/// keeps them separate: the program hid the cursor, the program switched it
/// off, or the view is scrolled back past it. A cursor drawn in any of those
/// three states is a cursor pointing at the wrong cell.
fn cursor_cell(snapshot: &TerminalSnapshot, grid: (u16, u16)) -> Option<(usize, usize)> {
    if !snapshot.cursor.visible {
        return None;
    }
    let (row, column) = (snapshot.cursor.row, snapshot.cursor.col);
    (row < grid.1 as usize && column < grid.0 as usize).then_some((row, column))
}

/// Draw the cursor over one cell.
fn paint_cursor(
    bounds: Bounds<Pixels>,
    shape: CursorShapeSnapshot,
    focus: FocusState,
    _palette: &Palette,
    window: &mut Window,
) {
    let foreground = opaque(CURSOR_COLOR);

    let mark = |window: &mut Window, bounds: Bounds<Pixels>| {
        window.paint_quad(quad(
            bounds,
            px(0.0),
            foreground,
            px(0.0),
            gpui_kit::transparent_black(),
            Default::default(),
        ));
    };

    match shape {
        CursorShapeSnapshot::Block => {
            if focus.draws_unfocused_cursor() {
                outline(bounds, &foreground, window);
            }
        }
        CursorShapeSnapshot::HollowBlock => outline(bounds, &foreground, window),
        CursorShapeSnapshot::Underline => {
            let thickness = bounds.size.height * CURSOR_BAR;
            let bar = Bounds {
                origin: point(
                    bounds.origin.x,
                    bounds.origin.y + bounds.size.height - thickness,
                ),
                size: gpui_kit::size(bounds.size.width, thickness),
            };
            if focus.draws_unfocused_cursor() {
                outline(bar, &foreground, window);
            } else {
                mark(window, bar);
            }
        }
        CursorShapeSnapshot::Beam => {
            let thickness = bounds.size.width * CURSOR_BAR;
            let bar = Bounds {
                origin: bounds.origin,
                size: gpui_kit::size(thickness, bounds.size.height),
            };
            if focus.draws_unfocused_cursor() {
                outline(bar, &foreground, window);
            } else {
                mark(window, bar);
            }
        }
        CursorShapeSnapshot::Hidden => {}
    }
}

/// A one-pixel frame around `bounds`.
fn outline(bounds: Bounds<Pixels>, color: &Hsla, window: &mut Window) {
    let edge = CURSOR_MIN_THICKNESS;
    for bar in [
        Bounds {
            origin: bounds.origin,
            size: gpui_kit::size(bounds.size.width, edge),
        },
        Bounds {
            origin: point(bounds.origin.x, bounds.origin.y + bounds.size.height - edge),
            size: gpui_kit::size(bounds.size.width, edge),
        },
        Bounds {
            origin: bounds.origin,
            size: gpui_kit::size(edge, bounds.size.height),
        },
        Bounds {
            origin: point(bounds.origin.x + bounds.size.width - edge, bounds.origin.y),
            size: gpui_kit::size(edge, bounds.size.height),
        },
    ] {
        window.paint_quad(quad(
            bar,
            px(0.0),
            *color,
            px(0.0),
            gpui_kit::transparent_black(),
            Default::default(),
        ));
    }
}

/// Move `color` toward `toward`, keeping it opaque.
fn fade_toward(color: Hsla, toward: Hsla) -> Hsla {
    let from = color.to_rgb();
    let to = toward.to_rgb();
    let mix = |a: f32, b: f32| a + (b - a) * DIM_BLEND;
    Hsla::from(Rgba {
        r: mix(from.r, to.r),
        g: mix(from.g, to.g),
        b: mix(from.b, to.b),
        a: from.a,
    })
}

/// Sample the coalesced wake, and repaint when one arrives.
///
/// `None` when the wake has already been taken: there is then nothing left to
/// coalesce, and a loop watching a token nobody sends is a thread that never
/// ends.
fn start_wake_task(
    session: Arc<TerminalSession>,
    cx: &mut Context<TerminalView>,
) -> Option<Task<()>> {
    let background = cx.background_executor().clone();
    let wake = session.take_wake()?;

    // An async closure rather than a closure returning an async block: the
    // second one borrows the context that is handed to it, and a future cannot
    // borrow from itself.
    Some(cx.spawn(async move |view, cx| {
        // The channel holds one token, so taking it empties it and the whole
        // burst behind it collapses into one repaint. That is the coalescing: a
        // `yes` in the shell is thousands of 64 KiB reads and this is one frame
        // for all of them.
        while !session.is_closing() {
            background.timer(WAKE_POLL).await;
            if wake.take() && view.update(cx, |_, cx| cx.notify()).is_err() {
                return;
            }
        }
    }))
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.measure(window);

        // The cursor is drawn differently when the window is not the one being
        // typed into, and nothing else changes when that flips.
        if !self.observing {
            self.observing = true;
            cx.observe_window_activation(window, |_, _, cx| cx.notify())
                .detach();
        }

        let focus = self.focus_state(window);
        // One snapshot for the frame, read here rather than in the paint pass:
        // it is one lock, and reading it inside the closure would be one lock
        // per element the closure happened to be painted into.
        let snapshot = self.session.snapshot();
        self.selection_range.set(snapshot.selection_range);
        let frame = self.frame();
        let canvas_origin = Rc::clone(&self.canvas_origin);
        let applied = Rc::clone(&self.applied);
        let cell = self.cell;

        div()
            .id("muster-terminal")
            .size_full()
            .track_focus(&self.focus)
            // A click anywhere in the grid is a click on the terminal. There is
            // nothing inside it to click, so without this the window would take
            // focus only when the pointer happened to land on the chrome.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui_kit::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    let (cols, rows) = this.applied.get();
                    let snapshot = this.session.snapshot();
                    let modified = event.modifiers.shift
                        || event.modifiers.control
                        || event.modifiers.alt
                        || event.modifiers.platform;
                    if cols > 0 && rows > 0 && !snapshot.mouse_tracking {
                        cx.stop_propagation();
                        let point = grid_point(
                            event.position,
                            this.canvas_origin.get(),
                            this.cell.unwrap_or(CellSize {
                                width: px(1.0),
                                height: px(1.0),
                            }),
                            (cols, rows),
                        );
                        this.selection_origin.set(point);
                        this.left_mouse =
                            Some((event.position.x.as_f32(), event.position.y.as_f32()));
                        this.dragged = false;
                        this.dragging = false;
                        this.click_origin = Some(point);
                        this.modified_click = modified;
                        // Every click that starts a new gesture drops the old
                        // selection, modified or not: a selection that survives
                        // a click somewhere else is one the person cannot get
                        // rid of, and `Cmd+C` would then copy text they can no
                        // longer see.
                        this.session.clear_selection();
                        this.selection_range.set(None);
                        cx.notify();
                        cx.stop_propagation();
                    }
                }),
            )
            .on_mouse_move(
                cx.listener(|this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                    if let Some(origin) = this
                        .click_origin
                        .filter(|_| event.pressed_button == Some(MouseButton::Left))
                    {
                        let point = grid_point(
                            event.position,
                            this.canvas_origin.get(),
                            this.cell.unwrap_or(CellSize {
                                width: px(1.0),
                                height: px(1.0),
                            }),
                            this.applied.get(),
                        );
                        if point != origin {
                            this.dragging = true;
                        }
                        if this.left_mouse.is_some_and(|(x, y)| {
                            (event.position.x.as_f32() - x).hypot(event.position.y.as_f32() - y)
                                > 3.0
                        }) {
                            this.dragged = true;
                        }
                        if this.dragging && !this.modified_click {
                            this.session.select(origin, point);
                            this.selection_range.set(this.snapshot_range());
                        }
                        cx.notify();
                        cx.stop_propagation();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if let Some(point) = this.click_origin.take() {
                        if !this.modified_click && !this.dragged {
                            // A plain click always clears any old selection, even
                            // when it lands on output rather than the editable line.
                            this.session.clear_selection();
                            this.selection_range.set(None);
                            let now = std::time::Instant::now();
                            let double = this.last_click.is_some_and(|(time, previous)| {
                                previous == point
                                    && now.duration_since(time) < Duration::from_millis(500)
                            });
                            if double {
                                this.session.select(
                                    (point.0, point.1.saturating_sub(1)),
                                    (point.0, point.1 + 1),
                                );
                                this.selection_range.set(this.snapshot_range());
                                this.copy_selection(cx);
                                this.last_click = None;
                            } else {
                                this.last_click = Some((now, point));
                                let snapshot = this.session.snapshot();
                                if let Some(count) =
                                    this.session.move_prompt_caret(point.0, point.1)
                                {
                                    this.move_prompt_caret(count, point.1 > snapshot.cursor.col);
                                }
                            }
                        }
                    }
                    if this.dragged && !this.modified_click {
                        this.copy_selection(cx);
                    }
                    this.left_mouse = None;
                    this.dragged = false;
                    this.dragging = false;
                    this.modified_click = false;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.dragged && !this.modified_click {
                        this.copy_selection(cx);
                    }
                    this.left_mouse = None;
                    this.dragged = false;
                    this.click_origin = None;
                    this.dragging = false;
                    this.modified_click = false;
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                canvas(
                    {
                        let applied = Rc::clone(&applied);
                        let view = Arc::clone(&self.session);
                        move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut App| {
                            canvas_origin.set(bounds.origin);
                            let cell = cell?;
                            let size = (
                                cell.columns_in(bounds.size.width),
                                cell.rows_in(bounds.size.height),
                            );
                            // A window measuring nothing is invisible or minimised.
                            // `resize` refuses zero already; asking only when the
                            // answer changed is what keeps a hidden window from
                            // asking sixty times a second.
                            if size.0 > 0 && size.1 > 0 && applied.get() != size {
                                applied.set(size);
                                if let Err(error) = view.resize(size.0, size.1) {
                                    log::warn!("muster-native: the grid was not resized: {error}");
                                }
                            }
                            Some((cell, size))
                        }
                    },
                    move |bounds, state, window, cx| {
                        let Some((cell, (columns, rows))) = state else {
                            return;
                        };
                        frame.paint(
                            bounds,
                            FrameInput {
                                snapshot: &snapshot,
                                cell,
                                columns,
                                rows,
                                focus,
                                selection_range: snapshot.selection_range,
                            },
                            window,
                            cx,
                        );
                    },
                )
                // A canvas has no content to measure, so in a flex column it
                // stretches to the parent's width and keeps a zero height. The
                // grid is the window, so it has to fill both axes itself.
                .size_full(),
            )
    }
}

impl gpui_kit::Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;

    fn cell(c: char) -> CellSnapshot {
        CellSnapshot {
            c,
            fg: ColorSnapshot::Default,
            bg: ColorSnapshot::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            strikethrough: false,
            inverse: false,
        }
    }

    fn row(chars: &str) -> Vec<CellSnapshot> {
        chars.chars().map(cell).collect()
    }

    /// Every character in the test rows is one column, which is what a
    /// headless `NoopTextSystem` measures: no font is loaded, so nothing is
    /// ever wide.
    fn never_wide(_: char) -> bool {
        false
    }

    /// Wide only for CJK, which is the case the arithmetic exists for.
    fn cjk_is_wide(c: char) -> bool {
        ('\u{1100}'..='\u{9fff}').contains(&c) || ('\u{ac00}'..='\u{d7af}').contains(&c)
    }

    /// The terminal under a window's own chrome, the way the app draws it.
    struct Chrome {
        view: gpui_kit::Entity<TerminalView>,
        header: gpui_kit::Pixels,
    }

    impl Render for Chrome {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            gpui_kit::div()
                .size_full()
                .flex_col()
                .child(gpui_kit::div().h(self.header).child("title bar"))
                .child(self.view.clone())
        }
    }

    #[gpui_kit::test]
    fn selection_works_under_window_chrome(cx: &mut gpui_kit::TestAppContext) {
        // The app draws a title bar above the grid, so the canvas does not start
        // at the window's origin. A drag whose coordinates are read against the
        // wrong origin selects the wrong cells, or nothing.
        const HEADER: f32 = 40.;
        let session = echo_session();
        let metrics = Arc::new(Mutex::new(CellMetrics {
            width: 8,
            height: 16,
        }));
        let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(session, metrics, cx));
        let session = view.read_with(cx, |view, _| Arc::clone(view.session()));
        let (terminal, cx) = cx.add_window_view(|_, _| Chrome {
            view,
            header: px(HEADER),
        });
        let view = terminal.read_with(cx, |chrome, _| chrome.view.clone());
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }
        assert!(
            wait_for_char(&session, 's'),
            "the child never drew its text"
        );

        let (origin, cell) = view.read_with(cx, |view, _| {
            (
                view.canvas_origin.get(),
                view.cell.expect("the grid measured a cell"),
            )
        });
        assert!(
            origin.y >= px(HEADER),
            "the grid reports {origin:?}, so it does not know it sits under the title bar"
        );
        let row_y = origin.y + cell.height * 0.5;
        let at = |column: f32| gpui_kit::point(origin.x + cell.width * column, row_y);

        cx.simulate_mouse_down(at(0.2), MouseButton::Left, Default::default());
        cx.simulate_mouse_move(at(13.8), MouseButton::Left, Default::default());
        cx.simulate_mouse_up(at(13.8), MouseButton::Left, Default::default());

        let selected = session
            .snapshot()
            .selection
            .expect("a drag under the chrome selected text");
        assert!(
            selected.contains("selection-text"),
            "selected: {selected:?}"
        );

        let _ = session.close().wait();
    }

    #[gpui_kit::test]
    fn the_platform_paste_shortcut_sends_the_clipboard_to_the_child(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let session = echo_session();
        let metrics = Arc::new(Mutex::new(CellMetrics {
            width: 8,
            height: 16,
        }));
        let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(session, metrics, cx));
        let session = view.read_with(cx, |view, _| Arc::clone(view.session()));
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }

        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            "pasted-text".to_owned(),
        ));
        // A click first, because that is what focuses the grid: shortcuts
        // belong to the focused element, and a test that skipped it would be
        // testing a window nobody is typing into.
        cx.simulate_click(gpui_kit::point(px(4.), px(4.)), Default::default());
        cx.simulate_keystrokes("cmd-v");

        let pasted = wait_for_char(&session, 'p');
        let _ = session.close().wait();
        assert!(pasted, "the clipboard never reached the child's write path");
    }

    /// A session whose child prints `selection-text` and then waits for input.
    ///
    /// `cat` after a `printf`, so the grid holds text to select and a paste has
    /// something to echo, without the child exiting: a test that raced an exiting
    /// child would be a test about scheduling.
    fn echo_session() -> TerminalSession {
        let backend = Arc::new(crate::pty::TerminalBackend::default());
        let mut config = crate::terminal::TerminalSessionConfig::new(
            format!("muster-native-view-{}", std::process::id()),
            std::env::temp_dir(),
            80,
            24,
            Some(std::path::PathBuf::from("/bin/sh")),
        );
        config.args = Some(vec![
            "-c".to_owned(),
            "printf selection-text; cat".to_owned(),
        ]);
        config.drain_deadline = Duration::from_millis(500);
        TerminalSession::spawn(Arc::clone(&backend), config).expect("the test session spawns")
    }

    /// Whether the grid holds `wanted`, within a bounded wait.
    fn wait_for_char(session: &TerminalSession, wanted: char) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if session
                .snapshot()
                .rows
                .iter()
                .any(|row| row.cells.iter().any(|cell| cell.c == wanted))
            {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[gpui_kit::test]
    fn ctrl_c_still_interrupts_even_with_a_selection(cx: &mut gpui_kit::TestAppContext) {
        // The one shortcut a terminal may not repurpose: Ctrl+C is SIGINT.
        // A selection must never turn it into a copy, on any platform, and the
        // shortcut that copies stays on the platform's own modifier.
        let session = echo_session();
        let metrics = Arc::new(Mutex::new(CellMetrics {
            width: 8,
            height: 16,
        }));
        let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(session, metrics, cx));
        let session = view.read_with(cx, |view, _| Arc::clone(view.session()));
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }
        assert!(
            wait_for_char(&session, 's'),
            "the child never drew its text"
        );

        let (origin, cell) = view.read_with(cx, |view, _| {
            (
                view.canvas_origin.get(),
                view.cell.expect("the grid measured a cell"),
            )
        });
        let row_y = origin.y + cell.height * 0.5;
        let at = |column: f32| gpui_kit::point(origin.x + cell.width * column, row_y);
        cx.simulate_mouse_down(at(0.2), MouseButton::Left, Default::default());
        cx.simulate_mouse_move(at(13.8), MouseButton::Left, Default::default());
        cx.simulate_mouse_up(at(13.8), MouseButton::Left, Default::default());
        assert!(
            session.snapshot().selection.is_some(),
            "the drag selected nothing to interrupt over"
        );

        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(String::new()));
        cx.simulate_keystrokes("ctrl-c");

        // `cat` dies on the interrupt, so nothing more is echoed: the child is
        // gone, which is exactly what Ctrl+C is for.
        let closed = wait_for_close(&session);
        let copied = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        let _ = session.close().wait();
        assert!(
            copied.is_empty(),
            "Ctrl+C copied the selection instead of interrupting: {copied:?}"
        );
        assert!(closed, "Ctrl+C did not interrupt the child");
    }

    /// Whether the session's child has exited, within a bounded wait.
    fn wait_for_close(session: &TerminalSession) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if matches!(
                session.status().stream,
                crate::terminal::StreamState::Ended(_)
            ) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A prompt that reports where it is, and a child that reads one line and
    /// prints back what it was sent.
    ///
    /// `printf 'ab\033]133;B\007'; read line; printf 'GOT:%s\n' "$line"` is a
    /// prompt the terminal can trust: the marker says the input starts after
    /// `ab`, and whatever the click sends comes back as the child's own answer.
    fn prompt_session() -> TerminalSession {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let backend = Arc::new(crate::pty::TerminalBackend::default());
        let mut config = crate::terminal::TerminalSessionConfig::new(
            // A session id of its own per test: ids are how the session store
            // tells two terminals apart, and three tests sharing one would be
            // three tests writing over each other's shell.
            format!(
                "muster-native-prompt-{}",
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
            std::env::temp_dir(),
            80,
            24,
            Some(std::path::PathBuf::from("/bin/sh")),
        );
        config.args = Some(vec![
            "-c".to_owned(),
            // The pause is the resize: the grid is measured after the child
            // starts, and a resize drops an anchor it cannot place, so a prompt
            // that announced itself first would be forgotten.
            "stty -echo; sleep 0.3; printf 'abcd\\033]133;B\\007cd'; IFS= read -r line; printf 'GOT:%s\\n' \"$line\" | cat -v; sleep 30"
                .to_owned(),
        ]);
        config.drain_deadline = Duration::from_millis(500);
        TerminalSession::spawn(Arc::clone(&backend), config).expect("the prompt session spawns")
    }

    /// What the child answered, once it has answered.
    fn wait_for_line(session: &TerminalSession, wanted: &str) -> Option<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let text: String = session
                .snapshot()
                .rows
                .iter()
                .flat_map(|row| row.cells.iter())
                .map(|cell| cell.c)
                .collect();
            if let Some(rest) = text.split(wanted).nth(1) {
                return Some(rest.trim_end().to_owned());
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Clicking the prompt moves the shell's caret to the right.
    #[gpui_kit::test]
    fn clicking_the_prompt_moves_the_shell_caret(cx: &mut gpui_kit::TestAppContext) {
        assert_eq!(
            caret_moves_for(cx, 8.),
            Some("^[[C^[[C".to_owned()),
            "two columns right of the caret is two right arrows"
        );
    }

    #[gpui_kit::test]
    fn clicking_left_of_the_caret_sends_left_arrows(cx: &mut gpui_kit::TestAppContext) {
        assert_eq!(
            caret_moves_for(cx, 4.),
            Some("^[[D^[[D".to_owned()),
            "two columns left of the caret is two left arrows"
        );
    }

    /// Output above the prompt is not an editable line, so a click there is a
    /// selection and not a command to the shell.
    #[gpui_kit::test]
    fn clicking_output_above_the_prompt_sends_nothing(cx: &mut gpui_kit::TestAppContext) {
        let sent = click_and_read(cx, |_, _| {});
        assert_eq!(
            sent, None,
            "a click that moved nothing must leave the child waiting"
        );
    }

    /// Click the grid at `column` of the prompt line and let the child answer:
    /// whatever the click sent is what `read` prints back.
    fn caret_moves_for(cx: &mut gpui_kit::TestAppContext, column: f32) -> Option<String> {
        click_and_read(cx, move |cx, point| {
            cx.simulate_click(point(column, 0.), Default::default());
            // `read` answers a line, so the movement needs its newline.
            cx.simulate_keystrokes("enter");
        })
    }

    /// A session over [`prompt_session`], clicked on its prompt line.
    ///
    /// The child's echo is off and it prints its own input, so the grid's caret
    /// sits where the fixture put it and nothing sent to the child comes back as
    /// output to be mistaken for the answer.
    fn click_and_read(
        cx: &mut gpui_kit::TestAppContext,
        click: impl FnOnce(
            &mut gpui_kit::VisualTestContext,
            &dyn Fn(f32, f32) -> gpui_kit::Point<gpui_kit::Pixels>,
        ),
    ) -> Option<String> {
        let session = prompt_session();
        let metrics = Arc::new(Mutex::new(CellMetrics {
            width: 8,
            height: 16,
        }));
        let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(session, metrics, cx));
        let session = view.read_with(cx, |view, _| Arc::clone(view.session()));
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }
        assert!(wait_for_char(&session, 'c'), "the prompt never drew");

        let (origin, cell) = view.read_with(cx, |view, _| {
            (
                view.canvas_origin.get(),
                view.cell.expect("the grid measured a cell"),
            )
        });
        let point = move |column: f32, row: f32| {
            gpui_kit::point(
                origin.x + cell.width * column,
                origin.y + cell.height * (row + 0.5),
            )
        };
        let visual = cx;
        // Focus first, with a click on the row below the prompt: output is not
        // editable, so that click is free of consequences and cannot be the
        // double click that a second press on one cell would be.
        visual.simulate_click(point(0., 1.), Default::default());
        assert!(
            session.snapshot().prompt_anchor.is_some(),
            "the child's marker never reached the core: {:?}",
            session.snapshot()
        );
        click(visual, &point);
        let answer = wait_for_line(&session, "GOT:");
        let _ = session.close().wait();
        answer
    }

    #[test]
    fn the_xterm_256_computation_is_exact_at_every_boundary() {
        // The sixteen named entries, which are a table and not a computation.
        assert_eq!(xterm_256(0), (0x28, 0x2c, 0x34));
        assert_eq!(xterm_256(1), (0xe0, 0x6c, 0x75));
        assert_eq!(xterm_256(6), (0x56, 0xb6, 0xc2));
        assert_eq!(xterm_256(7), (0xab, 0xb2, 0xbf));
        assert_eq!(xterm_256(9), (0xea, 0x85, 0x8b));
        assert_eq!(xterm_256(12), (0x85, 0xc1, 0xff));
        assert_eq!(xterm_256(8), (0x63, 0x6d, 0x83));
        assert_eq!(xterm_256(13), (0xd3, 0x98, 0xeb));
        assert_eq!(xterm_256(14), (0x6e, 0xd5, 0xde));
        assert_eq!(xterm_256(15), (0xfa, 0xfa, 0xfa));

        let palette = Palette::default();
        assert_eq!(
            palette.foreground,
            (0xdc, 0xe0, 0xe5),
            "legacy content text"
        );
        assert_eq!(
            palette.background,
            (0x22, 0x24, 0x36),
            "legacy content background"
        );

        // The cube: 16 is its black corner and 231 its white one.
        assert_eq!(xterm_256(16), (0, 0, 0));
        assert_eq!(xterm_256(231), (255, 255, 255));
        // Inside the cube, so the levels are `55 + 40 * step` and not a ramp.
        assert_eq!(xterm_256(196), (255, 0, 0), "pure red is 5, 0, 0");
        assert_eq!(xterm_256(46), (0, 255, 0), "pure green is 0, 5, 0");
        assert_eq!(xterm_256(21), (0, 0, 255), "pure blue is 0, 0, 5");

        // The grey ramp: 8 to 238 in steps of 10, which is a different rule.
        assert_eq!(xterm_256(232), (8, 8, 8));
        assert_eq!(xterm_256(255), (238, 238, 238));

        for (index, expected) in [
            (16, (0u8, 0u8, 0u8)),
            (231, (255, 255, 255)),
            (232, (8, 8, 8)),
            (255, (238, 238, 238)),
        ] {
            for color in [
                palette.foreground(ColorSnapshot::Indexed(index)),
                palette.background(ColorSnapshot::Indexed(index)),
            ] {
                let rgb = color.to_rgb();
                assert_eq!(
                    (rgb.r, rgb.g, rgb.b),
                    (
                        f32::from(expected.0) / 255.0,
                        f32::from(expected.1) / 255.0,
                        f32::from(expected.2) / 255.0,
                    )
                );
            }
        }
    }

    #[test]
    fn block_cursor_color_logic_inverts_only_when_focused_and_visible() {
        let foreground = opaque((1, 2, 3));
        let background = opaque((4, 5, 6));
        let focused = FocusState {
            focused: true,
            window_active: true,
        };
        let inactive = FocusState {
            focused: true,
            window_active: false,
        };

        let inverted = block_cursor_colors(foreground, background, true, focused);
        assert_eq!(inverted.foreground, background);
        assert_eq!(inverted.background, foreground);

        let unfocused = block_cursor_colors(foreground, background, true, inactive);
        assert_eq!(
            unfocused,
            CellColors {
                foreground,
                background
            }
        );
        let hidden = block_cursor_colors(foreground, background, false, focused);
        assert_eq!(
            hidden,
            CellColors {
                foreground,
                background
            }
        );
    }

    #[test]
    fn a_wide_character_is_placed_once_and_its_second_column_is_never_visited() {
        // One CJK glyph between two ASCII ones, with the parser's placeholder in
        // the column the glyph already owns. The placeholder is part of the
        // input and not something `place_row` adds: the parser handed the wide
        // glyph two columns and left the second one for the next character.
        let cells = vec![cell('a'), cell('中'), cell(' '), cell('b')];
        assert_eq!(cells.len(), 4, "the parser gave the wide glyph two columns");

        let placements = place_row(&cells, cjk_is_wide);
        assert_eq!(
            placements,
            vec![
                Placement {
                    column: 0,
                    columns: 1,
                    glyph: 'a'
                },
                Placement {
                    column: 1,
                    columns: 2,
                    glyph: '中'
                },
                Placement {
                    column: 3,
                    columns: 1,
                    glyph: 'b'
                },
            ],
            "column 2 is claimed by the wide glyph and never offered a glyph of its own"
        );

        // The property that matters: every glyph is drawn exactly once, and the
        // placeholder cell is never one of them.
        let drawn: Vec<char> = placements.iter().map(|placement| placement.glyph).collect();
        assert_eq!(drawn, vec!['a', '中', 'b']);
        assert!(!drawn.contains(&' '), "the spacer cell drew something");
        assert_eq!(
            placements.iter().map(|p| p.columns).sum::<usize>(),
            cells.len(),
            "every column of the row is claimed by exactly one glyph"
        );
        assert!(
            placements
                .windows(2)
                .all(|pair| pair[0].column + pair[0].columns <= pair[1].column),
            "two placements claim the same column"
        );
    }

    #[test]
    fn a_wide_character_at_the_end_of_a_row_does_not_run_past_it() {
        let cells = vec![cell('中'), cell(' ')];
        let placements = place_row(&cells, cjk_is_wide);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].columns, 2);
        assert_eq!(
            placements[0].column + placements[0].columns,
            cells.len(),
            "the last glyph claims the rest of the row and no more"
        );
    }

    #[test]
    fn contiguous_style_runs_only_join_compatible_narrow_cells() {
        let plain = cell('a');
        let mut bold = cell('b');
        bold.bold = true;
        let mut colored = cell('c');
        colored.fg = ColorSnapshot::Indexed(1);
        assert_eq!(
            compatible_run_end(&[plain.clone(), plain.clone(), bold, colored], 0, false),
            2
        );
        assert_eq!(
            compatible_run_end(&[plain.clone(), plain, cell('中')], 0, true),
            1
        );
    }

    #[test]
    fn a_run_of_same_styled_characters_is_one_shaping_run() {
        // A ligature is a property of a shaped sequence, so a grid that hands
        // the shaper one character at a time cannot produce one, however many
        // features the style asks for.
        assert_eq!(compatible_run_end(&row("=>="), 0, false), 3);
    }

    #[test]
    fn a_row_with_nothing_wide_is_one_placement_per_column() {
        let cells = row("hello");
        let placements = place_row(&cells, never_wide);
        assert_eq!(placements.len(), cells.len());
        assert!(placements.iter().all(|p| p.columns == 1));
        assert_eq!(
            placements.iter().map(|p| p.column).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn the_grid_counts_whole_cells_and_never_a_fraction_of_one() {
        let cell = CellSize {
            width: px(8.0),
            height: px(16.0),
        };
        assert_eq!(cell.columns_in(px(80.0)), 10);
        assert_eq!(
            cell.columns_in(px(87.0)),
            10,
            "7 pixels cannot hold a column"
        );
        assert_eq!(cell.rows_in(px(400.0)), 25);
        let spaced_cell = CellSize {
            height: px(20.0),
            ..cell
        };
        assert_eq!(spaced_cell.rows_in(px(400.0)), 20);
        assert_eq!(
            cell.columns_in(px(4.0)),
            0,
            "a window under one cell is not a size"
        );
        assert_eq!(
            cell.rows_in(px(0.0)),
            0,
            "neither is a window measuring nothing"
        );
    }
}
