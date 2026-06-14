use std::io::{BufRead, BufReader};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::LlmConfig;

use super::{
    TokenUsage,
    provider::{
        FinishReason, ModelMessage, ModelProvider, ModelRequest, ModelResponse, ToolCall, ToolSpec,
    },
};

const MAX_STREAM_TOOL_CALLS: usize = 16;
const MAX_STREAM_TOOL_ARGUMENT_BYTES: usize = 64 * 1024;

pub struct OpenAiProvider {
    config: LlmConfig,
}

impl OpenAiProvider {
    pub fn new(config: LlmConfig) -> Self {
        Self { config }
    }
}

impl ModelProvider for OpenAiProvider {
    fn complete(&mut self, request: ModelRequest) -> Result<ModelResponse> {
        complete_chat(&self.config, request)
    }

    fn stream(
        &mut self,
        request: ModelRequest,
        on_delta: &mut dyn FnMut(String),
    ) -> Result<ModelResponse> {
        stream_chat(&self.config, request, on_delta)
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ChatTool>>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(Serialize)]
struct ChatTool {
    r#type: &'static str,
    function: ChatToolFunction,
}

#[derive(Serialize)]
struct ChatToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ChatToolCall {
    id: String,
    r#type: String,
    function: ChatToolCallFunction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ChatToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct StreamResponse {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Deserialize)]
struct StreamToolCall {
    index: usize,
    id: Option<String>,
    r#type: Option<String>,
    function: Option<StreamToolCallFunction>,
}

#[derive(Deserialize)]
struct StreamToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
}

impl From<OpenAiUsage> for TokenUsage {
    fn from(usage: OpenAiUsage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cached_prompt_tokens: usage
                .prompt_tokens_details
                .map(|details| details.cached_tokens),
        }
    }
}

#[derive(Default)]
struct StreamingState {
    saw_done: bool,
    assistant_text: String,
    tool_calls: Vec<StreamingToolCall>,
    finish_reason: Option<String>,
    usage: Option<TokenUsage>,
}

#[derive(Default)]
struct StreamingToolCall {
    id: String,
    r#type: String,
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ChatToolCall>>,
}

fn complete_chat(config: &LlmConfig, request: ModelRequest) -> Result<ModelResponse> {
    let request = ChatRequest {
        model: config.model.clone(),
        messages: request
            .messages
            .iter()
            .map(chat_message_from_model)
            .collect::<Result<Vec<_>>>()?,
        temperature: config.temperature,
        max_tokens: request.max_tokens.unwrap_or(config.max_tokens),
        stream: None,
        stream_options: None,
        tools: chat_tools(request.tools),
    };

    let response: ChatResponse = ureq::post(&format!("{}/chat/completions", config.base_url))
        .set("Authorization", &format!("Bearer {}", config.api_key))
        .set("Content-Type", "application/json")
        .send_json(serde_json::to_value(request)?)
        .context("request failed")?
        .into_json()
        .context("invalid response")?;

    let choice = response
        .choices
        .into_iter()
        .next()
        .context("response did not include a choice")?;

    model_response_from_choice(choice, response.usage.map(TokenUsage::from))
}

fn stream_chat(
    config: &LlmConfig,
    request: ModelRequest,
    on_delta: &mut dyn FnMut(String),
) -> Result<ModelResponse> {
    let request = ChatRequest {
        model: config.model.clone(),
        messages: request
            .messages
            .iter()
            .map(chat_message_from_model)
            .collect::<Result<Vec<_>>>()?,
        temperature: config.temperature,
        max_tokens: request.max_tokens.unwrap_or(config.max_tokens),
        stream: Some(true),
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        tools: chat_tools(request.tools),
    };

    let response = ureq::post(&format!("{}/chat/completions", config.base_url))
        .set("Authorization", &format!("Bearer {}", config.api_key))
        .set("Content-Type", "application/json")
        .send_json(serde_json::to_value(request)?)
        .context("request failed")?;

    let mut state = StreamingState::default();
    for line in BufReader::new(response.into_reader()).lines() {
        let line = line.context("failed to read streaming response")?;
        let Some(payload) = line
            .strip_prefix("data:")
            .map(|payload| payload.strip_prefix(' ').unwrap_or(payload))
        else {
            continue;
        };
        if payload == "[DONE]" {
            state.saw_done = true;
            break;
        }
        if payload.trim().is_empty() {
            continue;
        }

        let chunk: StreamResponse = serde_json::from_str(payload).with_context(|| {
            format!(
                "invalid streaming response chunk: {}",
                truncate_chunk(payload)
            )
        })?;
        state.apply_chunk(chunk, on_delta)?;
    }

    state.into_model_response()
}

