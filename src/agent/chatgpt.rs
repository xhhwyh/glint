//! Subscription Responses transport. Glint owns the prompt, history and tools.
use super::{
    TokenUsage,
    provider::{
        FinishReason, ModelMessage, ModelProvider, ModelRequest, ModelResponse, ModelRole, ToolCall,
    },
};
use crate::config::LlmConfig;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    io::{BufRead, BufReader},
    time::Duration,
};

const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_STREAM_BYTES: usize = 32 * 1024 * 1024;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_TOOL_CALLS: usize = 16;
const MAX_OUTPUT_ITEMS: usize = 64;

pub struct ChatGptProvider {
    config: LlmConfig,
    client: Result<Client>,
    reasoning: Vec<ReasoningTurn>,
}
struct ReasoningTurn {
    prefix: Vec<ModelMessage>,
    assistant: ModelMessage,
    items: Vec<Value>,
}
impl ChatGptProvider {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            config,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("glint/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("failed to initialize ChatGPT HTTP client"),
            reasoning: Vec::new(),
        }
    }
    fn request_body(&mut self, request: &ModelRequest) -> Result<Value> {
        // An exact history prefix prevents replay after compaction or context changes.
        self.reasoning.retain(|turn| {
            request.messages.starts_with(&turn.prefix)
                && request
                    .messages
                    .get(turn.prefix.len())
                    .is_some_and(|message| {
                        // Glint normalizes paths and hooks can replace arguments before
                        // recording history. Response identity survives those rewrites.
                        message.role == turn.assistant.role
                            && message.content == turn.assistant.content
                            && message.tool_call_id == turn.assistant.tool_call_id
                            && message.tool_calls.len() == turn.assistant.tool_calls.len()
                            && message
                                .tool_calls
                                .iter()
                                .zip(&turn.assistant.tool_calls)
                                .all(|(current, original)| {
                                    current.id == original.id && current.name == original.name
                                })
                    })
        });
        let mut instructions = Vec::new();
        let mut input = Vec::new();
        for (index, message) in request.messages.iter().enumerate() {
            for turn in self
                .reasoning
                .iter()
                .filter(|turn| turn.prefix.len() == index)
            {
                input.extend(turn.items.iter().cloned());
            }
            match message.role {
                ModelRole::System => {
                    if let Some(content) = &message.content {
                        instructions.push(content.clone());
                    }
                }
                ModelRole::User | ModelRole::Assistant => {
                    if let Some(content) = message.content.as_ref().filter(|s| !s.is_empty()) {
                        let content_type = if message.role == ModelRole::User {
                            "input_text"
                        } else {
                            "output_text"
                        };
                        input.push(json!({"type":"message","role":message.role.as_str(),"content":[{"type":content_type,"text":content}]}));
                    }
                    for call in &message.tool_calls {
                        input.push(json!({"type":"function_call","call_id":call.id,"name":call.name,"arguments":serde_json::to_string(&call.arguments)?}));
                    }
                }
                ModelRole::Tool => {
                    let call_id = message
                        .tool_call_id
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .context("ChatGPT tool result is missing call id")?;
                    input.push(json!({"type":"function_call_output","call_id":call_id,"output":message.content.as_deref().unwrap_or("")}));
                }
            }
        }
        let tools: Vec<Value> = request.tools.iter().map(|t| json!({"type":"function","name":t.name,"description":t.description,"parameters":t.parameters,"strict":false})).collect();
        let mut body = json!({"model":self.config.model,"instructions":instructions.join("\n\n"),"input":input,"tools":tools,"tool_choice":"auto","parallel_tool_calls":true,"stream":true,"store":false,"include":["reasoning.encrypted_content"]});
        if let Some(effort) = &self.config.reasoning_effort {
            body["reasoning"] = json!({"effort": effort});
        }
        Ok(body)
    }
    fn send(
        &mut self,
        endpoint: &str,
        token: &str,
        account: &str,
        residency: Option<&str>,
        request: ModelRequest,
        on_delta: &mut dyn FnMut(String),
    ) -> Result<ModelResponse> {
        let body = self.request_body(&request)?;
        let client = self
            .client
            .as_ref()
            .map_err(|_| anyhow::anyhow!("failed to initialize ChatGPT HTTP client"))?;
        let mut outgoing = client
            .post(endpoint)
            .bearer_auth(token)
            .header("ChatGPT-Account-Id", account)
            .header("originator", "glint")
            .header("accept", "text/event-stream")
            .timeout(Duration::from_secs(300))
            .json(&body);
        if let Some(residency) = residency {
            outgoing = outgoing.header("x-openai-internal-codex-residency", residency);
        }
        let response = outgoing.send().context("ChatGPT request failed")?;
        if !response.status().is_success() {
            // Do not echo upstream bodies, which may contain credentials or prompts.
            bail!("ChatGPT request failed (HTTP {})", response.status());
        }
        let (response, items) = parse_stream(BufReader::new(response), on_delta)?;
        if !items.is_empty() {
            if self.reasoning.len() >= 32 {
                self.reasoning.remove(0);
            }
            self.reasoning.push(ReasoningTurn {
                prefix: request.messages,
                assistant: ModelMessage::assistant(
                    response.assistant_text.clone(),
                    response.tool_calls.clone(),
                ),
                items,
            });
        }
        Ok(response)
    }
}
impl ModelProvider for ChatGptProvider {
    fn complete(&mut self, request: ModelRequest) -> Result<ModelResponse> {
        self.stream(request, &mut |_| {})
    }
    fn stream(
        &mut self,
        request: ModelRequest,
        on_delta: &mut dyn FnMut(String),
    ) -> Result<ModelResponse> {
        let credentials = crate::chatgpt::credentials()?;
        self.send(
            ENDPOINT,
            &credentials.access_token,
            &credentials.account_id,
            credentials.compute_residency.as_deref(),
            request,
            on_delta,
        )
    }
}

