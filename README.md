# Glint

Glint is a Rust terminal coding agent for OpenAI-compatible LLM endpoints. It provides a Ratatui interface, streaming responses, conversation resume, approvals, local tools, MCP, plugins, LSP integration, and subagents.

## Install

Glint currently supports macOS, Linux, and Windows through WSL. Native Windows is not supported yet.
The installer requires `curl` and a POSIX-compatible shell such as `sh`, `bash`, or `zsh`.

Install the latest release:

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/xhhwyh/glint/releases/latest/download/glint-installer.sh | sh
```

The installer places `glint` in `~/.local/bin`. Start it from the project you want Glint to work on:

```bash
cd your-project
glint
```

Verify the installation without loading a configuration:

```bash
glint --version
glint --help
```

## Configure

Create a starter configuration:

```bash
glint init
```

This writes `$XDG_CONFIG_HOME/glint/config.yaml` when `XDG_CONFIG_HOME` is set, or `~/.config/glint/config.yaml` otherwise. Edit the endpoint and model, then export the environment variable named by `api_key_env`:

```bash
export LLM_API_KEY="your-api-key"
glint
```

Never put an API key in `config.yaml`.

Glint selects its configuration in this order:

1. `glint --config PATH`
2. `GLINT_CONFIG`
3. `.glint/config.yaml` in the current project
4. the user configuration path above
5. `config.yaml` in the current directory for compatibility with older checkouts

`glint init` refuses to overwrite an existing file. Use an explicit destination when you want another configuration:

```bash
glint init --config ./my-glint.yaml
glint --config ./my-glint.yaml
```

The complete LLM, LSP, MCP, and plugin schemas are documented in [AGENTS.md](AGENTS.md) and [EXTENSIONS.md](EXTENSIONS.md).
Commands that update configuration, such as adding an MCP server, write back to the same file Glint selected at startup.

## Update or uninstall

Rerun the install command to update to the latest release.

Remove an installer-managed Glint binary with:

```bash
rm ~/.local/bin/glint
```

User configuration, plugin state, and saved conversations live under the configuration path and `~/.glint`; uninstalling the binary does not delete them.

## Develop

Glint requires a recent Rust toolchain with Rust 2024 edition support.

```bash
cargo build
cargo test
cargo fmt --check
cargo clippy -- -D warnings
```

For local development, the checked-in `config.yaml` remains the final fallback, so `cargo run` continues to work from the repository root after its selected API-key environment variable is exported.

## Release

The version in `Cargo.toml` and the Git tag must match. After the release commit reaches `main`:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The generated dist workflow builds the supported platform archives and publishes a GitHub Release containing checksums, a manifest, and `glint-installer.sh`.
