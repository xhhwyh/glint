use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    agent::{
        TokenUsage,
        provider::{FinishReason, ModelMessage, ToolCall, ToolResult},
    },
    message::Message,
};

const SCHEMA: u16 = 3;
const COMPACT_UI_MESSAGE: &str = "Compacted conversation";
const CLEAR_UI_MESSAGE: &str = "Cleared conversation context";
const SECONDS_PER_DAY: u64 = 86_400;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TranscriptEntry {
    pub timestamp: u64,
    #[serde(rename = "type")]
    pub entry_type: TranscriptEntryType,
    pub payload: TranscriptPayload,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptEntryType {
    SessionMeta,
    TurnContext,
    ResponseItem,
    EventMsg,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum TranscriptPayload {
    SessionMeta(SessionMeta),
    TurnContext(TurnContext),
    ResponseItem(ResponseItem),
    EventMsg(EventMsg),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionMeta {
    pub schema: u16,
    pub session_id: String,
    pub cwd: String,
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TurnContext {
    pub turn_id: String,
    pub cwd: String,
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    Message {
        role: TranscriptRole,
        content: Vec<ContentBlock>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: Value,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
        #[serde(default)]
        #[serde(skip_serializing_if = "is_false")]
        is_error: bool,
    },
    CompactBoundary {
        trigger: CompactTrigger,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pre_prompt_tokens: Option<u64>,
    },
    ClearBoundary,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    InputText { text: String },
    OutputText { text: String },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger {
    Auto,
    Manual,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventMsg {
    TaskStarted {
        turn_id: String,
    },
    UserMessage {
        turn_id: Option<String>,
        message: String,
    },
    TokenCount {
        turn_id: Option<String>,
        usage: TokenUsage,
    },
    TaskComplete {
        turn_id: Option<String>,
    },
    TurnAborted {
        turn_id: Option<String>,
        reason: String,
    },
}

#[derive(Debug)]
pub struct TranscriptStore {
    path: PathBuf,
    entries: Vec<TranscriptEntry>,
    current_turn_id: Option<String>,
    pending_usage: Option<TokenUsage>,
    pending_usage_tool_calls: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptSessionSummary {
    pub path: PathBuf,
    pub session_id: String,
    pub title: String,
    pub last_timestamp: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceUsageStats {
    pub session_count: usize,
    pub turn_count: usize,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub total_cached_prompt_tokens: u64,
    pub daily_tokens: Vec<DailyTokenTotal>,
    pub model_tokens: Vec<ModelTokenTotal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyTokenTotal {
    pub day: i64,
    pub tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelTokenTotal {
    pub model: String,
    pub tokens: u64,
}

pub struct AssistantTranscript {
    pub content: String,
    pub provider: String,
    pub model: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: FinishReason,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct EntryKind {
    #[serde(rename = "type")]
    entry_type: String,
}

impl TranscriptStore {
    pub fn create_new(cwd: &str) -> Result<Self> {
        let project_dir = transcript_project_dir(cwd)?;
        Self::create_new_in_project_dir(project_dir)
    }

    fn create_new_in_project_dir(project_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&project_dir).context("failed to create transcript directory")?;
        let session_id = new_id();
        Ok(Self::empty(project_dir.join(format!("{session_id}.jsonl"))))
    }

    pub fn create_new_sibling(&self) -> Result<Self> {
        let project_dir = self
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::create_new_in_project_dir(project_dir)
    }

    pub fn load_path(path: PathBuf) -> Result<Self> {
        let mut store = Self::empty(path);
        store.load_entries()?;
        Ok(store)
    }

    pub fn tool_results_dir(&self) -> PathBuf {
        self.session_dir().join("tool-results")
    }

    pub fn archive_current(&self) -> Result<()> {
        self.archive_current_in_dir(archive_dir()?)
    }

    pub fn delete_current(&self) -> Result<()> {
        if self.path.exists() {
            fs::remove_file(&self.path).context("failed to delete transcript")?;
        }
        let session_dir = self.session_dir();
        if session_dir.exists() {
            fs::remove_dir_all(&session_dir).context("failed to delete transcript artifacts")?;
        }
        Ok(())
    }

    pub fn prune_archive_older_than(days: u64) -> Result<()> {
        let archive_dir = archive_dir()?;
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(days.saturating_mul(SECONDS_PER_DAY)))
            .unwrap_or(UNIX_EPOCH);
        prune_archive_entries_before(&archive_dir, cutoff)
    }

    pub fn prune_archive_older_than_in_background(days: u64) {
        let _ = std::thread::Builder::new()
            .name("glint-archive-prune".to_owned())
            .spawn(move || {
                let _ = Self::prune_archive_older_than(days);
            });
    }

    pub fn sessions(cwd: &str) -> Result<Vec<TranscriptSessionSummary>> {
        let project_dir = transcript_project_dir(cwd)?;
        if !project_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        for entry in fs::read_dir(project_dir).context("failed to read transcript directory")? {
            let path = entry.context("failed to read transcript entry")?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(Some(summary)) = session_summary(&path) {
                sessions.push(summary);
            }
        }
        sessions.sort_by_key(|session| Reverse(session.last_timestamp));
        Ok(sessions)
    }

    pub fn workspace_usage_stats(cwd: &str) -> Result<WorkspaceUsageStats> {
        let project_dir = transcript_project_dir(cwd)?;
        workspace_usage_stats_in_project_dir(&project_dir)
    }

    pub fn start_turn(&mut self, cwd: String, provider: String, model: String) -> Result<()> {
        self.ensure_session_meta(&cwd, &provider, &model)?;
        let turn_id = new_id();
        self.current_turn_id = Some(turn_id.clone());
        self.append(
            TranscriptEntryType::TurnContext,
            TranscriptPayload::TurnContext(TurnContext {
                turn_id: turn_id.clone(),
                cwd,
                provider,
                model,
            }),
        )?;
        self.append(
            TranscriptEntryType::EventMsg,
            TranscriptPayload::EventMsg(EventMsg::TaskStarted { turn_id }),
        )
    }

    pub fn model_history(&self) -> Vec<ModelMessage> {
        let items = self.response_items();
        let mut history = Vec::new();
        let mut index = match last_context_boundary(&items) {
            Some(ContextBoundary::Compact { index, summary }) => {
                history.push(ModelMessage::user(compact_summary_message(summary)));
                index + 1
            }
            Some(ContextBoundary::Clear { index }) => index + 1,
            None => 0,
        };

        while index < items.len() {
            match items[index] {
                ResponseItem::Message {
                    role: TranscriptRole::User,
                    content,
                    ..
                } => {
                    history.push(ModelMessage::user(content_text(content)));
                    index += 1;
                }
                ResponseItem::Message {
                    role: TranscriptRole::Assistant,
                    content,
                    ..
                } => {
                    let start = history.len();
                    let mut tool_calls = Vec::new();
                    index += 1;

                    while let Some(ResponseItem::FunctionCall {
                        call_id,
                        name,
                        arguments,
                    }) = items.get(index)
                    {
                        tool_calls.push(ToolCall {
                            id: call_id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        });
                        index += 1;
                    }

                    let text = content_text(content);
                    history.push(ModelMessage::assistant(
                        (!text.is_empty()).then_some(text),
                        tool_calls.clone(),
                    ));

                    let mut pending_calls = tool_calls
                        .iter()
                        .map(|call| call.id.clone())
                        .collect::<Vec<_>>();
                    while !pending_calls.is_empty() {
                        let Some(ResponseItem::FunctionCallOutput {
                            call_id,
                            output,
                            is_error,
                        }) = items.get(index)
                        else {
                            break;
                        };
                        let Some(call_index) = pending_calls.iter().position(|id| id == call_id)
                        else {
                            break;
                        };
                        history.push(ModelMessage::tool_result(&ToolResult {
                            call_id: call_id.clone(),
                            content: output.clone(),
                            is_error: *is_error,
                        }));
                        pending_calls.remove(call_index);
                        index += 1;
                    }

                    if !pending_calls.is_empty() {
                        history.truncate(start);
                        break;
                    }
                }
                ResponseItem::FunctionCall { .. } | ResponseItem::FunctionCallOutput { .. } => {
                    index += 1;
                }
                ResponseItem::CompactBoundary { .. } => {
                    history.clear();
                    if let ResponseItem::CompactBoundary { summary, .. } = items[index] {
                        history.push(ModelMessage::user(compact_summary_message(summary)));
                    }
                    index += 1;
                }
                ResponseItem::ClearBoundary => {
                    history.clear();
                    index += 1;
                }
            }
        }

        history
    }

    pub fn ui_messages(&self) -> Vec<Message> {
        let mut messages = Vec::new();
        let mut tool_indexes = HashMap::new();

        for item in self.response_items() {
            match item {
                ResponseItem::Message {
                    role: TranscriptRole::User,
                    content,
                    ..
                } => messages.push(Message::user(content_text(content))),
                ResponseItem::Message {
                    role: TranscriptRole::Assistant,
                    content,
                    ..
                } => {
                    let text = content_text(content);
                    if !text.is_empty() {
                        messages.push(Message::assistant(text));
                    }
                }
                ResponseItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    let index = messages.len();
                    messages.push(Message::tool_with_description(
                        call_id.clone(),
                        name.clone(),
                        tool_input_summary(name, arguments),
                        tool_description(name, arguments),
                    ));
                    tool_indexes.insert(call_id.as_str(), index);
                }
                ResponseItem::FunctionCallOutput {
                    call_id,
                    output,
                    is_error: _,
                } => {
                    if let Some(message) = tool_indexes
                        .get(call_id.as_str())
                        .and_then(|index| messages.get_mut(*index))
                    {
                        message.content = output.clone();
                        message.tool_finished = true;
                    }
                }
                ResponseItem::CompactBoundary { .. } => {
                    messages.push(Message::assistant(COMPACT_UI_MESSAGE));
                }
                ResponseItem::ClearBoundary => {
                    messages.clear();
                    tool_indexes.clear();
                    messages.push(Message::assistant(CLEAR_UI_MESSAGE));
                }
            }
        }

        messages
    }

    pub fn token_usages(&self) -> impl Iterator<Item = TokenUsage> + '_ {
        let last_clear_index = self.entries.iter().rposition(|entry| {
            matches!(
                entry.payload,
                TranscriptPayload::ResponseItem(ResponseItem::ClearBoundary)
            )
        });
        self.entries
            .iter()
            .enumerate()
            .filter(move |(index, _)| last_clear_index.is_none_or(|clear| *index > clear))
            .filter_map(|(_, entry)| match &entry.payload {
                TranscriptPayload::EventMsg(EventMsg::TokenCount { usage, .. }) => Some(*usage),
                _ => None,
            })
    }

    pub fn append_user(&mut self, content: String) -> Result<()> {
        self.append(
            TranscriptEntryType::EventMsg,
            TranscriptPayload::EventMsg(EventMsg::UserMessage {
                turn_id: self.current_turn_id.clone(),
                message: content.clone(),
            }),
        )?;
        self.append(
            TranscriptEntryType::ResponseItem,
            TranscriptPayload::ResponseItem(ResponseItem::Message {
                role: TranscriptRole::User,
                content: vec![ContentBlock::InputText { text: content }],
                provider: None,
                model: None,
                finish_reason: None,
                error: None,
            }),
        )
    }

    pub fn append_assistant(&mut self, assistant: AssistantTranscript) -> Result<()> {
        let tool_call_ids = assistant
            .tool_calls
            .iter()
            .map(|call| call.id.clone())
            .collect::<Vec<_>>();

        self.append(
            TranscriptEntryType::ResponseItem,
            TranscriptPayload::ResponseItem(ResponseItem::Message {
                role: TranscriptRole::Assistant,
                content: vec![ContentBlock::OutputText {
                    text: assistant.content,
                }],
                provider: Some(assistant.provider),
                model: Some(assistant.model),
                finish_reason: Some(finish_reason_text(assistant.finish_reason)),
                error: assistant.error,
            }),
        )?;

        for call in assistant.tool_calls {
            self.append(
                TranscriptEntryType::ResponseItem,
                TranscriptPayload::ResponseItem(ResponseItem::FunctionCall {
                    call_id: call.id,
                    name: call.name,
                    arguments: call.arguments,
                }),
            )?;
        }

        if let Some(usage) = assistant.usage {
            if tool_call_ids.is_empty() {
                self.append_usage(usage)?;
            } else {
                self.pending_usage = Some(usage);
                self.pending_usage_tool_calls = tool_call_ids;
            }
        }

        Ok(())
    }

    pub fn append_tool(&mut self, call_id: String, output: String, is_error: bool) -> Result<()> {
        let finished_call_id = call_id.clone();
        self.append(
            TranscriptEntryType::ResponseItem,
            TranscriptPayload::ResponseItem(ResponseItem::FunctionCallOutput {
                call_id,
                output,
                is_error,
            }),
        )?;

        self.pending_usage_tool_calls
            .retain(|id| id != &finished_call_id);
        if self.pending_usage_tool_calls.is_empty()
            && let Some(usage) = self.pending_usage.take()
        {
            self.append_usage(usage)?;
        }
        Ok(())
    }

    pub fn append_compact_boundary(
        &mut self,
        trigger: CompactTrigger,
        summary: String,
        pre_prompt_tokens: Option<u64>,
    ) -> Result<()> {
        self.flush_pending_usage()?;
        self.append(
            TranscriptEntryType::ResponseItem,
            TranscriptPayload::ResponseItem(ResponseItem::CompactBoundary {
                trigger,
                summary,
                pre_prompt_tokens,
            }),
        )
    }

    pub fn append_clear_boundary(&mut self) -> Result<()> {
        self.flush_pending_usage()?;
        self.append(
            TranscriptEntryType::ResponseItem,
            TranscriptPayload::ResponseItem(ResponseItem::ClearBoundary),
        )
    }

    pub fn complete_turn(&mut self) -> Result<()> {
        self.flush_pending_usage()?;
        let turn_id = self.current_turn_id.take();
        self.append(
            TranscriptEntryType::EventMsg,
            TranscriptPayload::EventMsg(EventMsg::TaskComplete { turn_id }),
        )
    }

    pub fn abort_turn(&mut self, reason: String) -> Result<()> {
        self.flush_pending_usage()?;
        let turn_id = self.current_turn_id.take();
        self.append(
            TranscriptEntryType::EventMsg,
            TranscriptPayload::EventMsg(EventMsg::TurnAborted { turn_id, reason }),
        )
    }

    fn load_entries(&mut self) -> Result<()> {
        let content = fs::read_to_string(&self.path).context("failed to read transcript")?;
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            let kind: EntryKind = serde_json::from_str(line).context("invalid transcript entry")?;
            if matches!(
                kind.entry_type.as_str(),
                "session_meta" | "turn_context" | "response_item" | "event_msg"
            ) {
                let entry: TranscriptEntry =
                    serde_json::from_str(line).context("invalid transcript entry payload")?;
                self.entries.push(entry);
            }
        }
        Ok(())
    }

    fn response_items(&self) -> Vec<&ResponseItem> {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                TranscriptPayload::ResponseItem(item) => Some(item),
                _ => None,
            })
            .collect()
    }

    fn append(
        &mut self,
        entry_type: TranscriptEntryType,
        payload: TranscriptPayload,
    ) -> Result<()> {
        let entry = TranscriptEntry {
            timestamp: now(),
            entry_type,
            payload,
        };
        append_json(&self.path, &entry)?;
        self.entries.push(entry);
        Ok(())
    }

    fn ensure_session_meta(&mut self, cwd: &str, provider: &str, model: &str) -> Result<()> {
        if self
            .entries
            .iter()
            .any(|entry| matches!(entry.payload, TranscriptPayload::SessionMeta(_)))
        {
            return Ok(());
        }

        let session_id = self
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(new_id);
        self.append(
            TranscriptEntryType::SessionMeta,
            TranscriptPayload::SessionMeta(SessionMeta {
                schema: SCHEMA,
                session_id,
                cwd: cwd.to_owned(),
                provider: provider.to_owned(),
                model: model.to_owned(),
            }),
        )
    }

    fn append_usage(&mut self, usage: TokenUsage) -> Result<()> {
        self.append(
            TranscriptEntryType::EventMsg,
            TranscriptPayload::EventMsg(EventMsg::TokenCount {
                turn_id: self.current_turn_id.clone(),
                usage,
            }),
        )
    }

    fn flush_pending_usage(&mut self) -> Result<()> {
        self.pending_usage_tool_calls.clear();
        if let Some(usage) = self.pending_usage.take() {
            self.append_usage(usage)?;
        }
        Ok(())
    }

    fn archive_current_in_dir(&self, archive_dir: PathBuf) -> Result<()> {
        let session_dir = self.session_dir();
        if !self.path.exists() && !session_dir.exists() {
            return Ok(());
        }

        fs::create_dir_all(&archive_dir).context("failed to create archive directory")?;
        let archive_stem = unique_archive_stem(&archive_dir, &self.session_stem());
        let archive_path = archive_dir.join(format!("{archive_stem}.jsonl"));
        let archive_session_dir = archive_dir.join(archive_stem);

        if self.path.exists() {
            fs::rename(&self.path, archive_path).context("failed to archive transcript")?;
        }
        if session_dir.exists() {
            fs::rename(session_dir, archive_session_dir)
                .context("failed to archive transcript artifacts")?;
        }
        Ok(())
    }

    fn session_dir(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(self.session_stem())
    }

    fn session_stem(&self) -> String {
        self.path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("session")
            .to_owned()
    }

    fn empty(path: PathBuf) -> Self {
        Self {
            path,
            entries: Vec::new(),
            current_turn_id: None,
            pending_usage: None,
            pending_usage_tool_calls: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_empty(path: PathBuf) -> Self {
        Self::empty(path)
    }
}

fn session_summary(path: &Path) -> Result<Option<TranscriptSessionSummary>> {
    let content = fs::read_to_string(path).context("failed to read transcript")?;
    let mut session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("session")
        .to_owned();
    let mut title = None;
    let mut last_timestamp = 0;

    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).context("invalid transcript entry")?;
        last_timestamp = last_timestamp.max(entry_timestamp(&value));
        if let Some(id) = value
            .get("payload")
            .and_then(|payload| payload.get("session_id"))
            .and_then(Value::as_str)
        {
            session_id = id.to_owned();
        }
        if title.is_none() {
            title = first_user_text(&value);
        }
    }

    if last_timestamp == 0 {
        return Ok(None);
    }

    Ok(Some(TranscriptSessionSummary {
        path: path.to_path_buf(),
        session_id,
        title: title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Untitled session".to_owned()),
        last_timestamp,
    }))
}

fn workspace_usage_stats_in_project_dir(project_dir: &Path) -> Result<WorkspaceUsageStats> {
    if !project_dir.exists() {
        return Ok(WorkspaceUsageStats::default());
    }

    let mut stats = UsageStatsAccumulator::default();
    for entry in fs::read_dir(project_dir).context("failed to read transcript directory")? {
        let path = entry.context("failed to read transcript entry")?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(store) = TranscriptStore::load_path(path) else {
            continue;
        };
        if store.entries.is_empty() {
            continue;
        }
        stats.session_count += 1;
        stats.accumulate(&store);
    }

    Ok(stats.finish())
}

#[derive(Default)]
struct UsageStatsAccumulator {
    session_count: usize,
    turn_count: usize,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    total_tokens: u64,
    total_cached_prompt_tokens: u64,
    daily_tokens: BTreeMap<i64, u64>,
    model_tokens: BTreeMap<String, u64>,
}

impl UsageStatsAccumulator {
    fn accumulate(&mut self, store: &TranscriptStore) {
        let mut turn_models = HashMap::new();
        let mut turn_ids = HashSet::new();

        for entry in &store.entries {
            if let TranscriptPayload::TurnContext(TurnContext {
                turn_id,
                provider,
                model,
                ..
            }) = &entry.payload
            {
                turn_models.insert(turn_id.clone(), format!("{model} · {provider}"));
                turn_ids.insert(turn_id.clone());
            }
        }
        self.turn_count += turn_ids.len();

        for entry in &store.entries {
            let TranscriptPayload::EventMsg(EventMsg::TokenCount { turn_id, usage }) =
                &entry.payload
            else {
                continue;
            };
            let model = turn_id
                .as_ref()
                .and_then(|id| turn_models.get(id))
                .cloned()
                .unwrap_or_else(|| "unknown".to_owned());
            let day = (entry.timestamp / 86_400) as i64;

            self.total_prompt_tokens = self.total_prompt_tokens.saturating_add(usage.prompt_tokens);
            self.total_completion_tokens = self
                .total_completion_tokens
                .saturating_add(usage.completion_tokens);
            self.total_tokens = self.total_tokens.saturating_add(usage.total_tokens);
            self.total_cached_prompt_tokens = self
                .total_cached_prompt_tokens
                .saturating_add(usage.cached_prompt_tokens.unwrap_or(0));
            *self.daily_tokens.entry(day).or_default() += usage.total_tokens;
            *self.model_tokens.entry(model).or_default() += usage.total_tokens;
        }
    }

    fn finish(self) -> WorkspaceUsageStats {
        let daily_tokens = self
            .daily_tokens
            .into_iter()
            .map(|(day, tokens)| DailyTokenTotal { day, tokens })
            .collect();
        let mut model_tokens = self
            .model_tokens
            .into_iter()
            .map(|(model, tokens)| ModelTokenTotal { model, tokens })
            .collect::<Vec<_>>();
        model_tokens.sort_by(|left, right| {
            right
                .tokens
                .cmp(&left.tokens)
                .then_with(|| left.model.cmp(&right.model))
        });

        WorkspaceUsageStats {
            session_count: self.session_count,
            turn_count: self.turn_count,
            total_prompt_tokens: self.total_prompt_tokens,
            total_completion_tokens: self.total_completion_tokens,
            total_tokens: self.total_tokens,
            total_cached_prompt_tokens: self.total_cached_prompt_tokens,
            daily_tokens,
            model_tokens,
        }
    }
}

fn entry_timestamp(value: &Value) -> u64 {
    value
        .get("timestamp")
        .and_then(Value::as_u64)
        .or_else(|| value.get("ts").and_then(Value::as_u64))
        .unwrap_or(0)
}

fn first_user_text(value: &Value) -> Option<String> {
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) == Some("user_message") {
        return payload
            .get("message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    if payload.get("type").and_then(Value::as_str) == Some("message")
        && payload.get("role").and_then(Value::as_str) == Some("user")
    {
        return payload
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| content_blocks_text(blocks));
    }
    if value.get("type").and_then(Value::as_str) == Some("message")
        && value.get("role").and_then(Value::as_str) == Some("user")
    {
        return value
            .get("content")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    None
}

fn content_blocks_text(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect()
}

fn append_json(path: &PathBuf, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .context("failed to open transcript")?;
    serde_json::to_writer(&mut file, value).context("failed to serialize transcript entry")?;
    writeln!(file).context("failed to write transcript entry")?;
    Ok(())
}

fn unique_archive_stem(archive_dir: &Path, preferred: &str) -> String {
    let mut suffix = 0;
    loop {
        let candidate = if suffix == 0 {
            preferred.to_owned()
        } else {
            format!("{preferred}-{}-{suffix}", now())
        };
        let transcript_path = archive_dir.join(format!("{candidate}.jsonl"));
        let session_dir = archive_dir.join(&candidate);
        if !transcript_path.exists() && !session_dir.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

fn prune_archive_entries_before(archive_dir: &Path, cutoff: SystemTime) -> Result<()> {
    if !archive_dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(archive_dir).context("failed to read archive directory")? {
        let path = entry.context("failed to read archive entry")?.path();
        let metadata = fs::metadata(&path).context("failed to read archive metadata")?;
        let modified = metadata
            .modified()
            .context("failed to read archive modification time")?;
        if modified >= cutoff {
            continue;
        }
        if metadata.is_dir() {
            fs::remove_dir_all(&path).context("failed to delete archived session directory")?;
        } else {
            fs::remove_file(&path).context("failed to delete archived session")?;
        }
    }
    Ok(())
}

fn archive_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".glint").join("archive"))
}

fn transcript_project_dir(cwd: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".glint")
        .join("projects")
        .join(sanitize_cwd(cwd)))
}

fn sanitize_cwd(cwd: &str) -> String {
    let sanitized = cwd.replace('/', "-");
    if sanitized.is_empty() {
        "-".to_owned()
    } else {
        sanitized
    }
}

fn finish_reason_text(reason: FinishReason) -> String {
    match reason {
        FinishReason::Stop => "stop".to_owned(),
        FinishReason::ToolCalls => "tool_calls".to_owned(),
        FinishReason::Length => "length".to_owned(),
        FinishReason::Other(reason) => reason,
    }
}

enum ContextBoundary<'a> {
    Compact { index: usize, summary: &'a str },
    Clear { index: usize },
}

fn last_context_boundary<'a>(items: &[&'a ResponseItem]) -> Option<ContextBoundary<'a>> {
    items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| match item {
            ResponseItem::CompactBoundary { summary, .. } => Some(ContextBoundary::Compact {
                index,
                summary: summary.as_str(),
            }),
            ResponseItem::ClearBoundary => Some(ContextBoundary::Clear { index }),
            _ => None,
        })
}

