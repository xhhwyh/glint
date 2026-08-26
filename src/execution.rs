#![allow(dead_code)] // Task 4 renders and routes the execution-card presentation types.

use std::{path::PathBuf, time::Duration};

pub const MAX_EXPANDED_OUTPUT_ROWS: u16 = 8;
pub const HOVER_TRANSITION: Duration = Duration::from_millis(160);

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
