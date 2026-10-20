//! Forward VT operations unchanged, observing the operations that move images.
//! This adapter avoids forking the hash-pinned terminal crate. In particular,
//! scrolls are observed even after scrollback is full and inside scroll regions.
use super::graphics::{Graphics, PLACEHOLDER};
use alacritty_terminal::vte::ansi::cursor_icon::CursorIcon;
use alacritty_terminal::{
    event::EventListener,
    grid::{Dimensions, GridCell},
    term::{Term, TermMode},
    vte::ansi::*,
};
use unicode_width::UnicodeWidthChar;

pub(super) struct GraphicsHandler<'a, T> {
    pub(super) term: &'a mut Term<T>,
    pub(super) graphics: &'a mut Graphics,
}
impl<T: EventListener> GraphicsHandler<'_, T> {
    fn alt(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }
    fn linefeed_scroll(&mut self) {
        if self.term.grid().cursor.point.line.0 + 1 == self.graphics.region.end {
            self.graphics
                .scroll(self.alt(), self.graphics.region.clone(), 1);
        }
    }
    fn scroll(&mut self, origin: i32, count: usize, down: bool) {
        let end = self.graphics.region.end;
        let count = count.min((end - origin).max(0) as usize) as i32;
        self.graphics
            .scroll(self.alt(), origin..end, if down { -count } else { count });
    }
    fn private(&mut self, mode: PrivateMode, set: bool) {
        let alt = self.alt();
        if set {
            self.term.set_private_mode(mode);
        } else {
            self.term.unset_private_mode(mode);
        }
        if self.alt() != alt {
            self.graphics.swap_screen();
        }
        if mode.raw() == 3 {
            self.graphics.region = 0..self.term.screen_lines() as i32;
            self.graphics.clear(self.alt(), &ClearMode::All);
        }
    }
}
impl<T: EventListener> Handler for GraphicsHandler<'_, T> {
    fn set_title(&mut self, a0: Option<String>) {
        self.term.set_title(a0);
    }
    fn set_cursor_style(&mut self, a0: Option<CursorStyle>) {
        self.term.set_cursor_style(a0);
    }
    fn set_cursor_shape(&mut self, a0: CursorShape) {
        self.term.set_cursor_shape(a0);
    }
    fn goto(&mut self, a0: i32, a1: usize) {
        self.term.goto(a0, a1);
    }
    fn goto_line(&mut self, a0: i32) {
        self.term.goto_line(a0);
    }
    fn goto_col(&mut self, a0: usize) {
        self.term.goto_col(a0);
    }
    fn insert_blank(&mut self, a0: usize) {
        self.term.insert_blank(a0);
    }
    fn move_up(&mut self, a0: usize) {
        self.term.move_up(a0);
    }
    fn move_down(&mut self, a0: usize) {
        self.term.move_down(a0);
    }
    fn identify_terminal(&mut self, a0: Option<char>) {
        self.term.identify_terminal(a0);
    }
    fn device_status(&mut self, a0: usize) {
        self.term.device_status(a0);
    }
    fn move_forward(&mut self, a0: usize) {
        self.term.move_forward(a0);
    }
    fn move_backward(&mut self, a0: usize) {
        self.term.move_backward(a0);
    }
    fn move_down_and_cr(&mut self, a0: usize) {
        self.term.move_down_and_cr(a0);
    }
    fn move_up_and_cr(&mut self, a0: usize) {
        self.term.move_up_and_cr(a0);
    }
    fn put_tab(&mut self, a0: u16) {
        self.term.put_tab(a0);
    }
    fn backspace(&mut self) {
        self.term.backspace();
    }
    fn carriage_return(&mut self) {
        self.term.carriage_return();
    }
    fn bell(&mut self) {
        self.term.bell();
    }
    fn substitute(&mut self) {
        self.term.substitute();
    }
    fn set_horizontal_tabstop(&mut self) {
        self.term.set_horizontal_tabstop();
    }
    fn erase_chars(&mut self, a0: usize) {
        self.term.erase_chars(a0);
    }
    fn delete_chars(&mut self, a0: usize) {
        self.term.delete_chars(a0);
    }
    fn move_backward_tabs(&mut self, a0: u16) {
        self.term.move_backward_tabs(a0);
    }
    fn move_forward_tabs(&mut self, a0: u16) {
        self.term.move_forward_tabs(a0);
    }
    fn save_cursor_position(&mut self) {
        self.term.save_cursor_position();
    }
    fn restore_cursor_position(&mut self) {
        self.term.restore_cursor_position();
    }
    fn clear_line(&mut self, a0: LineClearMode) {
        self.term.clear_line(a0);
    }
    fn clear_tabs(&mut self, a0: TabulationClearMode) {
        self.term.clear_tabs(a0);
    }
    fn set_tabs(&mut self, a0: u16) {
        self.term.set_tabs(a0);
    }
    fn terminal_attribute(&mut self, a0: Attr) {
        self.term.terminal_attribute(a0);
    }
    fn set_mode(&mut self, a0: Mode) {
        self.term.set_mode(a0);
    }
    fn unset_mode(&mut self, a0: Mode) {
        self.term.unset_mode(a0);
    }
    fn report_mode(&mut self, a0: Mode) {
        self.term.report_mode(a0);
    }
    fn report_private_mode(&mut self, a0: PrivateMode) {
        self.term.report_private_mode(a0);
    }
    fn set_keypad_application_mode(&mut self) {
        self.term.set_keypad_application_mode();
    }
    fn unset_keypad_application_mode(&mut self) {
        self.term.unset_keypad_application_mode();
    }
    fn set_active_charset(&mut self, a0: CharsetIndex) {
        self.term.set_active_charset(a0);
    }
    fn configure_charset(&mut self, a0: CharsetIndex, a1: StandardCharset) {
        self.term.configure_charset(a0, a1);
    }
    fn set_color(&mut self, a0: usize, a1: Rgb) {
        self.term.set_color(a0, a1);
    }
    fn dynamic_color_sequence(&mut self, a0: String, a1: usize, a2: &str) {
        self.term.dynamic_color_sequence(a0, a1, a2);
    }
    fn reset_color(&mut self, a0: usize) {
        self.term.reset_color(a0);
    }
    fn clipboard_store(&mut self, a0: u8, a1: &[u8]) {
        self.term.clipboard_store(a0, a1);
    }
    fn clipboard_load(&mut self, a0: u8, a1: &str) {
        self.term.clipboard_load(a0, a1);
    }
    fn push_title(&mut self) {
        self.term.push_title();
    }
    fn pop_title(&mut self) {
        self.term.pop_title();
    }
    fn text_area_size_pixels(&mut self) {
        self.term.text_area_size_pixels();
    }
    fn text_area_size_chars(&mut self) {
        self.term.text_area_size_chars();
    }
    fn set_hyperlink(&mut self, a0: Option<Hyperlink>) {
        self.term.set_hyperlink(a0);
    }
    fn set_mouse_cursor_icon(&mut self, a0: CursorIcon) {
        self.term.set_mouse_cursor_icon(a0);
    }
    fn report_keyboard_mode(&mut self) {
        self.term.report_keyboard_mode();
    }
    fn push_keyboard_mode(&mut self, a0: KeyboardModes) {
        self.term.push_keyboard_mode(a0);
    }
    fn pop_keyboard_modes(&mut self, a0: u16) {
        self.term.pop_keyboard_modes(a0);
    }
    fn set_keyboard_mode(&mut self, a0: KeyboardModes, a1: KeyboardModesApplyBehavior) {
        self.term.set_keyboard_mode(a0, a1);
    }
    fn set_modify_other_keys(&mut self, a0: ModifyOtherKeys) {
        self.term.set_modify_other_keys(a0);
    }
    fn report_modify_other_keys(&mut self) {
        self.term.report_modify_other_keys();
    }
    fn set_scp(&mut self, a0: ScpCharPath, a1: ScpUpdateMode) {
        self.term.set_scp(a0, a1);
    }
    fn input(&mut self, c: char) {
        let width = c.width().unwrap_or(0);
        if width == 0 {
            let grid = self.term.grid();
            let mut col = grid.cursor.point.column;
            if !grid.cursor.input_needs_wrap {
                col.0 = col.0.saturating_sub(1);
            }
            let cell = &grid[grid.cursor.point.line][col];
            // Only three diacritics carry graphics metadata. Do not accumulate
            // unbounded combining text on a placeholder that never advances.
            if cell.c == PLACEHOLDER && cell.zerowidth().is_some_and(|z| z.len() >= 3) {
                return;
            }
        } else if self.term.mode().contains(TermMode::LINE_WRAP) {
            let grid = self.term.grid();
            let wraps = grid.cursor.input_needs_wrap
                || (width == 2 && grid.cursor.point.column.0 + 1 >= grid.columns());
            if wraps {
                self.linefeed_scroll();
            }
        }
        self.term.input(c);
    }
    fn linefeed(&mut self) {
        self.linefeed_scroll();
        self.term.linefeed();
    }
    fn newline(&mut self) {
        self.linefeed_scroll();
        self.term.newline();
    }
    fn scroll_up(&mut self, count: usize) {
        self.scroll(self.graphics.region.start, count, false);
        self.term.scroll_up(count);
    }
    fn scroll_down(&mut self, count: usize) {
        self.scroll(self.graphics.region.start, count, true);
        self.term.scroll_down(count);
    }
    fn insert_blank_lines(&mut self, count: usize) {
        let line = self.term.grid().cursor.point.line.0;
        if self.graphics.region.contains(&line) {
            self.scroll(line, count, true);
        }
        self.term.insert_blank_lines(count);
    }
    fn delete_lines(&mut self, count: usize) {
        let line = self.term.grid().cursor.point.line.0;
        if self.graphics.region.contains(&line) {
            self.scroll(line, count, false);
        }
        self.term.delete_lines(count);
    }
    fn reverse_index(&mut self) {
        if self.term.grid().cursor.point.line.0 == self.graphics.region.start {
            self.scroll(self.graphics.region.start, 1, true);
        }
        self.term.reverse_index();
    }
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        let end = bottom.unwrap_or(self.term.screen_lines());
        if top < end {
            self.graphics.region = (top as i32 - 1).min(self.term.screen_lines() as i32)
                ..end.min(self.term.screen_lines()) as i32;
        }
        self.term.set_scrolling_region(top, bottom);
    }
    fn reset_state(&mut self) {
        self.graphics.reset(self.term.screen_lines());
        self.term.reset_state();
    }
    fn set_private_mode(&mut self, mode: PrivateMode) {
        self.private(mode, true);
    }
    fn unset_private_mode(&mut self, mode: PrivateMode) {
        self.private(mode, false);
    }
    fn clear_screen(&mut self, mode: ClearMode) {
        self.graphics.clear(self.alt(), &mode);
        if matches!(mode, ClearMode::All) && !self.alt() {
            // Match Grid::clear_viewport's actual scroll, not history growth.
            let grid = self.term.grid();
            let lines = (0..grid.screen_lines())
                .rev()
                .find(|line| {
                    grid[alacritty_terminal::index::Line(*line as i32)][..]
                        .iter()
                        .any(|cell| !cell.is_empty())
                })
                .map_or(0, |line| line + 1);
            self.graphics
                .scroll(false, 0..grid.screen_lines() as i32, lines as i32);
        }
        self.term.clear_screen(mode);
    }
    fn decaln(&mut self) {
        self.graphics.clear(self.alt(), &ClearMode::All);
        self.term.decaln();
    }
}
