use std::collections::BTreeMap;

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

pub(crate) fn upsert_mcp_server_value(
    existing: Option<&serde_yaml::Value>,
    name: &str,
    server: &McpServerConfig,
) -> Result<serde_yaml::Value> {
    let mut validation = McpConfig::default();
    validation.servers.insert(name.to_owned(), server.clone());
    validation.validate()?;

    let mut updated = existing
        .cloned()
        .unwrap_or_else(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    let root = updated
        .as_mapping_mut()
        .context("mcp configuration must be a mapping")?;
    let servers = root
        .entry(serde_yaml::Value::String("servers".to_owned()))
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
        .as_mapping_mut()
        .context("mcp.servers configuration must be a mapping")?;
    let serialized: BTreeMap<String, serde_yaml::Value> =
        serde_yaml::from_str(&server_yaml(name, server)?)?;
    let server = serialized
        .get(name)
        .cloned()
        .context("serialized MCP server is missing")?;
    servers.insert(serde_yaml::Value::String(name.to_owned()), server);

    let validation: McpConfig = serde_yaml::from_value(updated.clone())?;
    validation.validate()?;
    Ok(updated)
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

#[cfg(test)]
mod tests {
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
    fn upsert_preserves_existing_mcp_fields_and_replaces_only_the_named_server() {
        let existing: serde_yaml::Value = serde_yaml::from_str(
            "custom: keep\nservers:\n  old:\n    transport: stdio\n    command: old-server\n  filesystem:\n    transport: stdio\n    command: replaced-server\n",
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

        let updated = upsert_mcp_server_value(Some(&existing), "filesystem", &server).unwrap();
        let parsed: McpConfig = serde_yaml::from_value(updated.clone()).unwrap();

        assert_eq!(updated["custom"], "keep");
        assert_eq!(
            parsed.servers["old"].transport,
            McpTransportConfig::Stdio {
                command: "old-server".to_owned(),
                args: Vec::new(),
                env: BTreeMap::new(),
                env_vars: Vec::new(),
                cwd: None,
            }
        );
        let McpTransportConfig::Stdio {
            command, env_vars, ..
        } = &parsed.servers["filesystem"].transport
        else {
            panic!("expected stdio server");
        };
        assert_eq!(command, "npx");
        assert_eq!(env_vars, &["MCP_TOKEN"]);
    }

    #[test]
    fn upsert_creates_the_first_mcp_servers_mapping() {
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

        let updated = upsert_mcp_server_value(None, "remote", &server).unwrap();
        let parsed: McpConfig = serde_yaml::from_value(updated).unwrap();

        assert_eq!(parsed.servers["remote"].approval, McpApprovalPolicy::Allow);
    }
}
