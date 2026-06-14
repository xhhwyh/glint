use std::{sync::mpsc::Sender, thread};

use anyhow::{Context, Result, bail};

use crate::config::LlmConfig;

use super::{
    AgentEvent, TokenUsage,
    openai::OpenAiProvider,
    provider::{FinishReason, ModelMessage, ModelProvider, ModelRequest, ModelResponse},
};

const COMPACT_SYSTEM_PROMPT: &str = "You summarize conversations for continuation.";
pub const COMPACT_MAX_OUTPUT_TOKENS: u32 = 20_000;
pub const AUTO_COMPACT_BUFFER_TOKENS: u64 = 13_000;
pub const MANUAL_COMPACT_BUFFER_TOKENS: u64 = 3_000;
pub const MAX_AUTO_COMPACT_FAILURES: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactThresholds {
    pub effective_window: u64,
    pub auto_threshold: u64,
    pub blocking_threshold: u64,
}

#[derive(Clone)]
pub struct CompactRunInput {
    pub llm: LlmConfig,
    pub conversation: Vec<ModelMessage>,
    pub pre_prompt_tokens: Option<u64>,
}

struct CompactRunResult {
    summary: String,
    pre_prompt_tokens: Option<u64>,
    _usage: Option<TokenUsage>,
}

pub fn spawn_compact_loop(input: CompactRunInput, tx: Sender<AgentEvent>) {
    thread::spawn(move || {
        tx.send(AgentEvent::CompactStarted).ok();
        let mut provider = OpenAiProvider::new(input.llm.clone());
        match run_compact(input, &mut provider) {
            Ok(result) => {
                tx.send(AgentEvent::CompactFinished {
                    summary: result.summary,
                    pre_prompt_tokens: result.pre_prompt_tokens,
                })
                .ok();
            }
            Err(error) => {
                tx.send(AgentEvent::CompactFailed(format!("{error:#}")))
                    .ok();
            }
        }
    });
}

pub fn should_auto_compact(
    llm: &LlmConfig,
    prompt_tokens: Option<u64>,
    consecutive_failures: u8,
) -> bool {
    if consecutive_failures >= MAX_AUTO_COMPACT_FAILURES {
        return false;
    }
    let Some(prompt_tokens) = prompt_tokens else {
        return false;
    };
    let Some(thresholds) = compact_thresholds(llm) else {
        return false;
    };
    prompt_tokens >= thresholds.auto_threshold
}

pub fn compact_thresholds(llm: &LlmConfig) -> Option<CompactThresholds> {
    let effective_window = compact_effective_context_window(llm)?;
    Some(CompactThresholds {
        effective_window,
        auto_threshold: effective_window.saturating_sub(AUTO_COMPACT_BUFFER_TOKENS),
        blocking_threshold: effective_window.saturating_sub(MANUAL_COMPACT_BUFFER_TOKENS),
    })
}

pub fn compact_effective_context_window(llm: &LlmConfig) -> Option<u64> {
    let context_window = llm.context_window?;
    let summary_tokens = u64::from(llm.max_tokens.min(COMPACT_MAX_OUTPUT_TOKENS));
    Some(context_window.saturating_sub(summary_tokens))
}

fn run_compact(
    input: CompactRunInput,
    provider: &mut impl ModelProvider,
) -> Result<CompactRunResult> {
    if input.conversation.is_empty() {
        bail!("No messages to compact");
    }

    let max_tokens = input.llm.max_tokens.min(COMPACT_MAX_OUTPUT_TOKENS);
    let response = provider
        .complete(ModelRequest {
            messages: compact_messages(input.conversation),
            tools: Vec::new(),
            max_tokens: Some(max_tokens),
        })
        .context("compact request failed")?;

    let summary = summary_from_response(&response)?;
    Ok(CompactRunResult {
        summary,
        pre_prompt_tokens: input.pre_prompt_tokens,
        _usage: response.usage,
    })
}

fn compact_messages(conversation: Vec<ModelMessage>) -> Vec<ModelMessage> {
    let mut messages = Vec::with_capacity(conversation.len() + 2);
    messages.push(ModelMessage::system(COMPACT_SYSTEM_PROMPT));
    messages.extend(conversation);
    messages.push(ModelMessage::user(compact_prompt()));
    messages
}

fn compact_prompt() -> String {
    r#"CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.

Your task is to create a detailed summary of the conversation so far, paying close attention to the user's explicit requests and your previous actions. This summary should capture technical details, code patterns, file paths, decisions, errors, fixes, current work, and pending tasks that are essential for continuing without losing context.

Before the final summary, wrap your analysis in <analysis> tags. Then provide the final continuation summary in <summary> tags.

The <summary> section must include:
1. Primary request and intent
2. Key technical concepts and decisions
3. Files and code sections examined or changed
4. Errors encountered and fixes
5. Pending tasks
6. Current work and the most useful next step

Do not ask follow-up questions. Do not mention that you cannot access files. Summarize only from the conversation above.

Return exactly:
<analysis>
...
</analysis>

<summary>
...
</summary>"#
        .to_owned()
}

fn summary_from_response(response: &ModelResponse) -> Result<String> {
    if !response.tool_calls.is_empty() {
        bail!("compaction model attempted to call tools");
    }
    if response.finish_reason == FinishReason::Length {
        bail!("compaction summary was cut off");
    }

    let text = response
        .assistant_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .context("compaction response did not include summary text")?;
    Ok(format_compact_summary(text))
}

