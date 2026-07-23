use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

impl McpConfig {
    pub fn validate(&self) -> Result<()> {
        for (name, server) in &self.servers {
            if name.trim().is_empty() {
                bail!("MCP server name must not be empty");
            }
            if server.startup_timeout_ms == 0 || server.tool_timeout_ms == 0 {
                bail!("MCP server '{name}' timeouts must be greater than zero");
            }
            match &server.transport {
                McpTransportConfig::Stdio {
                    command, env_vars, ..
                } => {
                    if command.trim().is_empty() {
                        bail!("MCP server '{name}' command must not be empty");
                    }
                    if env_vars.iter().any(|variable| variable.trim().is_empty()) {
                        bail!("MCP server '{name}' has an empty env_vars entry");
                    }
                }
                McpTransportConfig::StreamableHttp {
                    url,
                    bearer_token_env,
                    oauth,
                    ..
                } => {
                    let parsed = reqwest::Url::parse(url).map_err(|error| {
                        anyhow::anyhow!("MCP server '{name}' URL is invalid: {error}")
                    })?;
                    if !matches!(parsed.scheme(), "http" | "https") {
                        bail!("MCP server '{name}' URL must use HTTP or HTTPS");
                    }
                    if bearer_token_env
                        .as_ref()
                        .is_some_and(|variable| variable.trim().is_empty())
                    {
                        bail!("MCP server '{name}' bearer_token_env must not be empty");
                    }
                    if bearer_token_env.is_some() && oauth.is_some() {
                        bail!(
                            "MCP server '{name}' cannot configure both bearer_token_env and oauth"
                        );
                    }
                    if let Some(oauth) = oauth {
                        reqwest::Url::parse(&oauth.redirect_uri).map_err(|error| {
                            anyhow::anyhow!(
                                "MCP server '{name}' OAuth redirect_uri is invalid: {error}"
                            )
                        })?;
                        if oauth.scopes.iter().any(|scope| scope.trim().is_empty()) {
                            bail!("MCP server '{name}' has an empty OAuth scope");
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default = "default_startup_timeout_ms")]
    pub startup_timeout_ms: u64,
    #[serde(default = "default_tool_timeout_ms")]
    pub tool_timeout_ms: u64,
    #[serde(default)]
    pub approval: McpApprovalPolicy,
    #[serde(default)]
    pub tool_approval: BTreeMap<String, McpApprovalPolicy>,
    #[serde(default)]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(flatten)]
    pub transport: McpTransportConfig,
}

impl McpServerConfig {
    pub fn approval_for_tool(&self, tool: &str) -> McpApprovalPolicy {
        self.tool_approval
            .get(tool)
            .copied()
            .unwrap_or(self.approval)
    }

    pub fn tool_enabled(&self, tool: &str) -> bool {
        !self.disabled_tools.iter().any(|disabled| disabled == tool)
            && self
                .enabled_tools
                .as_ref()
                .is_none_or(|enabled| enabled.iter().any(|candidate| candidate == tool))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpTransportConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        env_vars: Vec<String>,
        cwd: Option<String>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        bearer_token_env: Option<String>,
        #[serde(default)]
        oauth: Option<McpOAuthConfig>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct McpOAuthConfig {
    pub redirect_uri: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalPolicy {
    Allow,
    Deny,
    #[default]
    Prompt,
}

fn enabled_by_default() -> bool {
    true
}

fn default_startup_timeout_ms() -> u64 {
    20_000
}

fn default_tool_timeout_ms() -> u64 {
    60_000
}

pub(crate) fn persist_mcp_server(path: &Path, name: &str, server: &McpServerConfig) -> Result<()> {
    let mut validation = McpConfig::default();
    validation.servers.insert(name.to_owned(), server.clone());
    validation.validate()?;

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let _: serde_yaml::Value =
        serde_yaml::from_str(&content).context("failed to parse existing config.yaml")?;
    let snippet = server_yaml(name, server)?;
    let updated = insert_server_yaml(&content, &snippet)?;

    let temporary = path.with_extension(format!("yaml.tmp-{}", std::process::id()));
    fs::write(&temporary, updated)
        .with_context(|| format!("failed to write temporary config {}", temporary.display()))?;
    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(&temporary, metadata.permissions()).with_context(|| {
            format!(
                "failed to preserve permissions for temporary config {}",
                temporary.display()
            )
        })?;
    }
    fs::rename(&temporary, path).with_context(|| format!("failed to replace {}", path.display()))
}

fn server_yaml(name: &str, server: &McpServerConfig) -> Result<String> {
    let mut values = serde_yaml::Mapping::new();
    values.insert("enabled".into(), server.enabled.into());
    if server.startup_timeout_ms != default_startup_timeout_ms() {
        values.insert(
            "startup_timeout_ms".into(),
            server.startup_timeout_ms.into(),
        );
    }
    if server.tool_timeout_ms != default_tool_timeout_ms() {
        values.insert("tool_timeout_ms".into(), server.tool_timeout_ms.into());
    }
    if server.approval != McpApprovalPolicy::Prompt {
        values.insert("approval".into(), approval_label(server.approval).into());
    }
    match &server.transport {
        McpTransportConfig::Stdio {
            command,
            args,
            env,
            env_vars,
            cwd,
        } => {
            values.insert("transport".into(), "stdio".into());
            values.insert("command".into(), command.clone().into());
            if !args.is_empty() {
                values.insert("args".into(), serde_yaml::to_value(args)?);
            }
            if !env.is_empty() {
                values.insert("env".into(), serde_yaml::to_value(env)?);
            }
            if !env_vars.is_empty() {
                values.insert("env_vars".into(), serde_yaml::to_value(env_vars)?);
            }
            if let Some(cwd) = cwd {
                values.insert("cwd".into(), cwd.clone().into());
            }
        }
        McpTransportConfig::StreamableHttp {
            url,
            headers,
            bearer_token_env,
            oauth,
        } => {
            values.insert("transport".into(), "streamable_http".into());
            values.insert("url".into(), url.clone().into());
            if !headers.is_empty() {
                values.insert("headers".into(), serde_yaml::to_value(headers)?);
            }
            if let Some(variable) = bearer_token_env {
                values.insert("bearer_token_env".into(), variable.clone().into());
            }
            if let Some(oauth) = oauth {
                let mut oauth_values = serde_yaml::Mapping::new();
                oauth_values.insert("redirect_uri".into(), oauth.redirect_uri.clone().into());
                if !oauth.scopes.is_empty() {
                    oauth_values.insert("scopes".into(), serde_yaml::to_value(&oauth.scopes)?);
                }
                values.insert("oauth".into(), serde_yaml::Value::Mapping(oauth_values));
            }
        }
    }
    if !server.tool_approval.is_empty() {
        let approvals = server
            .tool_approval
            .iter()
            .map(|(tool, approval)| (tool.clone(), approval_label(*approval)))
            .collect::<BTreeMap<_, _>>();
        values.insert("tool_approval".into(), serde_yaml::to_value(approvals)?);
    }
    if let Some(enabled) = &server.enabled_tools {
        values.insert("enabled_tools".into(), serde_yaml::to_value(enabled)?);
    }
    if !server.disabled_tools.is_empty() {
        values.insert(
            "disabled_tools".into(),
            serde_yaml::to_value(&server.disabled_tools)?,
        );
    }

    let root = BTreeMap::from([(name.to_owned(), serde_yaml::Value::Mapping(values))]);
    serde_yaml::to_string(&root).context("failed to serialize MCP server configuration")
}

fn approval_label(approval: McpApprovalPolicy) -> &'static str {
    match approval {
        McpApprovalPolicy::Allow => "allow",
        McpApprovalPolicy::Deny => "deny",
        McpApprovalPolicy::Prompt => "prompt",
    }
}

fn insert_server_yaml(content: &str, snippet: &str) -> Result<String> {
    let mut lines = content.lines().map(str::to_owned).collect::<Vec<_>>();
    let indented = snippet
        .trim_end()
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>();
    let Some(mcp_index) = lines.iter().position(|line| yaml_key_line(line, 0, "mcp")) else {
        if lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("mcp:".to_owned());
        lines.push("  servers:".to_owned());
        lines.extend(indented);
        return Ok(format!("{}\n", lines.join("\n")));
    };
    ensure_block_key(&lines[mcp_index], "mcp")?;
    let mcp_end = section_end(&lines, mcp_index + 1, 0);
    let servers_index =
        (mcp_index + 1..mcp_end).find(|index| yaml_key_line(&lines[*index], 2, "servers"));
    match servers_index {
        Some(servers_index) => {
            ensure_block_key(&lines[servers_index], "servers")?;
            let insert_at = section_end(&lines, servers_index + 1, 2);
            lines.splice(insert_at..insert_at, indented);
        }
        None => {
            let mut addition = vec!["  servers:".to_owned()];
            addition.extend(indented);
            lines.splice(mcp_index + 1..mcp_index + 1, addition);
        }
    }
    Ok(format!("{}\n", lines.join("\n")))
}

fn yaml_key_line(line: &str, indent: usize, key: &str) -> bool {
    let actual_indent = line.len() - line.trim_start_matches(' ').len();
    if actual_indent != indent {
        return false;
    }
    line[indent..]
        .strip_prefix(key)
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some()
}

fn ensure_block_key(line: &str, key: &str) -> Result<()> {
    let value = line
        .trim_start()
        .strip_prefix(key)
        .and_then(|rest| rest.strip_prefix(':'))
        .expect("key line was already identified")
        .trim();
    if value.is_empty() || value.starts_with('#') {
        Ok(())
    } else {
        bail!("config.yaml uses an inline '{key}' value; expand it to block YAML before adding")
    }
}

fn section_end(lines: &[String], start: usize, parent_indent: usize) -> usize {
    (start..lines.len())
        .find(|index| {
            let line = lines[*index].as_str();
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                return false;
            }
            let indent = line.len() - line.trim_start_matches(' ').len();
            indent <= parent_indent
        })
        .unwrap_or(lines.len())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parses_stdio_http_filters_and_approval_policies() {
        let config: McpConfig = serde_yaml::from_str(
            r#"
servers:
  local:
    transport: stdio
    command: node
    args: [server.js]
    approval: prompt
    tool_approval:
      read: allow
    enabled_tools: [read, write]
    disabled_tools: [write]
  remote:
    transport: streamable_http
    url: https://example.test/mcp
    oauth:
      redirect_uri: http://127.0.0.1:8765/callback
      scopes: [read, write]
  token:
    transport: streamable_http
    url: https://example.test/mcp
    bearer_token_env: MCP_TOKEN
"#,
        )
        .unwrap();
        config.validate().unwrap();

        let local = &config.servers["local"];
        assert_eq!(local.approval_for_tool("read"), McpApprovalPolicy::Allow);
        assert_eq!(local.approval_for_tool("other"), McpApprovalPolicy::Prompt);
        assert!(local.tool_enabled("read"));
        assert!(!local.tool_enabled("write"));
        assert!(!local.tool_enabled("other"));
        assert!(matches!(
            config.servers["remote"].transport,
            McpTransportConfig::StreamableHttp { .. }
        ));
        let McpTransportConfig::StreamableHttp { oauth, .. } = &config.servers["remote"].transport
        else {
            panic!("expected HTTP transport");
        };
        assert_eq!(oauth.as_ref().unwrap().scopes, ["read", "write"]);
    }

    #[test]
    fn rejects_ambiguous_http_authentication() {
        let config: McpConfig = serde_yaml::from_str(
            r#"
servers:
  remote:
    transport: streamable_http
    url: https://example.test/mcp
    bearer_token_env: MCP_TOKEN
    oauth:
      redirect_uri: http://127.0.0.1:8765/callback
"#,
        )
        .unwrap();

        let error = config.validate().unwrap_err();
        assert!(format!("{error:#}").contains("both bearer_token_env and oauth"));
    }

    #[test]
    fn persists_server_without_reformatting_existing_yaml() {
        let root =
            std::env::temp_dir().join(format!("glint-mcp-config-persist-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.yaml");
        fs::write(
            &path,
            "# keep this comment\nllm:\n  provider: demo\nmcp:\n  servers:\n    old:\n      transport: stdio\n      command: old-server\nplugins: {}\n",
        )
        .unwrap();
        let server = McpServerConfig {
            enabled: true,
            startup_timeout_ms: default_startup_timeout_ms(),
            tool_timeout_ms: default_tool_timeout_ms(),
            approval: McpApprovalPolicy::Prompt,
            tool_approval: BTreeMap::new(),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            transport: McpTransportConfig::Stdio {
                command: "npx".to_owned(),
                args: vec![
                    "-y".to_owned(),
                    "@modelcontextprotocol/server-filesystem".to_owned(),
                ],
                env: BTreeMap::new(),
                env_vars: vec!["MCP_TOKEN".to_owned()],
                cwd: Some(".".to_owned()),
            },
        };

        persist_mcp_server(&path, "filesystem", &server).unwrap();

        let persisted = fs::read_to_string(&path).unwrap();
        assert!(persisted.starts_with("# keep this comment\nllm:"));
        assert!(persisted.contains("    old:\n"));
        assert!(persisted.contains("    filesystem:\n"));
        assert!(persisted.contains("      command: npx\n"));
        assert!(persisted.contains("      env_vars:\n      - MCP_TOKEN\n"));
        assert!(persisted.ends_with("plugins: {}\n"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn persists_first_mcp_section_at_end_of_config() {
        let root =
            std::env::temp_dir().join(format!("glint-mcp-config-first-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.yaml");
        fs::write(&path, "llm:\n  provider: demo\n").unwrap();
        let server = McpServerConfig {
            enabled: true,
            startup_timeout_ms: default_startup_timeout_ms(),
            tool_timeout_ms: default_tool_timeout_ms(),
            approval: McpApprovalPolicy::Allow,
            tool_approval: BTreeMap::new(),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            transport: McpTransportConfig::StreamableHttp {
                url: "https://example.test/mcp".to_owned(),
                headers: BTreeMap::new(),
                bearer_token_env: Some("MCP_TOKEN".to_owned()),
                oauth: None,
            },
        };

        persist_mcp_server(&path, "remote", &server).unwrap();

        let persisted = fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("\nmcp:\n  servers:\n    remote:\n"));
        assert!(persisted.contains("      approval: allow\n"));
        assert!(persisted.contains("      bearer_token_env: MCP_TOKEN\n"));
        fs::remove_dir_all(root).ok();
    }
}
