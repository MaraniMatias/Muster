//! Bounded Kitty graphics, owned by a terminal session, never by the clipboard.
//!
//! Direct PNG/RGB/RGBA transfers are decoded on the PTY reader, not in paint.
//! Files, shared memory, animation and relative placements are deliberately
//! refused: terminal output must not acquire a second path to the filesystem.
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
    ops::Range,
    sync::Arc,
};

use alacritty_terminal::{
    event::EventListener,
    grid::Dimensions,
    term::{Term, TermMode},
    vte::ansi::{Color, Handler},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use gpui_kit::RenderImage;
use image::{Frame, ImageReader, Limits, RgbaImage};

pub(super) const PLACEHOLDER: char = '\u{10eeee}';
pub(super) const MAX_PACKET: usize = 8192;
const MAX_TRANSFER: usize = 16 * 1024 * 1024;
const MAX_PIXELS_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESIDENT: usize = 64 * 1024 * 1024;
const MAX_IMAGES: usize = 64;
const MAX_PLACEMENTS: usize = 256;

type Result<T> = std::result::Result<T, &'static str>;

/// One immutable raster placement visible in the terminal viewport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphicSnapshot {
    pub(crate) image: Arc<RenderImage>,
    pub(crate) id: u32,
    pub(crate) row: i32,
    pub(crate) column: u32,
    pub(crate) columns: u32,
    pub(crate) rows: u32,
    pub(crate) source: [u32; 4],
    pub(crate) offset: [u32; 2],
    pub(crate) placeholder: Option<[u32; 2]>,
    pub(crate) natural_size: bool,
    pub(crate) clip: [i32; 2],
    pub(crate) z: i32,
}

#[derive(Clone, Default)]
struct Control(BTreeMap<u8, String>);

impl Control {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).map_err(|_| "EINVAL: invalid controls")?;
        if text.len() > 1024 {
            return Err("EINVAL: controls too long");
        }
        let mut values = BTreeMap::new();
        if !text.is_empty() {
            for part in text.split(',') {
                let (key, value) = part.split_once('=').ok_or("EINVAL: invalid control")?;
                if key.len() != 1
                    || value.is_empty()
                    || values.insert(key.as_bytes()[0], value.to_owned()).is_some()
                {
                    return Err("EINVAL: duplicate or invalid control");
                }
            }
        }
        Ok(Self(values))
    }
    fn n(&self, key: u8) -> Result<u32> {
        self.0
            .get(&key)
            .map_or(Ok(0), |s| s.parse().map_err(|_| "EINVAL: invalid integer"))
    }
    fn ch(&self, key: u8, default: u8) -> Result<u8> {
        self.0.get(&key).map_or(Ok(default), |s| {
            if s.len() == 1 {
                Ok(s.as_bytes()[0])
            } else {
                Err("EINVAL: invalid character")
            }
        })
    }
    fn z(&self) -> Result<i32> {
        self.0
            .get(&b'z')
            .map_or(Ok(0), |s| s.parse().map_err(|_| "EINVAL: invalid z-index"))
    }
    fn reply(&self, result: Result<()>) -> Option<String> {
        let id = self.n(b'i').ok()?;
        let quiet = self.n(b'q').ok()?;
        if id == 0 || quiet >= 2 || (quiet == 1 && result.is_ok()) {
            return None;
        }
        let placement = self
            .n(b'p')
            .ok()
            .filter(|p| *p != 0)
            .map(|p| format!(",p={p}"))
            .unwrap_or_default();
        Some(format!(
            "\x1b_Gi={id}{placement};{}\x1b\\",
            result.err().unwrap_or("OK")
        ))
    }
}

struct Upload {
    control: Control,
    encoded: Vec<u8>,
    refused: bool,
}

#[derive(Default)]
struct Buffer {
    images: BTreeMap<u32, Arc<RenderImage>>,
    placements: Vec<Placement>,
    upload: Option<Upload>,
    next_id: u32,
}

#[derive(Clone)]
struct Placement {
    id: u32,
    placement: u32,
    virtual_: bool,
    graphic: GraphicSnapshot,
}

