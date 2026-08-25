# Inline Execution Cards Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove Glint's terminal mode and replace Bash and subagent terminal output with borderless, expandable execution cards inside the conversation transcript.

**Architecture:** Task control moves to a terminal-independent channel keyed by `task_id`; Bash remains the only shell tool. Live subagent events update a serializable `SubagentTranscript` associated with the originating Subagent tool call, while Bash continues to use `Message::Tool`. Both sources project into UI-only execution cards with application-owned expansion, scrolling, lazy full-output loading, and hover state.

**Tech Stack:** Rust 2024, Ratatui 0.29, Crossterm 0.28, Serde/serde_json.

**Spec:** `docs/superpowers/specs/2026-08-25-inline-execution-cards-design.md`

## Global Constraints

- Execution rows are borderless and have no right-side disclosure arrow.
- The entire summary row toggles expansion.
- Expanded output is capped at exactly eight rendered rows and scrolls independently.
- Hover transitions last 160 milliseconds using the existing 40-millisecond redraw cadence.
- Text selection and OSC52 copying remain available inside expanded output.
- Bash is the only shell tool for main agents and subagents; interactive shell programs remain unsupported.
- `TaskList`, `TaskWait`, `TaskSend`, `TaskCancel`, `SubagentOutcome`, and the two-subagent concurrency limit retain their semantics.
- Completed subagent presentation snapshots are UI-only and never enter `model_history()`.
- Persisted large output is loaded only after expansion; unreadable files show a card-local error.
- Preserve the user's unrelated `config.yaml` modification.

---

## File Structure

- Create `src/subagent_transcript.rs` for live subagent presentation state and its serializable completed snapshot.
- Create `src/execution.rs` for execution identity, expansion/scroll/hover state, hitboxes, and persisted-output source resolution.
- Create `src/ui/execution.rs` for card projection, borderless rendering, color interpolation, output wrapping, and hitbox metadata.
- Keep `src/message.rs` as the Bash tool-call source of truth.
- Keep `src/tasks.rs` as the lifecycle and task-request source of truth.
- Keep `src/app.rs` as the only owner that mutates interaction state and applies runtime events.
- Keep `src/ui/mod.rs` side-effect free; it derives document lines and hitboxes from `App`.
- Delete `src/terminal.rs`, `src/ui/terminal.rs`, and `src/tools/terminal_run/` after their task and presentation responsibilities have moved.

---

### Task 1: Terminal-Independent Task Request Channel

**Files:**
- Modify: `src/tasks.rs`
- Modify: `src/runtime/mod.rs`
- Modify: `src/query/mod.rs`
- Modify: `src/tools/mod.rs`
- Modify: `src/tools/subagent/mod.rs`
- Modify: `src/tools/task_control/mod.rs`
- Modify: `src/terminal.rs`
- Test: `src/tasks.rs`, `src/runtime/mod.rs`, `src/tools/mod.rs`

**Interfaces:**
- Consumes: existing `SubagentRequest`, `TaskSnapshot`, `TaskWaitResponse`, and response-channel behavior.
- Produces: `tasks::TaskRequest`, `SessionRuntime::task_request_sender()`, and `SessionRuntime::try_recv_task_request()`.
- Preserves temporarily: command-only `terminal::TerminalRequest::{Run, CancelActive}` until TerminalRun is removed in Task 6.

- [ ] **Step 1: Write failing channel-separation tests**

Add tests that send task operations without constructing a terminal tab:

```rust
#[test]
fn task_request_channel_carries_list_requests() {
    let runtime = runtime();
    let sender = runtime.task_request_sender();
    let (response, receiver) = std::sync::mpsc::channel();

    sender.send(TaskRequest::List { response }).unwrap();

    assert!(matches!(runtime.try_recv_task_request(), Some(TaskRequest::List { .. })));
    drop(receiver);
}
```

Update registry tests to assert Subagent and task-control tools accept a `Sender<TaskRequest>` independently of the terminal command channel.

- [ ] **Step 2: Run the focused tests and verify they fail**

```bash
rtk cargo test task_request_channel -- --nocapture
rtk cargo test task_control -- --nocapture
```

Expected: compilation fails because `TaskRequest` and the task-channel accessors do not exist.

- [ ] **Step 3: Add `TaskRequest` and route task tools through it**

Define the task-only request enum in `src/tasks.rs`:

```rust
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
```

Add `task_request_tx` and `task_requests` to `SessionRuntime`. Rename the tool-facing fields and parameters from `terminal_requests` to `task_requests` in Subagent and task-control code. `AgentRunInput` temporarily carries both channels so TerminalRun continues compiling until Task 6.

Expose these registry constructors so later tasks use one stable interface:

```rust
pub fn with_task_requests(task_requests: Option<Sender<TaskRequest>>) -> Self;
pub fn for_subagent(task_requests: Option<Sender<TaskRequest>>) -> Self;
```

