use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::agent::provider::ToolResult;

const DEFAULT_MAX_TOOL_RESULT_CHARS: usize = 50_000;
const PREVIEW_CHARS: usize = 2_000;

#[derive(Clone, Debug)]
pub struct ToolResultBudget {
    directory: PathBuf,
    max_chars: usize,
    preview_chars: usize,
}

impl ToolResultBudget {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            max_chars: DEFAULT_MAX_TOOL_RESULT_CHARS,
            preview_chars: PREVIEW_CHARS,
        }
    }

    #[cfg(test)]
    fn with_limits(directory: PathBuf, max_chars: usize, preview_chars: usize) -> Self {
        Self {
            directory,
            max_chars,
            preview_chars,
        }
    }

    pub fn apply(&self, tool_name: &str, result: ToolResult) -> ToolResult {
        if result.content.chars().count() <= self.max_chars {
            return result;
        }

        let ToolResult {
            call_id,
            content,
            is_error,
        } = result;

        match self.persist(tool_name, &call_id, &content) {
            Ok(replacement) => ToolResult {
                call_id,
                content: replacement,
                is_error,
            },
            Err(_) => ToolResult {
                call_id,
                content,
                is_error,
            },
        }
    }

    fn persist(&self, tool_name: &str, call_id: &str, content: &str) -> std::io::Result<String> {
        fs::create_dir_all(&self.directory)?;
        let path = self.directory.join(format!(
            "{}-{}.txt",
            sanitize_segment(call_id),
            sanitize_segment(tool_name)
        ));
        fs::write(&path, content)?;

        Ok(persisted_output_message(
            tool_name,
            content,
            &path,
            self.max_chars,
            self.preview_chars,
        ))
    }
}

fn persisted_output_message(
    tool_name: &str,
    content: &str,
    path: &Path,
    max_chars: usize,
    preview_chars: usize,
) -> String {
    let preview = preview_content(content, preview_chars);
    let char_count = content.chars().count();
    format!(
        "{preview}\n\n<persisted-output>\nFull {tool_name} output was {char_count} characters, exceeding the {max_chars} character tool-result budget. The full output was written to:\n{}\nUse a narrower tool call if you need more focused output.\n</persisted-output>",
        path.display()
    )
}

fn preview_content(content: &str, preview_chars: usize) -> String {
    let mut preview = content.chars().take(preview_chars).collect::<String>();
    if preview.chars().count() < content.chars().count()
        && let Some(index) = preview.rfind('\n')
        && index > preview.len() / 2
    {
        preview.truncate(index);
    }
    preview
}

fn sanitize_segment(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "tool".to_owned()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_small_tool_results_inline() {
        let budget = ToolResultBudget::with_limits(std::env::temp_dir(), 10, 5);
        let result = ToolResult {
            call_id: "call-1".to_owned(),
            content: "small".to_owned(),
            is_error: false,
        };

        assert_eq!(budget.apply("Glob", result).content, "small");
    }

    #[test]
    fn persists_large_tool_results_and_returns_preview() {
        let dir =
            std::env::temp_dir().join(format!("glint-tool-results-test-{}", uuid::Uuid::new_v4()));
        let budget = ToolResultBudget::with_limits(dir.clone(), 10, 8);
        let result = ToolResult {
            call_id: "call/1".to_owned(),
            content: "line one\nline two\nline three".to_owned(),
            is_error: false,
        };

        let result = budget.apply("Glob", result);

        assert!(result.content.contains("<persisted-output>"));
        assert!(result.content.contains("Full Glob output was"));
        assert!(result.content.contains(&dir.display().to_string()));
        assert!(dir.join("call_1-Glob.txt").exists());

        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sanitizes_empty_path_segments() {
        assert_eq!(sanitize_segment(""), "tool");
        assert_eq!(sanitize_segment("call/1 Glob"), "call_1_Glob");
    }
}
