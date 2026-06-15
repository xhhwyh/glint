pub(super) const DESCRIPTION: &str = concat!(
    "Read a UTF-8 text file. ",
    "Use current-directory-relative paths for files under the current directory; ",
    "use absolute paths only outside it."
);
pub(super) const REQUIRED_ARGS: &[&str] = &["file_path"];
