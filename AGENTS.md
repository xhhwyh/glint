# AGENTS.md

Guidance for Codex and other coding agents in this repository.

## Snapshot

Glint is a Rust 2024 TUI for chatting with an OpenAI-compatible LLM endpoint. It uses Ratatui + Crossterm, runs synchronously as a single-session app, and spawns a background thread per submitted prompt. Package: `glint`.

## Commands

```bash
cargo run                         # Run TUI; requires config.toml and API key env var
cargo build                       # Build debug binary
cargo fmt                         # Format Rust code
cargo check                       # Fast compile check
cargo clippy -- -D warnings       # Lint with warnings as errors
cargo test                        # Run tests
cargo test test_name              # Run tests matching a pattern
cargo test -- --nocapture         # Show test stdout
```

There are no committed tests yet; `cargo test` should report zero tests unless test modules are added.

## Worktrees

Keep local worktrees under ignored `.worktree/`:

```bash
git worktree add -b design/slash-command .worktree/slash-command main
```

After merging back to `main`, clean up:

```bash
git worktree remove .worktree/slash-command
git branch -d design/slash-command
```

Use `git branch -d` so Git verifies the branch has merged. If merge happened via remote PR, update local `main` first.

## Runtime Config

`Config::load` reads `config.toml` and `prompts/system.md` from the current working directory. The selected `llm.provider` must match an entry under `llm.providers`:

```toml
[llm]
provider = "default"
temperature = 0.7
max_tokens = 8196
context_window = 65536 # optional

[llm.providers.default]
base_url = "https://example.com/v1"
model = "model-name"
api_key_env = "LLM_API_KEY"
```

Provider entries own `base_url`, `model`, and `api_key_env`; global LLM settings own `temperature`, `max_tokens`, and optional `context_window`. The API key comes from the env var named by `api_key_env`; never hardcode or leak secrets. The HTTP client trims trailing slashes from `base_url`, posts to `{base_url}/chat/completions`, and expects `choices[0].message.content`.

## Architecture

```text
terminal event -> KeyAction/MouseAction -> AppEvent -> App::update -> ui::render
agent thread -> AgentEvent -> AppEvent::Agent -> App::update -> ui::render
```

- `src/main.rs`: config load, terminal lifecycle, render loop, Crossterm polling, agent event draining.
- `src/config.rs`: TOML load, base URL trim, API key env resolution.
- `src/app.rs`: central state machine; route state changes through `App::update`.
- `src/event.rs`: map Crossterm input to `KeyAction`/`MouseAction`.
- `src/input.rs`: editable multiline buffer and cursor behavior.
- `src/message.rs`: chat message model and roles.
- `src/agent/`: `AgentStatus`, `AgentEvent`, `spawn_agent_loop`, OpenAI-compatible HTTP integration.
- `src/ui/`: state-driven, side-effect-free rendering; includes layout, markdown, and idle star mark.

## Behavior And Limits

- `Enter` submits, `Shift+Enter` inserts newline, `Ctrl+C` quits, arrows/page keys/mouse wheel scroll.
- While the agent is not idle, text input is disabled and up/down scroll the transcript.
- Assistant responses appear after the blocking HTTP request returns; network deltas do not stream yet.
- Conversation history is displayed, but only the latest user prompt is sent to the LLM.
- No async runtime, persistence, MCP/tools, cancellation, retries, or multi-session state yet.

## Style

- Prefer concise, direct code; add abstractions or dependencies only when they reduce real complexity.
- Keep rendering, state updates, input mapping, config loading, and agent events separated.
- Do not add defensive handling for states that cannot occur in the current design.
- Use idiomatic Rust: `?`, `cargo fmt`, `cargo clippy -- -D warnings`, and no production `unwrap()` unless unreachable.
- Validate system-boundary input early with useful context, especially config and secrets.

## Commits

```text
<type>(<topic>): <abstract>

- describe the first meaningful change
- describe the second meaningful change
```

Common types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `ci`. Keep the first line short and specific.

## Extension Notes

Use the agent event model for cancellation, streaming deltas, conversation context, tool requests, retries, and richer failures. Terminal and agent events update `App`; `ui::render` only displays `App`.
