pub const DESCRIPTION: &str = r#"Launch a read-only Codex subagent for substantial, self-contained work that can run independently.

Prefer delegation for parallel investigation, review, verification, and focused repository research. Do not use it for trivial work, tightly coupled steps, duplicate work, or tasks requiring edits, approvals, or nested delegation. The subagent runs in the requested working directory and its live transcript is preserved with the task. The tool returns after the subagent starts. Use TaskList for occasional status checks, TaskWait when the result is needed, TaskSend to refine the task, and TaskCancel to stop obsolete work.
"#;

pub const REQUIRED_ARGS: &[&str] = &["description", "prompt"];
