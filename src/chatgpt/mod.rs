mod auth;
#[cfg(test)]
pub use auth::CodexModel;
pub use auth::cached_reasoning_options;
pub use auth::{LoginEvent, LoginHandle, cached_context_window, credentials};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReasoningOptions {
    pub default_effort: Option<String>,
    pub levels: Vec<ReasoningLevel>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReasoningLevel {
    pub effort: String,
    pub description: String,
}