fn chat_message_from_model(message: &ModelMessage) -> Result<ChatMessage> {
    Ok(ChatMessage {
        role: message.role.as_str().to_owned(),
        content: message.content.clone(),
        tool_call_id: message.tool_call_id.clone(),
        tool_calls: if message.tool_calls.is_empty() {
            None
        } else {
            Some(
                message
                    .tool_calls
                    .iter()
                    .map(chat_tool_call_from_model)
                    .collect::<Result<Vec<_>>>()?,
            )
        },
    })
}

fn chat_tool_call_from_model(call: &ToolCall) -> Result<ChatToolCall> {
    Ok(ChatToolCall {
        id: call.id.clone(),
        r#type: "function".to_owned(),
        function: ChatToolCallFunction {
            name: call.name.clone(),
            arguments: serde_json::to_string(&call.arguments)
                .context("failed to serialize tool arguments")?,
        },
    })
}

fn chat_tools(tools: Vec<ToolSpec>) -> Option<Vec<ChatTool>> {
    if tools.is_empty() {
        return None;
    }

    Some(
        tools
            .into_iter()
            .map(|tool| ChatTool {
                r#type: "function",
                function: ChatToolFunction {
                    name: tool.name,
                    description: tool.description,
                    parameters: tool.parameters,
                },
            })
            .collect(),
    )
}

fn model_response_from_choice(choice: Choice, usage: Option<TokenUsage>) -> Result<ModelResponse> {
    let tool_calls = choice
        .message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(model_tool_call_from_chat)
        .collect::<Result<Vec<_>>>()?;

    Ok(ModelResponse {
        assistant_text: choice.message.content,
        tool_calls,
        finish_reason: finish_reason(choice.finish_reason),
        usage,
    })
}

fn model_tool_call_from_chat(call: ChatToolCall) -> Result<ToolCall> {
    let arguments = serde_json::from_str(&call.function.arguments).with_context(|| {
        format!(
            "tool call '{}' arguments are not valid JSON",
            call.function.name
        )
    })?;

    Ok(ToolCall {
        id: call.id,
        name: call.function.name,
        arguments,
    })
}

impl StreamingState {
    fn apply_chunk(
        &mut self,
        chunk: StreamResponse,
        on_delta: &mut dyn FnMut(String),
    ) -> Result<()> {
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage.into());
        }

        let Some(choice) = chunk.choices.into_iter().next() else {
            return Ok(());
        };

        if let Some(content) = choice.delta.content.filter(|content| !content.is_empty()) {
            self.assistant_text.push_str(&content);
            on_delta(content);
        }

        if let Some(tool_calls) = choice.delta.tool_calls {
            for call in tool_calls {
                self.apply_tool_call_delta(call)?;
            }
        }

        if choice.finish_reason.is_some() {
            self.finish_reason = choice.finish_reason;
        }

        Ok(())
    }

    fn apply_tool_call_delta(&mut self, call: StreamToolCall) -> Result<()> {
        if call.index >= MAX_STREAM_TOOL_CALLS {
            bail!("streaming tool call index exceeds limit");
        }
        if self.tool_calls.len() <= call.index {
            self.tool_calls
                .resize_with(call.index + 1, StreamingToolCall::default);
        }

        let target = &mut self.tool_calls[call.index];
        if let Some(id) = call.id {
            target.id.push_str(&id);
        }
        if let Some(kind) = call.r#type {
            target.r#type.push_str(&kind);
        }
        if let Some(function) = call.function {
            if let Some(name) = function.name {
                target.name.push_str(&name);
            }
            if let Some(arguments) = function.arguments {
                if target.arguments.len() + arguments.len() > MAX_STREAM_TOOL_ARGUMENT_BYTES {
                    bail!("streaming tool call arguments exceed limit");
                }
                target.arguments.push_str(&arguments);
            }
        }
        Ok(())
    }

    fn into_model_response(self) -> Result<ModelResponse> {
        let finish_reason = self.finish_reason();
        let tool_calls = self
            .tool_calls
            .into_iter()
            .filter(|call| !call.id.is_empty() || !call.name.is_empty())
            .map(model_tool_call_from_stream)
            .collect::<Result<Vec<_>>>()?;
        let assistant_text = if self.assistant_text.is_empty() {
            None
        } else {
            Some(self.assistant_text)
        };

        Ok(ModelResponse {
            assistant_text,
            tool_calls,
            finish_reason,
            usage: self.usage,
        })
    }

    fn finish_reason(&self) -> FinishReason {
        if self.finish_reason.is_some() {
            return finish_reason(self.finish_reason.clone());
        }
        if self.saw_done && !self.tool_calls.is_empty() {
            return FinishReason::ToolCalls;
        }
        if self.saw_done {
            return FinishReason::Stop;
        }
        finish_reason(None)
    }
}

fn model_tool_call_from_stream(call: StreamingToolCall) -> Result<ToolCall> {
    if call.id.is_empty() {
        bail!("streaming tool call is missing id");
    }
    if call.name.is_empty() {
        bail!("streaming tool call is missing function name");
    }

    let arguments = if call.arguments.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(&call.arguments)
            .with_context(|| format!("tool call '{}' arguments are not valid JSON", call.name))?
    };

    Ok(ToolCall {
        id: call.id,
        name: call.name,
        arguments,
    })
}

