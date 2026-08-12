pub const LIST_DESCRIPTION: &str =
    r#"List delegated tasks and their current status, including completed results when available."#;
pub const WAIT_DESCRIPTION: &str = r#"Wait until all selected delegated tasks finish or the timeout expires. Use this when their results are needed before continuing."#;
pub const SEND_DESCRIPTION: &str = r#"Send additional instructions to a running delegated task. The message is injected at the next safe model-turn boundary."#;
pub const CANCEL_DESCRIPTION: &str = r#"Request cancellation of a running delegated task."#;

pub const NO_REQUIRED_ARGS: &[&str] = &[];
pub const WAIT_REQUIRED_ARGS: &[&str] = &["task_ids"];
pub const SEND_REQUIRED_ARGS: &[&str] = &["task_id", "message"];
pub const CANCEL_REQUIRED_ARGS: &[&str] = &["task_id"];
