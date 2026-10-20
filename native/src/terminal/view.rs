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

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::input::{Copy as EditCopy, Paste as EditPaste};
#[cfg(test)]
use gpui_kit::test::TestSupportExt;
use gpui_kit::{
    canvas, div, point, px, quad, App, Bounds, Context, FocusHandle, FontStyle, FontWeight, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Render,
    Rgba, SharedString, StatefulInteractiveElement, StrikethroughStyle, Styled, Task, TextAlign,
    TextStyle, UnderlineStyle, Window, WindowTextSystem,
};
use unicode_width::UnicodeWidthStr;

use super::input::{self, FocusState};
use super::{
    CellMetrics, CellSnapshot, ColorSnapshot, CursorShapeSnapshot, GridRange, TerminalSession,
    TerminalShiftTab, TerminalSnapshot, TerminalTab,
};

/// The terminal's font size and line spacing, matching the legacy terminal defaults.
///
/// Published so the document and the diff can be drawn at this same size: the
/// grid, a file and a diff are read in one window, and code three pixels
/// smaller than the terminal beside it is the first thing anybody notices.
pub(crate) const FONT_SIZE: Pixels = px(16.0);
const LINE_HEIGHT: f32 = 1.2;
/// `--muster-cursor` from the legacy dark theme, which is also the editor's caret.
const CURSOR_COLOR: (u8, u8, u8) = (0xc2, 0xc9, 0xf1);

/// `--muster-selection` from the legacy dark theme: the content accent at a quarter.
///
/// It is translucent and it does not replace a cell's colours, because legacy
/// drew it into xterm.js' background layer and left the glyph in whatever
/// colour it arrived with. A selection is a band over what it covers, so the
/// text under one still reads and a block cursor over one still inverts
/// against the cell rather than against the band.
const SELECTION_COLOR: (u8, u8, u8, u8) = (0x74, 0xad, 0xe8, 0x3d);

/// The grid overlays its thumb at the same inset and minimum size as the
/// toolkit scrollbar, rather than reserving a text column for it.
const SCROLLBAR_WIDTH: Pixels = px(6.);
const SCROLLBAR_INSET: Pixels = px(4.);
const MIN_SCROLLBAR_THUMB: f32 = 48.;

/// Where this app's scrollbar thumb sits on a track of a known height.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ScrollbarGeometry {
    /// How far down the track the thumb starts.
    top: f32,
    /// How tall it is, never more than the track.
    height: f32,
    /// Whether there is anything to scroll, which is whether a thumb is worth drawing.
    scrollable: bool,
}

/// Legacy's arithmetic (`legacy/src/lib/overlay-scrollbar.ts`), over lines
/// rather than pixels: the scrollback is the content, the visible rows are the
/// viewport, and the offset is how far back the viewport has been scrolled.
///
/// Every number is clamped rather than trusted, because a scrollport is
/// measured by a grid whose content changes between one frame and the next.
fn scrollbar_geometry(history: usize, offset: usize, rows: usize, track: f32) -> ScrollbarGeometry {
    let viewport = rows.max(1);
    let content = history.saturating_add(viewport).max(viewport);
    let max_offset = content.saturating_sub(viewport);
    let track = track.max(0.);
    if track == 0. {
        return ScrollbarGeometry {
            top: 0.,
            height: 0.,
            scrollable: max_offset > 0,
        };
    }
    if max_offset == 0 {
        return ScrollbarGeometry {
            top: 0.,
            height: track,
            scrollable: false,
        };
    }
    // `offset` counts back from the newest line, so the viewport's position
    // measured from the top of the scrollback is the history it has left.
    let scroll_top = history.saturating_sub(offset).min(max_offset);
    let height = track.min(MIN_SCROLLBAR_THUMB.max(track * viewport as f32 / content as f32));
    ScrollbarGeometry {
        top: scroll_top as f32 / max_offset as f32 * (track - height),
        height,
        scrollable: true,
    }
}
/// Display columns occupied by an IME preedit at the terminal cursor.
///
/// Composition is Unicode text, not a sequence of VT cells yet. Use the same
/// Unicode display-width model as terminal rendering and keep the returned span
/// inside the grid, even when the cursor is at its final column.
fn composition_columns(columns: usize, cursor_column: usize, text: &str) -> Option<usize> {
    let remaining = columns.checked_sub(cursor_column)?;
    if remaining == 0 {
        return None;
    }
    Some(UnicodeWidthStr::width(text).max(1).min(remaining))
}

const MAX_ACCESSIBLE_TERMINAL_CELLS: usize = 65_536;

struct AccessibleTerminalText {
    text: String,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
}

/// A bounded text view of the visible grid, with VT-cell positions translated
/// into character offsets for AccessKit's text selection.
fn accessible_terminal_text(
    snapshot: &TerminalSnapshot,
    columns: usize,
    rows: usize,
) -> AccessibleTerminalText {
    if columns == 0 || rows == 0 {
        return AccessibleTerminalText {
            text: String::new(),
            cursor: None,
            selection: None,
        };
    }
    let columns = columns.min(MAX_ACCESSIBLE_TERMINAL_CELLS);
    let row_limit = rows.min(MAX_ACCESSIBLE_TERMINAL_CELLS / columns);
    let mut text = String::new();
    let mut positions = Vec::with_capacity(row_limit);
    let mut character_count = 0usize;

    for (row_index, row) in snapshot.rows.iter().take(row_limit).enumerate() {
        if character_count == MAX_ACCESSIBLE_TERMINAL_CELLS {
            break;
        }
        let mut row_positions = Vec::with_capacity(columns + 1);
        for column in 0..columns {
            row_positions.push(character_count);
            if let Some(cell) = row.cells.get(column) {
                if cell.width != 0 && character_count < MAX_ACCESSIBLE_TERMINAL_CELLS {
                    let character = if cell.c == '\0' { ' ' } else { cell.c };
                    text.push(character);
                    character_count += 1;
                    for combining in cell
                        .zerowidth
                        .iter()
                        .take(MAX_ACCESSIBLE_TERMINAL_CELLS - character_count)
                    {
                        text.push(*combining);
                        character_count += 1;
                    }
                }
            } else if character_count < MAX_ACCESSIBLE_TERMINAL_CELLS {
                text.push(' ');
                character_count += 1;
            }
        }
        row_positions.push(character_count);
        positions.push(row_positions);
        if row_index + 1 < row_limit
            && row_index + 1 < snapshot.rows.len()
            && character_count < MAX_ACCESSIBLE_TERMINAL_CELLS
        {
            text.push('\n');
            character_count += 1;
        }
    }

    let point_offset =
        |(row, column): (usize, usize)| positions.get(row).map(|row| row[column.min(columns)]);
    let cursor = point_offset((snapshot.cursor.row, snapshot.cursor.col));
    let selection = snapshot.selection_range.and_then(|(start, end)| {
        let start = point_offset(start)?;
        let end = point_offset((end.0, end.1.saturating_add(1)))?;
        Some((start.min(end), start.max(end)))
    });
    AccessibleTerminalText {
        text,
        cursor,
        selection,
    }
}

fn load_bundled_fonts(window: &mut Window) -> Result<bool, String> {
    // The app registers these faces before its first window so the sidebar is
    // already measured with them. A standalone terminal view (including a
    // headless test) still needs the same faces, but adding them twice to GPUI's
    // family is what produces duplicate PostScript-name warnings.
    if window
        .text_system()
        .all_font_names()
        .iter()
        .any(|name| name.eq_ignore_ascii_case("Fira Code"))
    {
        return Ok(true);
    }
    window
        .text_system()
        .add_fonts(vec![
            std::borrow::Cow::Borrowed(
                include_bytes!("../../assets/fonts/FiraCode-Regular.ttf").as_slice(),
            ),
            std::borrow::Cow::Borrowed(
                include_bytes!("../../assets/fonts/FiraCode-Bold.ttf").as_slice(),
            ),
            std::borrow::Cow::Borrowed(
                include_bytes!("../../assets/fonts/NerdSymbols.ttf").as_slice(),
            ),
        ])
        .map_err(|error| error.to_string())?;
    Ok(true)
}

/// The bundled family a private-use glyph draws with, under the name that
/// family's own `name` table declares.
///
/// Fira Code 6.2 carries no Nerd Font glyphs, so a terminal that draws a
/// prompt's powerline, an agent's icon or a file-type symbol with the text
/// faces alone answers it with a tofu box. This face is cut from the patched
/// font those two came from, so its glyphs keep this grid's advances — 1200 of
/// 1950 units, one cell — and land in the cell the parser gave them.
const ICON_FONT_FAMILY: &str = "FiraCode Nerd Font Mono";

/// The family a character draws with when it is not the text face's.
///
/// The private use ranges are the Nerd Font glyphs' own, and nothing else in
/// the app asks for them: the ten symbols those patched fonts also moved are
/// ordinary Unicode that a system face already answers, and Legacy's stack
/// drew them from the text face for the same reason.
fn icon_family_for(ch: char) -> Option<&'static str> {
    matches!(ch as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD).then_some(ICON_FONT_FAMILY)
}

/// The style one cell draws with: the terminal's, unless the cell holds a
/// private use character, which only the icon face answers.
///
/// A cell is at most one character wide unless it is a wide one, so the first
/// character is the one that decides: an emoji's variation selector and a
/// combining mark ride along with the base character they belong to, and the
/// base character is what has to come out of the right face.
fn cell_style(base: &TextStyle, text: &str) -> TextStyle {
    let mut style = base.clone();
    if let Some(family) = text.chars().next().and_then(icon_family_for) {
        style.font_family = family.into();
    }
    style
}

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

/// ANSI entries 0 to 15, transcribed from `--muster-ansi-*` in `legacy/src/muster.css`.
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
/// The defaults and ANSI colors for the terminal palette. Every one of them is
/// a `--muster-*` token out of the legacy dark theme in `legacy/src/muster.css`
///: legacy read that file to build xterm.js' theme, and this is the same table
/// rather than a second one that happens to agree — except the background,
/// which is the surface the window is already painted, not a token legacy chose
/// for the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// `--muster-content-text`: what a cell with no colour of its own is drawn in.
    pub foreground: (u8, u8, u8),
    /// `--muster-bg-0`: the same, behind the character.
    ///
    /// Legacy drew the grid on `--muster-content-bg-0`, `#222436`: a
    /// violet-tinted well from the content palette, right for the editor and the
    /// diff it shared it with. The grid here is the body of the window, not a
    /// document inside one, so it takes the surface the chrome around it wears.
    pub background: (u8, u8, u8),
    /// `--muster-cursor`, and `--muster-bg-0` as the block's glyph.
    pub cursor: (u8, u8, u8),
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            foreground: (0xdc, 0xe0, 0xe5),
            background: (0x16, 0x18, 0x1d),
            cursor: CURSOR_COLOR,
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
    rgba((r, g, b, 255))
}

