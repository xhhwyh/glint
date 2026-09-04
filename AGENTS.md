# AGENTS.md

Guidance for Codex and other coding agents in this repository.

## Snapshot

Glint is a Rust 2024 TUI for chatting with an OpenAI-compatible LLM endpoint. It uses Ratatui + Crossterm, runs synchronously as a single-session app, and spawns a background thread per submitted prompt. Package: `glint`.

## Commands

```bash
cargo run                         # Run TUI; opens interactive model setup when unconfigured
cargo build                       # Build debug binary
cargo fmt                         # Format Rust code
cargo check                       # Fast compile check
cargo clippy -- -D warnings       # Lint with warnings as errors
cargo test                        # Run tests
cargo test test_name              # Run tests matching a pattern
cargo test -- --nocapture         # Show test stdout
```

The repository has unit tests for agent orchestration, tools, transcript loading, config, and UI formatting. Run `cargo test` before committing behavior changes.

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

`GlintPaths::discover` derives one fixed state root from `HOME`: `~/.glint`. It never reads configuration from the workspace, source tree, or an override environment variable. The user-editable document is always `~/.glint/config.yaml`; the default system prompt and built-in provider catalog are embedded in the executable. There is no initialization subcommand or configuration-path option.

The model setup UI owns built-in provider selection and credentials. Built-in provider metadata remains embedded; enabling one with its API key makes all of its shipped models available. An existing protected `~/.glint/auth.json` is authoritative; otherwise credentials prefer the OS keyring. A fresh installation falls back to that protected file when the keyring is unavailable. If a configured provider's keyring becomes unavailable, startup enters setup/repair; the first explicit key save creates and switches to the file backend. Never add API keys to YAML or environment-based LLM settings.

The persisted model schema is intentionally small:

```yaml
version: 1
llm:
  provider: deepseek
  model: deepseek-v4-flash
  temperature: 0.7
  max_tokens: 8196
configured_providers:
  - deepseek
custom_providers:
  Team Gateway:
    base_url: https://llm.example.com/v1
    models: [code-large, code-fast]
lsp:
  servers:
    rust:
      command: rust-analyzer
      args: []
      extension_to_language:
        .rs: rust
      startup_timeout_ms: 20000
      max_restarts: 3
```

`configured_providers` contains only built-in IDs; `custom_providers` contains a display name, compatible base URL, and ordered non-empty model names. `llm` is omitted when no model is configured. Empty optional `configured_providers`, `custom_providers`, `mcp`, `plugins`, and `lsp` sections are omitted. Custom names are case-insensitively unique and cannot collide with built-in identities.

The optional `lsp.servers` block configures stdio language servers by file extension. If `lsp.servers` is omitted, Glint registers the Rust default shown above. If present, its configured servers replace that default. The `mcp` and `plugins` blocks use the schemas in `EXTENSIONS.md`.

Core state lives below `~/.glint`: configuration, optional `auth.json`, default plugin cache and state, MCP OAuth state, and sessions. `plugins.cache_dir` can instead place plugin cache and install state at an explicit path, including an absolute one. The startup working directory remains the workspace. It is the root for coding tools and LSP, the default and relative cwd for MCP processes, MCP's advertised root, and hook process cwd. Plugins resolve relative sources from `~/.glint`, while hooks retain `GLINT_PLUGIN_ROOT` and `CLAUDE_PLUGIN_ROOT` for plugin-owned resources.

## Architecture

```text
terminal event -> KeyAction/MouseAction -> AppEvent -> App::update -> ui::render
agent thread -> AgentEvent -> AppEvent::Agent -> App::update -> ui::render
```

- `src/main.rs`: config load, terminal lifecycle, render loop, Crossterm polling, agent event draining.
- `src/config.rs`: user YAML schema, runtime LLM/LSP configuration, and extension-section parsing.
- `src/app.rs`: central state machine; route state changes through `App::update`.
- `src/commands/`: slash-command registry and matching.
- `src/context/`: runtime context and initial model-message construction.
- `src/event.rs`: map Crossterm input to `KeyAction`/`MouseAction`.
- `src/input.rs`: editable multiline buffer and cursor behavior.
- `src/message.rs`: chat message model and roles.
- `src/agent/`: agent event/status types, compaction entry points, model provider types, and OpenAI-compatible HTTP integration.
- `src/query/`: model-turn orchestration, tool-call batching, approval flow, and `spawn_agent_loop`.
- `src/services/`: cross-cutting agent services such as tool-result budgeting.
- `src/services/mcp/`: persistent MCP client runtime, transports, OAuth, elicitation, dynamic tools, resources, and prompts.
- `src/plugins/`: local/Git plugin discovery, manifests, commands, skills, agents, hooks, settings, MCP, and LSP contributions.
- `src/tools/`: Glint tool registry, `utils.rs` helpers, and per-tool directories with local descriptions.
- `src/transcript.rs`: session persistence, resume summaries, model history, and UI-message reconstruction.
- `src/ui/`: state-driven, side-effect-free rendering; includes layout, markdown, and idle star mark.

## Behavior And Limits

- `Enter` submits, `Shift+Enter` inserts newline, `Ctrl+C` quits, arrows/page keys/mouse wheel scroll.
- While the agent is not idle, text input is disabled and up/down scroll the transcript.
- Assistant responses stream into the transcript as provider deltas arrive.
- Conversation history is persisted and model requests include prior model history plus the current user request.
- Tool execution supports Read, Glob, Grep, LSP, Bash, Edit, and TodoWrite with approval, cancellation, read-only batching, Bash command execution with inline execution-card output, progress checklist updates, and large-result budgeting.
- Plugins contribute namespaced commands, skills, specialized subagents, hooks, MCP servers, LSP servers, and settings.
- MCP supports stdio and Streamable HTTP, bearer and OAuth authentication, tools, resources, prompts, notifications, cancellation, timeouts, approvals, and elicitation on an isolated Tokio runtime; the TUI remains synchronous.
- No general model-request retries or multi-session tabs yet.

## Style

- Prefer concise, direct code; add abstractions or dependencies only when they reduce real complexity.
- Do not make unrequested changes, refactors, or design additions; keep edits scoped to the user's requested outcome.
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

Use the agent event model for cancellation, streaming deltas, conversation context, tool requests, retries, and richer failures. Input and agent events update `App`; `ui::render` only displays `App`.
