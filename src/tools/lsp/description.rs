pub(super) const DESCRIPTION: &str = concat!(
    "Query Rust language-server semantic information through rust-analyzer. ",
    "Use this for symbol-aware questions such as definitions, references, hover docs, ",
    "document symbols, and workspace symbols. Use Grep for plain text search."
);
pub(super) const REQUIRED_ARGS: &[&str] = &["operation"];
