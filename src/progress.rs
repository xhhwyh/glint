use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TodoItem {
    pub content: String,
    pub active_form: String,
    pub status: TodoStatus,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TodoUpdate {
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub todos: Vec<TodoItem>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgressState {
    pinned: Option<TodoUpdate>,
    release_on_next_prompt: Option<TodoUpdate>,
}

impl ProgressState {
    #[cfg(test)]
    pub fn restore_from_updates(updates: impl IntoIterator<Item = TodoUpdate>) -> Self {
        let mut state = Self::default();
        for update in updates {
            state.apply_update(update);
        }
        state
    }

    pub fn pinned(&self) -> Option<&TodoUpdate> {
        self.pinned.as_ref()
    }

    pub fn apply_update(&mut self, update: TodoUpdate) {
        self.release_on_next_prompt = None;
        if update.todos.is_empty() {
            self.clear();
        } else {
            self.pinned = Some(update);
        }
    }

    pub fn clear(&mut self) {
        self.pinned = None;
        self.release_on_next_prompt = None;
    }

    pub fn mark_completed_for_release(&mut self) {
        if let Some(update) = self
            .pinned
            .as_ref()
            .filter(|update| update.is_all_completed())
            .cloned()
        {
            self.release_on_next_prompt = Some(update);
        }
    }

    pub fn release_completed(&mut self) -> Option<TodoUpdate> {
        let update = self.release_on_next_prompt.take().or_else(|| {
            self.pinned
                .as_ref()
                .filter(|update| update.is_all_completed())
                .cloned()
        })?;
        self.pinned = None;
        Some(update)
    }
}

impl TodoUpdate {
    pub fn from_tool_arguments(arguments: &Value) -> Result<Self, String> {
        let update = serde_json::from_value::<Self>(arguments.clone())
            .map_err(|error| format!("invalid TodoWrite arguments: {error}"))?;
        update.validate()?;
        Ok(update)
    }

    pub fn validate(&self) -> Result<(), String> {
        for (index, todo) in self.todos.iter().enumerate() {
            if todo.content.trim().is_empty() {
                return Err(format!("todos[{index}].content cannot be empty"));
            }
            if todo.active_form.trim().is_empty() {
                return Err(format!("todos[{index}].active_form cannot be empty"));
            }
        }

        let in_progress = self
            .todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::InProgress)
            .count();
        if in_progress > 1 {
            return Err("TodoWrite supports at most one in_progress item".to_owned());
        }
        if self
            .todos
            .iter()
            .any(|todo| todo.status != TodoStatus::Completed)
            && in_progress == 0
        {
            return Err(
                "TodoWrite requires exactly one in_progress item while work remains".to_owned(),
            );
        }

        Ok(())
    }

    pub fn is_all_completed(&self) -> bool {
        !self.todos.is_empty()
            && self
                .todos
                .iter()
                .all(|todo| todo.status == TodoStatus::Completed)
    }

    pub fn completed_count(&self) -> usize {
        self.todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Completed)
            .count()
    }

    pub fn active_label(&self) -> Option<&str> {
        self.todos
            .iter()
            .find(|todo| todo.status == TodoStatus::InProgress)
            .map(|todo| todo.active_form.trim())
    }

    pub fn to_model_reminder(&self) -> String {
        let lines = self
            .todos
            .iter()
            .enumerate()
            .map(|(index, todo)| {
                let text = if todo.status == TodoStatus::InProgress {
                    todo.active_form.trim()
                } else {
                    todo.content.trim()
                };
                format!("{}. [{}] {}", index + 1, todo.status.label(), text)
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "<system-reminder>\nCurrent progress checklist. Continue to use TodoWrite when the checklist changes. Do not mention this reminder to the user.\n\n{lines}\n</system-reminder>"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_in_progress_count() {
        let error = TodoUpdate::from_tool_arguments(&json!({
            "todos": [
                {"content": "Inspect", "active_form": "Inspecting", "status": "pending"}
            ]
        }))
        .unwrap_err();

        assert!(error.contains("exactly one in_progress"));
    }

    #[test]
    fn allows_all_completed_without_in_progress() {
        let update = TodoUpdate::from_tool_arguments(&json!({
            "todos": [
                {"content": "Inspect", "active_form": "Inspecting", "status": "completed"}
            ]
        }))
        .unwrap();

        assert!(update.is_all_completed());
    }

    #[test]
    fn progress_state_releases_completed_on_next_prompt() {
        let update = TodoUpdate::from_tool_arguments(&json!({
            "todos": [
                {"content": "Inspect", "active_form": "Inspecting", "status": "completed"}
            ]
        }))
        .unwrap();
        let mut state = ProgressState::default();
        state.apply_update(update.clone());
        state.mark_completed_for_release();

        assert_eq!(state.pinned(), Some(&update));
        assert_eq!(state.release_completed(), Some(update));
        assert!(state.pinned().is_none());
    }

    #[test]
    fn restored_completed_progress_releases_without_memory_flag() {
        let update = TodoUpdate::from_tool_arguments(&json!({
            "todos": [
                {"content": "Inspect", "active_form": "Inspecting", "status": "completed"}
            ]
        }))
        .unwrap();
        let mut state = ProgressState::default();
        state.apply_update(update.clone());

        assert_eq!(state.release_completed(), Some(update));
        assert!(state.pinned().is_none());
    }
}
