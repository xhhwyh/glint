# Inline Execution Cards Design

## Summary

Glint will remove the visible terminal mode and its bottom pane. Bash command output and Codex subagent activity will instead appear as expandable execution rows inside the main conversation transcript.

An execution row is borderless. It shows a compact summary by default, changes color smoothly while hovered, and expands when its summary is clicked. Expanded content is capped at eight rendered rows and has its own mouse-wheel scrolling. Bash and subagent cards share one presentation model while retaining their existing runtime sources of truth.

The change also wraps each Ratatui draw in Crossterm synchronized-update markers. This prevents terminal emulators from displaying or retaining partially applied scroll frames, including the reported repeated `(fetch)` suffix artifact.

## Goals

- Remove `/terminal`, `TerminalRun`, the PTY, terminal focus, bottom terminal tabs, and terminal-specific shortcuts.
- Keep non-interactive shell work available through the existing Bash tool.
- Render Bash output as a compact, expandable row in the conversation.
- Render live and completed subagent transcripts as compact, expandable rows in the conversation.
- Preserve `TaskList`, `TaskWait`, `TaskSend`, and `TaskCancel` behavior without routing them through terminal-named types.
- Make expanded output readable without allowing one tool result to take over the full screen.
- Preserve mouse text selection and copying inside expanded output.
- Preserve full tool output when Glint has persisted a large result to the session tool-results directory.
- Persist completed subagent presentation data so a resumed conversation can reconstruct its inline card.
- Prevent persistent partial-frame artifacts while scrolling.

## Non-Goals

- Supporting interactive shell programs such as `vim`, `less`, `ssh`, password prompts, or nested TUIs.
- Turning every Glint tool into the same runtime abstraction. Read, Grep, Glob, LSP, Edit, and MCP tools keep their current execution paths.
- Changing task scheduling, the two-subagent concurrency limit, steering, cancellation, or outcome semantics.
- Putting subagent presentation snapshots into model-visible conversation history.
- Adding general model-request retries or multi-session tabs.

## Confirmed Visual Direction

Execution rows have no persistent border and no right-side disclosure arrow. The whole summary row is clickable.

The resting state uses the existing transcript background. Hovering transitions the row to a slightly brighter and more saturated blue-cyan treatment and animates the existing `◇` mark. The transition lasts approximately 160 milliseconds and uses the existing 40-millisecond redraw loop. Terminals with reduced animation capability still receive the final hover color without relying on intermediate frames.

The summary contains:

- Tool or task name.
- Command, task description, or current activity, truncated to the available width.
- Status and output line count.
- A textual `click to expand` or `click to collapse` hint rather than a disclosure icon.

## Runtime Architecture

### Remove Terminal Mode

The following product surface is removed:

- The `/terminal` slash command.
- The `TerminalRun` tool specification, implementation, approval wording, model context, and prompt instructions.
- PTY creation, input forwarding, resizing, parsing, scrollback, cancellation, and terminal status.
- The bottom terminal pane and tab switcher.
- Terminal focus, terminal hitboxes, terminal mouse routing, and terminal keyboard shortcuts.
- User notices that describe Bash or TerminalRun mode transitions.

The main loop renders the document across the full frame. It no longer computes terminal height, terminal geometry, terminal tab hitboxes, or terminal resize operations.

### Replace TerminalRequest With TaskRequest

`TerminalRequest` currently mixes two responsibilities:

- Running commands in a visible PTY.
- Starting and controlling Codex subagents.

The command-running variants are deleted. The remaining subagent and task-control variants become a terminal-independent `TaskRequest` channel owned by the runtime/task layer:

- Start a subagent.
- List tasks.
- Wait for tasks.
- Send a task message.
- Cancel a task.

The channel is renamed consistently in `SessionRuntime`, `ToolRegistry`, task-control tools, and the Subagent tool.

`SubagentStartResponse`, `TaskSnapshot`, `RunningSubagent`, and `SubagentRuntimeEvent` stop carrying `terminal_tab`. A stable `task_id` identifies a run everywhere.

### Shell Tool Surface

The main agent and subagents use Bash for non-interactive shell-only commands. The runtime context and tool registry expose one shell tool instead of switching between Bash and TerminalRun.

Subagents retain their current restrictions:

- No Edit tool.
- No nested Subagent tool.
- Task-control tools remain unavailable inside a subagent.

`SubagentOutcome` and `TaskManager` remain the authoritative completion and lifecycle state. UI presentation never becomes a protocol boundary for task completion.

