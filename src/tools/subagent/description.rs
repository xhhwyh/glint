pub const DESCRIPTION: &str = r#"Launch a Codex subagent in a visible terminal tab for delegated work.

Use this when a task is useful to hand off to a separate Codex agent while continuing the main conversation. The subagent runs in the requested working directory and its live transcript appears in a bottom terminal tab. The tool returns after the subagent starts. Do not poll or predict the result; when it completes, its final answer is added to the model context automatically.
"#;

pub const REQUIRED_ARGS: &[&str] = &["description", "prompt"];
