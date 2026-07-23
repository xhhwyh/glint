# Plugins and MCP

Glint loads plugins and MCP servers from `config.yaml` at startup. Plugin contributions are merged before the agent, LSP manager, MCP manager, slash-command registry, and system prompt are created. Treat plugins and MCP servers as trusted code: command hooks and stdio servers run local processes with the Glint user's permissions.

## MCP configuration

Configure standalone servers under `mcp.servers`:

```yaml
mcp:
  servers:
    filesystem:
      transport: stdio
      command: npx
      args: ["-y", "@modelcontextprotocol/server-filesystem", "."]
      cwd: .
      env:
        LOG_LEVEL: info
      env_vars: [OPTIONAL_SECRET_FROM_ENV]
      startup_timeout_ms: 20000
      tool_timeout_ms: 60000
      approval: prompt
      tool_approval:
        read_file: allow
        delete_file: deny
      enabled_tools: [read_file, write_file, delete_file]
      disabled_tools: [delete_file]

    remote:
      transport: streamable_http
      url: https://example.com/mcp
      headers:
        X-Client: glint
      bearer_token_env: EXAMPLE_MCP_TOKEN
      approval: prompt

    oauth_remote:
      transport: streamable_http
      url: https://example.com/mcp
      oauth:
        redirect_uri: http://127.0.0.1:8765/callback
        scopes: [read, write]
```

`approval` and each `tool_approval` value can be `allow`, `prompt`, or `deny`. The per-tool value overrides the server default. `enabled_tools` is an allowlist; `disabled_tools` is applied after it. A server cannot use both `bearer_token_env` and `oauth`.

MCP tools are exposed to the model as `mcp__<server>__<tool>`. Resources, resource templates, subscriptions, and prompts are exposed through generated gateway tools. Tool-list, resource-list, and prompt-list change notifications refresh the registry. Calls have cancellation and timeouts; read-only annotations permit parallel execution but never bypass configured approval.

The client advertises roots and both form and URL elicitation. Server elicitation is shown in Glint's approval panel; form answers are entered as JSON. HTTP sessions transparently reinitialize when a server expires a session.

Use `/mcp` to open the full-screen MCP manager. It lists configured servers and shows each
server's connection details, model-visible tools, resources, resource templates, prompts, and
approval policy. Use arrow keys to select or scroll, `Tab`/`Left`/`Right` to switch focus,
`Enter` for full details, `R` to reconnect, `A` to authorize with OAuth, `L` to log out, and
`Esc` to go back or close. Secret values are never shown; the manager displays only environment
variable and header names and redacts URL credentials and query strings.

Select `＋ Add MCP server` to add a standalone stdio, Streamable HTTP, or OAuth server. The form
accepts inherited environment-variable names for secrets, validates the configuration, appends it
to `config.yaml` without reformatting the rest of the file, and activates the server immediately.
Use `Up`/`Down`/`Tab` to choose a field, `Left`/`Right` to change transport or approval, `Enter` to
save, and `Esc` to cancel.

The command forms remain available for scripting and direct use:

```text
/mcp reconnect <server>
/mcp auth <server>
/mcp auth-callback <server> <complete-redirected-url>
/mcp logout <server>
```

For OAuth, open the URL returned by `/mcp auth`, authorize it, then paste the browser's complete redirected URL into `/mcp auth-callback`. Glint stores OAuth credentials under `~/.glint/mcp/oauth/` with private permissions and reuses and refreshes them across launches. `/mcp logout` deletes the stored credentials.

## Plugin configuration

Plugins can be local directories or Git repositories:

```yaml
plugins:
  cache_dir: ~/.glint/plugins/cache
  entries:
    - ./plugins/local-tools
    - source: https://github.com/example/glint-plugin.git
      ref: v1.2.0
      subdir: plugins/review-tools
      enabled: true
    - source: ./plugins/disabled
      enabled: false
  marketplaces:
    - ./plugins/marketplace.json
    - https://example.com/glint-marketplace.json
```

Local paths are resolved relative to the working directory. Git sources are cloned into the cache, fetched on later launches, optionally checked out at `ref`, and can select a monorepo plugin root with `subdir`. Subdirectory checkouts use Git sparse checkout and reject absolute paths, `..`, and symlink escapes.

Marketplaces can be GitHub `owner/repo` shorthands, Git URLs, local marketplace directories/files, or remote JSON catalogs. Glint understands relative plugin paths and the Claude marketplace `github`, `url`, and `git-subdir` source objects. Remote JSON catalogs cannot use relative plugin paths because the catalog does not include the referenced files. NPM marketplace sources are reported as unsupported.

Use the built-in commands to manage marketplaces and installed plugins:

```text
/plugins marketplace add anthropics/claude-code
/plugins marketplace update
/plugins marketplace remove claude-code-plugins
/plugins install frontend-design@claude-code-plugins
/plugins disable frontend-design@claude-code-plugins
/plugins enable frontend-design@claude-code-plugins
/plugins uninstall frontend-design@claude-code-plugins
/reload-plugins
```