pub(super) struct Graphics {
    buffers: [Buffer; 2],
    pub(super) region: Range<i32>,
    pub(super) cell: [u16; 2],
}

impl Graphics {
    pub(super) fn new(rows: u16) -> Self {
        Self {
            buffers: Default::default(),
            region: 0..i32::from(rows),
            cell: [8, 16],
        }
    }
    fn buffer(&mut self, alt: bool) -> &mut Buffer {
        &mut self.buffers[usize::from(alt)]
    }
    pub(super) fn abort(&mut self, alt: bool) {
        if let Some(upload) = &mut self.buffer(alt).upload {
            upload.encoded.clear();
            upload.refused = true;
        }
    }
    pub(super) fn reset(&mut self, rows: usize) {
        self.buffers = Default::default();
        self.region = 0..rows as i32;
    }
    pub(super) fn swap_screen(&mut self) {
        self.buffers[1] = Buffer::default();
    }

    pub(super) fn resize(&mut self, rows: usize) {
        self.region = 0..rows as i32;
        // Classic placements have no cell identity through text reflow. TUIs
        // redraw them on resize; Unicode placements follow their actual cells.
        for buffer in &mut self.buffers {
            buffer.placements.retain(|p| p.virtual_);
        }
    }

    pub(super) fn scroll(&mut self, alt: bool, region: Range<i32>, delta: i32) {
        if delta == 0 {
            return;
        }
        let buffer = self.buffer(alt);
        for p in &mut buffer.placements {
            if p.virtual_ {
                continue;
            }
            let g = &mut p.graphic;
            let top = g.row.max(g.clip[0]);
            let bottom = (g.row + g.rows as i32).min(g.clip[1]);
            if top >= region.end || bottom <= region.start && (region.start != 0 || delta < 0) {
                continue;
            }
            // A full-screen upward scroll moves history too, including when
            // history is full. No inference from history_size is involved.
            let min = if region.start == 0 && delta > 0 && !alt {
                i32::MIN / 2
            } else {
                region.start
            };
            g.row = g.row.saturating_sub(delta);
            g.clip[0] = g.clip[0].saturating_sub(delta).max(min);
            g.clip[1] = g.clip[1].saturating_sub(delta).min(region.end);
        }
        buffer.placements.retain(|p| {
            p.virtual_
                || p.graphic.row.max(p.graphic.clip[0])
                    < (p.graphic.row + p.graphic.rows as i32).min(p.graphic.clip[1])
        });
    }

    pub(super) fn clear(&mut self, alt: bool, mode: &alacritty_terminal::vte::ansi::ClearMode) {
        use alacritty_terminal::vte::ansi::ClearMode;
        self.buffer(alt).placements.retain(|p| {
            p.virtual_
                || match mode {
                    ClearMode::All => p.graphic.row + p.graphic.rows as i32 <= 0,
                    ClearMode::Saved => p.graphic.row >= 0,
                    _ => true,
                }
        });
    }

    pub(super) fn prune(&mut self, alt: bool, history: usize) {
        self.buffer(alt)
            .placements
            .retain(|p| p.virtual_ || p.graphic.row + p.graphic.rows as i32 > -(history as i32));
    }

