# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Commands

```bash
cargo run                         # Run the TUI locally
cargo build                       # Build debug binary
cargo fmt                         # Format Rust code
cargo check                       # Fast compile check
cargo clippy -- -D warnings       # Lint with warnings treated as errors
cargo test                        # Run all tests
cargo test test_name              # Run tests matching a name/pattern
cargo test -- --nocapture         # Run tests and show stdout
```

There are currently no committed tests, so `cargo test` should report zero tests unless new test modules are added.

## Architecture

This is a small Rust terminal UI built with Ratatui + Crossterm. The current implementation is intentionally minimal: it simulates an agent loop with a fake streaming response and does not yet include real LLM calls, tools, async runtime, persistence, or MCP integration.

The main flow is:

```text
terminal key event -> KeyAction -> AppEvent -> App::update -> ui::render
background fake agent thread -> AgentEvent -> AppEvent::Agent -> App::update
```

Key pieces:

- `src/main.rs` owns terminal setup/teardown, raw mode, alternate screen, the render loop, Crossterm polling, and draining background agent events from the channel.
- `src/app.rs` is the central state machine. `App` stores messages, input state, scroll position, quit flag, agent status, and the agent event channel. Route state changes through `App::update` rather than mutating UI state from rendering code.
- `src/event.rs` converts Crossterm `KeyEvent`s into app-level `KeyAction`s. Keep terminal-specific input mapping here so `App` stays independent of Crossterm details.
- `src/ui.rs` renders the current `App` state with Ratatui. Rendering should stay state-driven and side-effect free.
- `src/message.rs` defines the chat message model and roles.
- `src/input.rs` holds the simple input buffer behavior.
- `src/agent/` contains the agent-facing event types and the current fake streaming loop. Future real model/tool integration should extend `AgentEvent` rather than coupling provider logic directly into the UI loop.

## Code Style

Keep this codebase intentionally simple and easy to change:

- Prefer concise, direct code over broad abstractions.
- Do not add defensive handling for states that cannot occur in the current design.
- Keep rendering, state updates, input mapping, and agent events separated.
- Add dependencies only when they clearly reduce complexity.
- Use idiomatic Rust: `cargo fmt`, `cargo clippy -- -D warnings`, `Result` with `?`, and no `unwrap()` in production paths unless the state is genuinely unreachable.

## Git Commit Convention

Use this commit message shape:

```text
<type>(<topic>): <abstract>

- describe the first meaningful change
- describe the second meaningful change
```

Examples:

```text
feat(init): bootstrap agent tui
fix(input): handle backspace on empty buffer
refactor(agent): split streaming loop events
```

Common types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `ci`.

Keep the first line short and specific. Use bullet points in the body when a commit includes multiple meaningful changes.

## Extension Notes

The current agent event model is the intended seam for future work. Add new states/events there for features such as cancellation, real model streaming, tool request/approval, tool start/finish, and failures. Keep the same separation: terminal events and agent events update `App`; `ui::render` only displays `App`.