- [ ] **Step 4: Run focused and compile checks**

```bash
rtk cargo test task_request_channel -- --nocapture
rtk cargo test task_control -- --nocapture
rtk cargo check
rtk git diff --check
```

Expected: task tools pass through `TaskRequest`; TerminalRun behavior is unchanged.

- [ ] **Step 5: Commit the channel split**

```bash
rtk git add src/tasks.rs src/runtime/mod.rs src/query/mod.rs src/tools/mod.rs src/tools/subagent/mod.rs src/tools/task_control/mod.rs src/terminal.rs
rtk git commit -m "refactor(tasks): decouple task requests from terminal" -m "- route subagent lifecycle controls through TaskRequest
- leave the terminal channel command-only pending removal"
```

---

### Task 2: Task-ID Subagent Transcript And UI-Only Persistence

**Files:**
- Create: `src/subagent_transcript.rs`
- Modify: `src/main.rs`
- Modify: `src/message.rs`
- Modify: `src/tasks.rs`
- Modify: `src/runtime/mod.rs`
- Modify: `src/app.rs`
- Modify: `src/transcript.rs`
- Test: `src/subagent_transcript.rs`, `src/runtime/mod.rs`, `src/transcript.rs`, `src/app.rs`

**Interfaces:**
- Consumes: `AgentEvent`, `TaskStatus`, `Message`, `SubagentRequest`, and the originating Subagent `ToolCall.id`.
- Produces: `SubagentTranscript`, `SubagentTranscriptSnapshot`, `LoadedTranscript::subagent_transcripts`, and task-ID runtime events.
- Produces association: `SubagentRequest::tool_call_id: String` links a transcript to its chronological `Message::Tool` anchor.

- [ ] **Step 1: Write failing transcript lifecycle and resume tests**

Add tests covering streaming assistant text, tool completion, terminal status, serialization, and model-history exclusion:

```rust
#[test]
fn transcript_applies_agent_events_by_task_id() {
    let request = request("a1", "call-subagent");
    let mut transcript = SubagentTranscript::new(&request);

    transcript.apply(&AgentEvent::Started);
    transcript.apply(&AgentEvent::AssistantDelta("checking".to_owned()));
    transcript.apply(&AgentEvent::ToolStarted {
        id: "tool-1".to_owned(),
        name: "Grep".to_owned(),
        input_summary: "TerminalRun".to_owned(),
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
    assert_eq!(transcript.tool_use_count(), 1);
    assert!(transcript.messages().iter().any(|message| message.content.contains("checking")));
}

#[test]
fn completed_subagent_snapshot_restores_ui_without_model_history() {
    let mut store = store();
    let snapshot = completed_snapshot("a1", "call-subagent");
    store.append_subagent_presentation(&snapshot).unwrap();

    assert_eq!(store.ui_subagent_transcripts(), vec![snapshot]);
    assert!(store.model_history().is_empty());
}
```

- [ ] **Step 2: Run focused tests and verify they fail**

```bash
rtk cargo test subagent_transcript -- --nocapture
rtk cargo test completed_subagent_snapshot -- --nocapture
```

Expected: compilation fails because the new module, fields, and persistence methods do not exist.

- [ ] **Step 3: Implement live transcript state and snapshot persistence**

Derive Serde support for `Role`, `Message`, and `TaskStatus`; add `tool_is_error: bool` with `#[serde(default)]`, and define:

```rust
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
```

Implement `new`, `from_snapshot`, `snapshot`, accessors, `apply(&AgentEvent)`, `append_steering`, and `finish(&TaskSnapshot)`. Taking events by reference allows the migration step to update both the inline transcript and the old tab until Task 6. Match the existing `update_subagent_tab_event` behavior for assistant and tool messages, including Read output elision and failure text.

Add `tool_call_id` to `SubagentRequest`, populated from `call.id` in `tools/subagent`. Change `SubagentRuntimeEvent::{Agent, Finished}` to carry `task_id`; retain `terminal_tab` only until Task 6 so the old pane can be updated during migration.

Store completed snapshots as an unconstrained JSON payload so malformed UI-only data cannot fail the session parser:

```rust
EventMsg::SubagentPresentation {
    task_id: String,
    snapshot: serde_json::Value,
}
```

Implement `TranscriptStore::append_subagent_presentation` with `serde_json::to_value`, and `ui_subagent_transcripts()` with `serde_json::from_value(...).ok()`. Ignore presentation entries before the most recent `ClearBoundary`.

Add `subagent_transcripts: Vec<SubagentTranscriptSnapshot>` to `LoadedTranscript`. On new session and resume, rebuild `App.subagent_transcripts: BTreeMap<String, SubagentTranscript>` keyed by `task_id`. During live execution, update this collection and continue updating the old terminal tab until Task 6.