    pub(super) fn apply<T: EventListener>(
        &mut self,
        payload: &[u8],
        term: &mut Term<T>,
    ) -> Option<String> {
        let payload = payload.strip_prefix(b"G")?;
        let split = payload
            .iter()
            .position(|b| *b == b';')
            .unwrap_or(payload.len());
        let control = match Control::parse(&payload[..split]) {
            Ok(control) => control,
            Err(_) => {
                self.abort(term.mode().contains(TermMode::ALT_SCREEN));
                return None;
            }
        };
        let data = payload.get(split + 1..).unwrap_or_default();
        let alt = term.mode().contains(TermMode::ALT_SCREEN);
        let cell = self.cell;
        let buffer = self.buffer(alt);
        let continuation = buffer.upload.is_some() && control.0.keys().all(|k| b"miq".contains(k));
        let mut reply_control = control.clone();
        let result = (|| {
            let more = control.n(b'm')?;
            if more > 1 || control.n(b'q')? > 2 {
                return Err("EINVAL: invalid transfer flags");
            }
            let action = control.ch(b'a', b't')?;
            if !continuation {
                buffer.upload = None;
            }
            if continuation || matches!(action, b't' | b'T' | b'q') {
                if !continuation {
                    buffer.upload = Some(Upload {
                        control: control.clone(),
                        encoded: Vec::new(),
                        refused: false,
                    });
                }
                let upload = buffer.upload.as_mut().ok_or("EINVAL: missing upload")?;
                if continuation
                    && control.n(b'i')? != 0
                    && control.n(b'i')? != upload.control.n(b'i')?
                {
                    upload.refused = true;
                }
                if upload.encoded.len().saturating_add(data.len()) > MAX_TRANSFER {
                    upload.refused = true;
                }
                if upload.refused {
                    upload.encoded.clear();
                } else {
                    upload.encoded.extend_from_slice(data);
                }
                if more == 1 {
                    return Ok(false);
                }
                let upload = buffer.upload.take().ok_or("EINVAL: missing upload")?;
                reply_control = upload.control.clone();
                if upload.refused {
                    return Err("ENOSPC: transfer refused");
                }
                let mut control = upload.control;
                validate(&control)?;
                let pixels = decode(&control, &upload.encoded)?;
                if control.ch(b'a', b't')? == b'q' {
                    return Ok(true);
                }
                let mut id = control.n(b'i')?;
                if id == 0 {
                    loop {
                        buffer.next_id = buffer.next_id.wrapping_add(1).max(1);
                        if !buffer.images.contains_key(&buffer.next_id) {
                            break;
                        }
                    }
                    id = buffer.next_id;
                    control.0.insert(b'i', id.to_string());
                }
                let bytes = pixels.len();
                let resident: usize = buffer
                    .images
                    .iter()
                    .filter(|(old, _)| **old != id)
                    .map(|(_, image)| image.as_bytes(0).map_or(0, <[u8]>::len))
                    .sum();
                if resident + bytes > MAX_RESIDENT
                    || (!buffer.images.contains_key(&id) && buffer.images.len() >= MAX_IMAGES)
                {
                    return Err("ENOSPC: image quota exceeded");
                }
                let render = Arc::new(RenderImage::new([Frame::new(pixels)]));
                let old_placements: Vec<_> = buffer
                    .placements
                    .iter()
                    .filter(|p| p.id == id)
                    .cloned()
                    .collect();
                buffer.placements.retain(|p| p.id != id);
                let old_image = buffer.images.insert(id, render);
                if control.ch(b'a', b't')? == b'T' {
                    if let Err(error) = buffer.place(&control, term, cell) {
                        buffer.images.remove(&id);
                        if let Some(image) = old_image {
                            buffer.images.insert(id, image);
                        }
                        buffer.placements.extend(old_placements);
                        return Err(error);
                    }
                }
                Ok(true)
            } else {
                validate(&control)?;
                match action {
                    b'p' => buffer.place(&control, term, cell)?,
                    b'd' => buffer.delete(&control, term)?,
                    _ => return Err("ENOTSUP: unsupported graphics action"),
                }
                Ok(true)
            }
        })();
        match result {
            Ok(false) => None,
            Ok(true) => reply_control.reply(Ok(())),
            Err(error) => {
                buffer.upload = None;
                reply_control.reply(Err(error))
            }
        }
    }

