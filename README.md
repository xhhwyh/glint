# Glint

Glint is a Rust terminal coding agent for OpenAI-compatible LLM endpoints. It provides a Ratatui interface, streaming responses, conversation resume, approvals, local tools, MCP, plugins, LSP integration, and subagents.

## Install and start

Glint supports macOS, Linux, and Windows through WSL. Native Windows is not supported yet. The installer requires `curl` and a POSIX-compatible shell such as `sh`, `bash`, or `zsh`.

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/xhhwyh/glint/releases/latest/download/glint-installer.sh | sh
```

The installer puts `glint` in `~/.local/bin`. Launch it from the workspace you want it to work on:

```bash
cd your-project
glint
```

`glint --version` and `glint --help` work before setup.

## First run and models

On the first interactive launch, Glint opens Welcome. Choose `Add model`, select a built-in provider, enter its masked API key, and save. The provider list then shows it as configured and enables every model that Glint ships for that provider; setup never checks the key over the network. The first saved built-in provider selects its embedded default model; a first custom provider selects its first model. Add other providers if wanted, then choose `Start Glint`.

Use `/model` while chatting to switch among configured models or choose `Add model` to return to provider management. The normal picker shows only providers with a readable credential and their configured models. Built-in providers expose their complete embedded model lists. A custom provider asks for a display name, an OpenAI-compatible base URL, a masked key, and one or more model-name rows. Tab to a row's trailing `×` and press Enter or Delete to remove it; removing the only row clears it. Existing custom-provider names are read-only identities, while their URL, key, and models remain editable. Model names are validated locally, must be unique, and are kept in their entered order.

## Configuration and credentials

Core Glint state has one fixed root, `~/.glint`. The user-editable settings file is `~/.glint/config.yaml`; Glint creates a concise file and omits empty optional sections. A typical file looks like this:

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
```

Built-in provider metadata and keys are deliberately absent from this file. An existing protected `~/.glint/auth.json` is authoritative; otherwise Glint prefers the operating-system keyring. If the keyring is unavailable on a fresh install, it creates the protected file instead. If a configured provider's keyring becomes unavailable, Glint opens setup with a safe repair notice; provider deletion is blocked so configuration cannot be removed while its key may remain orphaned. Re-entering a key explicitly creates and switches to the protected file backend. Never add credentials to YAML, commands, logs, or transcripts.

Optional `mcp`, `plugins`, and `lsp` blocks also belong in this file. MCP and session state remain below `~/.glint`; plugin cache and install state do too by default. An explicit `plugins.cache_dir`, including an absolute path, moves that plugin cache and state outside the root. Their schemas and workspace behavior are documented in [EXTENSIONS.md](EXTENSIONS.md). The core contributor-facing schema is in [AGENTS.md](AGENTS.md).

## Upgrading and uninstalling

This release does not import a previous working-directory configuration. After upgrading, start Glint interactively and set providers up again; API keys must be re-entered. You may manually copy compatible `mcp`, `plugins`, and `lsp` blocks into `~/.glint/config.yaml`.

Rerun the installer to update. To remove the installer-managed binary:

```bash
rm ~/.local/bin/glint
```

This intentionally leaves `~/.glint`—configuration, credentials, default plugin state, MCP state, and saved conversations—intact. It also leaves any explicitly configured plugin cache directory intact.

## Develop

Glint requires a recent Rust toolchain with Rust 2024 edition support.

```bash
cargo build
cargo test
cargo fmt --check
cargo clippy -- -D warnings
```

Run `cargo run` from any workspace. An unconfigured non-interactive launch reports that setup needs an interactive terminal.

## Release

The version in `Cargo.toml` and the Git tag must match. After the release commit reaches `main`:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The generated dist workflow builds the supported platform archives and publishes a GitHub Release containing checksums, a manifest, and `glint-installer.sh`.