- [ ] **Step 4: Run focused and repository checks**

```bash
rtk cargo test subagent_transcript -- --nocapture
rtk cargo test completed_subagent_snapshot -- --nocapture
rtk cargo test subagent -- --nocapture
rtk cargo check
rtk git diff --check
```

Expected: live transcripts update by task ID, completed snapshots resume, and model history remains unchanged.

- [ ] **Step 5: Commit transcript state and persistence**

```bash
rtk git add src/main.rs src/message.rs src/tasks.rs src/runtime/mod.rs src/app.rs src/transcript.rs src/subagent_transcript.rs src/tools/subagent/mod.rs
rtk git commit -m "feat(tasks): persist inline subagent transcripts" -m "- track live subagent presentation by task id
- restore completed snapshots outside model history"
```

---

### Task 3: Execution Interaction State And Lazy Full Output

**Files:**
- Create: `src/execution.rs`
- Modify: `src/main.rs`
- Modify: `src/app.rs`
- Modify: `src/message.rs`
- Modify: `src/transcript.rs`
- Test: `src/execution.rs`, `src/app.rs`, `src/transcript.rs`

**Interfaces:**
- Consumes: Bash `Message::Tool`, `SubagentTranscript`, and `<persisted-output>` markers produced by `ToolResultBudget`.
- Produces: `ExecutionId`, `ExecutionHitbox`, `ExecutionRegion`, `ExecutionOutputSource`, and presentation-only state in `App`.
- Produces App methods: `toggle_execution`, `scroll_execution`, `set_execution_hitboxes`, `execution_output`, and `set_hovered_execution`.

- [ ] **Step 1: Write failing identity, persisted-output, and state tests**

```rust
#[test]
fn persisted_output_path_uses_the_structured_marker() {
    let output = "preview\n\n<persisted-output>\nFull Bash output was 60000 characters, exceeding the 50000 character tool-result budget. The full output was written to:\n/tmp/tool-results/call-Bash.txt\nUse a narrower tool call if you need more focused output.\n</persisted-output>";

    assert_eq!(
        ExecutionOutputSource::from_tool_output(output),
        ExecutionOutputSource::Persisted(PathBuf::from(
            "/tmp/tool-results/call-Bash.txt"
        ))
    );
}

#[test]
fn expanding_persisted_output_reads_once_and_keeps_failure_local() {
    let mut app = App::test_empty();
    let id = ExecutionId::Tool("call-1".to_owned());
    app.messages.push(finished_bash_message(
        "call-1",
        "preview\n\n<persisted-output>\nFull Bash output was 60000 characters, exceeding the 50000 character tool-result budget. The full output was written to:\n/missing/output.txt\nUse a narrower tool call if you need more focused output.\n</persisted-output>",
    ));

    app.toggle_execution(id.clone(), 6);

    assert!(app.is_execution_expanded(&id));
    assert!(app.execution_output(&id).unwrap().contains("Could not read full output"));
}
```

Also add a transcript test proving `tool_is_error` survives `FunctionCallOutput { is_error: true }` reconstruction.

- [ ] **Step 2: Run focused tests and verify they fail**

```bash
rtk cargo test persisted_output_path -- --nocapture
rtk cargo test expanding_persisted_output -- --nocapture
rtk cargo test tool_error_state -- --nocapture
```

Expected: compilation fails because execution types and `Message::tool_is_error` do not exist.

- [ ] **Step 3: Implement execution identity, output sources, and App state**

Define focused presentation types:

```rust
pub const MAX_EXPANDED_OUTPUT_ROWS: u16 = 8;
pub const HOVER_TRANSITION: Duration = Duration::from_millis(160);

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum ExecutionId {
    Tool(String),
    Task(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionRegion {
    Summary,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionHitbox {
    pub id: ExecutionId,
    pub region: ExecutionRegion,
    pub start_row: u16,
    pub end_row: u16,
    pub start_column: u16,
    pub end_column: u16,
    pub expansion_rows: u16,
    pub max_output_scroll: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionOutputSource {
    Inline(String),
    Persisted(PathBuf),
}
```

Parse only a path that follows `The full output was written to:` inside a complete `<persisted-output>` block. Do not treat arbitrary output lines as paths.

Add these fields to `App`:

```rust
expanded_executions: HashSet<ExecutionId>,
execution_scrolls: HashMap<ExecutionId, u16>,
execution_outputs: HashMap<ExecutionId, String>,
execution_hitboxes: Vec<ExecutionHitbox>,
hovered_execution: Option<ExecutionId>,
previous_hovered_execution: Option<ExecutionId>,
hover_changed_at: Option<Instant>,
```