    pub(super) fn snapshot<T: EventListener>(&self, term: &Term<T>) -> Vec<GraphicSnapshot> {
        let buffer = &self.buffers[usize::from(term.mode().contains(TermMode::ALT_SCREEN))];
        let offset = term.grid().display_offset() as i32;
        let height = term.screen_lines() as i32;
        let mut graphics = Vec::new();
        for p in &buffer.placements {
            if p.virtual_ {
                continue;
            }
            let mut g = p.graphic.clone();
            g.row += offset;
            g.clip[0] = g.clip[0].saturating_add(offset).max(0);
            g.clip[1] = g.clip[1].saturating_add(offset).min(height);
            if g.row < g.clip[1] && g.row + g.rows as i32 > g.clip[0] {
                graphics.push(g);
            }
        }
        // Placeholder metadata remains in the parser's cells. Scrolling,
        // erasure, alternate screens and reflow therefore move the image with
        // the text itself, not a separately guessed cursor position.
        if !buffer.placements.iter().any(|p| p.virtual_) {
            graphics.sort_by_key(|g| (g.z, g.id));
            return graphics;
        }
        for row in 0..height {
            let mut previous: Option<Placeholder> = None;
            for column in 0..term.columns() {
                let cell = &term.grid()[alacritty_terminal::index::Line(row - offset)]
                    [alacritty_terminal::index::Column(column)];
                let Some(placeholder) = Placeholder::read(cell, previous.as_ref()) else {
                    previous = None;
                    continue;
                };
                let p = buffer.placements.iter().find(|p| {
                    p.virtual_
                        && p.id == placeholder.id
                        && (placeholder.placement == 0 || p.placement == placeholder.placement)
                });
                if let Some(p) = p.filter(|p| {
                    placeholder.row < p.graphic.rows && placeholder.column < p.graphic.columns
                }) {
                    let mut g = p.graphic.clone();
                    g.row = row - placeholder.row as i32;
                    g.column = column as u32;
                    // Paint only this placeholder cell, using the virtual
                    // rectangle's origin to preserve scaling and cropping.
                    g.placeholder = Some([placeholder.column, placeholder.row]);
                    g.clip = [row, row + 1];
                    graphics.push(g);
                }
                previous = Some(placeholder);
            }
        }
        graphics.sort_by_key(|g| (g.z, g.id));
        graphics
    }
}

fn validate(c: &Control) -> Result<()> {
    for key in c.0.keys() {
        if !b"aftsvipmqrCczxywhXYUdNo".contains(key) {
            return Err("ENOTSUP: unsupported graphics control");
        }
    }
    if c.ch(b't', b'd')? != b'd' {
        return Err("ENOTSUP: only direct graphics transfers are allowed");
    }
    if c.n(b'C')? > 1 || c.n(b'U')? > 1 {
        return Err("EINVAL: invalid placement flags");
    }
    // Validate all numeric fields before publishing any state.
    for key in b"svipmqrCcxywhXYUN" {
        c.n(*key)?;
    }
    c.z()?;
    Ok(())
}

fn decode(c: &Control, encoded: &[u8]) -> Result<RgbaImage> {
    let mut data = STANDARD
        .decode(encoded)
        .map_err(|_| "EINVAL: invalid base64")?;
    match c.ch(b'o', 0)? {
        0 => {}
        b'z' => {
            let mut expanded = Vec::new();
            flate2::read::ZlibDecoder::new(data.as_slice())
                .take(MAX_PIXELS_BYTES as u64 + 1)
                .read_to_end(&mut expanded)
                .map_err(|_| "EINVAL: invalid compressed pixels")?;
            if expanded.len() > MAX_PIXELS_BYTES {
                return Err("ENOSPC: decompressed image exceeds quota");
            }
            data = expanded;
        }
        _ => return Err("ENOTSUP: unsupported compression"),
    }
    let format = c.n(b'f')?;
    let mut pixels = if format == 100 {
        let mut reader = ImageReader::with_format(Cursor::new(data), image::ImageFormat::Png);
        let mut limits = Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(MAX_PIXELS_BYTES as u64);
        reader.limits(limits);
        let decoder = reader.into_decoder().map_err(|_| "EINVAL: invalid PNG")?;
        use image::ImageDecoder;
        let (w, h) = decoder.dimensions();
        pixel_bytes(w, h)?;
        image::DynamicImage::from_decoder(decoder)
            .map_err(|_| "EINVAL: invalid PNG")?
            .into_rgba8()
    } else {
        let (w, h) = (c.n(b's')?, c.n(b'v')?);
        let count = pixel_bytes(w, h)?;
        match format {
            0 | 32 if data.len() == count => {
                RgbaImage::from_raw(w, h, data).ok_or("EINVAL: invalid RGBA")?
            }
            24 if data.len() == count / 4 * 3 => {
                let mut rgba = Vec::with_capacity(count);
                for rgb in data.chunks_exact(3) {
                    rgba.extend_from_slice(rgb);
                    rgba.push(255);
                }
                RgbaImage::from_raw(w, h, rgba).ok_or("EINVAL: invalid RGB")?
            }
            24 | 32 | 0 => return Err("EINVAL: pixel size mismatch"),
            _ => return Err("ENOTSUP: unsupported image format"),
        }
    };
    // GPUI's raster contract is BGRA, not RGBA. No decoder or per-pixel
    // conversion is performed by the UI thread.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(pixels)
}

