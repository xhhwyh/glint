use std::path::Path;
use std::process::Command;

use crate::agent::provider::{ToolCall, ToolResult};

mod description;

use super::{
    ToolBehavior,
    utils::{
        command_result, error, is_protected_path, missing_arg, requires_path_approval, string_arg,
    },
};

pub(super) struct BashTool;

impl ToolBehavior for BashTool {
    fn name(&self) -> &'static str {
        "Bash"
    }

    fn description(&self) -> &'static str {
        description::DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        bash(call, is_cancelled)
    }

    fn execute_approved(
        &self,
        call: &ToolCall,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> ToolResult {
        bash_approved(call, is_cancelled)
    }

    fn requires_approval(
        &self,
        call: &ToolCall,
        bash_prefix_allowed: bool,
        _edit_allowed: bool,
    ) -> bool {
        if requires_path_approval(call) {
            return true;
        }
        string_arg(call, "command")
            .is_none_or(|command| bash_command_requires_approval(command, bash_prefix_allowed))
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "command").map(str::to_owned)
    }

    fn input_description(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "description").map(str::to_owned)
    }
}

fn bash(call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
    let Some(command) = string_arg(call, "command") else {
        return missing_arg(call, "command");
    };

    if bash_requires_approval(command) {
        return error(
            call,
            format!("Approval required before running Bash command: {command}"),
        );
    }

    run_bash(call, command, is_cancelled)
}

fn bash_approved(call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
    let Some(command) = string_arg(call, "command") else {
        return missing_arg(call, "command");
    };

    run_bash(call, command, is_cancelled)
}

fn run_bash(call: &ToolCall, command: &str, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
    if let Some(replacement) = dedicated_tool_replacement(command) {
        return error(
            call,
            format!(
                "Bash command was not run because a dedicated tool should handle this action. {replacement}"
            ),
        );
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    command_result(
        call,
        Command::new(shell).args(["-lc", command]),
        is_cancelled,
    )
}

#[derive(Clone, Debug)]
struct BashCommandAnalysis {
    parse_error: bool,
    has_shell_control: bool,
    has_sensitive_path: bool,
    requires_approval: bool,
    dedicated_tool_replacement: Option<&'static str>,
}

fn analyze_bash_command(command: &str) -> BashCommandAnalysis {
    let words = shlex::split(command);
    let parse_error = words.is_none();
    let words = words.unwrap_or_default();
    let has_shell_control = contains_shell_control(command);
    let has_sensitive_path = command_words_have_sensitive_path(&words);
    let dedicated_tool_replacement = dedicated_tool_replacement_from_words(&words, command);
    let read_only = !parse_error && is_preapproved_read_only_words(&words, has_shell_control);

    BashCommandAnalysis {
        parse_error,
        has_shell_control,
        has_sensitive_path,
        requires_approval: parse_error || (dedicated_tool_replacement.is_none() && !read_only),
        dedicated_tool_replacement,
    }
}

pub(super) fn bash_requires_approval(command: &str) -> bool {
    analyze_bash_command(command).requires_approval
}

pub(super) fn bash_command_requires_approval(command: &str, bash_prefix_allowed: bool) -> bool {
    let analysis = analyze_bash_command(command);
    analysis.parse_error
        || (analysis.dedicated_tool_replacement.is_none()
            && (analysis.has_sensitive_path
                || (analysis.requires_approval
                    && (!bash_prefix_allowed || analysis.has_shell_control))))
}

fn command_words_have_sensitive_path(words: &[String]) -> bool {
    words
        .iter()
        .skip(1)
        .filter(|word| !is_shell_operator(word))
        .any(|word| sensitive_arg(word))
}

fn sensitive_arg(arg: &str) -> bool {
    arg.starts_with('/') || arg.starts_with('~') || arg.contains("..") || is_protected_path(arg)
}

fn is_preapproved_read_only_words(words: &[String], has_shell_control: bool) -> bool {
    if words.is_empty() || has_shell_control {
        return false;
    }

    let program = program_name(&words[0]);
    if words
        .iter()
        .skip(1)
        .filter(|word| !is_shell_operator(word))
        .any(|part| {
            sensitive_arg(part)
                || matches!(
                    part.as_str(),
                    "--hidden" | "--no-ignore" | "--no-ignore-vcs" | "-uuu" | "-uu" | "-a"
                )
        })
    {
        return false;
    }

    match program {
        "git" => matches!(
            words.get(1).map(|word| word.as_str()),
            Some("status" | "rev-parse" | "ls-files")
        ),
        "pwd" => words.len() == 1,
        "which" => words.len() >= 2,
        "rustc" => words.get(1).map(|word| word.as_str()) == Some("--version"),
        _ => false,
    }
}

pub(super) fn contains_shell_control(command: &str) -> bool {
    scan_shell_control(command).has_control || shlex::split(command).is_none()
}

pub(super) fn dedicated_tool_replacement(command: &str) -> Option<&'static str> {
    let words = shlex::split(command)?;
    dedicated_tool_replacement_from_words(&words, command)
}

fn dedicated_tool_replacement_from_words(words: &[String], command: &str) -> Option<&'static str> {
    for (index, word) in words.iter().enumerate() {
        let program = program_name(word);
        let next = words.get(index + 1).map(|word| program_name(word));

        match program {
            "|" | "||" | "&&" | ";" | ">" | ">>" | "<" | "<<" => {}
            "cat" | "head" | "tail" | "less" | "more" => {
                return Some("Use Read to inspect file contents.");
            }
            "sed" | "awk" => {
                return Some("Use Read for inspection or Edit for file modifications.");
            }
            "find" | "fd" | "ls" | "tree" => return Some("Use Glob to find or list files."),
            "grep" | "rg" => return Some("Use Grep to search file contents."),
            "git" if next == Some("grep") => return Some("Use Grep to search file contents."),
            "echo" | "printf" if scan_shell_control(command).has_output_redirect => {
                return Some("Use Edit for file modifications.");
            }
            "echo" | "printf" => {
                return Some("Output text directly to the user instead of running echo or printf.");
            }
            _ => {}
        }
    }

    None
}

fn is_shell_operator(word: &str) -> bool {
    matches!(
        word,
        "|" | "||" | "&&" | ";" | ">" | ">>" | "<" | "<<" | "2>" | "2>>" | "&>"
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ShellControl {
    has_control: bool,
    has_output_redirect: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuoteState {
    None,
    Single,
    Double,
}

fn scan_shell_control(command: &str) -> ShellControl {
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let mut control = ShellControl::default();

    for ch in command.chars() {
        if escaped {
            escaped = false;
            continue;
        }

        match quote {
            QuoteState::None => match ch {
                '\'' => quote = QuoteState::Single,
                '"' => quote = QuoteState::Double,
                '\\' => control.has_control = true,
                '>' => {
                    control.has_control = true;
                    control.has_output_redirect = true;
                }
                '|' | '<' | ';' | '&' | '$' | '`' | '*' | '?' | '[' | ']' | '{' | '}' | '('
                | ')' | '\n' => control.has_control = true,
                _ => {}
            },
            QuoteState::Single => {
                if ch == '\'' {
                    quote = QuoteState::None;
                }
            }
            QuoteState::Double => match ch {
                '"' => quote = QuoteState::None,
                '\\' => {
                    escaped = true;
                    control.has_control = true;
                }
                '$' | '`' | '\n' => control.has_control = true,
                _ => {}
            },
        }
    }

    if quote != QuoteState::None {
        control.has_control = true;
    }

    control
}

fn program_name(word: &str) -> &str {
    Path::new(word)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(word)
}