fn truncate_chunk(chunk: &str) -> String {
    const MAX_CHUNK_CHARS: usize = 160;

    if chunk.chars().count() <= MAX_CHUNK_CHARS {
        return chunk.to_owned();
    }

    format!(
        "{}...",
        chunk.chars().take(MAX_CHUNK_CHARS).collect::<String>()
    )
}

fn finish_reason(reason: Option<String>) -> FinishReason {
    match reason.as_deref() {
        Some("stop") => FinishReason::Stop,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        Some(other) => FinishReason::Other(other.to_owned()),
        None => FinishReason::Other("missing finish_reason".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_state_emits_text_deltas_as_chunks_arrive() {
        let mut state = StreamingState::default();
        let mut deltas = Vec::new();

        state
            .apply_chunk(text_chunk("hel", None), &mut |delta| deltas.push(delta))
            .unwrap();
        state
            .apply_chunk(text_chunk("lo", Some("stop")), &mut |delta| {
                deltas.push(delta)
            })
            .unwrap();

        let response = state.into_model_response().unwrap();
        assert_eq!(deltas, vec!["hel", "lo"]);
        assert_eq!(response.assistant_text.as_deref(), Some("hello"));
        assert_eq!(response.finish_reason, FinishReason::Stop);
        assert_eq!(response.usage, None);
    }

    #[test]
    fn streaming_state_records_usage_chunk_without_choices() {
        let mut state = StreamingState::default();
        let mut deltas = Vec::new();

        state
            .apply_chunk(
                StreamResponse {
                    choices: Vec::new(),
                    usage: Some(OpenAiUsage {
                        prompt_tokens: 100,
                        completion_tokens: 25,
                        total_tokens: 125,
                        prompt_tokens_details: Some(PromptTokensDetails { cached_tokens: 40 }),
                    }),
                },
                &mut |delta| deltas.push(delta),
            )
            .unwrap();

        let response = state.into_model_response().unwrap();
        assert!(deltas.is_empty());
        assert_eq!(
            response.usage.map(|usage| usage.cached_prompt_tokens),
            Some(Some(40))
        );
    }

    #[test]
    fn streaming_state_records_usage_without_cache_details() {
        let mut state = StreamingState::default();
        let mut deltas = Vec::new();

        state
            .apply_chunk(
                StreamResponse {
                    choices: Vec::new(),
                    usage: Some(OpenAiUsage {
                        prompt_tokens: 100,
                        completion_tokens: 25,
                        total_tokens: 125,
                        prompt_tokens_details: None,
                    }),
                },
                &mut |delta| deltas.push(delta),
            )
            .unwrap();

        let response = state.into_model_response().unwrap();
        assert_eq!(
            response.usage.map(|usage| usage.cached_prompt_tokens),
            Some(None)
        );
    }

    #[test]
    fn streaming_state_assembles_split_tool_call_arguments() {
        let mut state = StreamingState::default();
        let mut deltas = Vec::new();

        state
            .apply_chunk(
                tool_chunk(
                    0,
                    Some("call-1"),
                    Some("function"),
                    Some("Read"),
                    Some("{\"file_path\":\""),
                    None,
                ),
                &mut |delta| deltas.push(delta),
            )
            .unwrap();
        state
            .apply_chunk(
                tool_chunk(
                    0,
                    None,
                    None,
                    None,
                    Some(r#"src/app.rs"}"#),
                    Some("tool_calls"),
                ),
                &mut |delta| deltas.push(delta),
            )
            .unwrap();

        let response = state.into_model_response().unwrap();
        assert!(deltas.is_empty());
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "call-1");
        assert_eq!(response.tool_calls[0].name, "Read");
        assert_eq!(response.tool_calls[0].arguments["file_path"], "src/app.rs");
    }

    fn text_chunk(content: &str, finish_reason: Option<&str>) -> StreamResponse {
        StreamResponse {
            choices: vec![StreamChoice {
                delta: StreamDelta {
                    content: Some(content.to_owned()),
                    tool_calls: None,
                },
                finish_reason: finish_reason.map(str::to_owned),
            }],
            usage: None,
        }
    }

    fn tool_chunk(
        index: usize,
        id: Option<&str>,
        kind: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
        finish_reason: Option<&str>,
    ) -> StreamResponse {
        StreamResponse {
            choices: vec![StreamChoice {
                delta: StreamDelta {
                    content: None,
                    tool_calls: Some(vec![StreamToolCall {
                        index,
                        id: id.map(str::to_owned),
                        r#type: kind.map(str::to_owned),
                        function: Some(StreamToolCallFunction {
                            name: name.map(str::to_owned),
                            arguments: arguments.map(str::to_owned),
                        }),
                    }]),
                },
                finish_reason: finish_reason.map(str::to_owned),
            }],
            usage: None,
        }
    }
}