// Read bounded lines and complete SSE frames; BufRead::lines alone can allocate
// without limit on a corrupt or hostile stream.
fn parse_stream(
    mut reader: impl BufRead,
    on_delta: &mut dyn FnMut(String),
) -> Result<(ModelResponse, Vec<Value>)> {
    let mut data = String::new();
    let mut total = 0usize;
    let mut streamed_text = String::new();
    let mut arguments: HashMap<String, String> = HashMap::new();
    let mut done_items = BTreeMap::new();
    loop {
        let mut line = Vec::new();
        loop {
            let chunk = reader.fill_buf().context("ChatGPT stream read failed")?;
            if chunk.is_empty() {
                break;
            }
            let count = chunk
                .iter()
                .position(|&b| b == b'\n')
                .map_or(chunk.len(), |i| i + 1);
            if line.len() + count > MAX_EVENT_BYTES {
                bail!("ChatGPT stream event exceeds limit");
            }
            line.extend_from_slice(&chunk[..count]);
            reader.consume(count);
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        if line.is_empty() {
            bail!("ChatGPT stream ended before response.completed");
        }
        total += line.len();
        if total > MAX_STREAM_BYTES {
            bail!("ChatGPT stream exceeds limit");
        }
        let line = std::str::from_utf8(&line)
            .context("invalid ChatGPT stream encoding")?
            .trim_end_matches(['\r', '\n']);
        if !line.is_empty() {
            if let Some(value) = line.strip_prefix("data:") {
                if data.len() + value.len() + 1 > MAX_EVENT_BYTES {
                    bail!("ChatGPT stream event exceeds limit");
                }
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
            continue;
        }
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            bail!("ChatGPT stream ended before response.completed");
        }
        let event: Value = serde_json::from_str(&data).context("invalid ChatGPT stream event")?;
        data.clear();
        match event["type"]
            .as_str()
            .context("ChatGPT stream event is missing type")?
        {
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = event["delta"]
                    .as_str()
                    .context("ChatGPT text delta is missing text")?;
                streamed_text.push_str(delta);
                on_delta(delta.to_owned());
            }
            "response.function_call_arguments.delta" => {
                let id = event["item_id"]
                    .as_str()
                    .context("ChatGPT arguments delta is missing item id")?;
                let delta = event["delta"]
                    .as_str()
                    .context("ChatGPT arguments delta is missing text")?;
                if !arguments.contains_key(id) && arguments.len() >= MAX_TOOL_CALLS {
                    bail!("ChatGPT tool calls exceed limit");
                }
                let args = arguments.entry(id.to_owned()).or_default();
                if args.len() + delta.len() > MAX_ARGUMENT_BYTES {
                    bail!("ChatGPT tool arguments exceed limit");
                }
                args.push_str(delta);
            }
            "response.output_item.added" | "response.output_item.done" => {
                let index = event["output_index"]
                    .as_u64()
                    .filter(|index| *index < MAX_OUTPUT_ITEMS as u64)
                    .context("ChatGPT output item index is missing or exceeds limit")?
                    as usize;
                if event["type"] == "response.output_item.done" {
                    merge_output_item(&mut done_items, index, &event["item"])?;
                }
            }
            "response.failed" | "response.incomplete" | "error" => {
                bail!("ChatGPT response failed or was incomplete")
            }
            "response.completed" => {
                let response = &event["response"];
                if response["status"] != "completed" {
                    bail!("ChatGPT response was not completed");
                }
                let output = response["output"]
                    .as_array()
                    .context("ChatGPT completed response is missing output")?;
                if output.len() > MAX_OUTPUT_ITEMS {
                    bail!("ChatGPT output items exceed limit");
                }
                // The subscription backend may finish with output: []; done events
                // carry the actual items. Reconcile repeated full terminal output.
                for (index, item) in output.iter().enumerate() {
                    merge_output_item(&mut done_items, index, item)?;
                }
                let mut text = String::new();
                let mut tool_calls = Vec::new();
                let mut reasoning = Vec::new();
                for item in done_items.values() {
                    if let Some(status) = item["status"].as_str()
                        && status != "completed"
                    {
                        bail!("ChatGPT output item was not completed");
                    }
                    match item["type"].as_str() {
                        Some("message") => {
                            for part in item["content"]
                                .as_array()
                                .context("ChatGPT message is missing content")?
                            {
                                match part["type"].as_str() {
                                    Some("output_text") => text.push_str(
                                        part["text"]
                                            .as_str()
                                            .context("ChatGPT message is missing text")?,
                                    ),
                                    Some("refusal") => text.push_str(
                                        part["refusal"]
                                            .as_str()
                                            .context("ChatGPT refusal is missing text")?,
                                    ),
                                    _ => {}
                                }
                            }
                        }
                        Some("function_call") => {
                            if tool_calls.len() >= MAX_TOOL_CALLS {
                                bail!("ChatGPT tool calls exceed limit");
                            }
                            let id = required_string(item, "call_id")?;
                            let name = required_string(item, "name")?;
                            let args = required_string(item, "arguments")?;
                            if args.len() > MAX_ARGUMENT_BYTES {
                                bail!("ChatGPT tool arguments exceed limit");
                            }
                            if tool_calls.iter().any(|call: &ToolCall| call.id == id) {
                                bail!("ChatGPT returned duplicate tool call ids");
                            }
                            tool_calls.push(ToolCall {
                                id,
                                name,
                                arguments: serde_json::from_str(&args)
                                    .context("invalid ChatGPT tool arguments")?,
                            });
                        }
                        Some("reasoning") if item["encrypted_content"].is_string() => {
                            reasoning.push(item.clone())
                        }
                        _ => {}
                    }
                }
                if text.is_empty() && tool_calls.is_empty() {
                    bail!("ChatGPT completed response contained no text or tool calls");
                }
                if !text.starts_with(&streamed_text) {
                    bail!("ChatGPT completed text differs from streamed text");
                }
                if text.len() > streamed_text.len() {
                    on_delta(text[streamed_text.len()..].to_owned());
                }
                let usage = response
                    .get("usage")
                    .filter(|u| !u.is_null())
                    .map(|u| TokenUsage {
                        prompt_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                        completion_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        total_tokens: u["total_tokens"].as_u64().unwrap_or(0),
                        cached_prompt_tokens: u["input_tokens_details"]["cached_tokens"].as_u64(),
                    });
                return Ok((
                    ModelResponse {
                        reasoning: None,
                        assistant_text: (!text.is_empty()).then_some(text),
                        finish_reason: if tool_calls.is_empty() {
                            FinishReason::Stop
                        } else {
                            FinishReason::ToolCalls
                        },
                        tool_calls,
                        usage,
                    },
                    reasoning,
                ));
            }
            _ => {}
        }
    }
}
fn merge_output_item(items: &mut BTreeMap<usize, Value>, index: usize, item: &Value) -> Result<()> {
    if !item.is_object() || !item["type"].is_string() {
        bail!("ChatGPT output item is missing type");
    }
    if let Some(existing) = items.get(&index) {
        if existing != item {
            bail!("ChatGPT returned conflicting output items");
        }
        return Ok(());
    }
    if let Some(id) = item["id"].as_str()
        && items
            .values()
            .any(|existing| existing["id"].as_str() == Some(id))
    {
        bail!("ChatGPT returned duplicate output item ids");
    }
    items.insert(index, item.clone());
    Ok(())
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    Ok(value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("ChatGPT function call is missing {key}"))?
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn sse(events: Vec<Value>) -> String {
        events
            .into_iter()
            .map(|e| format!("data: {e}\n\n"))
            .collect()
    }
    fn completed(output: Value) -> Value {
        json!({"type":"response.completed","response":{"status":"completed","output":output,"usage":{"input_tokens":100,"output_tokens":12,"total_tokens":112,"input_tokens_details":{"cached_tokens":40}}}})
    }
    fn message(text: &str) -> Value {
        json!({"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]})
    }
    fn function() -> Value {
        json!({"type":"function_call","id":"fc_1","call_id":"call_1","name":"Read","arguments":"{\"path\":\"src/main.rs\"}","status":"completed"})
    }
    #[test]
    fn successful_stream_emits_deltas_and_usage() {
        let input = sse(vec![
            json!({"type":"response.output_text.delta","delta":"hello"}),
            completed(json!([message("hello")])),
        ]);
        let mut deltas = vec![];
        let (response, _) = parse_stream(input.as_bytes(), &mut |d| deltas.push(d)).unwrap();
        assert_eq!(deltas, vec!["hello"]);
        assert_eq!(response.assistant_text.as_deref(), Some("hello"));
        assert_eq!(response.usage.unwrap().cached_prompt_tokens, Some(40));
    }
    #[test]
    fn completed_stream_returns_text_and_function_calls_together() {
        let input = sse(vec![
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"Read","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"path\":"}),
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"\"src/main.rs\"}"}),
            completed(json!([message("Checking"), function()])),
        ]);
        let (response, _) = parse_stream(input.as_bytes(), &mut |_| {}).unwrap();
        assert_eq!(response.assistant_text.as_deref(), Some("Checking"));
        assert_eq!(response.tool_calls[0].arguments["path"], "src/main.rs");
        assert_eq!(response.tool_calls[0].id, "call_1");
    }
    #[test]
    fn completed_empty_output_preserves_done_text_tools_and_reasoning() {
        let reasoning =
            json!({"id":"rs_1","type":"reasoning","encrypted_content":"opaque","summary":[]});
        let input = sse(vec![
            json!({"type":"response.output_item.done","output_index":2,"item":function()}),
            json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
            json!({"type":"response.output_item.done","output_index":1,"item":message("Checking")}),
            completed(json!([])),
        ]);
        let mut deltas = vec![];
        let (response, reasoning) =
            parse_stream(input.as_bytes(), &mut |d| deltas.push(d)).unwrap();
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "call_1");
        assert_eq!(response.assistant_text.as_deref(), Some("Checking"));
        assert_eq!(deltas, vec!["Checking"]);
        assert_eq!(reasoning[0]["encrypted_content"], "opaque");
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    }
    #[test]
    fn reconciles_repeated_done_items_and_rejects_conflicting_terminal_output() {
        let done = json!({"type":"response.output_item.done","output_index":0,"item":function()});
        let input = sse(vec![
            done.clone(),
            done.clone(),
            completed(json!([function()])),
        ]);
        let (response, _) = parse_stream(input.as_bytes(), &mut |_| {}).unwrap();
        assert_eq!(response.tool_calls.len(), 1);
        let mut conflict = function();
        conflict["arguments"] = json!("{\"path\":\"different.rs\"}");
        assert!(
            parse_stream(
                sse(vec![done.clone(), completed(json!([conflict]))]).as_bytes(),
                &mut |_| {}
            )
            .is_err()
        );
        // A done item is not enough to authorize executing the tool.
        assert!(parse_stream(sse(vec![done]).as_bytes(), &mut |_| {}).is_err());
    }
    #[test]
    fn rejects_empty_success_and_out_of_bounds_item_indexes() {
        assert!(parse_stream(sse(vec![completed(json!([]))]).as_bytes(), &mut |_| {}).is_err());
        for event_type in ["response.output_item.added", "response.output_item.done"] {
            let input = sse(vec![
                json!({"type":event_type,"output_index":64,"item":function()}),
                completed(json!([message("ok")])),
            ]);
            assert!(parse_stream(input.as_bytes(), &mut |_| {}).is_err());
        }
    }
    #[test]
    fn rejects_truncated_failed_and_incomplete_streams() {
        for input in [
            sse(vec![
                json!({"type":"response.output_item.done","item":function()}),
            ]),
            "data: [DONE]\n\n".to_owned(),
            sse(vec![
                json!({"type":"response.failed","response":{"error":{"message":"secret"}}}),
            ]),
            sse(vec![json!({"type":"response.incomplete"})]),
        ] {
            assert!(parse_stream(input.as_bytes(), &mut |_| {}).is_err());
        }
    }
    #[test]
    fn rejects_malformed_tool_arguments_even_on_completed_response() {
        let mut item = function();
        item["arguments"] = json!("{");
        assert!(parse_stream(sse(vec![completed(json!([item]))]).as_bytes(), &mut |_| {}).is_err());
    }
    #[test]
    fn request_omits_default_reasoning_and_sends_explicit_effort() {
        let mut provider = provider();
        assert!(
            provider
                .request_body(&request())
                .unwrap()
                .get("reasoning")
                .is_none()
        );
        provider.config.reasoning_effort = Some("ultra".into());
        assert_eq!(
            provider.request_body(&request()).unwrap()["reasoning"],
            json!({"effort":"ultra"})
        );
    }
    fn provider() -> ChatGptProvider {
        ChatGptProvider::new(LlmConfig {
            reasoning_effort: None,
            provider: "chatgpt".into(),
            base_url: "https://untrusted.invalid".into(),
            model: "gpt-5.4".into(),
            providers: vec![],
            temperature: 0.7,
            max_tokens: 4096,
            context_window: None,
            api_key: "must-not-send".into(),
            default_context_window: None,
            prompt_cache: Default::default(),
        })
    }
    fn request() -> ModelRequest {
        ModelRequest {
            messages: vec![
                ModelMessage::system("Glint prompt"),
                ModelMessage::user("read source"),
            ],
            tools: vec![super::super::provider::ToolSpec {
                name: "Read".into(),
                description: "Read source".into(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            }],
            max_tokens: Some(12),
        }
    }
    #[test]
    fn http_request_uses_subscription_auth_and_native_tool_schema() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            let length: usize = headers
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let output = sse(vec![completed(json!([message("hello")]))]);
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",output.len(),output).unwrap();
            (headers, serde_json::from_slice::<Value>(&body).unwrap())
        });
        let mut provider = provider();
        provider.client = Ok(Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap());
        let response = provider
            .send(
                &endpoint,
                "fake-token",
                "fake-account",
                Some("eu"),
                request(),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(response.assistant_text.as_deref(), Some("hello"));
        let (headers, body) = server.join().unwrap();
        assert!(
            headers
                .to_lowercase()
                .contains("authorization: bearer fake-token")
        );
        assert!(
            headers
                .to_lowercase()
                .contains("chatgpt-account-id: fake-account")
        );
        assert!(!headers.contains("must-not-send"));
        assert!(
            headers
                .to_lowercase()
                .contains("x-openai-internal-codex-residency: eu")
        );
        assert_eq!(body["instructions"], "Glint prompt");
        assert_eq!(body["tools"][0]["name"], "Read");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("temperature").is_none());
    }
    #[test]
    fn replays_tool_history_and_only_matching_encrypted_reasoning() {
        let mut provider = provider();
        let mut request = request();
        let assistant = ModelMessage::assistant(
            Some("Checking".into()),
            vec![ToolCall {
                id: "call_1".into(),
                name: "Read".into(),
                arguments: json!({"path":"src/main.rs"}),
            }],
        );
        provider.reasoning.push(ReasoningTurn {
            prefix: request.messages.clone(),
            assistant: assistant.clone(),
            items: vec![json!({"type":"reasoning","encrypted_content":"opaque","summary":[]})],
        });
        request.messages.push(assistant);
        request.messages.push(ModelMessage::tool_result(
            &super::super::provider::ToolResult {
                call_id: "call_1".into(),
                content: "source".into(),
                is_error: false,
            },
        ));
        let body = provider.request_body(&request).unwrap();
        assert_eq!(body["input"][1]["encrypted_content"], "opaque");
        assert_eq!(body["input"][3]["type"], "function_call");
        assert_eq!(body["input"][4]["type"], "function_call_output");
        assert_eq!(body["input"][4]["call_id"], "call_1");
        request.messages[0] = ModelMessage::system("compacted context");
        let body = provider.request_body(&request).unwrap();
        assert!(!body.to_string().contains("opaque"));
    }
    #[test]
    fn reasoning_survives_tool_path_normalization_and_hook_argument_replacement() {
        use crate::tools::{ToolContext, ToolRegistry, with_tool_context};
        let workspace = std::env::current_dir().unwrap();
        let original = ToolCall {
            id: "call_normalized".into(),
            name: "Read".into(),
            arguments: json!({"file_path": workspace.join("Cargo.toml")}),
        };
        let normalized = with_tool_context(ToolContext::new(&workspace, &workspace), || {
            ToolRegistry::new().normalize_for_context(&original)
        });
        assert_eq!(normalized.arguments["file_path"], "Cargo.toml");
        let mut hook_replaced = normalized.clone();
        // BeforeToolCall only replaces arguments; call id and name stay intact.
        hook_replaced.arguments = json!({"file_path": "src/main.rs", "limit": 20});
        for call in [normalized, hook_replaced] {
            let mut provider = provider();
            let mut request = request();
            provider.reasoning.push(ReasoningTurn {
                prefix: request.messages.clone(),
                assistant: ModelMessage::assistant(Some("Checking".into()), vec![original.clone()]),
                items: vec![json!({"type":"reasoning","encrypted_content":"opaque","summary":[]})],
            });
            request.messages.push(ModelMessage::assistant(
                Some("Checking".into()),
                vec![call.clone()],
            ));
            let body = provider.request_body(&request).unwrap();
            assert_eq!(body["input"][1]["encrypted_content"], "opaque");
            assert_eq!(
                body["input"][3]["arguments"],
                serde_json::to_string(&call.arguments).unwrap()
            );
            // A different response cannot inherit reasoning just because text matches.
            request.messages.last_mut().unwrap().tool_calls[0].id = "different_call".into();
            assert!(
                !provider
                    .request_body(&request)
                    .unwrap()
                    .to_string()
                    .contains("opaque")
            );
        }
    }
    #[test]
    fn oversized_arguments_and_events_are_rejected() {
        let input = sse(vec![
            json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"x".repeat(MAX_ARGUMENT_BYTES+1)}),
        ]);
        assert!(
            parse_stream(input.as_bytes(), &mut |_| {})
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
        let input = format!("data: {}", "x".repeat(MAX_EVENT_BYTES));
        assert!(
            parse_stream(input.as_bytes(), &mut |_| {})
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
    }
}