Marketplace additions and installed-plugin state are stored in `~/.glint/plugins/state.json` by default. If `plugins.cache_dir` is set, the state file is stored inside that directory. Mutations reload skills, commands, hooks, MCP servers, and LSP servers into the current session without restarting Glint.

`/plugins` opens the full-screen plugin manager. Its `Installed` tab lists installed plugins, their enabled state, source, path, and registered commands, skills, agents, hooks, MCP servers, LSP servers, and settings. Press `Space` to enable or disable a marketplace-installed plugin and `Enter` to inspect it. The `Marketplaces` tab lists configured marketplaces and their available plugins. Select `Add marketplace` to enter a source; Git download activity is captured and displayed inside the TUI instead of writing to the terminal. Press `Space` on a marketplace plugin to install or uninstall it.

Glint searches each plugin root for `.glint-plugin/plugin.json`, `.claude-plugin/plugin.json`, `.codex-plugin/plugin.json`, or `plugin.json`, in that order. A manifest can declare one path or an array of paths for every contribution:

```json
{
  "name": "review-tools",
  "version": "1.2.0",
  "description": "Repository review workflows",
  "dependencies": ["shared-tools"],
  "commands": "./commands",
  "skills": "./skills",
  "agents": "./agents",
  "hooks": "./hooks/hooks.json",
  "mcpServers": "./.mcp.json",
  "lspServers": "./.lsp.json",
  "settings": "./settings/settings.json"
}
```

Missing contribution fields use these convention paths when they exist: `commands/`, `skills/`, `agents/`, `hooks/hooks.json`, `.mcp.json`, `.lsp.json`, and `settings/settings.json`. Plugin names contain only ASCII letters, digits, `-`, or `_`. Contribution paths are canonicalized and cannot escape the plugin root. Duplicate names, missing dependencies, and MCP/LSP collisions fail startup.

### Commands, skills, and agents

Commands are Markdown files. Their filename becomes `/<plugin>:<filename>` and optional YAML frontmatter supplies the description:

```markdown
---
description: Review the current changes
---
Review this repository. Focus on $ARGUMENTS and compare $1 with $2.
```

`$ARGUMENTS` receives the complete argument string; `$1` through `$9` receive whitespace-separated arguments. If no placeholder is present, Glint appends the arguments to the prompt.

Skills use `skills/<skill-name>/SKILL.md`; agents use Markdown files under `agents/`. Both support the same `description` frontmatter and are namespaced as `<plugin>:<name>`. Skills are advertised in the system prompt with their paths. A plugin agent is selected through the `Subagent` tool's `agent` field, and its definition is injected into the delegated agent's task.

### Hooks

Hooks run at `session_start`, `session_end`, `prompt_submit`, `before_model_call`, `after_model_call`, `before_tool_call`, `after_tool_call`, `before_compact`, `after_compact`, `agent_start`, and `agent_end`. A native hook file is a JSON array:

```json
[
  {
    "event": "before_tool_call",
    "matcher": "Bash|Edit",
    "command": "python3 ${GLINT_PLUGIN_ROOT}/hooks/check.py",
    "timeout_ms": 10000
  }
]
```

Claude-style event maps are also accepted, including `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PreCompact`, `SubagentStart`, `SubagentStop`, and `Stop`:

```json
{
  "PreToolUse": [
    {
      "matcher": "Bash|Edit",
      "hooks": [
        {"type": "command", "command": "python3 hooks/check.py", "timeout": 10}
      ]
    }
  ]
}
```

The event payload is JSON on stdin. The hook receives `GLINT_PLUGIN`, `GLINT_HOOK_EVENT`, `GLINT_PLUGIN_ROOT`, and `CLAUDE_PLUGIN_ROOT`; when settings are declared, `GLINT_PLUGIN_SETTINGS` contains their JSON. Its working directory is the plugin root. Exit `0` with no output to allow. Exit `2` to deny using stderr as the reason, or return JSON:

```json
{"decision":"deny","reason":"policy reason"}
```

To replace supported prompt or tool input, return `{"decision":"allow","replacement":{...}}`. Claude-style `hookSpecificOutput.permissionDecision`, `permissionDecisionReason`, and `updatedInput` are also understood. Other nonzero exits, invalid JSON, and timeouts fail the event.

### Plugin MCP, LSP, and settings

A plugin `.mcp.json` accepts either a top-level server map or `{ "mcpServers": { ... } }`. Each server uses `command` plus stdio fields, or `url` plus HTTP fields; Glint prefixes the server name with `<plugin>:`. The approval, filter, timeout, bearer-token, and OAuth fields are the same as standalone MCP configuration, except `transport` is inferred.

An `.lsp.json` file is a map of server names to Glint LSP server configuration. Names are also prefixed with `<plugin>:`. Settings JSON is loaded under the plugin's name so plugin-owned configuration stays isolated.