`toggle_execution(id, expansion_rows)` lazily resolves persisted content when expanding. For Bash, replace its structured marker with the referenced file contents or a short read error. For Subagent, build the combined transcript output and resolve every persisted tool result in place so one missing nested file produces an error at that tool entry without hiding the rest of the transcript. Cache the resulting display string by execution ID.

If `app.scroll == 0`, keep it at zero; otherwise add `expansion_rows` on expand and subtract it on collapse so the visible summary row remains stable. `set_execution_hitboxes` replaces the frame-derived hitboxes and clamps saved internal offsets to each card's `max_output_scroll`.

- [ ] **Step 4: Run focused and compile checks**

```bash
rtk cargo test persisted_output_path -- --nocapture
rtk cargo test expanding_persisted_output -- --nocapture
rtk cargo test tool_error_state -- --nocapture
rtk cargo check
rtk git diff --check
```

Expected: inline output is reused, persisted output is lazy, and file failures remain inside the card state.

- [ ] **Step 5: Commit execution state**

```bash
rtk git add src/main.rs src/execution.rs src/app.rs src/message.rs src/transcript.rs
rtk git commit -m "feat(ui): add execution card state" -m "- track expansion and internal output scrolling by execution id
- load persisted tool output lazily for display"
```

---

### Task 4: Borderless Execution Card Projection And Rendering

**Files:**
- Create: `src/ui/execution.rs`
- Modify: `src/ui/mod.rs`
- Modify: `src/ui/transcript_view.rs`
- Modify: `src/ui/theme.rs`
- Modify: `src/main.rs`
- Test: `src/ui/mod.rs`, `src/ui/transcript_view.rs`, `src/ui/execution.rs`

**Interfaces:**
- Consumes: `ExecutionId`, `SubagentTranscript`, Bash `Message::Tool`, and App expansion/output state.
- Produces: read-only `ExecutionCardView`, `ExecutionCardLines`, and `ui::execution_hitboxes(app, width, height)`.
- Preserves: existing rendering for Read, Glob, Grep, LSP, Edit, MCP, Task control, and TodoWrite.

- [ ] **Step 1: Write failing card rendering and layout tests**

```rust
#[test]
fn expanded_execution_output_is_capped_at_eight_rendered_rows() {
    let card = bash_card_with_output((1..=20).map(|line| format!("line {line}")).collect::<Vec<_>>().join("\n"));
    let lines = execution_card_lines(&card, 80, true, 0, 0.0);

    assert_eq!(lines.output_rows, 8);
    assert_eq!(lines.max_output_scroll, 12);
}

#[test]
fn execution_summary_has_no_border_or_disclosure_arrow() {
    let card = bash_card_with_output("origin repository (fetch)".to_owned());
    let rendered = execution_card_lines(&card, 100, false, 0, 0.0)
        .lines
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<String>();

    assert!(!rendered.contains('│'));
    assert!(!rendered.contains('╭'));
    assert!(!rendered.contains('╰'));
    assert!(!rendered.contains('›'));
    assert!(!rendered.contains('▼'));
    assert!(rendered.contains('◇'));
}
```

Add a document-layout test with Bash output containing adjacent `(fetch)` and `(push)` lines. Assert one summary hitbox and, when expanded, one output hitbox whose visible height is at most eight.

- [ ] **Step 2: Run focused tests and verify they fail**

```bash
rtk cargo test execution_summary -- --nocapture
rtk cargo test expanded_execution_output -- --nocapture
rtk cargo test execution_hitbox -- --nocapture
```

Expected: compilation fails because the execution UI module and card projection do not exist.

- [ ] **Step 3: Implement card projection, rendering, and document hitboxes**

Define the read-only view:

```rust
pub(super) struct ExecutionCardView<'a> {
    pub id: ExecutionId,
    pub name: &'a str,
    pub summary: &'a str,
    pub description: Option<&'a str>,
    pub status: String,
    pub output: &'a str,
    pub finished: bool,
    pub is_error: bool,
    pub streaming: bool,
}
```

Project only Bash and Subagent messages:

- Bash uses `ExecutionId::Tool(call_id)` and the existing message fields.
- Subagent finds the transcript whose `tool_call_id` matches the `Message::Tool` call ID, then uses `ExecutionId::Task(task_id)`.
- A Subagent tool message without a linked transcript falls back to the existing generic tool renderer.

Render a compact summary with the existing `◇`, name, truncated command or description, status, rendered line count, and textual `click to expand`/`click to collapse` hint. Do not draw box characters or a disclosure glyph. Expanded output uses `wrap_text`, applies the card's offset from the bottom, and selects at most `MAX_EXPANDED_OUTPUT_ROWS` rows.

Extend `DocumentLineMeta` with:

```rust
execution: Option<(ExecutionId, ExecutionRegion)>,
```

Have the same document projection used by `render_document` produce hitboxes after applying document scroll and viewport clipping. In `main.rs`, call `ui::execution_hitboxes` before drawing and pass the result to `App::set_execution_hitboxes`.