fn pixel_bytes(w: u32, h: u32) -> Result<usize> {
    let bytes = u64::from(w)
        .checked_mul(u64::from(h))
        .and_then(|n| n.checked_mul(4))
        .ok_or("ENOSPC: image dimensions exceed quota")?;
    if w == 0 || h == 0 || w > 8192 || h > 8192 || bytes > MAX_PIXELS_BYTES as u64 {
        return Err("ENOSPC: image dimensions exceed quota");
    }
    Ok(bytes as usize)
}

impl Buffer {
    fn place<T: EventListener>(
        &mut self,
        c: &Control,
        term: &mut Term<T>,
        cell: [u16; 2],
    ) -> Result<()> {
        let id = c.n(b'i')?;
        let image = self
            .images
            .get(&id)
            .ok_or("ENOENT: image not found")?
            .clone();
        let size = image.size(0);
        let (x, y) = (c.n(b'x')?, c.n(b'y')?);
        let width = c.n(b'w')?;
        let height = c.n(b'h')?;
        let w = if width == 0 {
            (size.width.0 as u32).saturating_sub(x)
        } else {
            width.min((size.width.0 as u32).saturating_sub(x))
        };
        let h = if height == 0 {
            (size.height.0 as u32).saturating_sub(y)
        } else {
            height.min((size.height.0 as u32).saturating_sub(y))
        };
        if w == 0 || h == 0 {
            return Err("EINVAL: empty source rectangle");
        }
        let (cw, ch) = (u32::from(cell[0].max(1)), u32::from(cell[1].max(1)));
        let (mut columns, mut rows) = (c.n(b'c')?, c.n(b'r')?);
        if columns > 65535 || rows > 65535 {
            return Err("EINVAL: placement too large");
        }
        if columns == 0 && rows == 0 {
            columns = w.div_ceil(cw);
            rows = h.div_ceil(ch);
        } else if columns == 0 {
            columns = (u64::from(rows) * u64::from(ch) * u64::from(w))
                .div_ceil(u64::from(h) * u64::from(cw))
                .min(u64::from(u32::MAX)) as u32;
        } else if rows == 0 {
            rows = (u64::from(columns) * u64::from(cw) * u64::from(h))
                .div_ceil(u64::from(w) * u64::from(ch))
                .min(u64::from(u32::MAX)) as u32;
        }
        if columns > 65535 || rows > 65535 {
            return Err("EINVAL: placement too large");
        }
        let placement = c.n(b'p')?;
        let virtual_ = c.n(b'U')? == 1;
        if virtual_ && (c.n(b'c')? == 0 || c.n(b'r')? == 0) {
            return Err("EINVAL: virtual placement requires rows and columns");
        }
        let offset = [c.n(b'X')?, c.n(b'Y')?];
        if offset[0] >= cw || offset[1] >= ch {
            return Err("EINVAL: cell offset outside cell");
        }
        if placement != 0 {
            self.placements
                .retain(|p| p.id != id || p.placement != placement);
        }
        if self.placements.len() >= MAX_PLACEMENTS {
            return Err("ENOSPC: placement quota exceeded");
        }
        let cursor = term.grid().cursor.point;
        self.placements.push(Placement {
            id,
            placement,
            virtual_,
            graphic: GraphicSnapshot {
                image,
                id,
                row: cursor.line.0,
                column: cursor.column.0 as u32,
                columns,
                rows,
                source: [x, y, w, h],
                offset,
                placeholder: None,
                natural_size: c.n(b'c')? == 0 && c.n(b'r')? == 0,
                clip: [i32::MIN / 2, i32::MAX / 2],
                z: c.z()?,
            },
        });
        if !virtual_ && c.n(b'C')? == 0 {
            term.move_down(rows as usize);
            term.move_forward(columns as usize);
        }
        Ok(())
    }
    fn delete<T: EventListener>(&mut self, c: &Control, term: &Term<T>) -> Result<()> {
        let mode = c.ch(b'd', b'a')?;
        let id = c.n(b'i')?;
        let placement = c.n(b'p')?;
        if !b"aAiIrR".contains(&mode) {
            return Err("ENOTSUP: unsupported deletion selector");
        }
        let mut removed = Vec::new();
        self.placements.retain(|p| {
            let selected = match mode.to_ascii_lowercase() {
                b'i' => p.id == id && (placement == 0 || p.placement == placement),
                b'r' => p.id >= c.n(b'x').unwrap_or(0) && p.id <= c.n(b'y').unwrap_or(0),
                _ => {
                    !p.virtual_
                        && p.graphic.row < term.screen_lines() as i32
                        && p.graphic.row + p.graphic.rows as i32 > 0
                }
            };
            if selected {
                removed.push(p.id);
            }
            !selected
        });
        if mode.is_ascii_uppercase() {
            self.images.retain(|image_id, _| {
                let selected = match mode.to_ascii_lowercase() {
                    b'i' => *image_id == id,
                    b'r' => {
                        *image_id >= c.n(b'x').unwrap_or(0) && *image_id <= c.n(b'y').unwrap_or(0)
                    }
                    _ => removed.contains(image_id),
                };
                !selected || self.placements.iter().any(|p| p.id == *image_id)
            });
        }
        Ok(())
    }
}

