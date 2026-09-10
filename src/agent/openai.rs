use std::io::{BufRead, BufReader};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::LlmConfig;

use super::{
    TokenUsage,
    provider::{
        FinishReason, ModelMessage, ModelProvider, ModelRequest, ModelResponse, ProviderReasoning,
        ReasoningData, ToolCall, ToolSpec,
    },
};

const MAX_STREAM_TOOL_CALLS: usize = 16;
const MAX_STREAM_TOOL_ARGUMENT_BYTES: usize = 64 * 1024;

pub struct OpenAiProvider {
    config: LlmConfig,
    client: Client,
}

impl OpenAiProvider {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }
}

impl ModelProvider for OpenAiProvider {
    fn complete(&mut self, request: ModelRequest) -> Result<ModelResponse> {
        complete_chat(&self.client, &self.config, request)
    }

    fn stream(
        &mut self,
        request: ModelRequest,
        on_delta: &mut dyn FnMut(String),
    ) -> Result<ModelResponse> {
        stream_chat(&self.client, &self.config, request, on_delta)
    }
}

impl ReasoningData {
    fn append(&mut self, delta: Self) -> Result<()> {
        for (target, incoming) in [
            (&mut self.reasoning_content, delta.reasoning_content),
            (&mut self.encrypted_content, delta.encrypted_content),
        ] {
            if let Some(text) = incoming {
                target.get_or_insert_default().push_str(&text);
            }
        }
        if let Some(details) = delta.reasoning_details {
            let target = self.reasoning_details.get_or_insert_default();
            for detail in details {
                let index = detail.get("index");
                if let Some(existing) = target
                    .iter_mut()
                    .find(|entry| index.is_some() && entry.get("index") == index)
                {
                    if let (Some(existing), Some(incoming)) =
                        (existing.as_object_mut(), detail.as_object())
                    {
                        for (key, value) in incoming {
                            if matches!(key.as_str(), "text" | "data" | "signature")
                                && let (Some(Value::String(text)), Value::String(fragment)) =
                                    (existing.get_mut(key), value)
                            {
                                text.push_str(fragment);
                            } else {
                                existing.insert(key.clone(), value.clone());
                            }
                        }
                    }
                } else {
                    anyhow::ensure!(target.len() < 256, "too many reasoning detail blocks");
                    target.push(detail);
                }
            }
        }
        Ok(())
    }
}

fn request_messages(config: &LlmConfig, messages: &[ModelMessage]) -> Result<Vec<ChatMessage>> {
    messages
        .iter()
        .map(|message| {
            let mut chat = chat_message_from_model(message)?;
            if message.role == super::provider::ModelRole::Assistant {
                if let Some(reasoning) = &message.reasoning
                    && reasoning.provider == config.provider
                    && reasoning.model == config.model
                {
                    chat.reasoning = reasoning.data.clone();
                }
                // Local command answers and legacy sessions have no recorded reasoning.
                // DeepSeek requires the field to be present for assistant history with tools.
                if config.provider == "deepseek" {
                    chat.reasoning.reasoning_content.get_or_insert_default();
                }
            }
            Ok(chat)
        })
        .collect()
}

fn response_reasoning(config: &LlmConfig, data: ReasoningData) -> Option<ProviderReasoning> {
    (data != ReasoningData::default()).then(|| ProviderReasoning {
        provider: config.provider.clone(),
        model: config.model.clone(),
        data,
    })
}

#[derive(Serialize)]
struct ChatRequest {
    #[serde(flatten)]
    reasoning_parameters: serde_json::Map<String, Value>,
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_retention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ChatTool>>,
}

#[derive(Serialize)]
struct ChatMessage {
    #[serde(flatten)]
    reasoning: ReasoningData,
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
    #[serde(flatten)]
    reasoning: ReasoningData,
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
    reasoning: ReasoningData,
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
    #[serde(flatten)]
    reasoning: ReasoningData,
    content: Option<String>,
    tool_calls: Option<Vec<ChatToolCall>>,
}

fn complete_chat(
    client: &Client,
    config: &LlmConfig,
    request: ModelRequest,
) -> Result<ModelResponse> {
    let request = ChatRequest {
        reasoning_parameters: crate::reasoning::request_fields(
            &config.provider,
            &config.model,
            config.reasoning_effort.as_deref(),
        )?,
        model: config.model.clone(),
        messages: request_messages(config, &request.messages)?,
        temperature: config.temperature,
        max_tokens: request.max_tokens.unwrap_or(config.max_tokens),
        prompt_cache_key: config.prompt_cache.key.clone(),
        prompt_cache_retention: config.prompt_cache.retention.clone(),
        stream: None,
        stream_options: None,
        tools: chat_tools(request.tools),
    };

    let response: ChatResponse = client
        .post(format!("{}/chat/completions", config.base_url))
        .bearer_auth(&config.api_key)
        .json(&request)
        .send()
        .context("request failed")?
        .error_for_status()
        .context("request failed")?
        .json()
        .context("invalid response")?;

    let choice = response
        .choices
        .into_iter()
        .next()
        .context("response did not include a choice")?;

    let data = choice.message.reasoning.clone();
    let mut response = model_response_from_choice(choice, response.usage.map(TokenUsage::from))?;
    response.reasoning = response_reasoning(config, data);
    Ok(response)
}

