use std::{
    cmp::Reverse,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::Sender,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const MAX_RUNNING_SUBAGENTS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    Subagent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentBackend {
    Codex,
}

impl SubagentBackend {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("codex").trim() {
            "" | "codex" => Ok(Self::Codex),
            backend => Err(format!("unsupported subagent backend: {backend}")),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentRequest {
    pub task_id: String,
    pub description: String,
    pub prompt: String,
    pub agent: Option<String>,
    pub backend: SubagentBackend,
    pub cwd: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentStartResponse {
    pub task_id: String,
    pub terminal_tab: Option<usize>,
    pub error: Option<String>,
}

impl SubagentStartResponse {
    pub fn started(task_id: String, terminal_tab: usize) -> Self {
        Self {
            task_id,
            terminal_tab: Some(terminal_tab),
            error: None,
        }
    }

    pub fn failed(task_id: String, error: impl Into<String>) -> Self {
        Self {
            task_id,
            terminal_tab: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub id: String,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub description: String,
    pub backend: SubagentBackend,
    pub cwd: String,
    pub terminal_tab: Option<usize>,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub summary: Option<String>,
    pub activity: Option<String>,
    pub tool_use_count: u32,
    pub result: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentOutcome {
    pub final_message: String,
    pub error: Option<String>,
    pub cancelled: bool,
}

impl SubagentOutcome {
    pub fn completed(final_message: impl Into<String>) -> Self {
        Self {
            final_message: final_message.into(),
            error: None,
            cancelled: false,
        }
    }

    pub fn failed(error: impl Into<String>, final_message: impl Into<String>) -> Self {
        Self {
            final_message: final_message.into(),
            error: Some(error.into()),
            cancelled: false,
        }
    }

    pub fn cancelled(final_message: impl Into<String>) -> Self {
        Self {
            final_message: final_message.into(),
            error: Some("cancelled".to_owned()),
            cancelled: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskWaitResponse {
    pub tasks: Vec<TaskSnapshot>,
    pub timed_out: bool,
}

pub enum TaskRequest {
    StartSubagent {
        request: SubagentRequest,
        response: Sender<SubagentStartResponse>,
    },
    List {
        response: Sender<Vec<TaskSnapshot>>,
    },
    Wait {
        task_ids: Vec<String>,
        timeout: Duration,
        response: Sender<Result<TaskWaitResponse, String>>,
    },
    Send {
        task_id: String,
        message: String,
        response: Sender<Result<TaskSnapshot, String>>,
    },
    Cancel {
        task_id: String,
        response: Sender<Result<TaskSnapshot, String>>,
    },
}

#[derive(Default)]
pub struct SubagentSteering {
    state: Mutex<SubagentSteeringState>,
}

#[derive(Default)]
struct SubagentSteeringState {
    closed: bool,
    messages: Vec<String>,
}

impl SubagentSteering {
    pub fn send(&self, message: String) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "subagent steering state is unavailable".to_owned())?;
        if state.closed {
            return Err("subagent is no longer accepting messages".to_owned());
        }
        state.messages.push(message);
        Ok(())
    }

    pub fn drain(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|mut state| std::mem::take(&mut state.messages))
            .unwrap_or_default()
    }

    pub fn finish_or_drain(&self) -> Option<Vec<String>> {
        let mut state = self.state.lock().ok()?;
        if state.messages.is_empty() {
            state.closed = true;
            None
        } else {
            Some(std::mem::take(&mut state.messages))
        }
    }

    pub fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
        }
    }
}

#[derive(Default)]
pub struct TaskManager {
    tasks: Vec<TaskSnapshot>,
}

impl TaskManager {
    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        let mut tasks = self.tasks.clone();
        tasks.sort_by_key(|task| Reverse(task.started_at_ms));
        tasks
    }

    pub fn running_subagent_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Subagent && task.status.is_running())
            .count()
    }

    pub fn snapshot(&self, task_id: &str) -> Option<TaskSnapshot> {
        self.tasks.iter().find(|task| task.id == task_id).cloned()
    }

    pub fn snapshots_for(&self, task_ids: &[String]) -> Result<Vec<TaskSnapshot>, String> {
        task_ids
            .iter()
            .map(|task_id| {
                self.snapshot(task_id)
                    .ok_or_else(|| format!("unknown task: {task_id}"))
            })
            .collect()
    }

    pub fn start_subagent(
        &mut self,
        request: &SubagentRequest,
        terminal_tab: usize,
    ) -> Result<TaskSnapshot, String> {
        if self.running_subagent_count() >= MAX_RUNNING_SUBAGENTS {
            return Err(format!(
                "too many running subagents; limit is {MAX_RUNNING_SUBAGENTS}"
            ));
        }
        if self.tasks.iter().any(|task| task.id == request.task_id) {
            return Err(format!("task already exists: {}", request.task_id));
        }

        let mut task = TaskSnapshot {
            id: request.task_id.clone(),
            kind: TaskKind::Subagent,
            status: TaskStatus::Queued,
            description: request.description.clone(),
            backend: request.backend,
            cwd: request.cwd.clone(),
            terminal_tab: Some(terminal_tab),
            started_at_ms: now_ms(),
            ended_at_ms: None,
            summary: None,
            activity: Some("Starting".to_owned()),
            tool_use_count: 0,
            result: None,
            error: None,
        };
        task.status = TaskStatus::Running;
        self.tasks.push(task.clone());
        Ok(task)
    }

    pub fn finish_subagent(
        &mut self,
        task_id: &str,
        outcome: SubagentOutcome,
    ) -> Option<TaskSnapshot> {
        let task = self.tasks.iter_mut().find(|task| task.id == task_id)?;
        task.status = subagent_outcome_status(&outcome);
        task.ended_at_ms = Some(now_ms());
        task.error = outcome.error;
        task.summary = Some(task_status_summary(task));
        task.activity = task.summary.clone();
        task.result = (!outcome.final_message.trim().is_empty())
            .then(|| outcome.final_message.trim().to_owned());
        Some(task.clone())
    }

    pub fn update_subagent_activity(
        &mut self,
        task_id: &str,
        activity: impl Into<String>,
        tool_started: bool,
    ) {
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == task_id) else {
            return;
        };
        if !task.status.is_running() {
            return;
        }
        let activity = activity.into();
        if !activity.trim().is_empty() {
            task.activity = Some(activity);
        }
        if tool_started {
            task.tool_use_count = task.tool_use_count.saturating_add(1);
        }
    }

    pub fn terminal_tab_has_running_task(&self, terminal_tab: usize) -> bool {
        self.tasks.iter().any(|task| {
            task.terminal_tab == Some(terminal_tab)
                && task.kind == TaskKind::Subagent
                && task.status.is_running()
        })
    }

    pub fn handle_terminal_tab_closed(&mut self, closed_index: usize) {
        for task in &mut self.tasks {
            let Some(tab) = task.terminal_tab else {
                continue;
            };
            if tab == closed_index {
                task.terminal_tab = None;
            } else if tab > closed_index {
                task.terminal_tab = Some(tab - 1);
            }
        }
    }
}

pub fn next_task_id() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    format!("a{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

pub fn task_started_message(task: &TaskSnapshot) -> String {
    format!("Started Codex subagent {}: {}", task.id, task.description)
}

pub fn task_finished_message(task: &TaskSnapshot) -> String {
    match task.status {
        TaskStatus::Completed => format!("Codex subagent {} completed.", task.id),
        TaskStatus::Failed => format!(
            "Codex subagent {} failed: {}",
            task.id,
            task.error.as_deref().unwrap_or("unknown error")
        ),
        TaskStatus::Cancelled => format!("Codex subagent {} cancelled.", task.id),
        TaskStatus::Queued | TaskStatus::Running => {
            format!("Codex subagent {} is still running.", task.id)
        }
    }
}

pub fn task_model_context_message(task: &TaskSnapshot) -> String {
    let summary = task
        .summary
        .clone()
        .unwrap_or_else(|| task_status_summary(task));
    let terminal_tab = task
        .terminal_tab
        .map(|tab| format!("\n<terminal_tab>{}</terminal_tab>", tab + 1))
        .unwrap_or_default();
    let result = task
        .result
        .as_deref()
        .filter(|result| !result.trim().is_empty())
        .map(|result| format!("\n<result>{}</result>", xml_escape(result.trim())))
        .unwrap_or_default();
    let error = task
        .error
        .as_deref()
        .map(|error| format!("\n<error>{}</error>", xml_escape(error)))
        .unwrap_or_default();

    format!(
        "<subagent-outcome>\n\
<task_id>{}</task_id>\n\
<status>{}</status>\n\
<summary>{}</summary>\n\
<description>{}</description>\n\
<backend>{}</backend>\n\
<cwd>{}</cwd>{terminal_tab}{result}{error}\n\
</subagent-outcome>",
        xml_escape(&task.id),
        task.status.label(),
        xml_escape(&summary),
        xml_escape(&task.description),
        task.backend.label(),
        xml_escape(&task.cwd)
    )
}

fn subagent_outcome_status(outcome: &SubagentOutcome) -> TaskStatus {
    if outcome.error.is_none() {
        TaskStatus::Completed
    } else if outcome.cancelled {
        TaskStatus::Cancelled
    } else {
        TaskStatus::Failed
    }
}

fn task_status_summary(task: &TaskSnapshot) -> String {
    match task.status {
        TaskStatus::Completed => format!("Codex subagent \"{}\" completed", task.description),
        TaskStatus::Failed => format!(
            "Codex subagent \"{}\" failed: {}",
            task.description,
            task.error.as_deref().unwrap_or("unknown error")
        ),
        TaskStatus::Cancelled => format!("Codex subagent \"{}\" was stopped", task.description),
        TaskStatus::Queued | TaskStatus::Running => {
            format!("Codex subagent \"{}\" is still running", task.description)
        }
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str) -> SubagentRequest {
        SubagentRequest {
            task_id: id.to_owned(),
            description: "inspect parser".to_owned(),
            prompt: "check the parser".to_owned(),
            agent: None,
            backend: SubagentBackend::Codex,
            cwd: "/tmp".to_owned(),
        }
    }

    #[test]
    fn task_manager_tracks_subagent_lifecycle() {
        let mut manager = TaskManager::default();
        let request = request("a1");
        manager.start_subagent(&request, 0).unwrap();
        assert_eq!(manager.running_subagent_count(), 1);

        let finished = manager
            .finish_subagent("a1", SubagentOutcome::completed("all done"))
            .unwrap();

        assert_eq!(finished.status, TaskStatus::Completed);
        assert_eq!(
            finished.summary.as_deref(),
            Some("Codex subagent \"inspect parser\" completed")
        );
        assert_eq!(finished.result.as_deref(), Some("all done"));
        assert_eq!(manager.running_subagent_count(), 0);
    }

    #[test]
    fn task_manager_enforces_running_subagent_limit() {
        let mut manager = TaskManager::default();
        manager.start_subagent(&request("a1"), 0).unwrap();
        manager.start_subagent(&request("a2"), 1).unwrap();

        assert!(manager.start_subagent(&request("a3"), 2).is_err());
    }

    #[test]
    fn failed_subagent_records_error_without_external_artifacts() {
        let mut manager = TaskManager::default();
        let request = request("a1");
        manager.start_subagent(&request, 0).unwrap();

        let finished = manager
            .finish_subagent("a1", SubagentOutcome::failed("model failed", "partial"))
            .unwrap();

        assert_eq!(finished.status, TaskStatus::Failed);
        assert_eq!(finished.result.as_deref(), Some("partial"));
        assert_eq!(finished.error.as_deref(), Some("model failed"));
    }

    #[test]
    fn task_model_context_message_is_model_visible_payload() {
        let mut manager = TaskManager::default();
        let request = request("a1");
        manager.start_subagent(&request, 0).unwrap();
        let finished = manager
            .finish_subagent("a1", SubagentOutcome::completed("final <answer>"))
            .unwrap();

        let message = task_model_context_message(&finished);

        assert!(message.contains("<subagent-outcome>"));
        assert!(message.contains("<status>completed</status>"));
        assert!(!message.contains("external artifact"));
        assert!(message.contains("<result>final &lt;answer&gt;</result>"));
    }

    #[test]
    fn task_model_context_message_does_not_embed_transcript_log() {
        let mut manager = TaskManager::default();
        let request = request("a1");
        manager.start_subagent(&request, 0).unwrap();
        let finished = manager
            .finish_subagent("a1", SubagentOutcome::completed("concise result"))
            .unwrap();

        let message = task_model_context_message(&finished);

        assert!(message.contains("<result>concise result</result>"));
        assert!(!message.contains("verbose tool log"));
    }

    #[test]
    fn task_result_preserves_full_final_message() {
        let final_message = "this final answer should remain intact because it is the model-visible handoff from the completed subagent and trimming it would drop useful details";
        let mut manager = TaskManager::default();
        let request = request("a1");
        manager.start_subagent(&request, 0).unwrap();
        let finished = manager
            .finish_subagent("a1", SubagentOutcome::completed(final_message))
            .unwrap();

        assert_eq!(finished.result.as_deref(), Some(final_message));
        assert!(task_model_context_message(&finished).contains(final_message));
    }

    #[test]
    fn steering_closes_atomically_after_the_final_drain() {
        let steering = SubagentSteering::default();
        steering.send("check the edge case".to_owned()).unwrap();

        assert_eq!(
            steering.finish_or_drain(),
            Some(vec!["check the edge case".to_owned()])
        );
        assert_eq!(steering.finish_or_drain(), None);
        assert!(steering.send("too late".to_owned()).is_err());
    }

    #[test]
    fn cancelled_subagent_has_cancelled_status() {
        let mut manager = TaskManager::default();
        manager.start_subagent(&request("a1"), 0).unwrap();

        let finished = manager
            .finish_subagent("a1", SubagentOutcome::cancelled("partial"))
            .unwrap();

        assert_eq!(finished.status, TaskStatus::Cancelled);
        assert_eq!(finished.result.as_deref(), Some("partial"));
    }
}
