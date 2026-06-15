pub(super) const DESCRIPTION: &str = concat!(
    "Search file contents by text or regex pattern. ",
    "Use current-directory-relative paths for files and directories under the current directory; ",
    "use absolute paths only outside it."
);
pub(super) const REQUIRED_ARGS: &[&str] = &["pattern"];