- [ ] **Step 4: Run focused and compile checks**

```bash
rtk cargo test execution_summary -- --nocapture
rtk cargo test expanded_execution_output -- --nocapture
rtk cargo test execution_hitbox -- --nocapture
rtk cargo check
rtk git diff --check
```

Expected: Bash and linked Subagent calls render as borderless cards; other tools retain their existing presentation.

- [ ] **Step 5: Commit card rendering**

```bash
rtk git add src/ui/execution.rs src/ui/mod.rs src/ui/transcript_view.rs src/ui/theme.rs src/main.rs
rtk git commit -m "feat(ui): render inline execution cards" -m "- project Bash and subagent activity into borderless rows
- cap expanded output and derive card hitboxes from document layout"
```

---

### Task 5: Mouse Toggle, Nested Scrolling, Selection, And Hover Animation

**Files:**
- Modify: `src/event.rs`
- Modify: `src/execution.rs`
- Modify: `src/app.rs`
- Modify: `src/ui/execution.rs`
- Modify: `src/ui/mod.rs`
- Test: `src/event.rs`, `src/app.rs`, `src/ui/execution.rs`, `src/ui/mod.rs`

**Interfaces:**
- Consumes: frame-derived `ExecutionHitbox` values and existing document text selection.
- Produces: `MouseAction::Move { column, row }`, `App::execution_hover_progress`, summary click routing, and output wheel routing.

- [ ] **Step 1: Write failing mouse and animation tests**

```rust
#[test]
fn mouse_move_keeps_pointer_coordinates() {
    let action = MouseAction::from(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 17,
        row: 9,
        modifiers: KeyModifiers::empty(),
    });
    assert_eq!(action, MouseAction::Move { column: 17, row: 9 });
}

#[test]
fn summary_click_toggles_but_output_click_starts_selection() {
    let mut app = app_with_execution_hitboxes();
    app.update(AppEvent::Mouse(MouseAction::LeftDown { column: 5, row: 4 }));
    assert!(app.is_execution_expanded(&ExecutionId::Tool("call-1".to_owned())));

    app.update(AppEvent::Mouse(MouseAction::LeftDown { column: 5, row: 7 }));
    assert!(app.text_selection.is_some());
    assert!(app.is_execution_expanded(&ExecutionId::Tool("call-1".to_owned())));
}

#[test]
fn wheel_over_output_scrolls_only_the_card() {
    let mut app = expanded_app_with_output_hitbox();
    app.scroll = 5;
    app.update(AppEvent::Mouse(MouseAction::ScrollUp { column: 8, row: 7 }));
    assert_eq!(app.execution_scroll(&ExecutionId::Tool("call-1".to_owned())), 3);
    assert_eq!(app.scroll, 5);
}
```

Add deterministic hover tests using `hover_progress_at(id, now)` for 0, 80, and 160 milliseconds, including fade-out after leaving all cards.

- [ ] **Step 2: Run focused tests and verify they fail**

```bash
rtk cargo test mouse_move_keeps -- --nocapture
rtk cargo test summary_click_toggles -- --nocapture
rtk cargo test wheel_over_output -- --nocapture
rtk cargo test hover_progress -- --nocapture
```

Expected: mouse movement is discarded and execution-specific routing does not exist.

- [ ] **Step 3: Implement mouse routing and the 160-millisecond palette transition**

Map `MouseEventKind::Moved` to `MouseAction::Move`. In `App::update_mouse`, order execution handling before generic document selection and scrolling:

1. Move updates the hovered summary ID or clears it.
2. LeftDown on a summary toggles and clears selection drag state.
3. LeftDown/Drag/Up inside output uses the existing document-coordinate selection path.
4. Wheel inside expanded output changes its offset by three rows and returns.
5. Wheel elsewhere changes `App.scroll` as before.

Expose deterministic progress:

```rust
pub fn execution_hover_progress_at(&self, id: &ExecutionId, now: Instant) -> f32 {
    let elapsed = self
        .hover_changed_at
        .map(|started| now.saturating_duration_since(started))
        .unwrap_or(HOVER_TRANSITION);
    let progress = (elapsed.as_secs_f32() / HOVER_TRANSITION.as_secs_f32()).clamp(0.0, 1.0);
    if self.hovered_execution.as_ref() == Some(id) {
        progress
    } else if self.previous_hovered_execution.as_ref() == Some(id) {
        1.0 - progress
    } else {
        0.0
    }
}
```

Interpolate the existing resting foreground/background toward the approved brighter blue-cyan palette. Apply the same interpolation to `◇`; do not add borders, arrows, blinking, or movement that changes row width.

For running subagents, offset zero follows new output. A nonzero offset remains fixed as content grows; scrolling back to zero resumes follow. `set_execution_hitboxes` clamps offsets after resize and wrapping changes.