## Presentation Data

### Bash

Existing `Message::Tool` data remains the source for Bash cards:

- Tool call ID provides the stable card identity.
- Tool name and input summary provide the collapsed label.
- Tool description provides secondary text.
- Tool content and completion state provide output and status.

No duplicate Bash execution store is introduced.

### SubagentTranscript

Subagent presentation moves out of `TerminalTab::Subagent` into a task-ID keyed `SubagentTranscript` collection. Each transcript retains the same message-level information currently shown in the bottom subagent tab:

- User steering and initial prompt messages.
- Streaming assistant deltas.
- Tool start summaries.
- Tool output and completion state.
- Current activity.
- Completed, failed, or cancelled status.

`SubagentRuntimeEvent` includes `task_id`, allowing `App` to update the corresponding transcript directly.

During a live run, the transcript is held in application state. When the task reaches a terminal state, Glint appends one UI-only subagent presentation snapshot to the session transcript. Resume loading reconstructs the card from that snapshot. The snapshot is excluded from `model_history()` and cannot affect prompt context.

### ExecutionCardView

Bash messages and subagent transcripts project into a read-only `ExecutionCardView` used only by layout and rendering:

```text
Message::Tool ───────┐
                     ├─> ExecutionCardView ─> transcript lines and hitboxes
SubagentTranscript ──┘
```

The view supplies:

- Stable execution ID.
- Kind and display name.
- Summary and status.
- Total rendered line count.
- Full-output source.
- Whether output is still streaming.

The projection does not merge Bash and subagent runtime logic.

## Full Output Handling

Tool results within the normal budget expand directly from the stored message content.

Results above the existing tool-result budget are already written beneath the session's `tool-results` directory. Their model-visible result contains a preview plus a persisted-output marker. The inline card recognizes that structured marker and keeps a persisted file reference as its full-output source.

The large file is not read while the card is collapsed. On expansion, Glint reads the file for display. A missing or unreadable file produces a short inline error inside that card and does not fail the application or alter model history.

Subagent tool results follow the same output-source rules when their transcript contains a persisted tool result.

## Interaction State

`App` owns presentation-only state keyed by execution ID:

- Expanded execution IDs.
- Per-output scroll offsets measured from the bottom.
- The currently hovered execution ID.
- Hover transition origin, target, and start time.

These values are not written into the session transcript. Resumed conversations start with cards collapsed and internal scroll at the bottom.

### Mouse Hitboxes

The UI computes execution hitboxes from the same rendered document projection used to draw the transcript. Each visible execution card can expose two regions:

- Summary region for hover and expand/collapse clicks.
- Expanded-output region for text selection and internal wheel scrolling.

Hitboxes are derived from the current width, document scroll position, card wrapping, and eight-row cap. `App` does not hard-code row geometry.

### Hover

`MouseEventKind::Moved` becomes a real `MouseAction::Move` instead of being discarded. Crossterm's existing mouse-capture setup already enables all-motion reporting.

Entering a summary region starts a transition toward the hover palette. Moving to a different card transitions the old and new rows appropriately. Moving outside all execution rows transitions back to the resting palette.

The renderer interpolates existing theme colors across approximately four redraws. The effect is limited to background, foreground emphasis, and the `◇` mark; it introduces no border or disclosure arrow.

### Expand And Collapse

Clicking a summary toggles that card. Clicking or dragging within expanded output keeps the existing text-selection behavior and does not collapse the card.

Expanded content uses `min(rendered_output_rows, 8)` rows. The clicked summary stays on the same screen row when practical. If the conversation is already at the bottom, expansion keeps the document pinned to the bottom instead.

### Nested Scrolling

When the pointer is over expanded output, mouse-wheel events adjust that card's internal offset and do not scroll the main transcript. Moving the pointer outside the output restores normal conversation scrolling.

Subagent output follows the newest content while its internal offset is zero. Scrolling upward pauses following. Scrolling back to zero restores automatic following.

Window resize recomputes wrapping, height, hitboxes, and maximum internal scroll, then clamps every saved offset.

## Status And Failure Behavior

- A running Bash card shows its existing tooling state. Bash does not add new incremental shell streaming in this change.
- A completed Bash card shows success or failure and remains expandable.
- Failed Bash output retains stdout and stderr supplied by the tool result.
- Running subagent cards update activity and transcript content from live `AgentEvent` values.
- Completed, failed, cancelled, and timed-out subagent cards retain all presentation content received before termination.
- A missing persisted-output file shows a local display error only.
- A malformed UI-only subagent snapshot is skipped with session-loading context rather than corrupting model history.