struct Placeholder {
    id: u32,
    placement: u32,
    row: u32,
    column: u32,
    foreground: Color,
    underline: Option<Color>,
}
impl Placeholder {
    fn read(cell: &alacritty_terminal::term::cell::Cell, previous: Option<&Self>) -> Option<Self> {
        if cell.c != PLACEHOLDER {
            return None;
        }
        let color_id = |color| match color {
            Color::Spec(rgb) => {
                Some(u32::from(rgb.r) << 16 | u32::from(rgb.g) << 8 | u32::from(rgb.b))
            }
            Color::Indexed(i) => Some(u32::from(i)),
            _ => None,
        };
        let low = color_id(cell.fg)?;
        let underline = cell.underline_color();
        let previous = previous.filter(|p| p.foreground == cell.fg && p.underline == underline);
        let diacritics = cell.zerowidth().unwrap_or_default();
        let number = |i| {
            diacritics
                .get(i)
                .and_then(|c| DIACRITICS.binary_search(&(*c as u32)).ok())
                .map(|n| n as u32)
        };
        let row = number(0).or_else(|| previous.map(|p| p.row)).unwrap_or(0);
        let previous = previous.filter(|p| p.row == row);
        let column = number(1)
            .or_else(|| previous.map(|p| p.column + 1))
            .unwrap_or(0);
        let high = number(2)
            .or_else(|| {
                previous
                    .filter(|p| p.column + 1 == column)
                    .map(|p| p.id >> 24)
            })
            .unwrap_or(0);
        if high > 255 {
            return None;
        }
        Some(Self {
            id: low | high << 24,
            placement: underline.and_then(color_id).unwrap_or(0),
            row,
            column,
            foreground: cell.fg,
            underline,
        })
    }
}

// Numeric wire alphabet from the Kitty graphics protocol's Unicode 6.0
// rowcolumn-diacritics.txt. These are protocol codepoints, not font glyphs.
#[cfg(test)]
#[path = "graphics_tests.rs"]
mod tests;

const DIACRITICS: &[u32] = &[
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];
