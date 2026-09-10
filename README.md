# Glint

Glint 是一个终端 AI 编程助手，支持多个模型供应商、ChatGPT 订阅登录和自定义 OpenAI 兼容接口。Glint 自己管理上下文、工具执行、审批和会话，提供流式回复、历史对话恢复、MCP、插件、LSP 和子代理。

## 安装

直接安装预编译程序，**不需要安装 Rust 或 Codex CLI**。支持 macOS、Linux，以及 Windows 上的 WSL；暂不提供原生 Windows 程序。

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/xhhwyh/glint/releases/latest/download/glint-installer.sh | sh
```

安装位置是 `~/.local/bin/glint`。安装后重新打开终端，检查版本：

```bash
glint --version
```

如果提示 `command not found`，先在当前终端执行：

```bash
export PATH="$HOME/.local/bin:$PATH"
```

如需长期生效，将这行加入所用 shell 的 `~/.bashrc` 或 `~/.zshrc`。安装器需要 `curl` 和 POSIX shell；Linux 需要 glibc，不适用于原生 Alpine/musl 环境。

### 编程工具依赖

文件查找和内容搜索需要 `ripgrep`（`rg`）：

```bash
# macOS（Homebrew）
brew install ripgrep

# Ubuntu / Debian / WSL Ubuntu
sudo apt-get update
sudo apt-get install ripgrep
```

其他 Linux 发行版请用系统包管理器安装 `ripgrep`。LSP 功能还需要对应的语言服务器；例如 Rust 项目可通过 `rustup component add rust-analyzer` 安装。使用 Git 操作时也需要本地安装 Git。

### 手动下载

也可以从 [GitHub Releases](https://github.com/xhhwyh/glint/releases/latest) 下载对应压缩包及 `.sha256` 校验文件。

| 系统 | 压缩包 |
| --- | --- |
| macOS Apple Silicon | `glint-aarch64-apple-darwin.tar.xz` |
| macOS Intel | `glint-x86_64-apple-darwin.tar.xz` |
| Linux / WSL x86_64 | `glint-x86_64-unknown-linux-gnu.tar.xz` |
| Linux ARM64 | `glint-aarch64-unknown-linux-gnu.tar.xz` |

解压后，将其中的 `glint` 可执行文件放进 PATH 下的目录，例如 `~/.local/bin`。下载页面同时提供固定版本安装命令。

## 开始使用

在你希望 Glint 操作的项目目录启动：

```bash
cd your-project
glint
```

第一次启动会进入 Welcome 界面。选择 **Add model**，完成以下一种配置，再选择 **Start Glint**：

- **API Key**：选择 DeepSeek、Volcengine、Zhipu、Kimi、DashScope 或 OpenRouter，填写并保存 Key。
- **ChatGPT 订阅**：选择 OpenAI，按界面提示打开浏览器并登录 ChatGPT 账号。可用模型与额度取决于账号权益。
- **自定义接口**：填写供应商名称、OpenAI 兼容 Base URL、API Key 和模型名称。

普通 API Key 保存时只做本地校验，实际可用性在请求模型时确认。配置完成后，在输入框描述任务并按 Enter 发送。

| 操作 | 按键或命令 |
| --- | --- |
| 发送消息 / 输入换行 | Enter / Shift+Enter |
| 选择模型 | `/model` |
| 调整支持的模型的思考强度 | 选中模型后按 →，用 ← / → 调整，Enter 确认 |
| 恢复历史会话 | `/resume` |
| 退出 | Ctrl+C |

初始设置界面支持鼠标悬停和点击，也可以通过键盘导航；按界面底部提示返回或删除条目。思考强度按模型单独记忆，支持范围见 [REASONING.md](REASONING.md)。

## 升级与卸载

重新执行上面的安装命令即可升级；现有 `~/.glint` 配置、登录状态和会话会保留。

卸载程序：

```bash
rm ~/.local/bin/glint
```

卸载不会删除 `~/.glint` 中的数据或自定义插件缓存目录。早期从源码运行、使用工作目录配置文件的版本需要重新设置供应商；当前版本统一使用 `~/.glint`。

## 从源码安装

需要支持 Rust 2024 edition 的新版本 Rust 工具链，以及系统编译工具。

```bash
git clone https://github.com/xhhwyh/glint.git
cd glint
git checkout v0.1.0
cargo install --path . --locked --root "$HOME/.local"
```

## First run and models

On the first interactive launch, Glint opens Welcome. Choose `Add model`, select a built-in provider, enter its masked API key, and save. The provider list then shows it as configured and enables every model that Glint ships for that provider; setup never checks the key over the network. The first saved built-in provider selects its embedded default model; a first custom provider selects its first model. Add other providers if wanted, then choose `Start Glint`.

Use `/model` while chatting to switch among configured models or choose `Add model` to return to provider management. The normal picker shows only providers with a readable credential and their configured models. Built-in providers expose their complete embedded model lists. A custom provider asks for a display name, an OpenAI-compatible base URL, a masked key, and one or more model-name rows. Tab to a row's trailing `×` and press Enter or Delete to remove it; removing the only row clears it. Existing custom-provider names are read-only identities, while their URL, key, and models remain editable. Model names are validated locally, must be unique, and are kept in their entered order.

### Sign in with ChatGPT

Glint can use the Codex model access included in a ChatGPT subscription through native OAuth and a Responses model adapter. Choose **Add model → OpenAI → Sign in with ChatGPT → Open browser**, then complete the login page. Glint discovers the account's available models. Choose **Start Glint**; if an API model was already selected, use `/model` to select OpenAI. Installing Codex CLI is not required.

Glint owns the agent loop, tools, approvals, MCP/LSP, plugins, conversation history and context compaction for this provider, just as it does for API providers. ChatGPT supplies model inference. There is no separate Codex thread or App Server process. Existing Glint sessions remain readable; old Codex tool cards stay visible but are not replayed as native tool calls.

Authentication is stored separately in protected `~/.glint/chatgpt-auth.json`, never YAML or the API-key store. When native credentials are absent, Glint can import the previous integration's isolated `~/.glint/codex/auth.json`, preserving the original. Startup uses cached model metadata without network login. Reopen ChatGPT setup to refresh models or repair login. Removing the provider with `Del` removes configured models and retains credentials.

Use `/model` to select **OpenAI → model**. The highlighted row shows the model’s supported effort scale. Press Right to focus the scale, Left/Right to adjust, Enter to save the model and effort, or Backspace to return. Until adjusted, an unsaved effort follows the model default. Choices are remembered per model; the status line shows the active setting. Reopen ChatGPT in Add model to refresh available levels. An unavailable saved level falls back to the model default.

Explicit choices are stored without changing the model ID:

```yaml
chatgpt:
  models: [gpt-6-astra]
  reasoning_efforts:
    gpt-6-astra: high
```


The subscription transport follows the approach used by [OpenCode](https://opencode.ai/docs/providers/#openai). It uses the ChatGPT Codex Responses backend, rather than the general Platform API; model access and limits depend on the account, and backend compatibility can change.


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

## Develop

Glint requires a recent Rust toolchain with Rust 2024 edition support.

```bash
cargo build
cargo test
cargo fmt --check
cargo clippy -- -D warnings
```

Run `cargo run` from the source checkout, or use the installed `glint` from any workspace. An unconfigured non-interactive launch reports that setup needs an interactive terminal.

## Release

The version in `Cargo.toml` and the Git tag must match. After the release commit reaches `main`:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The generated dist workflow builds the supported platform archives and publishes a GitHub Release containing checksums, a manifest, and `glint-installer.sh`.

Other supported providers use the same effort axis. See [model reasoning support](REASONING.md) for verified model levels, API differences, and persistence.
