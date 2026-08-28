use std::{path::PathBuf, time::Duration};

pub const COLLAPSED_PREVIEW_EDGE_LINES: usize = 3;
pub const MAX_COLLAPSED_PREVIEW_ROWS: u16 = 7;
pub const MAX_EXPANDED_OUTPUT_ROWS: u16 = MAX_COLLAPSED_PREVIEW_ROWS;
pub const MAX_OUTPUT_PREVIEW_LINE_CHARS: usize = 4_096;
const COMPLETE_PREVIEW_LINE_LIMIT: usize = MAX_COLLAPSED_PREVIEW_ROWS as usize;
pub const HOVER_TRANSITION: Duration = Duration::from_millis(160);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecutionOutputPreview {
    leading_lines: Vec<String>,
    trailing_lines: Vec<String>,
    total_lines: usize,
    content_truncated: bool,
}

impl ExecutionOutputPreview {
    pub fn from_text(text: &str) -> Self {
        let mut preview = Self::default();
        if text.is_empty() {
            return preview;
        }
        for line in text.lines() {
            preview.push_line(line);
        }
        preview
    }

    pub(crate) fn push_line(&mut self, line: impl Into<String>) {
        self.push_line_with_truncation(line, false);
    }

    pub(crate) fn push_line_with_truncation(
        &mut self,
        line: impl Into<String>,
        already_truncated: bool,
    ) {
        let (line, line_truncated) = bounded_preview_line(line.into());
        self.content_truncated |= already_truncated || line_truncated;
        self.total_lines = self.total_lines.saturating_add(1);
        if self.leading_lines.len() < COMPLETE_PREVIEW_LINE_LIMIT {
            self.leading_lines.push(line.clone());
        }
        self.trailing_lines.push(line);
        if self.trailing_lines.len() > COLLAPSED_PREVIEW_EDGE_LINES {
            self.trailing_lines.remove(0);
        }
    }

    pub(crate) fn append(&mut self, other: &Self) {
        if other.total_lines == 0 {
            return;
        }

        let missing_leading = COMPLETE_PREVIEW_LINE_LIMIT.saturating_sub(self.leading_lines.len());
        self.leading_lines
            .extend(other.leading_lines.iter().take(missing_leading).cloned());
        if other.total_lines >= COLLAPSED_PREVIEW_EDGE_LINES {
            self.trailing_lines = other.trailing_lines.clone();
        } else {
            for line in &other.leading_lines {
                self.trailing_lines.push(line.clone());
                if self.trailing_lines.len() > COLLAPSED_PREVIEW_EDGE_LINES {
                    self.trailing_lines.remove(0);
                }
            }
        }
        self.total_lines = self.total_lines.saturating_add(other.total_lines);
        self.content_truncated |= other.content_truncated;
    }

    pub(crate) fn prefix_first_line(&mut self, prefix: &str) {
        let Some(first) = self.leading_lines.first_mut() else {
            return;
        };
        let (prefixed, truncated) = bounded_preview_line(format!("{prefix}{first}"));
        *first = prefixed;
        self.content_truncated |= truncated;
        if self.total_lines <= COLLAPSED_PREVIEW_EDGE_LINES {
            self.trailing_lines[0] = self.leading_lines[0].clone();
        }
    }

    pub fn leading_lines(&self) -> &[String] {
        &self.leading_lines
    }

    pub fn trailing_lines(&self) -> &[String] {
        &self.trailing_lines
    }

    pub fn total_lines(&self) -> usize {
        self.total_lines
    }

    pub fn omitted_lines(&self) -> usize {
        if self.is_abridged() {
            self.total_lines
                .saturating_sub(COLLAPSED_PREVIEW_EDGE_LINES * 2)
        } else {
            0
        }
    }

    pub fn is_abridged(&self) -> bool {
        self.total_lines > COMPLETE_PREVIEW_LINE_LIMIT
    }

    pub fn has_truncated_content(&self) -> bool {
        self.content_truncated
    }

    #[cfg(test)]
    fn retained_line_count(&self) -> usize {
        self.leading_lines.len() + self.trailing_lines.len()
    }
}

