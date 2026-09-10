use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelRole {
    System,
    User,
    Assistant,
    Tool,
}

impl ModelRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct ReasoningData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Vec<Value>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ProviderReasoning {
    pub provider: String,
    pub model: String,
    #[serde(flatten)]
    pub data: ReasoningData,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelMessage {
    pub reasoning: Option<ProviderReasoning>,
    pub role: ModelRole,
    pub content: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

impl ModelMessage {
    pub fn with_reasoning(mut self, reasoning: Option<ProviderReasoning>) -> Self {
        self.reasoning = reasoning;
        self
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(ModelRole::System, Some(content.into()))
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(ModelRole::User, Some(content.into()))
    }

    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            reasoning: None,
            role: ModelRole::Assistant,
            content,
            tool_call_id: None,
            tool_calls,
        }
    }

    pub fn tool_result(result: &ToolResult) -> Self {
        Self {
            reasoning: None,
            role: ModelRole::Tool,
            content: Some(result.content.clone()),
            tool_call_id: Some(result.call_id.clone()),
            tool_calls: Vec::new(),
        }
    }

    fn new(role: ModelRole, content: Option<String>) -> Self {
        Self {
            reasoning: None,
            role,
            content,
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelResponse {
    pub reasoning: Option<ProviderReasoning>,
    pub assistant_text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: Option<super::TokenUsage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRequest {
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: Option<u32>,
}

pub trait ModelProvider {
    fn complete(&mut self, request: ModelRequest) -> Result<ModelResponse>;

    fn stream(
        &mut self,
        request: ModelRequest,
        on_delta: &mut dyn FnMut(String),
    ) -> Result<ModelResponse> {
        let response = self.complete(request)?;
        if let Some(text) = response
            .assistant_text
            .as_ref()
            .filter(|text| !text.is_empty())
        {
            on_delta(text.clone());
        }
        Ok(response)
    }
}

/// Select the transport once; every provider uses Glint's orchestration.
pub enum ConfiguredProvider {
    OpenAi(super::openai::OpenAiProvider),
    ChatGpt(super::chatgpt::ChatGptProvider),
}
impl ConfiguredProvider {
    pub fn new(config: crate::config::LlmConfig) -> Self {
        if config.provider == crate::config::CHATGPT_PROVIDER_ID {
            Self::ChatGpt(super::chatgpt::ChatGptProvider::new(config))
        } else {
            Self::OpenAi(super::openai::OpenAiProvider::new(config))
        }
    }
}
impl ModelProvider for ConfiguredProvider {
    fn complete(&mut self, request: ModelRequest) -> Result<ModelResponse> {
        match self {
            Self::OpenAi(provider) => provider.complete(request),
            Self::ChatGpt(provider) => provider.complete(request),
        }
    }
    fn stream(
        &mut self,
        request: ModelRequest,
        on_delta: &mut dyn FnMut(String),
    ) -> Result<ModelResponse> {
        match self {
            Self::OpenAi(provider) => provider.stream(request, on_delta),
            Self::ChatGpt(provider) => provider.stream(request, on_delta),
        }
    }
}
