pub(super) const DESCRIPTION: &str = concat!(
    "Request approval to replace one exact string in a UTF-8 text file. ",
    "Use current-directory-relative paths for files under the current directory; ",
    "use absolute paths only outside it."
);
pub(super) const REQUIRED_ARGS: &[&str] = &["file_path", "old_string", "new_string"];
