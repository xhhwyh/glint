pub(super) const DESCRIPTION: &str = concat!(
    "Run a non-interactive shell command in the visible agent terminal. ",
    "Use for shell-only commands such as git, build/test, package, environment, ",
    "and process commands. Do not use for interactive programs."
);
pub(super) const REQUIRED_ARGS: &[&str] = &["command", "description"];
