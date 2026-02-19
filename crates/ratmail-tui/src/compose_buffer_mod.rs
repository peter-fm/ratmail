use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::{
    ComposeBuffer, UiTheme, char_index_from_row_col, char_to_byte_idx, cursor_line_col,
    remove_char_at, replace_range_chars, text_char_len, wrapped_cursor_pos, wrapped_rows,
};

impl ComposeBuffer {
    pub(crate) fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            cursor: 0,
            tab_len: 4,
            scroll_top: 0,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(crate) fn cursor(&self) -> (usize, usize) {
        cursor_line_col(&self.text, self.cursor)
    }

    pub(crate) fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor = char_index_from_row_col(&self.text, row, col);
    }

    pub(crate) fn lines(&self) -> Vec<&str> {
        self.text.split('\n').collect()
    }

    pub(crate) fn update_scroll(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        let (row, _col) = self.cursor();
        if row < self.scroll_top {
            self.scroll_top = row;
        } else if self.scroll_top + height <= row {
            self.scroll_top = row + 1 - height;
        }
    }

    pub(crate) fn scroll_top(&self) -> usize {
        self.scroll_top
    }

    pub(crate) fn insert_char(&mut self, ch: char) {
        if ch == '\n' || ch == '\r' {
            self.insert_newline();
            return;
        }
        let idx = char_to_byte_idx(&self.text, self.cursor);
        self.text.insert(idx, ch);
        self.cursor = self.cursor.saturating_add(1);
    }

    pub(crate) fn insert_newline(&mut self) {
        let idx = char_to_byte_idx(&self.text, self.cursor);
        self.text.insert(idx, '\n');
        self.cursor = self.cursor.saturating_add(1);
    }

    pub(crate) fn delete_prev_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let idx = self.cursor.saturating_sub(1);
        remove_char_at(&mut self.text, idx);
        self.cursor = idx;
    }

    pub(crate) fn delete_next_char(&mut self) {
        if self.cursor >= text_char_len(&self.text) {
            return;
        }
        remove_char_at(&mut self.text, self.cursor);
    }

    pub(crate) fn move_cursor_back(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub(crate) fn move_cursor_forward(&mut self) {
        if self.cursor < text_char_len(&self.text) {
            self.cursor += 1;
        }
    }

    pub(crate) fn move_cursor_head(&mut self) {
        let (row, _col) = self.cursor();
        self.cursor = char_index_from_row_col(&self.text, row, 0);
    }

    pub(crate) fn move_cursor_end(&mut self) {
        let (row, _col) = self.cursor();
        let line = self.line_at(row).unwrap_or("");
        let col = line.chars().count();
        self.cursor = char_index_from_row_col(&self.text, row, col);
    }

    pub(crate) fn move_cursor_up(&mut self) {
        let (row, col) = self.cursor();
        if row == 0 {
            return;
        }
        let prev_line = self.line_at(row - 1).unwrap_or("");
        let prev_len = prev_line.chars().count();
        self.cursor = char_index_from_row_col(&self.text, row - 1, col.min(prev_len));
    }

    pub(crate) fn move_cursor_word_forward(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut idx = self.cursor.min(chars.len());
        while idx < chars.len() && !chars[idx].is_whitespace() {
            idx += 1;
        }
        while idx < chars.len() && chars[idx].is_whitespace() {
            idx += 1;
        }
        self.cursor = idx.min(chars.len());
    }

    pub(crate) fn move_cursor_word_back(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        if chars.is_empty() {
            return;
        }
        let mut idx = self.cursor.min(chars.len());
        if idx > 0 {
            idx -= 1;
        }
        while idx > 0 && chars[idx].is_whitespace() {
            idx = idx.saturating_sub(1);
        }
        while idx > 0 && !chars[idx - 1].is_whitespace() {
            idx -= 1;
        }
        self.cursor = idx;
    }

    pub(crate) fn delete_line_by_end(&mut self) {
        let (row, _col) = self.cursor();
        let start = char_index_from_row_col(&self.text, row, 0);
        let line = self.line_at(row).unwrap_or("");
        let mut end = start + line.chars().count();
        let text_len = text_char_len(&self.text);
        if end < text_len {
            end = end.saturating_add(1);
        }
        replace_range_chars(&mut self.text, start, end, "");
        if self.text.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = start.min(text_char_len(&self.text));
        }
    }

    pub(crate) fn line_at(&self, row: usize) -> Option<&str> {
        self.text.split('\n').nth(row)
    }

    pub(crate) fn input(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT)
        {
            return false;
        }
        match key.code {
            KeyCode::Char('\t') | KeyCode::Tab => {
                self.insert_char('\t');
                true
            }
            KeyCode::Char(c) => {
                self.insert_char(c);
                true
            }
            KeyCode::Enter => {
                self.insert_newline();
                true
            }
            KeyCode::Backspace => {
                self.delete_prev_char();
                true
            }
            KeyCode::Delete => {
                self.delete_next_char();
                true
            }
            KeyCode::Left => {
                self.move_cursor_back();
                true
            }
            KeyCode::Right => {
                self.move_cursor_forward();
                true
            }
            KeyCode::Home => {
                self.move_cursor_head();
                true
            }
            KeyCode::End => {
                self.move_cursor_end();
                true
            }
            _ => false,
        }
    }

    pub(crate) fn cursor_screen_position(&self, area: Rect) -> Option<(u16, u16)> {
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let lines = self.lines();
        if lines.is_empty() {
            return None;
        }
        let cursor_row = self.cursor().0.min(lines.len().saturating_sub(1));
        if cursor_row < self.scroll_top {
            return None;
        }
        let width = area.width as usize;
        let mut y = 0usize;
        for line in lines.iter().take(cursor_row).skip(self.scroll_top) {
            y = y.saturating_add(wrapped_rows(line, width, self.tab_len));
        }
        let cursor_col = self.cursor().1;
        let (row_offset, col_offset) =
            wrapped_cursor_pos(lines[cursor_row], cursor_col, width, self.tab_len);
        y = y.saturating_add(row_offset);
        if y >= area.height as usize {
            return None;
        }
        let x = col_offset.min(width.saturating_sub(1));
        Some((area.x + x as u16, area.y + y as u16))
    }
}

pub(crate) fn compose_buffer_from_body(_theme: Arc<UiTheme>, body: &str) -> ComposeBuffer {
    ComposeBuffer::new(body)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::compose_buffer_mod::ComposeBuffer;

    #[test]
    fn delete_line_by_end_removes_current_line() {
        let mut buf = ComposeBuffer::new("one\ntwo\nthree");
        buf.set_cursor(1, 1);
        buf.delete_line_by_end();
        assert_eq!(buf.text(), "one\nthree");
        assert_eq!(buf.cursor(), (1, 0));
    }

    #[test]
    fn word_navigation_moves_across_whitespace() {
        let mut buf = ComposeBuffer::new("alpha  beta gamma");
        buf.move_cursor_word_forward();
        assert_eq!(buf.cursor(), (0, 7));
        buf.move_cursor_word_forward();
        assert_eq!(buf.cursor(), (0, 12));
        buf.move_cursor_word_back();
        assert_eq!(buf.cursor(), (0, 7));
    }

    #[test]
    fn input_inserts_and_deletes_chars() {
        let mut buf = ComposeBuffer::new("");
        let _ = buf.input(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let _ = buf.input(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(buf.text(), "ab");
        let _ = buf.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(buf.text(), "a");
    }
}