## Synchronized Terminal Drawing

Every `terminal.draw(...)` call is wrapped in Crossterm synchronized-update boundaries. The begin marker is emitted before Ratatui flushes the frame and the end marker is emitted afterward, including when drawing returns an error.

This makes a Ratatui diff appear atomically on terminal emulators that support synchronized updates. Unsupported terminals ignore the markers and keep the current behavior.

This fix is still required after removing the bottom terminal because the reported artifact occurs while the main transcript scrolls over two similar long Bash output lines.

## Primary Code Areas

- `src/main.rs`: full-frame layout, synchronized drawing, removal of terminal updates.
- `src/event.rs`: remove terminal key actions and add mouse movement.
- `src/app.rs`: remove terminal state and routing; add execution-card state, hitboxes, and subagent transcript updates.
- `src/message.rs`: retain Bash tool identity and add only the presentation metadata needed for subagent reconstruction.
- `src/runtime/mod.rs`: replace terminal request plumbing with task requests and task-ID runtime events.
- `src/tasks.rs`: terminal-independent task request/response and task snapshots.
- `src/query/mod.rs`: use Bash-only shell mode and preserve raw/full-output references for UI presentation.
- `src/tools/mod.rs`: remove TerminalRun registration and rename task request plumbing.
- `src/tools/terminal_run/`: remove.
- `src/terminal.rs`: remove after task-control types and subagent presentation state move to their owning modules.
- `src/commands/mod.rs`: remove `/terminal` registration and matching.
- `src/approval.rs`: remove TerminalRun-specific approval handling while preserving Bash approval behavior.
- `src/ui/mod.rs`: execution-card document projection, hitboxes, scroll anchoring, and hover animation progress.
- `src/ui/transcript_view.rs`: borderless compact and expanded execution rendering.
- `src/ui/status.rs`: remove terminal labels and task-to-tab presentation.
- `src/ui/terminal.rs`: remove.
- `src/transcript.rs`: UI-only completed subagent snapshots and resume reconstruction.
- `src/context/mod.rs`, `prompts/system.md`, and tool descriptions: remove TerminalRun and visible-terminal wording.

## Testing Strategy

### Unit Tests

- Map Crossterm move events to the new mouse action.
- Remove terminal-only shortcuts and slash-command registration.
- Compute stable summary and output hitboxes under document scrolling.
- Toggle only from summary clicks.
- Preserve output text selection without toggling.
- Enforce the eight-row expansion cap.
- Route wheel events to internal output only while hovered over its body.
- Clamp internal scroll after resize and content changes.
- Pause and resume subagent automatic following.
- Interpolate hover colors at start, intermediate, completion, and leave states.
- Render no execution border and no disclosure arrow.
- Load persisted full output lazily and handle a missing file.
- Update subagent presentation from assistant and tool events by task ID.
- Persist and restore completed subagent presentation without adding it to model history.
- Emit synchronized-update begin and end markers around a draw, including the error path.

### Regression And Removal Tests

- Reproduce the visible `git remote -v` Bash output with `(fetch)` and `(push)` rows in a scrolling transcript fixture.
- Verify Bash remains registered for main agents and subagents.
- Verify TerminalRun, `/terminal`, terminal focus, terminal tabs, PTY creation, and terminal-mode notices have no operational source references.
- Verify TaskList, TaskWait, TaskSend, TaskCancel, and Subagent still function through task requests.

### Repository Verification

Run:

```bash
rtk cargo fmt --check
rtk cargo check
rtk cargo test
rtk cargo clippy -- -D warnings
rtk git diff --check
```

Also run a targeted source sweep for stale terminal-mode identifiers and user-facing wording.

## Acceptance Criteria

- The application has no visible or hidden terminal mode and no TerminalRun tool.
- The main document always owns the full available frame.
- Bash and subagent activity appear as borderless execution rows in chronological conversation order.
- Hovering produces a smooth color treatment without a border or right-side arrow.
- Clicking the summary toggles expansion; output selection remains usable.
- Expanded content never exceeds eight rows and scrolls independently under the mouse.
- Completed large Bash output and completed subagent output remain available after resume.
- Task control and subagent outcome semantics remain unchanged.
- Scrolling the reported `(fetch)` and `(push)` fixture does not leave persistent partial-frame artifacts on terminals supporting synchronized updates.
- The full test, check, format, and lint suite passes without modifying the user's unrelated `config.yaml` change.