fn stream_chat(
    client: &Client,
    config: &LlmConfig,
    request: ModelRequest,
    on_delta: &mut dyn FnMut(String),
) -> Result<ModelResponse> {
    let request = ChatRequest {
        reasoning_parameters: crate::reasoning::request_fields(
            &config.provider,
            &config.model,
            config.reasoning_effort.as_deref(),
        )?,
        model: config.model.clone(),
        messages: request_messages(config, &request.messages)?,
        temperature: config.temperature,
        max_tokens: request.max_tokens.unwrap_or(config.max_tokens),
        prompt_cache_key: config.prompt_cache.key.clone(),
        prompt_cache_retention: config.prompt_cache.retention.clone(),
        stream: Some(true),
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        tools: chat_tools(request.tools),
    };

    let response = client
        .post(format!("{}/chat/completions", config.base_url))
        .bearer_auth(&config.api_key)
        .json(&request)
        .send()
        .context("request failed")?
        .error_for_status()
        .context("request failed")?;

    let mut state = StreamingState::default();
    for line in BufReader::new(response).lines() {
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

    let data = state.reasoning.clone();
    let mut response = state.into_model_response()?;
    response.reasoning = response_reasoning(config, data);
    Ok(response)
}

fn chat_message_from_model(message: &ModelMessage) -> Result<ChatMessage> {
    Ok(ChatMessage {
        reasoning: ReasoningData::default(),
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
        reasoning: None,
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

        self.reasoning.append(choice.delta.reasoning)?;
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
            reasoning: None,
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
    fn reasoning_tool_roundtrip_preserves_streamed_provider_fields() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;
        for (provider_id, model, effort) in [
            ("deepseek", "deepseek-v4-pro", "max"),
            ("volcengine", "doubao-seed-2-1-pro-260628", "low"),
            ("openrouter", "deepseek/deepseek-v4-flash", "xhigh"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                for turn in 0..3 {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(&mut socket);
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(value) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    if provider_id == "openrouter" {
                        assert_eq!(request["reasoning"]["effort"], effort);
                    } else {
                        assert_eq!(request["reasoning_effort"], effort);
                    }
                    let (content_type, body) = if turn == 0 {
                        assert_eq!(request["stream"], true);
                        let mut delta = serde_json::json!({"content":"", "tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"Read","arguments":"{}"}}]});
                        if provider_id == "openrouter" {
                            delta["reasoning_details"] = serde_json::json!([{"index":0,"type":"reasoning.text","text":"private thought"}]);
                        } else {
                            delta["reasoning_content"] = Value::String("private thought".into());
                        }
                        if provider_id == "volcengine" {
                            delta["encrypted_content"] = Value::String("ciphertext".into());
                        }
                        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":"tool_calls"}]});
                        (
                            "text/event-stream",
                            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                        )
                    } else {
                        let assistant = &request["messages"][1];
                        assert_eq!(assistant["tool_calls"][0]["id"], "call-1");
                        if provider_id == "openrouter" {
                            assert_eq!(
                                assistant["reasoning_details"][0]["text"],
                                "private thought"
                            );
                        } else {
                            assert_eq!(assistant["reasoning_content"], "private thought");
                        }
                        if provider_id == "volcengine" {
                            assert_eq!(assistant["encrypted_content"], "ciphertext");
                        }
                        if turn == 2 {
                            let final_assistant = &request["messages"][3];
                            if provider_id == "openrouter" {
                                assert_eq!(
                                    final_assistant["reasoning_details"][0]["text"],
                                    "final thought"
                                );
                            } else {
                                assert_eq!(final_assistant["reasoning_content"], "final thought");
                            }
                            assert_eq!(request["messages"][4]["content"], "next prompt");
                        }
                        assert!(request.get("stream").is_none());
                        let mut final_message = serde_json::json!({"content":"done"});
                        if provider_id == "openrouter" {
                            final_message["reasoning_details"] = serde_json::json!([{"index":0,"type":"reasoning.text","text":"final thought"}]);
                        } else {
                            final_message["reasoning_content"] =
                                Value::String("final thought".into());
                        }
                        ("application/json", serde_json::json!({"choices":[{"message":final_message,"finish_reason":"stop"}]}).to_string())
                    };
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            });
            let mut config = crate::app::App::test_empty().config.llm.clone();
            config.provider = provider_id.into();
            config.model = model.into();
            config.base_url = format!("http://{address}");
            config.reasoning_effort = Some(effort.into());
            let mut provider = OpenAiProvider {
                config,
                client: Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
            };
            let first = ModelRequest {
                messages: vec![ModelMessage::user("probe")],
                tools: vec![],
                max_tokens: None,
            };
            let mut visible = String::new();
            let result = provider
                .stream(first.clone(), &mut |delta| visible.push_str(&delta))
                .unwrap();
            assert!(
                visible.is_empty(),
                "reasoning must not enter the answer transcript"
            );
            let mut messages = first.messages;
            // The query loop normalizes empty assistant text to None.
            messages.push(
                ModelMessage::assistant(None, result.tool_calls).with_reasoning(result.reasoning),
            );
            messages.push(ModelMessage {
                reasoning: None,
                role: super::super::provider::ModelRole::Tool,
                content: Some("tool result".into()),
                tool_call_id: Some("call-1".into()),
                tool_calls: vec![],
            });
            let final_response = provider
                .complete(ModelRequest {
                    messages: messages.clone(),
                    tools: vec![],
                    max_tokens: None,
                })
                .unwrap();
            assert_eq!(final_response.assistant_text.as_deref(), Some("done"));
            messages.push(
                ModelMessage::assistant(final_response.assistant_text, vec![])
                    .with_reasoning(final_response.reasoning),
            );
            messages.push(ModelMessage::user("next prompt"));
            // Each user prompt constructs a new provider in spawn_agent_loop.
            let mut next_provider = OpenAiProvider {
                config: provider.config.clone(),
                client: provider.client.clone(),
            };
            let result = next_provider
                .complete(ModelRequest {
                    messages,
                    tools: vec![ToolSpec {
                        name: "Read".into(),
                        description: "Read test file".into(),
                        parameters: serde_json::json!({"type":"object"}),
                    }],
                    max_tokens: None,
                })
                .unwrap();
            assert_eq!(result.assistant_text.as_deref(), Some("done"));
            server.join().unwrap();
        }
    }

    #[test]
    fn reasoning_history_is_scoped_to_origin_and_legacy_messages_remain_compatible() {
        let mut config = crate::app::App::test_empty().config.llm.clone();
        config.provider = "deepseek".into();
        config.model = "deepseek-v4-pro".into();
        let reason = ProviderReasoning {
            provider: "volcengine".into(),
            model: "doubao-seed-2-1-pro-260628".into(),
            data: ReasoningData {
                reasoning_content: Some("other provider thought".into()),
                encrypted_content: Some("other provider cipher".into()),
                reasoning_details: None,
            },
        };
        let messages = vec![
            ModelMessage::assistant(Some("old answer".into()), vec![]).with_reasoning(Some(reason)),
        ];
        let wire = serde_json::to_value(request_messages(&config, &messages).unwrap()).unwrap();
        assert_eq!(wire[0]["reasoning_content"], "");
        assert!(wire[0].get("encrypted_content").is_none());
        config.provider = "custom".into();
        let wire = serde_json::to_value(request_messages(&config, &messages).unwrap()).unwrap();
        assert!(wire[0].get("reasoning_content").is_none());
    }

    #[test]
    fn reasoning_deltas_join_text_and_encrypted_blocks_without_losing_metadata() {
        let mut data = ReasoningData::default();
        for part in ["first", "second"] {
            data.append(serde_json::from_value(serde_json::json!({
                "reasoning_content": part, "encrypted_content": part,
                "reasoning_details": [{"index": 0,"id":"reason-1","type":"reasoning.encrypted","data":part,"format":"provider-v1"}]
            })).unwrap()).unwrap();
        }
        assert_eq!(data.reasoning_content.as_deref(), Some("firstsecond"));
        assert_eq!(data.encrypted_content.as_deref(), Some("firstsecond"));
        let detail = &data.reasoning_details.unwrap()[0];
        assert_eq!(detail["data"], "firstsecond");
        assert_eq!(detail["id"], "reason-1");
        assert_eq!(detail["format"], "provider-v1");
    }

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
    fn chat_request_serializes_prompt_cache_options_when_configured() {
        let request = ChatRequest {
            reasoning_parameters: Default::default(),
            model: "gpt-5-codex".to_owned(),
            messages: Vec::new(),
            temperature: 0.0,
            max_tokens: 1024,
            prompt_cache_key: Some("glint-coding".to_owned()),
            prompt_cache_retention: Some("24h".to_owned()),
            stream: None,
            stream_options: None,
            tools: None,
        };

        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["prompt_cache_key"], "glint-coding");
        assert_eq!(value["prompt_cache_retention"], "24h");
    }

    #[test]
    fn chat_request_omits_prompt_cache_options_by_default() {
        let request = ChatRequest {
            reasoning_parameters: Default::default(),
            model: "test-model".to_owned(),
            messages: Vec::new(),
            temperature: 0.0,
            max_tokens: 1024,
            prompt_cache_key: None,
            prompt_cache_retention: None,
            stream: None,
            stream_options: None,
            tools: None,
        };

        let value = serde_json::to_value(request).unwrap();

        assert!(value.get("prompt_cache_key").is_none());
        assert!(value.get("prompt_cache_retention").is_none());
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
                    reasoning: ReasoningData::default(),
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
                    reasoning: ReasoningData::default(),
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
