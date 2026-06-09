# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Snapshot

Glint is a small Rust terminal UI for chatting with an OpenAI-compatible LLM endpoint. It is built with Ratatui + Crossterm and currently runs as a synchronous, single-session TUI with a background thread for each submitted prompt.

The package name is `glint` and the Rust edition is `2024`.

## Commands

```bash
cargo run                         # Run the TUI locally; requires config.toml and the configured API key env var
cargo build                       # Build debug binary
cargo fmt                         # Format Rust code
cargo check                       # Fast compile check
cargo clippy -- -D warnings       # Lint with warnings treated as errors
cargo test                        # Run all tests
cargo test test_name              # Run tests matching a name/pattern
cargo test -- --nocapture         # Run tests and show stdout
```

There are currently no committed tests, so `cargo test` should report zero tests unless new test modules are added.

## Runtime Configuration

`Config::load` reads `config.toml` from the current working directory at startup. The config shape is:

```toml
[llm]
base_url = "https://example.com/v1"
model = "model-name"
api_key_env = "LLM_API_KEY"
temperature = 0.7
max_tokens = 8196
```

The API key itself must come from the environment variable named by `api_key_env`; do not hardcode secrets in source or docs. The current HTTP client targets the OpenAI-compatible `POST {base_url}/chat/completions` endpoint and expects a response with `choices[0].message.content`.

## Architecture

The main flow is:

```text
terminal key/mouse event -> KeyAction/MouseAction -> AppEvent -> App::update -> ui::render
background LLM thread -> AgentEvent -> AppEvent::Agent -> App::update -> ui::render
```

Key pieces:

- `src/main.rs` owns config loading, terminal setup/teardown, raw mode, alternate screen, mouse capture, keyboard enhancement flags, the render loop, Crossterm polling, and draining background agent events from the channel.
- `src/config.rs` reads `config.toml`, trims the LLM base URL, and resolves the configured API key environment variable at startup.
- `src/app.rs` is the central state machine. `App` stores messages, input state, scroll position, quit flag, agent status, runtime config, current directory label, and the agent event channel. Route state changes through `App::update` rather than mutating UI state from rendering code.
- `src/event.rs` converts Crossterm key and mouse events into app-level `KeyAction` and `MouseAction` values. Keep terminal-specific input mapping here so `App` stays independent of Crossterm details.
- `src/input.rs` owns the editable multiline input buffer and cursor movement behavior.
- `src/message.rs` defines the chat message model and roles.
- `src/agent/` contains the agent-facing event types and LLM integration seam.
  - `src/agent/mod.rs` exposes `AgentStatus`, `AgentEvent`, and `spawn_agent_loop`.
  - `src/agent/openai.rs` runs a background thread, sends lifecycle events, and performs the OpenAI-compatible chat completion request with `ureq`.
- `src/ui/` renders the current `App` state with Ratatui. Rendering should stay state-driven and side-effect free.
  - `src/ui/mod.rs` lays out the Glint welcome panel, transcript, input box, status/help lines, cursor position, and scroll behavior.
  - `src/ui/markdown.rs` renders assistant markdown into Ratatui lines with styling for headings, code, lists, links, quotes, tables, task markers, and math text.
  - `src/ui/star.rs` generates the Glint star mark used by the idle panel.

## Current Capabilities and Limits

- Supports prompt submission with `Enter`, multiline input with `Shift+Enter`, quitting with `Ctrl+C`, keyboard scrolling with arrow/page keys, and mouse wheel scrolling.
- While the agent is not idle, text input is intentionally disabled and up/down controls scroll the transcript.
- Assistant responses are displayed after the blocking HTTP request returns; the current OpenAI-compatible path does not stream deltas from the network yet.
- Conversation history is displayed in the TUI but only the latest user prompt is sent to the LLM request.
- There is no async runtime, persistence, MCP integration, tool execution, cancellation, retry policy, or multi-session state yet.

## Code Style

Keep this codebase intentionally simple and easy to change:

- Prefer concise, direct code over broad abstractions.
- Do not add defensive handling for states that cannot occur in the current design.
- Keep rendering, state updates, input mapping, config loading, and agent events separated.
- Add dependencies only when they clearly reduce complexity.
- Use idiomatic Rust: `cargo fmt`, `cargo clippy -- -D warnings`, `Result` with `?`, and no `unwrap()` in production paths unless the state is genuinely unreachable.
- Treat API keys and config values as system-boundary input: validate early, add useful error context, and avoid leaking secrets in UI-facing messages.

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

The current agent event model is the intended seam for future work. Add new states/events there for features such as cancellation, real streaming deltas, conversation context, tool request/approval, tool start/finish, retries, and richer failures. Keep the same separation: terminal events and agent events update `App`; `ui::render` only displays `App`.
