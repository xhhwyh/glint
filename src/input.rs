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

    pub fn newline(&mut self) {
        self.push('\n');
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
}

fn byte_index_for_column(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}
