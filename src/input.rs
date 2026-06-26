#[derive(Default, Clone, Debug)]
pub struct InputState {
    pub value: String,
    pub cursor: usize,
    pub history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
}

impl InputState {
    pub fn push(&mut self, char: char) {
        self.value.insert(self.cursor, char);
        self.cursor += char.len_utf8();
        self.history_index = None;
    }

    pub fn backspace(&mut self) {
        if let Some((index, _)) = self.value[..self.cursor].char_indices().next_back() {
            self.value.drain(index..self.cursor);
            self.cursor = index;
            self.history_index = None;
        }
    }

    pub fn delete_forward(&mut self) {
        if let Some(char) = self.value[self.cursor..].chars().next() {
            let end = self.cursor + char.len_utf8();
            self.value.drain(self.cursor..end);
            self.history_index = None;
        }
    }

    pub fn delete_range(&mut self, start: usize, end: usize) {
        if start >= end
            || end > self.value.len()
            || !self.value.is_char_boundary(start)
            || !self.value.is_char_boundary(end)
        {
            return;
        }
        self.value.drain(start..end);
        self.cursor = start;
        self.history_index = None;
    }

    pub fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        if start > end
            || end > self.value.len()
            || !self.value.is_char_boundary(start)
            || !self.value.is_char_boundary(end)
        {
            return;
        }
        self.value.replace_range(start..end, replacement);
        self.cursor = start + replacement.len();
        self.history_index = None;
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.len();
        self.history_index = None;
    }

    pub fn move_left(&mut self) {
        if let Some((index, _)) = self.value[..self.cursor].char_indices().next_back() {
            self.cursor = index;
        }
    }

    pub fn move_right(&mut self) {
        if let Some(char) = self.value[self.cursor..].chars().next() {
            self.cursor += char.len_utf8();
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor == self.value.len() {
            self.show_history(-1);
        } else {
            self.move_vertical(-1);
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor == self.value.len() {
            self.show_history(1);
        } else {
            self.move_vertical(1);
        }
    }

    pub fn take_trimmed(&mut self) -> String {
        let value = self.value.trim().to_owned();
        if !value.is_empty() {
            self.history.push(value.clone());
        }
        self.value.clear();
        self.cursor = 0;
        self.history_index = None;
        value
    }

    fn move_vertical(&mut self, direction: isize) {
        let lines: Vec<&str> = self.value.split('\n').collect();
        let (column, row) = self.cursor_column_row();
        let next_row = row.saturating_add_signed(direction).min(lines.len() - 1);
        self.cursor = lines
            .iter()
            .take(next_row)
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + byte_index_for_column(lines[next_row], column);
    }

    fn cursor_column_row(&self) -> (usize, usize) {
        let before = &self.value[..self.cursor];
        let row = before.chars().filter(|char| *char == '\n').count();
        let column = before.rsplit('\n').next().unwrap_or("").chars().count();
        (column, row)
    }

    fn show_history(&mut self, direction: isize) {
        if self.history.is_empty() {
            return;
        }

        if self.history_index.is_none() {
            self.history_draft = self.value.clone();
        }

        let current = self.history_index.unwrap_or(self.history.len());
        let next = current
            .saturating_add_signed(direction)
            .min(self.history.len());
        self.history_index = (next < self.history.len()).then_some(next);
        self.value = self
            .history_index
            .map(|index| self.history[index].clone())
            .unwrap_or_else(|| self.history_draft.clone());
        self.cursor = self.value.len();
    }

    pub fn visual_position_byte_index(
        &self,
        target_row: usize,
        target_column: usize,
        width: usize,
    ) -> usize {
        let mut row = 0;
        let mut column = 0;

        for (index, char) in self.value.char_indices() {
            if char == '\n' {
                if row == target_row {
                    return index;
                }
                row += 1;
                column = 0;
                continue;
            }

            let char_width = unicode_width::UnicodeWidthChar::width(char).unwrap_or(0);
            if column + char_width > width && column > 0 {
                if row == target_row {
                    return index;
                }
                row += 1;
                column = 0;
            }

            if row == target_row && target_column <= column {
                return index;
            }
            column += char_width;
        }

        self.value.len()
    }
}

fn byte_index_for_column(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_position_byte_index_tracks_single_line() {
        let mut input = InputState::default();
        input.set("hello");

        assert_eq!(input.visual_position_byte_index(0, 3, 20), 3);
    }

    #[test]
    fn visual_position_byte_index_tracks_newlines() {
        let mut input = InputState::default();
        input.set("hello\nworld");

        assert_eq!(
            input.visual_position_byte_index(1, 2, 20),
            "hello\nwo".len()
        );
    }

    #[test]
    fn visual_position_byte_index_tracks_wrapped_rows() {
        let mut input = InputState::default();
        input.set("abcdef");

        assert_eq!(input.visual_position_byte_index(1, 1, 3), 4);
    }

    #[test]
    fn visual_position_byte_index_handles_wide_characters() {
        let mut input = InputState::default();
        input.set("a中b");

        assert_eq!(input.visual_position_byte_index(0, 2, 20), "a中".len());
    }

    #[test]
    fn delete_forward_removes_character_after_cursor() {
        let mut input = InputState::default();
        input.set("hello");
        input.cursor = 1;

        input.delete_forward();

        assert_eq!(input.value, "hllo");
        assert_eq!(input.cursor, 1);
    }

    #[test]
    fn delete_range_removes_selected_text_and_moves_cursor_to_start() {
        let mut input = InputState::default();
        input.set("hello");

        input.delete_range(1, 4);

        assert_eq!(input.value, "ho");
        assert_eq!(input.cursor, 1);
    }

    #[test]
    fn replace_range_replaces_selected_text() {
        let mut input = InputState::default();
        input.set("hello");

        input.replace_range(1, 4, "i");

        assert_eq!(input.value, "hio");
        assert_eq!(input.cursor, 2);
    }
}
