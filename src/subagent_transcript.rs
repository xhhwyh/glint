use serde::{Deserialize, Serialize};

use crate::{
    agent::AgentEvent,
    message::{Message, Role},
    tasks::{SubagentRequest, TaskSnapshot, TaskStatus},
};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SubagentTranscriptSnapshot {
    pub task_id: String,
    pub tool_call_id: String,
    pub description: String,
    pub prompt: String,
    pub messages: Vec<Message>,
    pub activity: Option<String>,
    pub status: TaskStatus,
    pub tool_use_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentTranscript {
    snapshot: SubagentTranscriptSnapshot,
}

impl SubagentTranscript {
    pub fn new(request: &SubagentRequest) -> Self {
        Self {
            snapshot: SubagentTranscriptSnapshot {
                task_id: request.task_id.clone(),
                tool_call_id: request.tool_call_id.clone(),
                description: request.description.clone(),
                prompt: request.prompt.clone(),
                messages: vec![Message::user(request.prompt.clone())],
                activity: Some("Starting".to_owned()),
                status: TaskStatus::Running,
                tool_use_count: 0,
            },
        }
    }

    pub fn from_snapshot(snapshot: SubagentTranscriptSnapshot) -> Self {
        Self { snapshot }
    }

    pub fn snapshot(&self) -> SubagentTranscriptSnapshot {
        self.snapshot.clone()
    }

    #[allow(dead_code)] // Used by the inline execution-card renderer added in the next task.
    pub fn task_id(&self) -> &str {
        &self.snapshot.task_id
    }

    #[allow(dead_code)] // Used by the inline execution-card renderer added in the next task.
    pub fn tool_call_id(&self) -> &str {
        &self.snapshot.tool_call_id
    }

    #[allow(dead_code)] // Used by the inline execution-card renderer added in the next task.
    pub fn description(&self) -> &str {
        &self.snapshot.description
    }

    #[allow(dead_code)] // Used by the inline execution-card renderer added in the next task.
    pub fn prompt(&self) -> &str {
        &self.snapshot.prompt
    }

    #[allow(dead_code)] // Used by the inline execution-card renderer added in the next task.
    pub fn messages(&self) -> &[Message] {
        &self.snapshot.messages
    }

    #[allow(dead_code)] // Used by the inline execution-card renderer added in the next task.
    pub fn activity(&self) -> Option<&str> {
        self.snapshot.activity.as_deref()
    }

    #[allow(dead_code)] // Used by the inline execution-card renderer added in the next task.
    pub fn status(&self) -> TaskStatus {
        self.snapshot.status
    }

    #[allow(dead_code)] // Used by the inline execution-card renderer added in the next task.
    pub fn tool_use_count(&self) -> u32 {
        self.snapshot.tool_use_count
    }

    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Started => {
                self.snapshot.status = TaskStatus::Running;
                self.snapshot.activity = Some("Thinking".to_owned());
                self.snapshot.messages.push(Message::assistant(""));
            }
            AgentEvent::AssistantDelta(delta) => {
                self.snapshot.activity = None;
                self.append_assistant_delta(delta);
            }
            AgentEvent::AssistantTurn { .. } => {}
            AgentEvent::ToolStarted {
                id,
                name,
                input_summary,
                input_description,
            } => {
                self.snapshot.activity = Some(format!("Running {name}: {input_summary}"));
                self.remove_empty_assistant_tail();
                self.snapshot.messages.push(Message::tool_with_description(
                    id,
                    name,
                    input_summary,
                    input_description.clone(),
                ));
                self.snapshot.tool_use_count = self.snapshot.tool_use_count.saturating_add(1);
            }
            AgentEvent::ToolFinished {
                id,
                name,
                output,
                is_error,
                output_summary,
            } => {
                self.snapshot.activity = Some(format!("Finished {name}: {output_summary}"));
                if let Some(message) = self.tool_message_mut(id) {
                    if name != "Read" {
                        message.content = output.clone();
                    }
                    message.tool_finished = true;
                    message.tool_is_error = *is_error;
                }
            }
            AgentEvent::ToolApprovalRequested(request) => {
                self.snapshot.activity = Some(format!("Approval unavailable: {}", request.command));
            }
            AgentEvent::ConversationPermissionChanged { .. }
            | AgentEvent::TodoUpdated(_)
            | AgentEvent::CompactStarted
            | AgentEvent::CompactFinished { .. }
            | AgentEvent::CompactFailed(_) => {}
            AgentEvent::AssistantFinished => {
                self.snapshot.activity = None;
            }
            AgentEvent::Failed(error) => {
                self.snapshot.activity = None;
                self.snapshot.status = TaskStatus::Failed;
                self.remove_empty_assistant_tail();
                self.append_assistant_delta(error);
            }
        }
    }

    pub fn append_steering(&mut self, message: String) {
        self.snapshot.messages.push(Message::user(message));
    }

    pub fn finish(&mut self, task: &TaskSnapshot) {
        self.snapshot.status = task.status;
        self.snapshot.activity = task.activity.clone();
        self.snapshot.tool_use_count = task.tool_use_count;
    }

    fn append_assistant_delta(&mut self, delta: &str) {
        if let Some(message) = self
            .snapshot
            .messages
            .last_mut()
            .filter(|message| message.role == Role::Assistant)
        {
            message.content.push_str(delta);
        } else {
            self.snapshot.messages.push(Message::assistant(delta));
        }
    }

    fn remove_empty_assistant_tail(&mut self) {
        if self
            .snapshot
            .messages
            .last()
            .is_some_and(|message| message.role == Role::Assistant && message.content.is_empty())
        {
            self.snapshot.messages.pop();
        }
    }

    fn tool_message_mut(&mut self, id: &str) -> Option<&mut Message> {
        self.snapshot
            .messages
            .iter_mut()
            .rev()
            .find(|message| message.tool_call_id.as_deref() == Some(id))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        agent::AgentEvent,
        message::Role,
        tasks::{SubagentBackend, SubagentRequest, TaskKind, TaskSnapshot, TaskStatus},
    };

    use super::*;

    fn request(task_id: &str, tool_call_id: &str) -> SubagentRequest {
        SubagentRequest {
            task_id: task_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
            description: "inspect parser".to_owned(),
            prompt: "check the parser".to_owned(),
            agent: None,
            backend: SubagentBackend::Codex,
            cwd: "/workspace".to_owned(),
        }
    }

    fn finished_task(status: TaskStatus) -> TaskSnapshot {
        TaskSnapshot {
            id: "a1".to_owned(),
            tool_call_id: "call-subagent".to_owned(),
            kind: TaskKind::Subagent,
            status,
            description: "inspect parser".to_owned(),
            backend: SubagentBackend::Codex,
            cwd: "/workspace".to_owned(),
            started_at_ms: 1,
            ended_at_ms: Some(2),
            summary: Some("completed".to_owned()),
            activity: Some("completed".to_owned()),
            tool_use_count: 1,
            result: Some("done".to_owned()),
            error: None,
        }
    }

    #[test]
    fn transcript_applies_agent_events_by_task_id() {
        let request = request("a1", "call-subagent");
        let mut transcript = SubagentTranscript::new(&request);

        transcript.apply(&AgentEvent::Started);
        transcript.apply(&AgentEvent::AssistantDelta("checking".to_owned()));
        transcript.apply(&AgentEvent::ToolStarted {
            id: "tool-1".to_owned(),
            name: "Grep".to_owned(),
            input_summary: "Bash".to_owned(),
            input_description: None,
        });
        transcript.apply(&AgentEvent::ToolFinished {
            id: "tool-1".to_owned(),
            name: "Grep".to_owned(),
            output: "src/app.rs:1".to_owned(),
            is_error: false,
            output_summary: "1 match".to_owned(),
        });

        assert_eq!(transcript.task_id(), "a1");
        assert_eq!(transcript.tool_call_id(), "call-subagent");
        assert_eq!(transcript.description(), "inspect parser");
        assert_eq!(transcript.prompt(), "check the parser");
        assert_eq!(transcript.status(), TaskStatus::Running);
        assert_eq!(transcript.tool_use_count(), 1);
        assert!(
            transcript
                .messages()
                .iter()
                .any(|message| message.content.contains("checking"))
        );
        let tool = transcript
            .messages()
            .iter()
            .find(|message| message.tool_call_id.as_deref() == Some("tool-1"))
            .expect("tool message");
        assert_eq!(tool.content, "src/app.rs:1");
        assert!(tool.tool_finished);
        assert!(!tool.tool_is_error);
    }

    #[test]
    fn transcript_elides_read_output_and_marks_tool_failures() {
        let mut transcript = SubagentTranscript::new(&request("a1", "call-subagent"));
        transcript.apply(&AgentEvent::ToolStarted {
            id: "read-1".to_owned(),
            name: "Read".to_owned(),
            input_summary: "src/app.rs".to_owned(),
            input_description: None,
        });
        transcript.apply(&AgentEvent::ToolFinished {
            id: "read-1".to_owned(),
            name: "Read".to_owned(),
            output: "large file contents".to_owned(),
            is_error: false,
            output_summary: "200 lines".to_owned(),
        });
        transcript.apply(&AgentEvent::ToolStarted {
            id: "grep-1".to_owned(),
            name: "Grep".to_owned(),
            input_summary: "missing".to_owned(),
            input_description: None,
        });
        transcript.apply(&AgentEvent::ToolFinished {
            id: "grep-1".to_owned(),
            name: "Grep".to_owned(),
            output: "grep failed".to_owned(),
            is_error: true,
            output_summary: "failed".to_owned(),
        });

        let read = transcript
            .messages()
            .iter()
            .find(|message| message.tool_call_id.as_deref() == Some("read-1"))
            .expect("read message");
        assert!(read.content.is_empty());
        assert!(read.tool_finished);
        let grep = transcript
            .messages()
            .iter()
            .find(|message| message.tool_call_id.as_deref() == Some("grep-1"))
            .expect("grep message");
        assert_eq!(grep.content, "grep failed");
        assert!(grep.tool_is_error);
    }

    #[test]
    fn transcript_serializes_steering_and_terminal_status() {
        let mut transcript = SubagentTranscript::new(&request("a1", "call-subagent"));
        transcript.append_steering("also inspect tests".to_owned());
        transcript.finish(&finished_task(TaskStatus::Completed));

        let snapshot = transcript.snapshot();
        let encoded = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let restored: SubagentTranscriptSnapshot =
            serde_json::from_str(&encoded).expect("deserialize snapshot");

        assert_eq!(restored.status, TaskStatus::Completed);
        assert_eq!(restored.activity.as_deref(), Some("completed"));
        assert_eq!(restored.tool_use_count, 1);
        assert!(restored.messages.iter().any(|message| {
            message.role == Role::User && message.content == "also inspect tests"
        }));
        assert_eq!(SubagentTranscript::from_snapshot(restored).task_id(), "a1");
    }

    #[test]
    fn transcript_records_agent_failure_text() {
        let mut transcript = SubagentTranscript::new(&request("a1", "call-subagent"));
        transcript.apply(&AgentEvent::Started);
        transcript.apply(&AgentEvent::Failed("model unavailable".to_owned()));

        assert_eq!(transcript.status(), TaskStatus::Failed);
        assert!(
            transcript
                .messages()
                .iter()
                .any(|message| message.content == "model unavailable")
        );
    }

    #[test]
    fn snapshot_defaults_legacy_tool_error_flag_to_false() {
        let snapshot: SubagentTranscriptSnapshot = serde_json::from_value(serde_json::json!({
            "task_id": "a1",
            "tool_call_id": "call-subagent",
            "description": "inspect parser",
            "prompt": "check parser",
            "messages": [{
                "role": "Tool",
                "content": "done",
                "tool_call_id": "tool-1",
                "tool_name": "Grep",
                "tool_input": "parser",
                "tool_description": null,
                "tool_finished": true,
                "progress": null
            }],
            "activity": "completed",
            "status": "Completed",
            "tool_use_count": 1
        }))
        .expect("deserialize legacy snapshot");

        assert!(!snapshot.messages[0].tool_is_error);
    }
}