fn compact_summary_message(summary: &str) -> String {
    format!(
        "This session is being continued from a previous conversation that was compacted. The summary below covers the earlier portion of the conversation.\n\n{}\n\nContinue from this context without asking the user to repeat prior details.",
        summary.trim()
    )
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::InputText { text } | ContentBlock::OutputText { text } => text.as_str(),
        })
        .collect()
}

fn tool_input_summary(name: &str, args: &Value) -> String {
    match name {
        "Read" | "Edit" => string_arg(args, "file_path"),
        "Glob" | "Grep" => string_arg(args, "pattern"),
        "Bash" | "TerminalRun" => string_arg(args, "command"),
        _ => None,
    }
    .unwrap_or_else(|| args.to_string())
}

fn tool_description(name: &str, args: &Value) -> Option<String> {
    matches!(name, "Bash" | "TerminalRun")
        .then(|| string_arg(args, "description"))
        .flatten()
}

fn string_arg(args: &Value, name: &str) -> Option<String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> TranscriptStore {
        TranscriptStore::empty(
            std::env::temp_dir().join(format!("glint-transcript-test-{}.jsonl", new_id())),
        )
    }

    fn assistant(content: &str) -> AssistantTranscript {
        AssistantTranscript {
            content: content.to_owned(),
            provider: "test".to_owned(),
            model: "test-model".to_owned(),
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: FinishReason::Stop,
            error: None,
        }
    }

    #[test]
    fn create_new_starts_empty_even_when_saved_sessions_exist() {
        let cwd = "/tmp/glint-create-new-test";
        let project_dir = std::env::temp_dir().join(format!("glint-create-new-test-{}", new_id()));

        let mut existing = TranscriptStore::create_new_in_project_dir(project_dir.clone())
            .expect("create existing session");
        existing
            .start_turn(cwd.to_owned(), "provider".to_owned(), "model".to_owned())
            .expect("start turn");
        existing
            .append_user("old prompt".to_owned())
            .expect("append user");

        let fresh = TranscriptStore::create_new_in_project_dir(project_dir.clone())
            .expect("create fresh session");

        assert!(fresh.ui_messages().is_empty());
        assert!(fresh.model_history().is_empty());
        assert_eq!(fresh.token_usages().count(), 0);

        fs::remove_dir_all(project_dir).ok();
    }

    #[test]
    fn load_path_accepts_successful_tool_outputs_without_is_error() {
        let cwd = "/tmp/glint-load-path-tool-output-test";
        let project_dir =
            std::env::temp_dir().join(format!("glint-load-path-tool-output-test-{}", new_id()));

        let mut store = TranscriptStore::create_new_in_project_dir(project_dir.clone())
            .expect("create session");
        store
            .start_turn(cwd.to_owned(), "provider".to_owned(), "model".to_owned())
            .expect("start turn");
        store
            .append_user("read file".to_owned())
            .expect("append user");
        store
            .append_assistant(AssistantTranscript {
                content: String::new(),
                provider: "provider".to_owned(),
                model: "model".to_owned(),
                tool_calls: vec![ToolCall {
                    id: "call-one".to_owned(),
                    name: "Read".to_owned(),
                    arguments: serde_json::json!({ "file_path": "Cargo.toml" }),
                }],
                usage: None,
                finish_reason: FinishReason::ToolCalls,
                error: None,
            })
            .expect("append assistant");
        store
            .append_tool("call-one".to_owned(), "tool output".to_owned(), false)
            .expect("append successful tool output");

        let loaded = TranscriptStore::load_path(store.path.clone()).expect("load transcript");
        let messages = loaded.ui_messages();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "read file");
        assert_eq!(messages[1].content, "tool output");
        assert!(messages[1].tool_finished);

        fs::remove_dir_all(project_dir).ok();
    }

    #[test]
    fn archive_current_moves_transcript_and_artifacts() {
        let project_dir =
            std::env::temp_dir().join(format!("glint-archive-session-test-{}", new_id()));
        let archive_dir =
            std::env::temp_dir().join(format!("glint-archive-target-test-{}", new_id()));
        let mut transcript =
            TranscriptStore::create_new_in_project_dir(project_dir.clone()).unwrap();
        transcript.append_user("archive me".to_owned()).unwrap();
        let original_path = transcript.path.clone();
        let original_session_dir = transcript.session_dir();
        let archived_stem = transcript.session_stem();
        let artifact_path = original_session_dir
            .join("tool-results")
            .join("large-output.txt");
        fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
        fs::write(&artifact_path, "large output").unwrap();

        transcript
            .archive_current_in_dir(archive_dir.clone())
            .unwrap();

        assert!(!original_path.exists());
        assert!(!original_session_dir.exists());
        assert!(archive_dir.join(format!("{archived_stem}.jsonl")).exists());
        assert!(
            archive_dir
                .join(archived_stem)
                .join("tool-results")
                .join("large-output.txt")
                .exists()
        );

        fs::remove_dir_all(project_dir).ok();
        fs::remove_dir_all(archive_dir).ok();
    }

    #[test]
    fn delete_current_removes_transcript_and_artifacts() {
        let project_dir =
            std::env::temp_dir().join(format!("glint-delete-session-test-{}", new_id()));
        let mut transcript =
            TranscriptStore::create_new_in_project_dir(project_dir.clone()).unwrap();
        transcript.append_user("delete me".to_owned()).unwrap();
        let original_path = transcript.path.clone();
        let original_session_dir = transcript.session_dir();
        let artifact_path = original_session_dir
            .join("tool-results")
            .join("large-output.txt");
        fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
        fs::write(&artifact_path, "large output").unwrap();

        transcript.delete_current().unwrap();

        assert!(!original_path.exists());
        assert!(!original_session_dir.exists());

        fs::remove_dir_all(project_dir).ok();
    }

    #[test]
    fn prune_archive_entries_before_removes_old_entries() {
        let archive_dir =
            std::env::temp_dir().join(format!("glint-prune-archive-test-{}", new_id()));
        let archived_file = archive_dir.join("old-session.jsonl");
        let archived_dir = archive_dir.join("old-session");
        fs::create_dir_all(&archived_dir).unwrap();
        fs::write(&archived_file, "{}\n").unwrap();
        fs::write(archived_dir.join("artifact.txt"), "artifact").unwrap();

        prune_archive_entries_before(&archive_dir, UNIX_EPOCH).unwrap();
        assert!(archived_file.exists());
        assert!(archived_dir.exists());

        prune_archive_entries_before(&archive_dir, SystemTime::now() + Duration::from_secs(1))
            .unwrap();
        assert!(!archived_file.exists());
        assert!(!archived_dir.exists());

        fs::remove_dir_all(archive_dir).ok();
    }

    #[test]
    fn model_history_is_unchanged_without_compact_boundary() {
        let mut transcript = store();
        transcript.append_user("first user".to_owned()).unwrap();
        transcript
            .append_assistant(assistant("first assistant"))
            .unwrap();

        let history = transcript.model_history();

        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content.as_deref(), Some("first user"));
        assert_eq!(history[1].content.as_deref(), Some("first assistant"));
    }

    #[test]
    fn model_history_starts_from_last_compact_boundary() {
        let mut transcript = store();
        transcript.append_user("old user".to_owned()).unwrap();
        transcript
            .append_assistant(assistant("old assistant"))
            .unwrap();
        transcript
            .append_compact_boundary(
                CompactTrigger::Manual,
                "important summary".to_owned(),
                Some(8),
            )
            .unwrap();
        transcript.append_user("new user".to_owned()).unwrap();

        let history = transcript.model_history();

        assert_eq!(history.len(), 2);
        assert!(
            history[0]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("important summary"))
        );
        assert_eq!(history[1].content.as_deref(), Some("new user"));
        assert!(
            !history
                .iter()
                .any(|message| message.content.as_deref() == Some("old user"))
        );
    }

    #[test]
    fn model_history_uses_only_the_latest_compact_boundary() {
        let mut transcript = store();
        transcript.append_user("old user".to_owned()).unwrap();
        transcript
            .append_compact_boundary(CompactTrigger::Manual, "first summary".to_owned(), None)
            .unwrap();
        transcript.append_user("middle user".to_owned()).unwrap();
        transcript
            .append_compact_boundary(CompactTrigger::Manual, "second summary".to_owned(), None)
            .unwrap();
        transcript.append_user("latest user".to_owned()).unwrap();

        let history = transcript.model_history();

        assert_eq!(history.len(), 2);
        assert!(
            history[0]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("second summary"))
        );
        assert!(
            !history
                .iter()
                .any(|message| message.content.as_deref().is_some_and(|content| {
                    content.contains("first summary") || content.contains("middle user")
                }))
        );
    }

    #[test]
    fn model_history_starts_after_clear_boundary() {
        let mut transcript = store();
        transcript.append_user("old user".to_owned()).unwrap();
        transcript
            .append_assistant(assistant("old assistant"))
            .unwrap();
        transcript.append_clear_boundary().unwrap();
        transcript.append_user("new user".to_owned()).unwrap();

        let history = transcript.model_history();

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content.as_deref(), Some("new user"));
    }

    #[test]
    fn clear_boundary_after_compact_drops_compact_summary() {
        let mut transcript = store();
        transcript
            .append_compact_boundary(CompactTrigger::Manual, "old summary".to_owned(), None)
            .unwrap();
        transcript.append_clear_boundary().unwrap();
        transcript.append_user("new user".to_owned()).unwrap();

        let history = transcript.model_history();

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content.as_deref(), Some("new user"));
    }

    #[test]
    fn ui_messages_show_compact_marker_without_summary() {
        let mut transcript = store();
        transcript.append_user("old user".to_owned()).unwrap();
        transcript
            .append_compact_boundary(
                CompactTrigger::Manual,
                "secret detailed summary".to_owned(),
                None,
            )
            .unwrap();

        let messages = transcript.ui_messages();

        assert!(
            messages
                .iter()
                .any(|message| message.content == COMPACT_UI_MESSAGE)
        );
        assert!(
            !messages
                .iter()
                .any(|message| message.content.contains("secret detailed summary"))
        );
    }

    #[test]
    fn ui_messages_start_after_clear_boundary() {
        let mut transcript = store();
        transcript.append_user("old user".to_owned()).unwrap();
        transcript.append_clear_boundary().unwrap();
        transcript.append_user("new user".to_owned()).unwrap();

        let messages = transcript.ui_messages();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, CLEAR_UI_MESSAGE);
        assert_eq!(messages[1].content, "new user");
    }

    #[test]
    fn token_usages_start_after_clear_boundary() {
        let mut transcript = store();
        transcript
            .append_usage(TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 25,
                total_tokens: 125,
                cached_prompt_tokens: None,
            })
            .unwrap();
        transcript.append_clear_boundary().unwrap();
        transcript
            .append_usage(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cached_prompt_tokens: None,
            })
            .unwrap();

        let usages = transcript.token_usages().collect::<Vec<_>>();

        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].total_tokens, 15);
    }

    #[test]
    fn workspace_usage_stats_group_by_day_and_model() {
        let cwd = "/tmp/glint-workspace-stats-test";
        let project_dir = std::env::temp_dir().join(format!("glint-workspace-stats-{}", new_id()));
        let mut transcript =
            TranscriptStore::create_new_in_project_dir(project_dir.clone()).unwrap();
        transcript
            .start_turn(cwd.to_owned(), "provider".to_owned(), "model".to_owned())
            .unwrap();
        transcript
            .append_usage(TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 25,
                total_tokens: 125,
                cached_prompt_tokens: Some(40),
            })
            .unwrap();

        let stats = workspace_usage_stats_in_project_dir(&project_dir).unwrap();

        assert_eq!(stats.session_count, 1);
        assert_eq!(stats.turn_count, 1);
        assert_eq!(stats.total_prompt_tokens, 100);
        assert_eq!(stats.total_completion_tokens, 25);
        assert_eq!(stats.total_tokens, 125);
        assert_eq!(stats.total_cached_prompt_tokens, 40);
        assert_eq!(stats.daily_tokens.len(), 1);
        assert_eq!(stats.daily_tokens[0].tokens, 125);
        assert_eq!(stats.model_tokens.len(), 1);
        assert_eq!(stats.model_tokens[0].model, "model · provider");
        assert_eq!(stats.model_tokens[0].tokens, 125);

        fs::remove_dir_all(project_dir).ok();
    }

    #[test]
    fn compact_trigger_auto_round_trips_as_snake_case() {
        let item = ResponseItem::CompactBoundary {
            trigger: CompactTrigger::Auto,
            summary: "summary".to_owned(),
            pre_prompt_tokens: Some(10),
        };

        let encoded = serde_json::to_string(&item).unwrap();
        let decoded: ResponseItem = serde_json::from_str(&encoded).unwrap();

        assert!(encoded.contains("\"trigger\":\"auto\""));
        assert!(matches!(
            decoded,
            ResponseItem::CompactBoundary {
                trigger: CompactTrigger::Auto,
                ..
            }
        ));
    }

    #[test]
    fn clear_boundary_round_trips_as_snake_case() {
        let item = ResponseItem::ClearBoundary;

        let encoded = serde_json::to_string(&item).unwrap();
        let decoded: ResponseItem = serde_json::from_str(&encoded).unwrap();

        assert!(encoded.contains("\"type\":\"clear_boundary\""));
        assert!(matches!(decoded, ResponseItem::ClearBoundary));
    }
}