/// A colour from its bytes, alpha included.
fn rgba((r, g, b, a): (u8, u8, u8, u8)) -> Hsla {
    Hsla::from(Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: f32::from(a) / 255.0,
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

/// The height of `bounds` this window can still show.
///
/// The canvas is laid out inside the window's own column, so a window that
/// gives it more height than it has room for -- the title bar above it, a bar
/// that appears later -- would otherwise have the grid measure rows that are
/// painted below the bottom edge. The window cuts those off, and the child was
/// already told about them: the line it writes last is a line nobody can read
/// and no scroll brings back.
///
/// Clamped rather than refused, because a window too small for a row is a
/// window that is small, not one that is broken, and a grid of no rows is what
/// a window with no room for one already draws.
fn shown_height(bounds: Bounds<Pixels>, window: &Window) -> Pixels {
    let below = window.viewport_size().height - bounds.origin.y;
    bounds.size.height.min(below).max(px(0.0))
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
    if wide
        || !first.zerowidth.is_empty()
        || (first.c == ' ' && !first.underline && !first.strikethrough)
    {
        return start + 1;
    }
    let mut end = start + 1;
    while let Some(next) = cells.get(end) {
        if next.width != 1
            || !next.zerowidth.is_empty()
            || next.fg != first.fg
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

fn mouse_button(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Navigate(_) => None,
    }
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
pub fn place_row(cells: &[CellSnapshot]) -> Vec<Placement> {
    let mut placements = Vec::with_capacity(cells.len());
    let mut column = 0;
    while column < cells.len() {
        let source = &cells[column];
        if source.width == 0 {
            column += 1;
            continue;
        }
        let glyph = source.c;
        let columns = usize::from(source.width).min(cells.len() - column);
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

/// A user input refusal, with an explanation but never the input payload.
pub struct InputRefused(pub String);

impl gpui_kit::EventEmitter<InputRefused> for TerminalView {}

/// One terminal view retaining its session, but never its workers.
pub struct TerminalView {
    session: Arc<TerminalSession>,
    _wake_task: Option<Task<()>>,
    repaint: Rc<RefCell<Repaint>>,
    blink_task: Option<Task<()>>,
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
    bold_font_loaded: bool,
    style: TextStyle,
    /// Whether a left-button text drag is in progress.
    dragging: bool,
    click_origin: Option<(usize, usize)>,
    last_click: Option<(std::time::Instant, (usize, usize), u8)>,
    click_count: u8,
    mouse_buttons: [bool; 3],
    mouse_cell: Option<(usize, usize, u8)>,
    gesture_screen: bool,
    gesture_tracking: bool,
    gesture_size: (u16, u16),
    scroll_remainder: f32,
    drag_point: (usize, usize),
    blink_visible: bool,
    blink_enabled: bool,
    blink_deadline: std::time::Instant,
    reported_focus: Option<(bool, bool)>,
    marked_text: String,
    marked_selection: std::ops::Range<usize>,
    modified_click: bool,
    left_mouse: Option<(f32, f32)>,
    dragged: bool,
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
    /// Record and report one refused input.
    ///
    /// Logged and reported together: the log is what outlives a banner the
    /// person dismissed, and the banner is what they can act on now.
    fn refuse(&mut self, what: &str, error: &super::SessionError, cx: &mut Context<Self>) {
        let message = format!("{what}: {error}");
        log::warn!("muster: {message}");
        self.report(message, cx);
        cx.notify();
    }

    /// Hand one refusal to whoever is listening, if anyone is.
    fn report(&self, message: String, cx: &mut Context<Self>) {
        cx.emit(InputRefused(message));
    }

    /// Raise one refusal the way an input path would.
    ///
    /// For the test that proves a refusal reaches the window. The refusal
    /// itself is already covered where it is raised; reproducing a full write
    /// queue through a real PTY would make the window wiring depend on how fast
    /// a child happens to read.
    #[cfg(test)]
    pub(crate) fn refuse_for_test(
        &mut self,
        what: &str,
        error: &super::SessionError,
        cx: &mut Context<Self>,
    ) {
        let message = format!("{what}: {error}");
        log::warn!("muster: {message}");
        self.report(message, cx);
    }
}

/// Do not consume another output wake until the requested frame is rendered.
/// Hidden views therefore leave a single coalesced token pending, not a loop.
#[derive(Default)]
struct Repaint {
    pending: bool,
    waker: Option<std::task::Waker>,
}

impl Repaint {
    fn rendered(&mut self) {
        self.pending = false;
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

impl TerminalView {
    /// Take the session's wake and start watching it.
    pub fn new(
        session: TerminalSession,
        metrics: Arc<Mutex<CellMetrics>>,
        cx: &mut Context<Self>,
    ) -> Self {
        // These headless tests use real PTY threads, not deterministic fake IO.
        #[cfg(test)]
        if cx.background_executor().is_test() {
            cx.background_executor().allow_parking();
        }
        let session = Arc::new(session);

        // Focus first, because every other input path in the window is
        // downstream of somebody having focused this element.
        let focus = cx.focus_handle();

        let repaint = Rc::new(RefCell::new(Repaint::default()));
        let wake_task = start_wake_task(&session, Rc::clone(&repaint), cx);

        Self {
            session,
            _wake_task: wake_task,
            repaint,
            blink_task: None,
            palette: Palette::default(),
            focus,
            observing: false,
            cell: None,
            bold_font_loaded: false,
            style: TextStyle {
                font_size: FONT_SIZE.into(),
                line_height: gpui_kit::DefiniteLength::Fraction(LINE_HEIGHT),
                ..TextStyle::default()
            },
            applied: Rc::new(Cell::new((0, 0))),
            metrics,
            dragging: false,
            click_origin: None,
            last_click: None,
            click_count: 1,
            mouse_buttons: [false; 3],
            mouse_cell: None,
            gesture_screen: false,
            gesture_tracking: false,
            gesture_size: (0, 0),
            scroll_remainder: 0.0,
            drag_point: (0, 0),
            blink_visible: true,
            blink_enabled: false,
            blink_deadline: std::time::Instant::now() + Duration::from_millis(500),
            reported_focus: None,
            marked_text: String::new(),
            marked_selection: 0..0,
            modified_click: false,
            left_mouse: None,
            dragged: false,
            canvas_origin: Rc::new(Cell::new(point(px(0.0), px(0.0)))),
        }
    }

    /// The session this draws. What the window closes through.
    pub fn session(&self) -> &Arc<TerminalSession> {
        &self.session
    }

    /// The size the PTY has last been told about, as the status line shows it.
    pub fn applied_size(&self) -> (u16, u16) {
        self.applied.get()
    }

    /// Where the bottom of the last row this view draws falls in the window.
    ///
    /// `None` until a frame has measured a cell. This is the number that says
    /// whether the grid is one the window can show: the rows are one per whole
    /// cell of the box the view was given, so a grid that reaches past the
    /// bottom edge is a shell writing a line nobody can read.
    #[cfg(test)]
    pub(crate) fn drawn_bottom(&self) -> Option<Pixels> {
        let cell = self.cell?;
        let (_, rows) = self.applied.get();
        Some(self.canvas_origin.get().y + cell.height * f32::from(rows))
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

        self.bold_font_loaded = match load_bundled_fonts(window) {
            Ok(loaded) => loaded,
            Err(error) => {
                log::error!("muster: bundled terminal fonts could not load: {error}");
                false
            }
        };

        let installed = window.text_system().all_font_names();
        let family = if self.bold_font_loaded
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
            log::error!("muster: bundled Fira Code did not resolve; using {family}");
        }
        // The icon face fails the other way round: a family the loader drops
        // answers with a fallback rather than with an error, so a private use
        // glyph would go back to being a tofu box with nothing said about it.
        let icons = window
            .text_system()
            .resolve_font(&gpui_kit::font(ICON_FONT_FAMILY));
        if window
            .text_system()
            .get_font_for_id(icons)
            .is_none_or(|font| font.family != ICON_FONT_FAMILY)
        {
            log::error!(
                "muster: bundled {ICON_FONT_FAMILY} did not resolve; \
                 private use glyphs are drawn by another face"
            );
        }
        self.style.font_family = SharedString::from(family);
        self.style.font_weight = FontWeight::NORMAL;
        self.style.font_features = gpui_kit::FontFeatures(std::sync::Arc::new(vec![
            ("liga".to_owned(), 1),
            ("calt".to_owned(), 1),
        ]));

        let font_size = self.style.font_size.to_pixels(window.rem_size());
        match CellSize::measure(window.text_system(), &self.style, font_size) {
            Some(cell) => {
                *self.metrics.lock().expect("measured cell poisoned") = CellMetrics {
                    width: cell_px(cell.width),
                    height: cell_px(cell.height),
                };
                self.cell = Some(cell);
            }
            None => log::warn!(
                "muster: the terminal font produced no metrics for {family}; \
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
            self.paste_clipboard(window, cx);
            cx.stop_propagation();
            return;
        }

        // Only one source of printable text exists here, which is what keeps a
        // press from being sent twice. See `input` for why it is this one.
        let scroll_shortcut = !modifiers.alt
            && !modifiers.control
            && if cfg!(target_os = "macos") {
                modifiers.platform
            } else {
                modifiers.shift
            };
        if scroll_shortcut && matches!(key.as_str(), "pageup" | "pagedown" | "home" | "end") {
            let snapshot = self.session.painting_snapshot();
            match key.as_str() {
                "pageup" => self.session.scroll_by(snapshot.rows.len() as i32),
                "pagedown" => self.session.scroll_by(-(snapshot.rows.len() as i32)),
                "home" => self.session.set_display_offset(snapshot.history_size),
                "end" => self.session.set_display_offset(0),
                _ => {
                    return;
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        self.reset_blink();
        cx.notify();
        let press = match input::press(&event.keystroke) {
            input::Input::Press(press) => press,
            input::Input::Text(text) => {
                // A Mac layout's symbol, ⌥⇧H being ◊: text, and the app
                // sends it as text. The platform's text input would only see
                // the key, and the meta prefix would turn the symbol into a
                // command the shell never asked for.
                cx.stop_propagation();
                if let Err(error) = self.session.send_text(&text) {
                    self.refuse("The text was not sent", &error, cx);
                }
                return;
            }
            input::Input::Application => return,
        };
        // Text commits belong exclusively to the native input handler. Let
        // plain printable keys and IME composition reach the platform first.
        if !press.ctrl
            && !press.alt
            && !press.meta
            && (matches!(press.key, super::Key::Char(_)) || !self.marked_text.is_empty())
        {
            return;
        }
        if key == "tab" {
            // The toolkit's own root binds Tab to focus traversal and answers
            // it before this handler runs, so this is the path Tab takes when
            // no other control answered first; [`Self::on_action_tab`] is the
            // one that normally does.
            cx.stop_propagation();
        }
        if !modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.platform {
            self.session.clear_selection();
            cx.stop_propagation();
            cx.notify();
        }
        cx.stop_propagation();
        if let Err(error) = self.session.send_key(&press) {
            self.refuse("A keystroke was not sent", &error, cx);
        }
    }

    /// Tab, answered before the window root answers it for itself.
    ///
    /// The same press `on_key_down` would have encoded, reached through a
    /// binding rather than a key event, because the root's focus traversal
    /// takes Tab first. Plain Tab completes; the shifted one asks the shell
    /// for the history instead, which is the other of the two Tab presses a
    /// shell reads differently.
    fn on_action_tab(&mut self, _: &TerminalTab, window: &mut Window, cx: &mut Context<Self>) {
        self.send_tab(false, window, cx);
    }

    fn on_action_shift_tab(
        &mut self,
        _: &TerminalShiftTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.send_tab(true, window, cx);
    }

    fn send_tab(&mut self, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_state(window).accepts_input() {
            return;
        }
        self.reset_blink();
        cx.notify();
        if let Err(error) = self.session.send_key(&super::KeyPress {
            key: super::Key::Tab,
            shift,
            ctrl: false,
            alt: false,
            meta: false,
            text: None,
        }) {
            self.refuse("A keystroke was not sent", &error, cx);
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
            .selection_text()
            .filter(|text| !text.is_empty())
        {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
        }
    }

    /// Read system clipboard text and queue a terminal-aware paste.
    fn paste_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste_text(text, window, cx);
        }
    }

    /// `⌘C` as the menu bar's Copy item sends it.
    ///
    /// A menu item with a key equivalent is answered by AppKit before the key
    /// reaches the window, so on macOS this is the path a copy takes and
    /// `on_key_down` is the one that serves the targets with no menu bar. Both
    /// call the same thing, which is what keeps them one answer rather than two.
    fn on_action_copy(&mut self, _: &EditCopy, _window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(cx);
    }

    /// `⌘V` as the menu bar's Paste item sends it. See [`Self::on_action_copy`].
    fn on_action_paste(&mut self, _: &EditPaste, window: &mut Window, cx: &mut Context<Self>) {
        self.paste_clipboard(window, cx);
    }

    fn paste_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        match self.session.paste(&text) {
            Err(super::SessionError::Paste(super::PasteError::Unsafe)) => {
                let prompt = window.prompt(gpui_kit::PromptLevel::Warning,
                        "Paste multiple lines?", Some("This program has not enabled bracketed paste. Pasting may execute commands."),
                        &["Cancel", "Paste"], cx);
                cx.spawn_in(window, async move |view, cx| {
                    if prompt.await == Ok(1) {
                        let _ = view.update(cx, |view, cx| {
                            if let Err(error) = view.session.paste_confirmed(&text) {
                                view.refuse("The paste was refused", &error, cx);
                            }
                            view.reset_blink();
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            Err(error) => {
                self.refuse("The clipboard paste was refused", &error, cx);
            }
            Ok(()) => {
                self.reset_blink();
                cx.notify();
            }
        }
    }

    fn reset_blink(&mut self) {
        self.blink_visible = true;
        self.blink_deadline = std::time::Instant::now() + Duration::from_millis(500);
    }

    fn point_at(&self, position: gpui_kit::Point<Pixels>) -> (usize, usize) {
        grid_point(
            position,
            self.canvas_origin.get(),
            self.cell.unwrap_or(CellSize {
                width: px(1.0),
                height: px(1.0),
            }),
            self.applied.get(),
        )
    }

    fn report_mouse(
        &self,
        position: gpui_kit::Point<Pixels>,
        button: u8,
        release: bool,
        motion: bool,
        modifiers: gpui_kit::Modifiers,
    ) {
        let (row, col) = self.point_at(position);
        if let Err(error) = self.session.send_mouse(super::MouseReport {
            button,
            row,
            col,
            release,
            motion,
            shift: modifiers.shift,
            alt: modifiers.alt,
            ctrl: modifiers.control,
        }) {
            log::warn!("muster: mouse input was refused: {error}");
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &gpui_kit::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        let (cols, rows) = self.applied.get();
        if cols == 0 || rows == 0 {
            return;
        }
        let (alt_screen, mouse_tracking, _) = self.session.mouse_state();
        if self.gesture_screen != alt_screen || self.gesture_tracking != mouse_tracking {
            self.last_click = None;
        }
        self.gesture_screen = alt_screen;
        self.gesture_tracking = mouse_tracking;
        self.gesture_size = self.applied.get();
        if mouse_tracking && !event.modifiers.shift {
            if let Some(button) = mouse_button(event.button) {
                self.mouse_buttons[button as usize] = true;
                self.report_mouse(event.position, button, false, false, event.modifiers);
            }
        } else if event.button == MouseButton::Left {
            let point = self.point_at(event.position);
            let now = std::time::Instant::now();
            self.click_count = self
                .last_click
                .filter(|(time, previous, _)| {
                    *previous == point && now.duration_since(*time) < Duration::from_millis(500)
                })
                .map_or(1, |(_, _, count)| count % 3 + 1);
            self.last_click = Some((now, point, self.click_count));
            self.left_mouse = Some((event.position.x.as_f32(), event.position.y.as_f32()));
            self.click_origin = Some(point);
            self.drag_point = point;
            self.dragged = false;
            self.dragging = false;
            self.modified_click = event.modifiers.shift
                || event.modifiers.control
                || event.modifiers.alt
                || event.modifiers.platform;
            self.session.start_selection(
                point,
                match self.click_count {
                    2 => super::SelectionKind::Word,
                    3 => super::SelectionKind::Line,
                    _ => super::SelectionKind::Simple,
                },
            );
        }
        self.reset_blink();
        cx.notify();
        cx.stop_propagation();
    }

    fn on_mouse_move(
        &mut self,
        event: &gpui_kit::MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(origin) = self
            .click_origin
            .filter(|_| event.pressed_button == Some(MouseButton::Left))
        {
            let point = self.point_at(event.position);
            self.dragged |= self.left_mouse.is_some_and(|(x, y)| {
                (event.position.x.as_f32() - x).hypot(event.position.y.as_f32() - y) > 3.0
            });
            self.dragging |= point != origin;
            self.drag_point = point;
            if self.dragging || self.click_count > 1 || self.dragged {
                self.session.update_selection(point);
            }
            cx.notify();
            cx.stop_propagation();
        } else if !event.modifiers.shift {
            let button = event.pressed_button.and_then(mouse_button).unwrap_or(3);
            let point = self.point_at(event.position);
            if self.mouse_cell != Some((point.0, point.1, button)) {
                self.mouse_cell = Some((point.0, point.1, button));
                self.report_mouse(event.position, button, false, true, event.modifiers);
            }
        }
    }

    fn on_mouse_up(
        &mut self,
        event: &gpui_kit::MouseUpEvent,
        outside: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(button) = mouse_button(event.button) {
            if std::mem::take(&mut self.mouse_buttons[button as usize]) {
                self.report_mouse(event.position, button, true, false, event.modifiers);
                cx.stop_propagation();
            }
        }
        if event.button != MouseButton::Left {
            return;
        }
        let (alt_screen, mouse_tracking, _) = self.session.mouse_state();
        if alt_screen != self.gesture_screen || mouse_tracking != self.gesture_tracking {
            self.click_origin = None;
        }
        if let Some(point) = self.click_origin.take() {
            if self.dragged || self.dragging || self.click_count > 1 {
                self.copy_selection(cx);
            } else {
                self.session.clear_selection();
                if !outside && !self.modified_click {
                    if let Err(error) = self.session.move_caret_to(point.0, point.1) {
                        self.refuse("Prompt movement was refused", &error, cx);
                    }
                }
            }
            cx.stop_propagation();
        }
        self.left_mouse = None;
        self.dragged = false;
        self.dragging = false;
        self.modified_click = false;
        self.reset_blink();
        cx.notify();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &gpui_kit::ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cell) = self.cell else {
            return;
        };
        self.scroll_remainder += match event.delta {
            gpui_kit::ScrollDelta::Pixels(delta) => delta.y / cell.height,
            gpui_kit::ScrollDelta::Lines(delta) => delta.y,
        };
        let lines = self.scroll_remainder.trunc().clamp(-100.0, 100.0) as i32;
        self.scroll_remainder -= lines as f32;
        let (alt_screen, mouse_tracking, alternate_scroll) = self.session.mouse_state();
        if mouse_tracking && !event.modifiers.shift {
            for _ in 0..lines.unsigned_abs() {
                self.report_mouse(
                    event.position,
                    if lines > 0 { 64 } else { 65 },
                    false,
                    false,
                    event.modifiers,
                );
            }
        } else if alt_screen && alternate_scroll && !event.modifiers.shift {
            let key = if lines > 0 {
                super::Key::Up
            } else {
                super::Key::Down
            };
            for _ in 0..lines.unsigned_abs() {
                if let Err(error) = self.session.send_key(&super::KeyPress {
                    key,
                    shift: false,
                    ctrl: false,
                    alt: false,
                    meta: false,
                    text: None,
                }) {
                    self.refuse("Scroll input was refused", &error, cx);
                    break;
                }
            }
        } else {
            self.session.scroll_by(lines);
            if self.click_origin.is_some() {
                self.session.update_selection(self.drag_point);
            }
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn frame(&self) -> Frame {
        Frame {
            palette: self.palette,
            style: self.style.clone(),
            bold_font_loaded: self.bold_font_loaded,
        }
    }
}

/// The per-frame drawing state, held by value.
#[derive(Debug, Clone)]
struct Frame {
    palette: Palette,
    style: TextStyle,
    bold_font_loaded: bool,
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
    composition: &'a str,
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
            composition,
        } = input;

        // A window too small for one whole cell draws its background and
        // nothing else. A cell cannot be drawn at a fraction of its own size,
        // and drawing half of one is how a terminal ends up with a column of
        // clipped glyphs at the edge of a resized window.
        if columns == 0 || rows == 0 {
            return;
        }

        window.paint_quad(quad(
            bounds,
            px(0.0),
            snapshot
                .background
                .map_or_else(|| self.palette.background(ColorSnapshot::Default), opaque),
            px(0.0),
            gpui_kit::transparent_black(),
            Default::default(),
        ));
        let cursor = cursor_cell(snapshot, (columns, rows));

        for (row_index, row) in snapshot.rows.iter().take(rows as usize).enumerate() {
            let y = bounds.origin.y + row_index as f32 * cell.height;
            let mut placements = place_row(&row.cells).into_iter().peekable();
            while let Some(placement) = placements.next() {
                let Some(source) = row.cells.get(placement.column) else {
                    break;
                };
                let x = bounds.origin.x + placement.column as f32 * cell.width;
                let span = Bounds {
                    origin: point(x, y),
                    size: gpui_kit::size(placement.columns as f32 * cell.width, cell.height),
                };
                let at_cursor = cursor.is_some_and(|(r, c)| {
                    r == row_index
                        && (placement.column..placement.column + placement.columns).contains(&c)
                });
                let selected = selection_range.is_some_and(|(start, end)| {
                    let point = (row_index, placement.column);
                    start <= point && point <= end
                });

                let mut run = source.c.to_string();
                run.extend(source.zerowidth.iter());
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
                    paint_cursor(
                        span,
                        snapshot.cursor.shape,
                        focus,
                        snapshot.cursor.color.unwrap_or(self.palette.cursor),
                        window,
                    );
                }
            }
        }
        self.paint_scrollbar(bounds, snapshot, rows, window, cx);
        if !composition.is_empty() {
            if let Some((row, col)) = cursor {
                if let (Some(source), Some(width_columns)) = (
                    snapshot.rows.get(row).and_then(|r| r.cells.get(col)),
                    composition_columns(columns as usize, col, composition),
                ) {
                    let mut source = source.clone();
                    source.underline = true;
                    source.inverse = false;
                    let span = Bounds {
                        origin: point(
                            bounds.origin.x + cell.width * col as f32,
                            bounds.origin.y + cell.height * row as f32,
                        ),
                        size: gpui_kit::size(cell.width * width_columns as f32, cell.height),
                    };
                    window.with_content_mask(
                        Some(gpui_kit::ContentMask { bounds: span }),
                        |window| {
                            window.paint_quad(quad(
                                span,
                                px(0.0),
                                self.palette.background(source.bg),
                                px(0.0),
                                gpui_kit::transparent_black(),
                                Default::default(),
                            ));
                            self.paint_text_run(
                                TextRun {
                                    bounds: span,
                                    cell: &source,
                                    text: composition,
                                    at_cursor: false,
                                    selected: false,
                                },
                                snapshot,
                                focus,
                                window,
                                cx,
                            );
                        },
                    );
                }
            }
        }
    }

    /// The scrollback thumb, drawn over the right edge of the grid.
    fn paint_scrollbar(
        &self,
        bounds: Bounds<Pixels>,
        snapshot: &TerminalSnapshot,
        rows: u16,
        window: &mut Window,
        cx: &App,
    ) {
        let geometry = scrollbar_geometry(
            snapshot.history_size,
            snapshot.display_offset,
            rows as usize,
            bounds.size.height.as_f32(),
        );
        if !geometry.scrollable || geometry.height <= 0. {
            return;
        }
        let height = px(geometry.height.min(bounds.size.height.as_f32()));
        window.paint_quad(quad(
            Bounds {
                origin: point(
                    bounds.origin.x + bounds.size.width - SCROLLBAR_INSET - SCROLLBAR_WIDTH,
                    bounds.origin.y + px(geometry.top),
                ),
                size: gpui_kit::size(SCROLLBAR_WIDTH, height),
            },
            px(0.0),
            gpui_kit::component::Theme::global(cx).scrollbar_thumb,
            px(0.0),
            gpui_kit::transparent_black(),
            Default::default(),
        ));
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
        if cell.dim {
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
        // The band goes over the cell and under the cursor block and the glyph,
        // in that order, which is the order legacy painted them in.
        if selected {
            window.paint_quad(quad(
                bounds,
                px(0.0),
                rgba(SELECTION_COLOR),
                px(0.0),
                gpui_kit::transparent_black(),
                Default::default(),
            ));
        }
        let cursor_color = snapshot
            .cursor
            .color
            .or_else(|| (cell.c == ' ' && cell.zerowidth.is_empty()).then_some(self.palette.cursor))
            .map(opaque);
        let cursor_colors = block_cursor_colors(
            foreground,
            background,
            at_cursor
                && snapshot.cursor.visible
                && snapshot.cursor.shape == CursorShapeSnapshot::Block,
            focus,
            cursor_color,
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
        // The cell's own face: an icon and the text beside it are two runs,
        // because a font is chosen per run and the icon face answers nothing
        // else. The cell is still the cell — the glyph is painted inside the
        // bounds the parser measured it into either way.
        let mut style = cell_style(&self.style, text);
        style.color = cursor_colors.foreground;
        style.font_weight = if cell.bold && self.bold_font_loaded {
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
            log::warn!("muster: a text run was not painted: {error}");
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
    color: Option<Hsla>,
) -> CellColors {
    if at_cursor && focus.accepts_input() {
        CellColors {
            foreground: background,
            background: color.unwrap_or(foreground),
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
    color: (u8, u8, u8),
    window: &mut Window,
) {
    let foreground = opaque(color);

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

/// Await output and at most one outstanding repaint, without retaining the session.
///
/// `None` when the wake has already been taken: there is then nothing left to
/// coalesce, and a loop watching a token nobody sends is a thread that never
/// ends.
fn start_wake_task(
    session: &TerminalSession,
    repaint: Rc<RefCell<Repaint>>,
    cx: &mut Context<TerminalView>,
) -> Option<Task<()>> {
    let wake = session.take_wake()?;

    // An async closure rather than a closure returning an async block: the
    // second one borrows the context that is handed to it, and a future cannot
    // borrow from itself.
    Some(cx.spawn(async move |view, cx| {
        // The channel holds one token, so taking it empties it and the whole
        // burst behind it collapses into one repaint. That is the coalescing: a
        // `yes` in the shell is thousands of 64 KiB reads and this is one frame
        // for all of them.
        while wake.recv().await {
            if view
                .update(cx, |_, cx| {
                    repaint.borrow_mut().pending = true;
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
            std::future::poll_fn(|cx| {
                let mut repaint = repaint.borrow_mut();
                if repaint.pending {
                    repaint.waker = Some(cx.waker().clone());
                    std::task::Poll::Pending
                } else {
                    std::task::Poll::Ready(())
                }
            })
            .await;
        }
    }))
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.repaint.borrow_mut().rendered();
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
        let mut snapshot = self.session.painting_snapshot();
        if self.gesture_screen != snapshot.alt_screen
            || self.gesture_tracking != snapshot.mouse_tracking
            || self.gesture_size != self.applied.get()
        {
            self.click_origin = None;
            self.left_mouse = None;
            self.mouse_buttons = [false; 3];
            self.mouse_cell = None;
            self.last_click = None;
            self.gesture_screen = snapshot.alt_screen;
            self.gesture_tracking = snapshot.mouse_tracking;
            self.gesture_size = self.applied.get();
        }
        let focus_state = (snapshot.modes.focus_reporting, focus.accepts_input());
        if self.reported_focus != Some(focus_state) {
            if let Err(error) = self.session.send_focus(focus_state.1) {
                log::warn!("muster: focus report was refused: {error}");
            }
            self.reported_focus = Some(focus_state);
        }
        let blink_enabled = snapshot.cursor.blinking
            && snapshot.cursor.visible
            && focus.accepts_input()
            && self.marked_text.is_empty();
        if self.blink_enabled != blink_enabled {
            self.reset_blink();
        }
        self.blink_enabled = blink_enabled;
        // A single deadline per rendered frame. Losing focus or unmounting the
        // view cannot leave a detached, repeating blink loop behind.
        self.blink_task = if blink_enabled {
            let deadline = self.blink_deadline;
            let timer = cx
                .background_executor()
                .timer(deadline.saturating_duration_since(std::time::Instant::now()));
            Some(cx.spawn_in(window, async move |view, cx| {
                timer.await;
                let _ = view.update_in(cx, |view, window, cx| {
                    if view.blink_enabled
                        && view.focus_state(window).accepts_input()
                        && window.is_visible()
                        && view.marked_text.is_empty()
                        && view.blink_deadline == deadline
                    {
                        view.blink_visible = !view.blink_visible;
                        view.blink_deadline =
                            std::time::Instant::now() + Duration::from_millis(500);
                        cx.notify();
                    }
                });
            }))
        } else {
            None
        };
        if blink_enabled && !self.blink_visible {
            snapshot.cursor.visible = false;
        }
        let frame = self.frame();
        let canvas_origin = Rc::clone(&self.canvas_origin);
        let applied = Rc::clone(&self.applied);
        let cell = self.cell;
        let input_view = cx.entity();
        let input_focus = self.focus.clone();
        let composition = self.marked_text.clone();
        let (columns, rows) = self.applied.get();
        let accessible = accessible_terminal_text(&snapshot, columns as usize, rows as usize);
        let terminal = div().id("muster-terminal");
        #[cfg(test)]
        let terminal = terminal.test_support();

        terminal
            .role(gpui_kit::Role::MultilineTextInput)
            .aria_label("Terminal")
            .aria_description("The visible terminal screen, including output, caret, and selection. From app chrome, press the application modifier plus Shift+Escape to focus the terminal.")
            .aria_keyshortcuts(if cfg!(target_os = "macos") {
                "Meta+Shift+Escape"
            } else {
                "Control+Shift+Escape"
            })
            .a11y_synthetic_children(move |builder| {
                let text_id = builder.synthetic_node_id("terminal-visible-text");
                let mut text_node = gpui_kit::accesskit::Node::new(gpui_kit::Role::TextRun);
                text_node.set_value(accessible.text.clone());
                text_node.set_character_lengths(
                    accessible
                        .text
                        .chars()
                        .map(|character| character.len_utf8() as u8)
                        .collect::<Vec<_>>(),
                );
                if builder.push_child(text_id, text_node) {
                    if let Some((start, end)) = accessible.selection {
                        builder.parent_node().set_text_selection(
                            gpui_kit::accesskit::TextSelection {
                                anchor: gpui_kit::accesskit::TextPosition {
                                    node: text_id,
                                    character_index: start,
                                },
                                focus: gpui_kit::accesskit::TextPosition {
                                    node: text_id,
                                    character_index: end,
                                },
                            },
                        );
                    } else if let Some(cursor) = accessible.cursor {
                        let caret = gpui_kit::accesskit::TextPosition {
                            node: text_id,
                            character_index: cursor,
                        };
                        builder.parent_node().set_text_selection(
                            gpui_kit::accesskit::TextSelection {
                                anchor: caret,
                                focus: caret,
                            },
                        );
                    }
                }
            })
            .size_full()
            .key_context(super::TAB_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_action_tab))
            .on_action(cx.listener(Self::on_action_shift_tab))
            .on_action(cx.listener(Self::on_action_copy))
            .on_action(cx.listener(Self::on_action_paste))
            // A click anywhere in the grid is a click on the terminal. There is
            // nothing inside it to click, so without this the window would take
            // focus only when the pointer happened to land on the chrome.
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event, _, cx| this.on_mouse_up(event, false, cx)),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, event, _, cx| this.on_mouse_up(event, false, cx)),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, event, _, cx| this.on_mouse_up(event, false, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event, _, cx| this.on_mouse_up(event, true, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Middle,
                cx.listener(|this, event, _, cx| this.on_mouse_up(event, true, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Right,
                cx.listener(|this, event, _, cx| this.on_mouse_up(event, true, cx)),
            )
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                canvas(
                    {
                        let applied = Rc::clone(&applied);
                        let view = Arc::clone(&self.session);
                        move |bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App| {
                            canvas_origin.set(bounds.origin);
                            let cell = cell?;
                            let size = (
                                cell.columns_in(bounds.size.width),
                                cell.rows_in(shown_height(bounds, window)),
                            );
                            // A window measuring nothing is invisible or minimised.
                            // `resize` refuses zero already; asking only when the
                            // answer changed is what keeps a hidden window from
                            // asking sixty times a second.
                            if size.0 > 0 && size.1 > 0 && applied.get() != size {
                                applied.set(size);
                                if let Err(error) = view.resize(size.0, size.1) {
                                    log::warn!("muster: the grid was not resized: {error}");
                                }
                            }
                            Some((cell, size))
                        }
                    },
                    move |bounds, state, window, cx| {
                        window.handle_input(
                            &input_focus,
                            gpui_kit::ElementInputHandler::new(bounds, input_view),
                            cx,
                        );
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
                                composition: &composition,
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

// The IME owns only its uncommitted buffer. The shell remains the owner of
// committed text; no native range edit guesses at Readline/ZLE's edit buffer.
impl gpui_kit::EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let utf16: Vec<_> = self.marked_text.encode_utf16().collect();
        let text = String::from_utf16(utf16.get(range.clone())?).ok()?;
        *adjusted = Some(range);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<gpui_kit::UTF16Selection> {
        self.focus_state(window)
            .accepts_input()
            .then(|| gpui_kit::UTF16Selection {
                range: self.marked_selection.clone(),
                reversed: false,
            })
    }

    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        (!self.marked_text.is_empty()).then(|| 0..self.marked_text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text.clear();
        self.marked_selection = 0..0;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<std::ops::Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus_state(window).accepts_input() || self.session.is_closing() {
            return;
        }
        let Some(committed) = replace_marked(&self.marked_text, range, text) else {
            return;
        };
        if let Err(error) = self.session.send_text(&committed) {
            self.refuse("The text was refused", &error, cx);
            return;
        }
        self.marked_text.clear();
        self.marked_selection = 0..0;
        self.session.clear_selection();
        self.reset_blink();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<std::ops::Range<usize>>,
        text: &str,
        selected: Option<std::ops::Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus_state(window).accepts_input() || self.session.is_closing() {
            return;
        }
        let Some(marked) = replace_marked(&self.marked_text, range, text) else {
            return;
        };
        let len = marked.encode_utf16().count();
        self.marked_selection = selected
            .filter(|r| r.start <= r.end && r.end <= len)
            .unwrap_or(len..len);
        self.marked_text = marked;
        self.reset_blink();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cell = self.cell?;
        let cursor = self.session.cursor_snapshot();
        Some(Bounds {
            origin: point(
                self.canvas_origin.get().x + cell.width * cursor.col as f32,
                self.canvas_origin.get().y + cell.height * cursor.row as f32,
            ),
            size: gpui_kit::size(cell.width, cell.height),
        })
    }

    fn character_index_for_point(
        &mut self,
        _: gpui_kit::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, window: &mut Window, _: &mut Context<Self>) -> bool {
        self.focus_state(window).accepts_input() && !self.session.is_closing()
    }

    fn paste(
        &mut self,
        item: gpui_kit::ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = item.text() {
            self.paste_text(text, window, cx);
        }
    }
}

fn replace_marked(old: &str, range: Option<std::ops::Range<usize>>, text: &str) -> Option<String> {
    let mut utf16: Vec<_> = old.encode_utf16().collect();
    let range = range.unwrap_or(0..utf16.len());
    // Refuse invalid offsets, including positions inside a surrogate pair.
    String::from_utf16(utf16.get(..range.start)?).ok()?;
    String::from_utf16(utf16.get(range.clone())?).ok()?;
    utf16.splice(range, text.encode_utf16());
    String::from_utf16(&utf16).ok()
}

impl gpui_kit::Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::MAX_PASTE_BYTES;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{EntityInputHandler, InputEvent};

    fn cell(c: char) -> CellSnapshot {
        CellSnapshot {
            c,
            zerowidth: Vec::new(),
            width: if cjk_is_wide(c) { 2 } else { 1 },
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
    /// test fixture supplies parser widths independently of font measurements.
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

    /// What the icon face has to answer, and what it has to be called.
    ///
    /// A file that parses is a table of contents: the terminal asks the text
    /// system for a *family* by name, and the glyph it wants by codepoint, and
    /// the face is only the icon face if it declares the one name and maps the
    /// others. That is also what makes the grafted `m` worth its oddity: it is
    /// the glyph Core Text's loader asks every family for before keeping it, so
    /// a face without one is dropped, and a dropped face answers the request
    /// with somebody else's.
    #[test]
    fn the_bundled_icon_face_is_the_family_the_terminal_asks_for() {
        let face = ttf_parser::Face::parse(
            include_bytes!("../../assets/fonts/NerdSymbols.ttf").as_slice(),
            0,
        )
        .expect("the bundled icon face parses: Core Graphics cannot read WOFF2");
        // The name table holds a record per platform, and only the Unicode
        // ones decode; the first record is not necessarily one of them.
        let declared = |id: u16| {
            face.names()
                .into_iter()
                .filter(|name| name.name_id == id)
                .find_map(|name| name.to_string())
        };
        assert_eq!(
            declared(ttf_parser::name_id::FAMILY).as_deref(),
            Some(ICON_FONT_FAMILY),
            "the terminal asks for a family the face does not declare"
        );
        // A typographic family is the one a text stack asks for when it is
        // there, so a second name is a second answer to this question.
        assert_eq!(
            declared(ttf_parser::name_id::TYPOGRAPHIC_FAMILY),
            None,
            "a typographic family would answer for the one above"
        );
        for icon in ['\u{e0b0}', '\u{ea60}', '\u{f15b}', '\u{f0001}'] {
            assert!(
                face.glyph_index(icon).is_some(),
                "the icon face has no glyph for {icon:?}, which the terminal routes to it"
            );
        }
        assert!(
            face.glyph_index('m').is_some(),
            "the glyph Core Text's loader asks a family for before keeping it"
        );
    }

    /// Which face a character is drawn with, before anything is drawn.
    ///
    /// The routing is the whole of the fix, so it is stated as what it is: a
    /// private use character goes to the icon face and everything else stays on
    /// the text face, including the ten symbols the patched fonts also moved,
    /// which a system face already answers.
    #[test]
    fn a_private_use_character_is_the_only_one_that_changes_face() {
        for icon in ['\u{e0b0}', '\u{f15b}', '\u{f0001}', '\u{ffffd}'] {
            assert_eq!(icon_family_for(icon), Some(ICON_FONT_FAMILY), "{icon:?}");
        }
        for text in ['a', 'Z', '0', '\u{26a1}', '\u{2764}', '\u{4e2d}', ' '] {
            assert_eq!(icon_family_for(text), None, "{text:?}");
        }
        // Either side of each range stays ordinary text: a boundary that
        // answered the icon face would take a character nobody asked about,
        // and the face has no glyph for it besides.
        for text in ['\u{d7ff}', '\u{f900}', '\u{effff}', '\u{10fffd}'] {
            assert_eq!(icon_family_for(text), None, "{text:?}");
        }
    }

    /// Tab reaches the child even though the window root wants it.
    ///
    /// The toolkit answers a key binding *before* the key reaches any
    /// element, and the window root binds `tab` to focus traversal and stops
    /// it there. `open_window` puts every window's app view under that root,
    /// so a terminal that waits for its own key handler is a shell with no
    /// completion at all. The two bindings here are the real ones: the root's
    /// on the outside, the terminal's on the grid, and the deeper context is
    /// the one that is answered first.
    #[gpui_kit::test]
    fn tab_reaches_the_child_even_though_the_window_root_binds_it(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.update(crate::terminal::install_keys);
        cx.update(|cx| {
            cx.bind_keys([gpui_kit::KeyBinding::new(
                "tab",
                RootFocusNext,
                Some("Root"),
            )])
        });
        // `cat` writes back what it is sent, so what lands in the grid is the
        // child's answer rather than a copy of what the test wrote. Raw mode
        // because `cat -v` is what turns the escape a shifted Tab sends into
        // something the grid can be asked about.
        let session = script_session("stty raw -echo; printf READY; cat -v");
        let (view, cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        let session = view.read_with(cx, |v, _| Arc::clone(v.session()));
        let (root, cx) = cx.add_window_view(|_, _| WindowRoot { view });
        let view = root.read_with(cx, |root, _| root.view.clone());
        focus_grid(cx, &view);
        cx.update(|window, _| {
            let terminal = window.find("muster-terminal");
            assert_eq!(terminal.role(), Some(gpui_kit::Role::MultilineTextInput));
            assert_eq!(terminal.label(), Some("Terminal"));
            assert_eq!(terminal.focused(), Some(true));
        });
        assert!(
            wait_for_char(&session, 'R'),
            "the child never drew its marker"
        );

        press_key(cx, "tab", Default::default());
        assert!(
            wait_for_char(&session, '\t'),
            "Tab never reached the child: the window root answered it instead"
        );
        // The other Tab a shell reads: the history instead of the completion.
        press_key(
            cx,
            "tab",
            gpui_kit::Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        assert!(
            wait_for_char(&session, '^'),
            "Shift+Tab never reached the child: {}",
            grid_text(&session)
        );
        let _ = session.close().wait();
    }

    /// A Mac layout's symbol reaches the child as the character it is.
    ///
    /// ⌥⇧H is ◊ on a US layout. The terminal's meta prefix would turn it
    /// into something the shell reads as a command, so the character goes out
    /// as text instead.
    #[cfg(target_os = "macos")]
    #[gpui_kit::test]
    fn a_mac_symbol_reaches_the_child_as_its_character(cx: &mut gpui_kit::TestAppContext) {
        // Plain `cat`: the character comes back as itself, and the grid can
        // be asked about it.
        let session = script_session("stty raw -echo; printf READY; cat");
        let (view, cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        let session = view.read_with(cx, |v, _| Arc::clone(v.session()));
        let (root, cx) = cx.add_window_view(|_, _| WindowRoot { view });
        let view = root.read_with(cx, |root, _| root.view.clone());
        focus_grid(cx, &view);
        assert!(
            wait_for_char(&session, 'R'),
            "the child never drew its marker"
        );

        press_key_with_char(
            cx,
            "h",
            Some("◊"),
            gpui_kit::Modifiers {
                alt: true,
                shift: true,
                ..Default::default()
            },
        );
        assert!(
            wait_for_char(&session, '◊'),
            "the symbol never reached the child: {}",
            grid_text(&session)
        );
        let _ = session.close().wait();
    }

    /// Every character the grid holds, one line per row.
    fn grid_text(session: &TerminalSession) -> String {
        session
            .snapshot()
            .rows
            .iter()
            .map(|row| row.cells.iter().map(|cell| cell.c).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// One key press, the way the platform delivers one.
    fn press_key(cx: &mut gpui_kit::VisualTestContext, key: &str, modifiers: gpui_kit::Modifiers) {
        press_key_with_char(cx, key, None, modifiers)
    }

    /// One key press that carries a character, the way a Mac layout delivers
    /// a symbol: the platform says what the key spelled, not only what it was.
    fn press_key_with_char(
        cx: &mut gpui_kit::VisualTestContext,
        key: &str,
        key_char: Option<&str>,
        modifiers: gpui_kit::Modifiers,
    ) {
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui_kit::PlatformInput::KeyDown(gpui_kit::KeyDownEvent {
                    keystroke: gpui_kit::Keystroke {
                        key: key.to_owned(),
                        key_char: key_char.map(str::to_owned),
                        modifiers,
                    },
                    is_held: false,
                    // What macOS delivers: the toolkit answers its bindings
                    // first, which is the whole reason this test exists.
                    prefer_character_input: false,
                }),
                cx,
            );
        });
    }

    gpui_kit::actions!(root_context, [RootFocusNext]);

    /// What every window this app opens is drawn under: the toolkit's own
    /// root, whose `tab` moves focus and keeps the key.
    struct WindowRoot {
        view: gpui_kit::Entity<TerminalView>,
    }

    impl Render for WindowRoot {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            gpui_kit::div()
                .key_context("Root")
                .on_action(cx.listener(
                    |_, _: &RootFocusNext, window: &mut Window, cx: &mut Context<Self>| {
                        window.focus_next(cx);
                    },
                ))
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
        // One window, with the chrome around the grid: a grid measures the box
        // it is drawn in, so two windows drawing one view would be two answers
        // to the same question and the grid would answer with whichever of them
        // painted last.
        let (terminal, cx) = cx.add_window_view(|_, cx| {
            let view = gpui_kit::AppContext::new(cx, |cx| {
                TerminalView::new(session, Arc::clone(&metrics), cx)
            });
            Chrome {
                view,
                header: px(HEADER),
            }
        });
        let view = terminal.read_with(cx, |chrome, _| chrome.view.clone());
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
        cx.simulate_keystrokes(platform_paste_keystroke());

        let pasted = wait_for_char(&session, 'p');
        let _ = session.close().wait();
        assert!(pasted, "the clipboard never reached the child's write path");
    }

    /// The same two answers, the way the menu bar asks for them.
    ///
    /// A menu item with a key equivalent is answered by AppKit before the key
    /// reaches the window, so on macOS `⌘C` and `⌘V` arrive here as
    /// `input::Copy` and `input::Paste` rather than as keys. This is what keeps
    /// the grid copyable and pasteable on a platform that has a menu bar.
    #[gpui_kit::test]
    fn the_menu_actions_copy_and_paste_the_grid(cx: &mut gpui_kit::TestAppContext) {
        let session = script_session("printf 'alpha'; cat");
        let (view, cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        let session = view.read_with(cx, |view, _| Arc::clone(view.session()));
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }
        assert!(wait_for_char(&session, 'l'));
        let (origin, cell) = view.read_with(cx, |v, _| (v.canvas_origin.get(), v.cell.unwrap()));
        cx.simulate_click(
            point(origin.x + cell.width * 0.2, origin.y),
            Default::default(),
        );
        cx.simulate_click(
            point(origin.x + cell.width * 0.2, origin.y),
            Default::default(),
        );
        assert_eq!(session.snapshot().selection.as_deref(), Some("alpha"));

        cx.update(|window, cx| window.dispatch_action(Box::new(EditCopy), cx));
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().unwrap().text())
                .as_deref(),
            Some("alpha"),
            "the menu's Copy is the grid's copy"
        );

        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            "pasted-text".to_owned(),
        ));
        cx.update(|window, cx| window.dispatch_action(Box::new(EditPaste), cx));
        let pasted = wait_for_char(&session, 'p');
        let _ = session.close().wait();
        assert!(pasted, "the menu's Paste is the grid's paste");
    }

    fn platform_paste_keystroke() -> &'static str {
        if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-shift-v"
        }
    }

    /// A session whose child prints `selection-text` and then waits for input.
    ///
    /// `cat` after a `printf`, so the grid holds text to select and a paste has
    /// something to echo, without the child exiting: a test that raced an exiting
    /// child would be a test about scheduling.
    fn echo_session() -> TerminalSession {
        script_session("printf selection-text; cat")
    }

    /// A view whose refusals are being listened for.
    ///
    /// The sink is the same shape the window installs: a sentence and the app to
    /// give it to. These tests are about the sentence.
    fn watched_view(
        cx: &mut gpui_kit::TestAppContext,
        session: TerminalSession,
    ) -> (
        gpui_kit::Entity<TerminalView>,
        &mut gpui_kit::VisualTestContext,
        Rc<RefCell<Vec<String>>>,
    ) {
        let heard: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorder = Rc::clone(&heard);
        let (view, cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        view.update(cx, |_, cx| {
            cx.subscribe(&view, move |_, _, event: &InputRefused, _| {
                recorder.borrow_mut().push(event.0.clone());
            })
            .detach();
        });
        (view, cx, heard)
    }

    /// Put the pointer on the grid, the way a person does before typing.
    ///
    /// The view refuses every keystroke it is not focused for, so a test that
    /// skips this would be testing a view that is not listening at all.
    fn focus_grid(cx: &mut gpui_kit::VisualTestContext, view: &gpui_kit::Entity<TerminalView>) {
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }
        // A click first, because that is what focuses the grid: shortcuts
        // belong to the focused element, and a test that skipped it would be
        // testing a window nobody is typing into.
        cx.simulate_click(gpui_kit::point(px(4.), px(4.)), Default::default());
        let _ = view;
    }

    /// A paste the terminal will not take is an operational fact, so it reaches
    /// the window rather than only the log.
    ///
    /// Through the same method the clipboard path calls, because that is the
    /// wiring under test: a refusal that only a direct call to `refuse` could
    /// reach would be a refusal no keystroke could ever produce.
    #[gpui_kit::test]
    fn a_refused_paste_is_reported_to_the_window(cx: &mut gpui_kit::TestAppContext) {
        let (view, cx, heard) = watched_view(cx, echo_session());
        focus_grid(cx, &view);
        // One byte over the limit, so the refusal is the size and not the shape
        // of the text.
        let text = format!("muster-paste-marker{}", "x".repeat(MAX_PASTE_BYTES));
        assert!(text.len() > MAX_PASTE_BYTES);
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
        cx.simulate_keystrokes(platform_paste_keystroke());
        let heard = heard.borrow().clone();
        assert_eq!(heard.len(), 1, "the window was told: {heard:?}");
        assert!(
            heard[0].contains("clipboard paste") || heard[0].contains("paste was refused"),
            "the message does not say what was refused: {heard:?}"
        );
    }

    /// The message says what happened and never what was refused.
    ///
    /// A refused paste is often a secret: a password, a token, a line of a
    /// `.env`. Logging or showing it would defeat the refusal.
    #[gpui_kit::test]
    fn a_refusal_never_quotes_what_it_refused(cx: &mut gpui_kit::TestAppContext) {
        let (view, cx, heard) = watched_view(cx, echo_session());
        focus_grid(cx, &view);
        let secret = "muster-".repeat(64) + "marker";
        let text = format!("{secret}{}", "x".repeat(MAX_PASTE_BYTES));
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
        cx.simulate_keystrokes(platform_paste_keystroke());
        let heard = heard.borrow().clone();
        assert_eq!(heard.len(), 1);
        assert!(
            !heard[0].contains(secret.as_str()),
            "the refusal quoted the text it refused: {heard:?}"
        );
        assert!(
            !heard[0].contains("x".repeat(16).as_str()),
            "the refusal quoted a run of the text it refused: {heard:?}"
        );
    }

    /// A keystroke the terminal will not take reaches the window too.
    ///
    /// A closing session is the ordinary way this happens: the window closes
    /// while the caret is still being typed into, and the person is left typing
    /// into a terminal that has already answered.
    ///
    /// A shortcut and not a character, because a printable character is the
    /// platform's to deliver: this view deliberately lets it past, and the
    /// refusal ordinary typing meets is the text commit's.
    /// character, because a printable character is the platform's to deliver
    /// and this view deliberately does not claim it.
    #[gpui_kit::test]
    fn a_refused_keystroke_is_reported_to_the_window(cx: &mut gpui_kit::TestAppContext) {
        let session = echo_session();
        let (view, cx, heard) = watched_view(cx, session.clone());
        assert!(
            wait_for_char(&session, 's'),
            "the child never drew its text"
        );
        focus_grid(cx, &view);
        session.begin_close().expect("the close begins");
        assert!(session.is_closing(), "the session did not begin closing");
        cx.simulate_keystrokes("ctrl-a");

        let heard = heard.borrow().clone();
        assert_eq!(heard.len(), 1, "the window was told: {heard:?}");
        assert!(
            heard[0].contains("keystroke"),
            "the message does not say what was refused: {heard:?}"
        );
        assert!(
            heard[0].contains("closing"),
            "the message does not say why: {heard:?}"
        );
    }
    /// A view with nobody listening still refuses: the message is not lost, it
    /// is only not delivered.
    #[gpui_kit::test]
    fn a_view_without_a_listener_does_not_panic_on_a_refusal(cx: &mut gpui_kit::TestAppContext) {
        let session = echo_session();
        let (_view, _cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        _cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            "x".repeat(MAX_PASTE_BYTES + 1),
        ));
        _cx.simulate_keystrokes("cmd-v");
    }

    #[gpui_kit::test]
    fn an_unmounted_view_coalesces_output_and_does_not_retain_its_session(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::AppContext;

        let session = echo_session();
        assert!(wait_for_char(&session, 's'));
        let handle = session.clone();
        let view = cx.new(|cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        let weak = view.read_with(cx, |view, _| Arc::downgrade(&view.session));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.repaint.borrow().pending);
            assert!(view.repaint.borrow().waker.is_some());
            assert!(view.blink_task.is_none());
        });
        handle.send_text("more-output\n").unwrap();
        assert!(wait_for_char(&handle, 'm'));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.repaint.borrow().pending);
            assert!(view.repaint.borrow().waker.is_some());
            assert!(view.blink_task.is_none());
        });
        drop(view);
        cx.update(|_| ());
        cx.run_until_parked();
        assert!(weak.upgrade().is_none(), "wake task retained the session");
        handle.close().wait().unwrap();
    }

    fn script_session(script: &str) -> TerminalSession {
        let backend = Arc::new(crate::pty::TerminalBackend::default());
        let mut config = crate::terminal::TerminalSessionConfig::new(
            format!("muster-view-{}", std::process::id()),
            std::env::temp_dir(),
            80,
            24,
            Some(std::path::PathBuf::from("/bin/sh")),
        );
        config.args = Some(vec!["-c".to_owned(), script.to_owned()]);
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
            format!("muster-prompt-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
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
            "stty -echo; sleep 0.3; printf 'abcd\\033]133;B\\007cdxy\\033[2D'; IFS= read -r line; printf 'GOT:%s\\n' \"$line\" | cat -v; sleep 30"
                .to_owned(),
        ]);
        config.drain_deadline = Duration::from_millis(500);
        TerminalSession::spawn(Arc::clone(&backend), config).expect("the prompt session spawns")
    }

    /// What the child answered, once it has answered.
    ///
    /// `od` pads its columns, so both sides are compared with runs of whitespace
    /// collapsed: the question is which bytes arrived, not how they were aligned.
    fn wait_for_line(session: &TerminalSession, wanted: &str) -> Option<String> {
        let squeezed = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        let wanted = squeezed(wanted);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let text: String = session
                .snapshot()
                .rows
                .iter()
                .flat_map(|row| row.cells.iter())
                .map(|cell| cell.c)
                .collect();
            if let Some(rest) = squeezed(&text).split(&wanted).nth(1) {
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
        assert!(wait_for_char(&session, 'y'), "the prompt never drew");

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
    fn terminal_accessible_text_is_bounded_and_tracks_cursor_and_selection() {
        let mut wide = cell('界');
        wide.width = 2;
        let mut spacer = cell(' ');
        spacer.width = 0;
        let mut base = cell('e');
        base.zerowidth.push('\u{301}');
        let snapshot = TerminalSnapshot {
            rows: vec![super::super::RowSnapshot {
                cells: vec![wide, spacer, base, cell(' ')],
            }],
            selection: Some("e\u{301}".into()),
            selection_range: Some(((0, 2), (0, 2))),
            cursor: super::super::CursorSnapshot {
                row: 0,
                col: 3,
                visible: true,
                shape: CursorShapeSnapshot::Block,
                blinking: false,
                color: None,
            },
            modes: Default::default(),
            title: None,
            alt_screen: false,
            mouse_tracking: false,
            display_offset: 0,
            history_size: 0,
            background: None,
            prompt_anchor: None,
        };
        let accessible = accessible_terminal_text(&snapshot, 4, 1);
        assert_eq!(accessible.text, "界e\u{301} ");
        assert_eq!(accessible.cursor, Some(3));
        assert_eq!(accessible.selection, Some((1, 3)));

        let oversized = TerminalSnapshot {
            rows: vec![super::super::RowSnapshot {
                cells: vec![cell('x'); MAX_ACCESSIBLE_TERMINAL_CELLS * 2],
            }],
            ..snapshot
        };
        let bounded = accessible_terminal_text(&oversized, MAX_ACCESSIBLE_TERMINAL_CELLS * 2, 1);
        assert_eq!(bounded.text.chars().count(), MAX_ACCESSIBLE_TERMINAL_CELLS);
    }

    #[test]
    fn ime_preedit_uses_terminal_display_columns_and_stays_inside_the_grid() {
        assert_eq!(composition_columns(80, 4, "abc"), Some(3));
        assert_eq!(composition_columns(80, 4, "界"), Some(2));
        assert_eq!(composition_columns(80, 4, "e\u{301}"), Some(1));
        assert_eq!(composition_columns(80, 79, "界"), Some(1));
        assert_eq!(composition_columns(80, 80, "x"), None);
        assert_eq!(composition_columns(80, 81, "x"), None);
        assert_eq!(composition_columns(80, 4, "\u{301}"), Some(1));
    }

    #[test]
    fn marked_text_replacements_use_utf16_without_splitting_surrogates() {
        assert_eq!(
            replace_marked("a😀b", Some(1..3), "中").as_deref(),
            Some("a中b")
        );
        assert_eq!(replace_marked("😀", Some(1..2), "x"), None);
        assert_eq!(replace_marked("a", Some(3..4), "x"), None);
        assert_eq!(replace_marked("日本", None, "語").as_deref(), Some("語"));
    }

    #[test]
    fn cursor_override_and_sidebar_focus_preserve_readability() {
        let fg = opaque((200, 80, 40));
        let bg = opaque((20, 30, 40));
        let normal = opaque(CURSOR_COLOR);
        let focused = FocusState {
            focused: true,
            window_active: true,
        };
        let empty = block_cursor_colors(fg, bg, true, focused, Some(normal));
        assert_eq!(empty.background, normal);
        assert_eq!(empty.foreground, bg);
        let hidden = block_cursor_colors(fg, bg, false, focused, Some(normal));
        assert_eq!(hidden.background, bg);
        assert_eq!(hidden.foreground, fg);
        let sidebar = FocusState {
            focused: false,
            window_active: true,
        };
        assert!(sidebar.draws_unfocused_cursor());
        let colors = block_cursor_colors(fg, bg, true, sidebar, None);
        assert_eq!(colors.foreground, fg);
        assert_eq!(colors.background, bg);
    }

    #[test]
    fn layout_uses_parser_widths_including_leading_spacers_and_combining_cells() {
        let mut spacer = cell(' ');
        spacer.width = 0;
        let mut wide = cell('x'); // Width is from the parser, not the font/character.
        wide.width = 2;
        let cells = [spacer.clone(), wide, spacer, cell('a')];
        assert_eq!(
            place_row(&cells)
                .iter()
                .map(|p| (p.column, p.columns))
                .collect::<Vec<_>>(),
            vec![(1, 2), (3, 1)]
        );
        let mut combined = cell('e');
        combined.zerowidth.push('\u{301}');
        assert_eq!(compatible_run_end(&[combined, cell('f')], 0, false), 1);
    }

    #[gpui_kit::test]
    fn word_line_and_shift_selection_copy_on_release(cx: &mut gpui_kit::TestAppContext) {
        let session = script_session("printf 'alpha beta'; cat");
        let (view, cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        let session = view.read_with(cx, |view, _| Arc::clone(view.session()));
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }
        assert!(wait_for_char(&session, 'b'));
        let (origin, cell) = view.read_with(cx, |v, _| (v.canvas_origin.get(), v.cell.unwrap()));
        let at = |col| point(origin.x + cell.width * col, origin.y + cell.height * 0.5);
        cx.simulate_click(at(2.2), Default::default());
        cx.simulate_click(at(2.2), Default::default());
        assert_eq!(session.snapshot().selection.as_deref(), Some("alpha"));
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().unwrap().text())
                .as_deref(),
            Some("alpha")
        );
        cx.simulate_click(at(2.2), Default::default());
        assert_eq!(
            session.snapshot().selection.as_deref(),
            Some("alpha beta\n")
        );
        let shift = gpui_kit::Modifiers {
            shift: true,
            ..Default::default()
        };
        cx.simulate_mouse_down(at(0.2), MouseButton::Left, shift);
        cx.simulate_mouse_move(at(9.8), MouseButton::Left, shift);
        cx.simulate_mouse_up(at(9.8), MouseButton::Left, shift);
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().unwrap().text())
                .as_deref(),
            Some("alpha beta")
        );
        let _ = session.close().wait();
    }

    #[gpui_kit::test]
    fn composition_stays_local_until_commit_and_plain_text_is_sent_once(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let session = echo_session();
        let (view, cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        let session = view.read_with(cx, |v, _| Arc::clone(v.session()));
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }
        assert!(wait_for_char(&session, 's'));
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        cx.update(|window, cx| view.update(cx, |v, cx| v.focus.focus(window, cx)));
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
                assert_eq!(v.marked_text_range(window, cx), Some(0..2));
            })
        });
        assert!(!session
            .snapshot()
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .any(|c| c.c == '日'));
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.replace_text_in_range(None, "日本", window, cx);
                assert_eq!(v.marked_text_range(window, cx), None);
            })
        });
        assert!(wait_for_char(&session, '日'));
        cx.simulate_keystrokes("q");
        assert!(wait_for_char(&session, 'q'));
        assert_eq!(
            session
                .snapshot()
                .rows
                .iter()
                .flat_map(|r| &r.cells)
                .filter(|c| c.c == 'q')
                .count(),
            1
        );
        let _ = session.close().wait();
    }

    #[gpui_kit::test]
    fn wheel_scrolls_history_and_typing_returns_to_the_live_view(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        let session = script_session("i=0; while [ $i -lt 200 ]; do printf 'history-%s\\n' \"$i\"; i=$((i+1)); done; printf Z; cat");
        let (view, cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        let session = view.read_with(cx, |v, _| Arc::clone(v.session()));
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }
        assert!(wait_for_char(&session, 'Z'));
        let (origin, cell) = view.read_with(cx, |v, _| (v.canvas_origin.get(), v.cell.unwrap()));
        let position = point(origin.x + cell.width * 2.5, origin.y + cell.height * 0.5);
        cx.simulate_click(position, Default::default());
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui_kit::ScrollWheelEvent {
                    position,
                    delta: gpui_kit::ScrollDelta::Lines(point(0., 3.)),
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
        });
        assert_eq!(session.snapshot().display_offset, 3);
        cx.update(|window, cx| window.render_frame(cx));
        // The right edge is terminal content, not a scrollbar hit target.
        let cols = view.read_with(cx, |v, _| v.applied.get().0);
        cx.simulate_click(
            point(
                origin.x + cell.width * f32::from(cols) - px(1.0),
                position.y,
            ),
            Default::default(),
        );
        assert_eq!(session.snapshot().display_offset, 3);
        // Two half-cell trackpad deltas must accumulate into one history line.
        for _ in 0..2 {
            cx.update(|window, cx| {
                window.dispatch_event(
                    gpui_kit::ScrollWheelEvent {
                        position,
                        delta: gpui_kit::ScrollDelta::Pixels(point(px(0.0), cell.height / 2.0)),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
            });
        }
        assert_eq!(session.snapshot().display_offset, 4);
        cx.update(|window, cx| window.render_frame(cx));
        cx.simulate_keystrokes("q");
        assert_eq!(session.snapshot().display_offset, 0);
        assert!(wait_for_char(&session, 'q'));
        let _ = session.close().wait();
    }

    #[gpui_kit::test]
    fn tui_mouse_reports_reach_the_child_but_shift_drag_remains_local(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let session = script_session("stty raw -echo; sleep 0.3; printf 'alpha beta\\033[?1000h\\033[?1006h'; dd bs=1 count=18 2>/dev/null | od -An -tx1; sleep 30");
        let (view, cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(
                session,
                Arc::new(Mutex::new(CellMetrics {
                    width: 8,
                    height: 16,
                })),
                cx,
            )
        });
        let session = view.read_with(cx, |v, _| Arc::clone(v.session()));
        cx.update(|window, _| window.activate_window());
        for _ in 0..3 {
            cx.update(|window, cx| window.render_frame(cx));
        }
        assert!(wait_for_char(&session, 'b'));
        assert!(session.snapshot().mouse_tracking);
        let (origin, cell) = view.read_with(cx, |v, _| (v.canvas_origin.get(), v.cell.unwrap()));
        let at = |col| point(origin.x + cell.width * col, origin.y + cell.height * 0.5);
        let shift = gpui_kit::Modifiers {
            shift: true,
            ..Default::default()
        };
        cx.simulate_mouse_down(at(0.2), MouseButton::Left, shift);
        cx.simulate_mouse_move(at(9.8), MouseButton::Left, shift);
        cx.simulate_mouse_up(at(9.8), MouseButton::Left, shift);
        assert_eq!(session.snapshot().selection.as_deref(), Some("alpha beta"));
        // No bytes were sent by the local override; dd still needs both reports.
        cx.simulate_click(at(2.2), Default::default());
        let got = wait_for_line(&session, "1b 5b 3c 30 3b 33 3b 31 4d").unwrap_or_else(|| {
            panic!(
                "missing SGR press: {}",
                session
                    .snapshot()
                    .rows
                    .iter()
                    .map(|r| r
                        .cells
                        .iter()
                        .map(|c| c.c)
                        .collect::<String>()
                        .trim_end()
                        .to_owned())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
        assert!(
            got.contains("6d"),
            "the SGR release also reached the child: {got}"
        );
        let _ = session.close().wait();
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
            (0x16, 0x18, 0x1d),
            "the surface the window is painted, not legacy's content well"
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
    fn every_default_is_the_legacy_dark_token_named_beside_it() {
        // Transcribed from `legacy/src/muster.css`, `:root` without
        // `data-theme="light"`, because legacy built xterm.js' theme by reading
        // exactly these names out of that block. If a value here is edited it is
        // the CSS that has to move with it, in both apps.
        const LEGACY_DARK: [(&str, (u8, u8, u8)); 19] = [
            ("--muster-content-text", (0xdc, 0xe0, 0xe5)),
            ("--muster-bg-0", (0x16, 0x18, 0x1d)),
            ("--muster-cursor", (0xc2, 0xc9, 0xf1)),
            ("--muster-ansi-black", (0x28, 0x2c, 0x34)),
            ("--muster-ansi-red", (0xe0, 0x6c, 0x75)),
            ("--muster-ansi-green", (0x98, 0xc3, 0x79)),
            ("--muster-ansi-yellow", (0xe5, 0xc0, 0x7b)),
            ("--muster-ansi-blue", (0x61, 0xaf, 0xef)),
            ("--muster-ansi-magenta", (0xc6, 0x78, 0xdd)),
            ("--muster-ansi-cyan", (0x56, 0xb6, 0xc2)),
            ("--muster-ansi-white", (0xab, 0xb2, 0xbf)),
            ("--muster-ansi-bright-black", (0x63, 0x6d, 0x83)),
            ("--muster-ansi-bright-red", (0xea, 0x85, 0x8b)),
            ("--muster-ansi-bright-green", (0xaa, 0xd5, 0x81)),
            ("--muster-ansi-bright-yellow", (0xff, 0xd8, 0x85)),
            ("--muster-ansi-bright-blue", (0x85, 0xc1, 0xff)),
            ("--muster-ansi-bright-magenta", (0xd3, 0x98, 0xeb)),
            ("--muster-ansi-bright-cyan", (0x6e, 0xd5, 0xde)),
            ("--muster-ansi-bright-white", (0xfa, 0xfa, 0xfa)),
        ];

        let palette = Palette::default();
        assert_eq!(palette.foreground, LEGACY_DARK[0].1, "{}", LEGACY_DARK[0].0);
        assert_eq!(palette.background, LEGACY_DARK[1].1, "{}", LEGACY_DARK[1].0);
        assert_eq!(palette.cursor, LEGACY_DARK[2].1, "{}", LEGACY_DARK[2].0);
        for (index, (token, expected)) in LEGACY_DARK.iter().skip(3).enumerate() {
            assert_eq!(xterm_256(index as u8), *expected, "{token}");
        }
    }

    /// The thumb says how much scrollback there is and where in it the viewport
    /// is, so both ends of the scrollback and a terminal that cannot scroll at
    /// all are the cases that decide whether the arithmetic is legacy's.
    #[test]
    fn the_scrollbar_thumb_says_where_the_viewport_is() {
        assert!(
            !scrollbar_geometry(0, 0, 24, 480.).scrollable,
            "a terminal with no scrollback has nothing to say"
        );
        let bottom = scrollbar_geometry(1000, 0, 24, 480.);
        assert!(bottom.scrollable);
        assert!(
            (bottom.top - (480. - bottom.height)).abs() < 0.01,
            "an unscrolled viewport sits at the end of the scrollback: {}",
            bottom.top
        );
        let oldest = scrollbar_geometry(1000, 1000, 24, 480.);
        assert!(
            oldest.top.abs() < 0.01,
            "the oldest line is the top of the track: {}",
            oldest.top
        );
        let half = scrollbar_geometry(1000, 500, 24, 480.);
        assert!((half.top - (oldest.top + bottom.top) / 2.).abs() < 0.01);
        assert!(bottom.height >= MIN_SCROLLBAR_THUMB);
        // An offset past the end of a scrollback that just shrank is clamped
        // rather than drawn off the bottom of its own track.
        let shrunk = scrollbar_geometry(0, 1000, 24, 480.);
        assert!(!shrunk.scrollable && shrunk.height == 480.);
        assert_eq!(
            scrollbar_geometry(1000, 0, 24, 0.),
            ScrollbarGeometry {
                top: 0.,
                height: 0.,
                scrollable: true
            }
        );
    }

    #[test]
    fn the_selection_is_a_band_over_the_cell_rather_than_a_fill() {
        // `--muster-selection` is `#74ade83d`: the content accent, `#74ade8`, at
        // an alpha of `0x3d`. Legacy drew that into xterm.js' background layer,
        // so what it covered still reads through it, and a block cursor drawn
        // over a selected cell still inverts against the cell rather than
        // against the band. `#2c3c4e` is this band over `#16181d`.
        assert_eq!(
            (SELECTION_COLOR.0, SELECTION_COLOR.1, SELECTION_COLOR.2),
            (0x74, 0xad, 0xe8),
            "the content accent, not a colour of its own"
        );
        assert_eq!(SELECTION_COLOR.3, 0x3d, "the accent at a quarter");

        let band = rgba(SELECTION_COLOR).to_rgb();
        let alpha = rgba(SELECTION_COLOR).a;
        assert!(
            alpha > 0.0 && alpha < 1.0,
            "a selection is a band over the cell, not a fill: {alpha}"
        );
        let background = Palette::default().background;
        for (channel, behind, composited) in [
            (band.r, background.0, 0x2cu8),
            (band.g, background.1, 0x3c),
            (band.b, background.2, 0x4e),
        ] {
            let expected = f32::from(behind) / 255.0;
            assert!(
                (channel * alpha + expected * (1.0 - alpha) - f32::from(composited) / 255.0)
                    .abs()
                    <= 1.0 / 255.0,
                "the band over the surface is not #{composited:02x}{composited:02x}{composited:02x}"
            );
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

        let inverted = block_cursor_colors(foreground, background, true, focused, None);
        assert_eq!(inverted.foreground, background);
        assert_eq!(inverted.background, foreground);

        let unfocused = block_cursor_colors(foreground, background, true, inactive, None);
        assert_eq!(
            unfocused,
            CellColors {
                foreground,
                background
            }
        );
        let hidden = block_cursor_colors(foreground, background, false, focused, None);
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

        let placements = place_row(&cells);
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
        let placements = place_row(&cells);
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
        let placements = place_row(&cells);
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