fn bounded_preview_line(line: String) -> (String, bool) {
    let mut chars = line.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_OUTPUT_PREVIEW_LINE_CHARS)
        .collect::<String>();
    let truncated = chars.next().is_some();
    (bounded, truncated)
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum ExecutionId {
    Tool(String),
    Task(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionRegion {
    Summary,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionHitbox {
    pub id: ExecutionId,
    pub region: ExecutionRegion,
    pub start_row: u16,
    pub end_row: u16,
    pub start_column: u16,
    pub end_column: u16,
    pub expansion_rows: u16,
    pub max_output_scroll: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionOutputSource {
    Inline(String),
    Persisted(PathBuf),
}

impl ExecutionOutputSource {
    pub fn from_tool_output(output: &str) -> Self {
        persisted_output_marker(output)
            .map(|(path, _, _)| Self::Persisted(path))
            .unwrap_or_else(|| Self::Inline(output.to_owned()))
    }
}

pub fn replace_persisted_output_marker(output: &str, replacement: &str) -> String {
    let Some((_, start, end)) = persisted_output_marker(output) else {
        return output.to_owned();
    };

    format!("{}{}{}", &output[..start], replacement, &output[end..])
}

fn persisted_output_marker(output: &str) -> Option<(PathBuf, usize, usize)> {
    const OPEN_MARKER: &str = "<persisted-output>";
    const CLOSE_MARKER: &str = "</persisted-output>";
    const PATH_INTRODUCTION: &str = "The full output was written to:";

    let mut search_from = 0;
    while let Some(open_offset) = output[search_from..].find(OPEN_MARKER) {
        let start = search_from + open_offset;
        let content_start = start + OPEN_MARKER.len();
        let after_open = &output[content_start..];
        let Some(close_offset) = after_open.find(CLOSE_MARKER) else {
            break;
        };
        let content_end = content_start + close_offset;
        let end = content_end + CLOSE_MARKER.len();
        let marker_content = &output[content_start..content_end];

        if let Some((_, path)) = marker_content.split_once(PATH_INTRODUCTION)
            && let Some(path) = path
                .strip_prefix('\n')
                .or_else(|| path.strip_prefix("\r\n"))
                .and_then(|path| path.lines().next())
                .map(str::trim)
                .filter(|path| !path.is_empty())
        {
            return Some((PathBuf::from(path), start, end));
        }

        search_from = end;
    }

    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn execution_output_preview_keeps_all_seven_lines() {
        let preview = ExecutionOutputPreview::from_text(
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7",
        );

        assert_eq!(preview.total_lines(), 7);
        assert_eq!(preview.omitted_lines(), 0);
        assert_eq!(
            preview.leading_lines(),
            [
                "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7"
            ]
        );
    }

    #[test]
    fn execution_output_preview_keeps_three_lines_from_each_end() {
        let preview = ExecutionOutputPreview::from_text(
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8",
        );

        assert_eq!(preview.total_lines(), 8);
        assert_eq!(preview.omitted_lines(), 2);
        assert_eq!(
            &preview.leading_lines()[..3],
            ["line 1", "line 2", "line 3"]
        );
        assert_eq!(preview.trailing_lines(), ["line 6", "line 7", "line 8"]);
    }

    #[test]
    fn execution_output_preview_composes_without_retaining_the_middle() {
        let mut preview = ExecutionOutputPreview::from_text("line 1\nline 2");
        let suffix = ExecutionOutputPreview::from_text(
            "line 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10",
        );

        preview.append(&suffix);

        assert_eq!(preview.total_lines(), 10);
        assert_eq!(preview.omitted_lines(), 4);
        assert_eq!(
            &preview.leading_lines()[..3],
            ["line 1", "line 2", "line 3"]
        );
        assert_eq!(preview.trailing_lines(), ["line 8", "line 9", "line 10"]);
        assert!(preview.retained_line_count() <= 10);
    }

    #[test]
    fn execution_output_preview_bounds_each_retained_line() {
        let preview = ExecutionOutputPreview::from_text(&"界".repeat(10_000));

        assert_eq!(preview.leading_lines()[0].chars().count(), 4_096);
        assert!(preview.has_truncated_content());
    }

    #[test]
    fn persisted_output_path_uses_the_structured_marker() {
        let output = "preview\n\n<persisted-output>\nFull Bash output was 60000 characters, exceeding the 50000 character tool-result budget. The full output was written to:\n/tmp/tool-results/call-Bash.txt\nUse a narrower tool call if you need more focused output.\n</persisted-output>";

        assert_eq!(
            ExecutionOutputSource::from_tool_output(output),
            ExecutionOutputSource::Persisted(PathBuf::from("/tmp/tool-results/call-Bash.txt"))
        );
    }

    #[test]
    fn persisted_output_path_requires_a_complete_structured_marker() {
        let incomplete = "preview\n<persisted-output>\nThe full output was written to:\n/tmp/tool-results/call-Bash.txt";
        let arbitrary = "The full output was written to:\n/tmp/not-a-marker.txt";

        assert_eq!(
            ExecutionOutputSource::from_tool_output(incomplete),
            ExecutionOutputSource::Inline(incomplete.to_owned())
        );
        assert_eq!(
            ExecutionOutputSource::from_tool_output(arbitrary),
            ExecutionOutputSource::Inline(arbitrary.to_owned())
        );
    }
}