- [ ] **Step 4: Run focused and compile checks**

```bash
rtk cargo test mouse_move_keeps -- --nocapture
rtk cargo test summary_click_toggles -- --nocapture
rtk cargo test wheel_over_output -- --nocapture
rtk cargo test hover_progress -- --nocapture
rtk cargo test selected_text -- --nocapture
rtk cargo check
rtk git diff --check
```

Expected: click, selection, nested wheel routing, resize clamping, auto-follow, and hover transition tests pass.

- [ ] **Step 5: Commit interaction behavior**

```bash
rtk git add src/event.rs src/execution.rs src/app.rs src/ui/execution.rs src/ui/mod.rs
rtk git commit -m "feat(ui): add execution card interactions" -m "- toggle and scroll inline output with pointer hitboxes
- animate hover color while preserving text selection"
```

---

### Task 6: Bash-Only Shell Surface And Task-ID Runtime

**Files:**
- Modify: `src/tasks.rs`
- Modify: `src/runtime/mod.rs`
- Modify: `src/query/mod.rs`
- Modify: `src/tools/mod.rs`
- Modify: `src/tools/subagent/mod.rs`
- Modify: `src/tools/subagent/description.rs`
- Modify: `src/tools/task_control/mod.rs`
- Modify: `src/context/mod.rs`
- Modify: `src/approval.rs`
- Modify: `src/app.rs`
- Modify: `src/transcript.rs`
- Modify: `prompts/system.md`
- Delete: `src/tools/terminal_run/mod.rs`
- Delete: `src/tools/terminal_run/description.rs`
- Test: `src/tools/mod.rs`, `src/query/mod.rs`, `src/context/mod.rs`, `src/tasks.rs`, `src/app.rs`

**Interfaces:**
- Consumes: `TaskRequest`, `SubagentTranscript`, Bash, and task lifecycle state.
- Produces: a single Bash shell surface for both main and subagent registries.
- Removes: `ShellToolMode`, TerminalRun registration/execution, `terminal_tab` fields, and subagent bottom-tab updates.

- [ ] **Step 1: Write failing Bash-only and task-ID tests**

```rust
#[test]
fn main_and_subagent_registries_expose_bash_without_terminal_run() {
    let main = ToolRegistry::with_task_requests(None);
    let subagent = ToolRegistry::for_subagent(None);

    for registry in [main, subagent] {
        let names = registry.specs().into_iter().map(|spec| spec.name).collect::<Vec<_>>();
        assert!(names.contains(&"Bash".to_owned()));
        assert!(!names.contains(&"TerminalRun".to_owned()));
    }
}

#[test]
fn task_snapshot_has_no_terminal_identity() {
    let mut manager = TaskManager::default();
    let task = manager.start_subagent(&request("a1", "call-subagent")).unwrap();

    assert_eq!(task.id, "a1");
    assert_eq!(task.tool_call_id, "call-subagent");
}
```

Update context tests to assert that main context lists Bash, Subagent, task tools, Edit, and TodoWrite; subagent context lists Read, Glob, Grep, LSP, and Bash only. Add a source-level query test ensuring TerminalRun is not a tool spec.

- [ ] **Step 2: Run focused tests and verify they fail**

```bash
rtk cargo test main_and_subagent_registries -- --nocapture
rtk cargo test task_snapshot_has_no_terminal -- --nocapture
rtk cargo test runtime_context_describes -- --nocapture
```

Expected: TerminalRun is still registered and task structures still carry terminal-tab data.

- [ ] **Step 3: Remove TerminalRun and complete task-ID routing**

Delete `ShellToolMode` and all `shell_tool_mode` fields from `StartPromptConfig`, `AgentRunInput`, `RuntimeContext` constructors, `ToolRegistry`, and call sites. Make Bash unconditional in both registry tool lists.

Remove the command-only `TerminalRequest` channel from `SessionRuntime`, `AgentRunInput`, `query`, and tools. Delete the TerminalRun module and approval explanation branches. Retain Bash project-prefix approval behavior.

Change task structures to terminal-independent identities:

```rust
pub struct SubagentStartResponse {
    pub task_id: String,
    pub error: Option<String>,
}

pub struct TaskSnapshot {
    pub id: String,
    pub tool_call_id: String,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub description: String,
    pub backend: SubagentBackend,
    pub cwd: String,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub summary: Option<String>,
    pub activity: Option<String>,
    pub tool_use_count: u32,
    pub result: Option<String>,
    pub error: Option<String>,
}
```

Remove `terminal_tab` from `RunningSubagent` and `SubagentRuntimeEvent`. Start subagents with `ShellToolMode` absent, `TaskRequest` present, and Bash available. Stop creating or updating `TerminalTab::Subagent`; update only `SubagentTranscript` and persist its completed snapshot.

