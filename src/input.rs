#[derive(Default)]
pub struct InputState {
    pub value: String,
}

impl InputState {
    pub fn push(&mut self, char: char) {
        self.value.push(char);
    }

    pub fn backspace(&mut self) {
        self.value.pop();
    }

    pub fn take_trimmed(&mut self) -> String {
        let value = self.value.trim().to_owned();
        self.value.clear();
        value
    }
}
