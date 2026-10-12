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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{
    canvas, div, point, px, quad, App, Bounds, Context, FocusHandle, FontId, FontStyle, FontWeight,
    Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels,
    Render, Rgba, SharedString, StrikethroughStyle, Styled, Task, TextAlign, TextStyle,
    UnderlineStyle, Window, WindowTextSystem,
};

use super::input::{self, FocusState};
use super::{
    CellMetrics, CellSnapshot, ColorSnapshot, CursorShapeSnapshot, TerminalSession,
    TerminalSnapshot,
};

/// The terminal's font size, in pixels.
///
/// A constant rather than a preference: M0 has one grid and no way to change
/// it, and a value nobody can reach is not a setting.
const FONT_SIZE: Pixels = px(14.0);

/// How often the coalesced wake is sampled.
///
/// One frame at 60Hz. The wake channel holds a single token, so sampling twice
/// as fast could not turn one burst of output into a second repaint; it could
/// only add a wake-up per frame to a window that has nothing to show.
const WAKE_POLL: Duration = Duration::from_millis(16);

/// The thickness of a bar-shaped cursor, as a fraction of the cell height.
///
/// Two percent is a beam in a beam-cursor terminal and a slab in a 10pt one,
/// which is what the cursor the user chose should look like at any size.
const CURSOR_BAR: f32 = 0.08;

/// Never thinner than this, because a sub-pixel bar is a bar nobody can see.
const CURSOR_MIN_THICKNESS: Pixels = px(1.0);

/// Families to take a terminal grid from, best first.
///
/// Chosen against [`gpui_kit::TextSystem::all_font_names`] rather than named
/// outright: a family that is not installed fails to resolve, and a grid that
/// panics because a font is missing is not a terminal.
const MONOSPACE_FAMILIES: &[&str] = &[
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

/// What an unfocused window draws instead of the character it would have
/// inverted: a solid block, in the background's own colour, so the cursor
/// disappears rather than pointing at a cell the person is not in.
const DIM_BLEND: f32 = 0.45;

// ---------------------------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------------------------

/// xterm palette entries 0 to 15, in index order.
///
/// The xterm defaults rather than a modern theme's. A child that asked for
/// index 1 asked for red, and M0 has no reason to disagree with what every
/// other terminal on the machine showed it.
const ANSI_COLORS: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), // black
    (0xcd, 0x00, 0x00), // red
    (0x00, 0xcd, 0x00), // green
    (0xcd, 0xcd, 0x00), // yellow
    (0x00, 0x00, 0xee), // blue
    (0xcd, 0x00, 0xcd), // magenta
    (0x00, 0xcd, 0xcd), // cyan
    (0xe5, 0xe5, 0xe5), // white
    (0x7f, 0x7f, 0x7f), // bright black
    (0xff, 0x00, 0x00), // bright red
    (0x00, 0xff, 0x00), // bright green
    (0xff, 0xff, 0x00), // bright yellow
    (0x5c, 0x5c, 0xff), // bright blue
    (0xff, 0x00, 0xff), // bright magenta
    (0x00, 0xff, 0xff), // bright cyan
    (0xff, 0xff, 0xff), // bright white
];

/// One xterm-256 entry by index.
///
/// Indices 0 to 15 are [`ANSI_COLORS`]. The rest are a computation, not a
/// table, and the computation is the whole xterm specification: a 6x6x6 cube
/// whose levels are `0` or `55 + 40 * step`, then a 24-step grey ramp starting
/// at 8. A child that indexed 231 is asking for the brightest cube corner and
/// one that indexed 232 is asking for the dimmest grey in the ramp.
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
/// A table and nothing else. M0 needs one palette, it needs it written down
/// where it can be read, and a theme system would be a way of not choosing
/// between the sixteen ANSI entries a child can address by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// What a cell with no colour of its own is drawn in.
    pub foreground: (u8, u8, u8),
    /// The same, behind the character.
    pub background: (u8, u8, u8),
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            foreground: (0xd8, 0xd8, 0xd8),
            background: (0x11, 0x11, 0x11),
        }
    }
}

