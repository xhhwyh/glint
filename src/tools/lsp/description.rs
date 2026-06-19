pub(super) const DESCRIPTION: &str = concat!(
    "Query configured language-server semantic information. ",
    "Use this for symbol-aware questions such as definitions, references, hover docs, ",
    "document symbols, and workspace symbols. Operations are goToDefinition, ",
    "findReferences, hover, documentSymbol, and workspaceSymbol. Use Grep for plain text search."
);
pub(super) const REQUIRED_ARGS: &[&str] = &["operation"];