fn format_compact_summary(summary: &str) -> String {
    let without_analysis = strip_tag_block(summary, "analysis");
    extract_tag_content(&without_analysis, "summary")
        .unwrap_or_else(|| without_analysis.trim().to_owned())
        .trim()
        .to_owned()
}

fn strip_tag_block(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = text.find(&open) else {
        return text.to_owned();
    };
    let after_open = start + open.len();
    let Some(relative_end) = text[after_open..].find(&close) else {
        return text.to_owned();
    };
    let end = after_open + relative_end + close.len();
    let mut stripped = String::new();
    stripped.push_str(text[..start].trim_end());
    if !stripped.is_empty() {
        stripped.push('\n');
    }
    stripped.push_str(text[end..].trim_start());
    stripped
}

fn extract_tag_content(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use anyhow::Result;

    use super::*;

    struct FakeProvider {
        responses: VecDeque<ModelResponse>,
        requests: Vec<ModelRequest>,
    }

    impl FakeProvider {
        fn new(responses: Vec<ModelResponse>) -> Self {
            Self {
                responses: responses.into(),
                requests: Vec::new(),
            }
        }
    }

    impl ModelProvider for FakeProvider {
        fn complete(&mut self, request: ModelRequest) -> Result<ModelResponse> {
            self.requests.push(request);
            self.responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("no fake response queued"))
        }
    }

    fn input() -> CompactRunInput {
        CompactRunInput {
            llm: LlmConfig {
                provider: "test".to_owned(),
                base_url: "http://localhost".to_owned(),
                model: "test-model".to_owned(),
                providers: Vec::new(),
                temperature: 0.0,
                max_tokens: 100,
                context_window: Some(1000),
                api_key: "test-key".to_owned(),
                default_context_window: Some(1000),
            },
            conversation: vec![ModelMessage::user("hello")],
            pre_prompt_tokens: Some(42),
        }
    }

    fn response(text: &str) -> ModelResponse {
        ModelResponse {
            assistant_text: Some(text.to_owned()),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    #[test]
    fn compact_request_uses_no_tools_and_output_budget() {
        let mut provider = FakeProvider::new(vec![response("<summary>keep this</summary>")]);
        let result = run_compact(input(), &mut provider).unwrap();

        assert_eq!(result.summary, "keep this");
        assert_eq!(result.pre_prompt_tokens, Some(42));
        assert_eq!(provider.requests.len(), 1);
        assert!(provider.requests[0].tools.is_empty());
        assert_eq!(provider.requests[0].max_tokens, Some(100));
        assert_eq!(
            provider.requests[0].messages[0].content.as_deref(),
            Some(COMPACT_SYSTEM_PROMPT)
        );
    }

    #[test]
    fn compact_output_budget_caps_at_constant() {
        let mut input = input();
        input.llm.max_tokens = COMPACT_MAX_OUTPUT_TOKENS + 1;
        let mut provider = FakeProvider::new(vec![response("<summary>keep this</summary>")]);

        run_compact(input, &mut provider).unwrap();

        assert_eq!(
            provider.requests[0].max_tokens,
            Some(COMPACT_MAX_OUTPUT_TOKENS)
        );
    }

    #[test]
    fn format_summary_removes_analysis_and_extracts_summary() {
        let formatted =
            format_compact_summary("<analysis>scratch</analysis>\n\n<summary>\nfinal\n</summary>");

        assert_eq!(formatted, "final");
    }

    #[test]
    fn format_summary_falls_back_to_trimmed_text() {
        assert_eq!(format_compact_summary("  plain summary  "), "plain summary");
    }

    #[test]
    fn length_finish_reason_is_rejected() {
        let response = ModelResponse {
            assistant_text: Some("<summary>partial</summary>".to_owned()),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Length,
            usage: None,
        };

        let error = summary_from_response(&response).unwrap_err();

        assert!(format!("{error:#}").contains("cut off"));
    }

    #[test]
    fn auto_compact_threshold_uses_effective_context_window() {
        let mut input = input();
        input.llm.context_window = Some(1_000_000);
        input.llm.max_tokens = 8_196;

        assert_eq!(compact_effective_context_window(&input.llm), Some(991_804));
        assert_eq!(
            compact_thresholds(&input.llm),
            Some(CompactThresholds {
                effective_window: 991_804,
                auto_threshold: 978_804,
                blocking_threshold: 988_804,
            })
        );
    }

    #[test]
    fn auto_compact_summary_reserve_caps_at_twenty_thousand() {
        let mut input = input();
        input.llm.context_window = Some(1_000_000);
        input.llm.max_tokens = COMPACT_MAX_OUTPUT_TOKENS + 1;

        assert_eq!(compact_effective_context_window(&input.llm), Some(980_000));
        assert_eq!(
            compact_thresholds(&input.llm).map(|thresholds| thresholds.auto_threshold),
            Some(967_000)
        );
    }

    #[test]
    fn should_auto_compact_requires_context_usage_and_available_failures() {
        let mut input = input();
        input.llm.context_window = Some(1_000_000);
        input.llm.max_tokens = 8_196;

        assert!(!should_auto_compact(&input.llm, None, 0));
        assert!(!should_auto_compact(&input.llm, Some(978_803), 0));
        assert!(should_auto_compact(&input.llm, Some(978_804), 0));
        assert!(!should_auto_compact(
            &input.llm,
            Some(978_804),
            MAX_AUTO_COMPACT_FAILURES
        ));

        input.llm.context_window = None;
        assert!(!should_auto_compact(&input.llm, Some(978_804), 0));
    }
}