Change the Subagent tool success text to:

```text
Started Codex subagent a1. Use TaskWait for its result, TaskSend to refine it, or TaskCancel to stop it.
```

Update `prompts/system.md`, runtime context, Subagent description, transcript input summaries, query approval text, and tests to remove TerminalRun and visible-terminal wording.

- [ ] **Step 4: Run focused and repository checks**

```bash
rtk cargo test main_and_subagent_registries -- --nocapture
rtk cargo test task_snapshot_has_no_terminal -- --nocapture
rtk cargo test runtime_context_describes -- --nocapture
rtk cargo test subagent -- --nocapture
rtk cargo test task_control -- --nocapture
rtk cargo check
rtk rg -n "TerminalRun|ShellToolMode|terminal_tab" src prompts/system.md
rtk git diff --check
```

Expected: tests pass; the source sweep returns only old transcript compatibility fixtures if one is intentionally retained, and no operational references.

- [ ] **Step 5: Commit the Bash-only runtime**

```bash
rtk git add src/tasks.rs src/runtime/mod.rs src/query/mod.rs src/tools src/context/mod.rs src/approval.rs src/app.rs src/transcript.rs prompts/system.md
rtk git commit -m "refactor(runtime): remove TerminalRun shell mode" -m "- expose Bash to main agents and subagents
- route subagent presentation and lifecycle entirely by task id"
```

---

### Task 7: Remove The PTY, Bottom Pane, Terminal Focus, And Slash Command

**Files:**
- Modify: `src/main.rs`
- Modify: `src/event.rs`
- Modify: `src/app.rs`
- Modify: `src/commands/mod.rs`
- Modify: `src/ui/mod.rs`
- Modify: `src/ui/status.rs`
- Modify: `src/ui/status_bar.rs`
- Delete: `src/terminal.rs`
- Delete: `src/ui/terminal.rs`
- Test: `src/main.rs`, `src/event.rs`, `src/app.rs`, `src/commands/mod.rs`, `src/ui/mod.rs`, `src/ui/status.rs`

**Interfaces:**
- Consumes: the full-frame conversation renderer and inline subagent transcript from earlier tasks.
- Produces: one full-frame document/composer layout with no terminal state or routing.
- Removes: `/terminal`, PTY lifecycle, terminal tabs, focus, geometry, keyboard input bytes, mouse routing, notices, and status labels.

- [ ] **Step 1: Write failing removal and full-frame tests**

```rust
#[test]
fn slash_commands_do_not_include_terminal() {
    assert!(!SLASH_COMMANDS.iter().any(|command| command.name == "/terminal"));
}

#[test]
fn former_terminal_shortcuts_are_regular_chat_input() {
    let ctrl_t = KeyInput::from(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    let alt_n = KeyInput::from(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));

    assert_eq!(ctrl_t.action, KeyAction::Char('t'));
    assert_eq!(alt_n.action, KeyAction::Char('n'));
}

#[test]
fn document_viewport_and_composer_use_the_full_height() {
    let app = App::test_empty();
    let composer = composer(&app, 100);
    let composer_height = composer_visible_height(&composer, 30);
    let document_height = document_viewport_height(&app, 100, 30);

    assert_eq!(document_height + composer_height, 30);
}
```

Add status tests asserting the general tab has no Terminal or Shell tool rows and task rows have no terminal-tab label.

- [ ] **Step 2: Run focused tests and verify they fail**

```bash
rtk cargo test slash_commands_do_not_include_terminal -- --nocapture
rtk cargo test former_terminal_shortcuts -- --nocapture
rtk cargo test document_viewport_and_composer_use_the_full_height -- --nocapture
```

Expected: `/terminal`, shortcut actions, and split-pane layout still exist.

- [ ] **Step 3: Delete the terminal product surface**

In `main.rs`, remove `mod terminal`, terminal-height calculations, resize/update calls, terminal hitbox updates, and the second `update_terminal()` call. Compute document viewport, composer hitboxes, return-bottom hitbox, and execution hitboxes using `size.height` directly.

In `event.rs`, delete `ToggleTerminalFocus`, `NewTerminalTab`, `CloseTerminalTab`, `SelectTerminalTab`, `terminal_input`, and terminal escape-sequence helpers. Preserve Ctrl+C quit/cancel semantics, Ctrl+X cut, normal navigation, and `MouseAction::Move`.

In `app.rs`, delete:

- Terminal fields and terminal structs.
- `update_terminal`, terminal request routing, PTY tick/resize/input methods.
- Terminal focus checks and terminal-specific mouse branches.
- Tab creation, selection, switching, closing, and status-to-terminal navigation.
- `/terminal` execution and terminal-mode notices.
- `shell_tool_mode()`.

Keep the generic document/input selection paths and execution hitbox branches added in Task 5.

