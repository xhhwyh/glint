pub(super) const DESCRIPTION: &str = concat!(
    "Find files by narrow glob pattern. ",
    "Use current-directory-relative paths for directories under the current directory; ",
    "use absolute paths only outside it. ",
    "Returns at most 100 files with a truncation note when more matches exist. ",
    "Common generated, dependency, VCS, and worktree directories are excluded by default. ",
    "Searches time out after 20 seconds, or 60 seconds on WSL; ",
    "set GLINT_GLOB_TIMEOUT_SECONDS to override."
);
pub(super) const REQUIRED_ARGS: &[&str] = &["pattern"];