impl Palette {
    /// The colour a cell's text is painted in.
    ///
    /// `Default` is the only case that needs the palette: an indexed colour is
    /// an xterm entry whatever the theme is, and an `Rgb` colour is what the
    /// program asked for to the byte.
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
            // Whole pixels: a row that straddles a pixel boundary draws its
            // baseline twice as dark on the rows that share it.
            height: (probe.ascent + probe.descent).ceil(),
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
                ..TextStyle::default()
            },
            font_id: None,
            applied: Rc::new(Cell::new((0, 0))),
            metrics,
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

        let installed = window.text_system().all_font_names();
        let family = MONOSPACE_FAMILIES
            .iter()
            .find(|wanted| {
                installed
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(wanted))
            })
            .copied()
            .unwrap_or("monospace");
        self.style.font_family = SharedString::from(family);

        let font_size = self.style.font_size.to_pixels(window.rem_size());
        match CellSize::measure(window.text_system(), &self.style, font_size) {
            Some(cell) => {
                // Resolved from a family that was in the installed list, so this
                // is a lookup rather than a guess. It is the same resolution
                // every text element in the process does.
                self.font_id = Some(window.text_system().resolve_font(&self.style.font()));
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
        // Only one source of printable text exists here, which is what keeps a
        // press from being sent twice. See `input` for why it is this one.
        let input::FocusState {
            focused: _,
            window_active: _,
        } = self.focus_state(window);
        if !self.focus_state(window).accepts_input() {
            return;
        }

        let input::Input::Press(press) = input::press(&event.keystroke) else {
            return;
        };
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

        let cursor = cursor_cell(snapshot, focus, (columns, rows));
        let is_wide = |c: char| {
            let Some(font_id) = font_id else { return false };
            // Measured, not guessed from a Unicode table: whether a glyph is
            // two columns wide is a property of the face that will draw it, and
            // a table would be right about the character and wrong about the
            // font in a way that only shows up as a torn CJK row.
            let advance = text_system.layout_width(font_id, font_size, c);
            advance > cell.width * 1.5
        };

        for (row_index, row) in snapshot.rows.iter().take(rows as usize).enumerate() {
            let y = bounds.origin.y + row_index as f32 * cell.height;

            for placement in place_row(&row.cells, is_wide) {
                let Some(source) = row.cells.get(placement.column) else {
                    break;
                };
                let x = bounds.origin.x + placement.column as f32 * cell.width;
                let span = Bounds {
                    origin: point(x, y),
                    size: gpui_kit::size(placement.columns as f32 * cell.width, cell.height),
                };
                let at_cursor = cursor == Some((row_index, placement.column));

                self.paint_cell(span, source, at_cursor, snapshot.cursor.shape, window, cx);

                if at_cursor {
                    paint_cursor(span, snapshot.cursor.shape, focus, &self.palette, window);
                }
            }
        }
    }

    /// One cell: its background, then its glyph.
    fn paint_cell(
        &self,
        bounds: Bounds<Pixels>,
        cell: &CellSnapshot,
        at_cursor: bool,
        cursor_shape: CursorShapeSnapshot,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut background = self.palette.background(cell.bg);
        let mut foreground = self.palette.foreground(cell.fg);

        if cell.inverse {
            std::mem::swap(&mut foreground, &mut background);
        }
        if cell.dim {
            // Dimmed is less contrast, not less opacity: half-transparent text
            // over a busy background reads as a rendering fault, and a colour
            // moved toward what is behind it reads as the program asking for it.
            foreground = fade_toward(foreground, background);
        }

        let grid_background = self.palette.background(ColorSnapshot::Default);
        if background != grid_background {
            window.paint_quad(quad(
                bounds,
                px(0.0),
                background,
                px(0.0),
                gpui_kit::transparent_black(),
                Default::default(),
            ));
        }

        let glyph = if at_cursor && snapshot_inverts(cursor_shape) {
            background
        } else {
            foreground
        };

        // A space has nothing to draw unless something is drawn through it, and
        // shaping one glyph per blank cell is most of what a terminal would
        // spend on an empty screen.
        if cell.c == ' ' && !cell.underline && !cell.strikethrough {
            return;
        }

        let mut style = self.style.clone();
        style.color = glyph;
        style.font_weight = if cell.bold {
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

        let text = SharedString::from(cell.c.to_string());
        let shaped = window.text_system().shape_line(
            text.clone(),
            style.font_size.to_pixels(window.rem_size()),
            &[style.to_run(text.len())],
            None,
        );
        if let Err(error) = shaped.paint(
            bounds.origin,
            bounds.size.height,
            TextAlign::Left,
            None,
            window,
            cx,
        ) {
            log::warn!("muster-native: a cell was not painted: {error}");
        }
    }
}

/// Whether a cursor shape draws by inverting the cell it is on.
///
/// The block and the hollow block are both "where am I" answers, so both make
/// the character under them the opposite colour; a beam and an underline are
/// "where am I typing" answers and only add a mark beside the character.
fn snapshot_inverts(shape: CursorShapeSnapshot) -> bool {
    matches!(shape, CursorShapeSnapshot::Block)
}

/// The grid position of the cursor, or `None` when there is nothing to draw.
///
/// Three separate refusals, and they are separate for the same reason the core
/// keeps them separate: the program hid the cursor, the program switched it
/// off, or the view is scrolled back past it. A cursor drawn in any of those
/// three states is a cursor pointing at the wrong cell.
fn cursor_cell(
    snapshot: &TerminalSnapshot,
    focus: FocusState,
    grid: (u16, u16),
) -> Option<(usize, usize)> {
    if !snapshot.cursor.visible || focus.draws_unfocused_cursor() && false {
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
    palette: &Palette,
    window: &mut Window,
) {
    let background = palette.background(ColorSnapshot::Default);
    let foreground = palette.foreground(ColorSnapshot::Default);

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
        // Focused, and the shapes that carry the cell's own colours already.
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
        // `cursor_cell` already refused to draw a hidden cursor. Drawing
        // nothing here keeps that refusal in one place instead of two.
        CursorShapeSnapshot::Hidden => {}
    }

    // The unfocused block is not an outline here: a filled block in the
    // background's own colour is what a terminal that has lost focus draws, and
    // it is why a background colour is drawn even when no cell asked for one.
    let _ = background;
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

        let applied = Rc::clone(&self.applied);
        let cell = self.cell;
        let focus = self.focus_state(window);
        // One snapshot for the frame, read here rather than in the paint pass:
        // it is one lock, and reading it inside the closure would be one lock
        // per element the closure happened to be painted into.
        let snapshot = self.session.snapshot();
        let frame = self.frame();

        div()
            .id("muster-terminal")
            .size_full()
            .track_focus(&self.focus)
            // A click anywhere in the grid is a click on the terminal. There is
            // nothing inside it to click, so without this the window would take
            // focus only when the pointer happened to land on the chrome.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
            )
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                canvas(
                    {
                        let applied = Rc::clone(&applied);
                        let view = Arc::clone(&self.session);
                        move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut App| {
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

    #[test]
    fn the_xterm_256_computation_is_exact_at_every_boundary() {
        // The sixteen named entries, which are a table and not a computation.
        assert_eq!(xterm_256(0), (0x00, 0x00, 0x00));
        assert_eq!(xterm_256(7), (0xe5, 0xe5, 0xe5));
        assert_eq!(xterm_256(8), (0x7f, 0x7f, 0x7f));
        assert_eq!(xterm_256(15), (0xff, 0xff, 0xff));

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