In `ui/mod.rs`, render `render_document(frame, app, frame.area())` after overlay checks and remove the terminal module/export. Delete `src/terminal.rs` and `src/ui/terminal.rs`. Remove terminal rows from status while leaving the Tasks tab and active-task composer panel functional.

- [ ] **Step 4: Run removal checks**

```bash
rtk cargo test slash_commands_do_not_include_terminal -- --nocapture
rtk cargo test former_terminal_shortcuts -- --nocapture
rtk cargo test document_viewport_and_composer_use_the_full_height -- --nocapture
rtk cargo test mouse_ -- --nocapture
rtk cargo check
rtk rg -n "TerminalTab|TerminalStatus|TerminalMouseScroll|terminal_visible|terminal_focused|terminal_top_row|/terminal" src prompts/system.md
rtk git diff --check
```

Expected: all tests pass and the source sweep has no operational terminal-mode references.

- [ ] **Step 5: Commit terminal removal**

```bash
rtk git add src/main.rs src/event.rs src/app.rs src/commands/mod.rs src/ui src/terminal.rs
rtk git commit -m "refactor(ui): remove terminal mode" -m "- delete the PTY pane, tabs, focus, and shortcuts
- give the conversation document the full frame"
```

---

### Task 8: Regression Fixtures, Full Verification, And Visual Acceptance

**Files:**
- Modify: `src/ui/mod.rs`
- Modify: `src/ui/execution.rs`
- Modify: `src/app.rs`
- Modify: `src/transcript.rs`
- Modify: tests in any touched module only when required by the final source sweep

**Interfaces:**
- Consumes: all behavior delivered by Tasks 1-7 and the synchronized-draw plan.
- Produces: regression proof for `(fetch)`/`(push)`, resume, removal boundaries, and the approved interaction design.

- [ ] **Step 1: Add the scrolling regression fixture and end-to-end state test**

Create a Bash tool message fixture using the reported command and output:

```rust
let message = finished_bash_message(
    "call-git-remote",
    "origin\thttps://github.com/xhhwyh/glint.git (fetch)\norigin\thttps://github.com/xhhwyh/glint.git (push)\n186bba1 2026-08-12 11:44:19 +0800 merge: integrate subagent task control",
);
```

Render it repeatedly with a `TestBackend` while changing main-document scroll, expand/collapse state, and internal output scroll. Assert each completed buffer contains one `(fetch)` row and one `(push)` row, and that collapsed buffers contain no stale expanded-output rows.

Add an App/transcript test that completes a subagent with assistant and tool messages, reloads the session, finds its card at the originating Subagent tool call, expands it, and confirms `model_history()` contains only the hidden `<subagent-outcome>` protocol message rather than the presentation snapshot.

- [ ] **Step 2: Run the new regression tests**

```bash
rtk cargo test git_remote_output -- --nocapture
rtk cargo test resumed_subagent_card -- --nocapture
```

Expected: both regression tests pass.

- [ ] **Step 3: Run the complete repository verification suite**

```bash
rtk cargo fmt --check
rtk cargo check
rtk cargo test
rtk cargo clippy -- -D warnings
rtk git diff --check
```

Expected: every command exits successfully with no warnings.

- [ ] **Step 4: Run exact removal and scope sweeps**

```bash
rtk rg -n "TerminalRun|ShellToolMode|TerminalTab|TerminalStatus|TerminalMouseScroll|terminal_visible|terminal_focused|terminal_tab|/terminal" src prompts/system.md
rtk git status --short --branch
rtk git diff -- config.yaml
```

Expected: the first command returns no operational source matches; status shows only intended feature changes plus the pre-existing `config.yaml` modification; the config diff remains the user's six-line `qwen3.8-max` entry.

- [ ] **Step 5: Perform manual TUI acceptance**

Run:

```bash
rtk cargo run
```

In one conversation, verify these exact interactions:

1. Run the reported `git remote -v && git log -1 --format='%h %ci %s'` command through Bash.
2. Scroll the conversation rapidly upward and downward; confirm no repeated `(fetch)` or `(push)` remnants persist.
3. Hover the Bash summary and confirm the color transition completes without a border or right arrow.
4. Click the summary, confirm at most eight output rows appear, and scroll them independently.
5. Select and copy text inside the expanded output without collapsing it.
6. Start a Subagent, expand its inline card while running, scroll upward to pause following, then return to offset zero and confirm following resumes.
7. Resume the session and confirm completed Bash and Subagent cards are collapsed and expandable.

- [ ] **Step 6: Commit final regression coverage**

```bash
rtk git add src/app.rs src/transcript.rs src/ui/mod.rs src/ui/execution.rs
rtk git commit -m "test(ui): cover inline execution regressions" -m "- reproduce the git remote scrolling artifact in rendered buffers
- verify resumed Bash and subagent card behavior"
```
