mod hooks;

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    config::{LspConfig, LspServerConfig},
    services::mcp::{
        McpApprovalPolicy, McpConfig, McpOAuthConfig, McpServerConfig, McpTransportConfig,
    },
};

pub use hooks::HookRunner;

type ProgressReporter = Arc<dyn Fn(String) + Send + Sync>;

thread_local! {
    static PROGRESS_REPORTER: RefCell<Option<ProgressReporter>> = RefCell::new(None);
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PluginsConfig {
    #[serde(default)]
    pub entries: Vec<PluginEntryConfig>,
    #[serde(default)]
    pub marketplaces: Vec<String>,
    #[serde(default)]
    pub cache_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PluginEntryConfig {
    Source(String),
    Detailed {
        source: String,
        #[serde(default = "enabled_by_default")]
        enabled: bool,
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        #[serde(default)]
        subdir: Option<String>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct ExtensionCatalog {
    pub plugins: Vec<LoadedPlugin>,
    pub installed_plugins: Vec<InstalledPluginStatus>,
    pub marketplaces: Vec<ConfiguredMarketplace>,
    pub marketplace_plugins: Vec<MarketplacePlugin>,
    pub commands: Vec<PluginCommand>,
    pub skills: Vec<PluginDocument>,
    pub agents: Vec<PluginDocument>,
    pub hooks: Vec<PluginHook>,
    pub settings: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PluginContributions {
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub lsp_servers: Vec<String>,
    #[serde(default)]
    pub settings: bool,
}

impl PluginContributions {
    pub fn total(&self) -> usize {
        self.commands.len()
            + self.skills.len()
            + self.agents.len()
            + self.hooks.len()
            + self.mcp_servers.len()
            + self.lsp_servers.len()
            + usize::from(self.settings)
    }
}

#[derive(Clone, Debug)]
pub struct InstalledPluginStatus {
    pub name: String,
    pub version: String,
    pub description: String,
    pub marketplace: Option<String>,
    pub enabled: bool,
    pub config_managed: bool,
    pub root: Option<PathBuf>,
    pub contributions: PluginContributions,
}

impl InstalledPluginStatus {
    pub fn spec(&self) -> String {
        self.marketplace
            .as_ref()
            .map(|marketplace| format!("{}@{marketplace}", self.name))
            .unwrap_or_else(|| self.name.clone())
    }
}

#[derive(Clone, Debug)]
pub struct ConfiguredMarketplace {
    pub name: String,
    pub alias: String,
    pub source: String,
    pub root: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MarketplacePlugin {
    pub name: String,
    #[serde(default = "default_plugin_version")]
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub source: MarketplacePluginSource,
    #[serde(skip)]
    pub marketplace: String,
    #[serde(skip)]
    pub installed: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum MarketplacePluginSource {
    Relative(String),
    Detailed(MarketplaceSourceSpec),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "source")]
pub enum MarketplaceSourceSpec {
    #[serde(rename = "github")]
    Github {
        repo: String,
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        sha: Option<String>,
    },
    #[serde(rename = "url")]
    Url {
        url: String,
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        sha: Option<String>,
    },
    #[serde(rename = "git-subdir")]
    GitSubdir {
        url: String,
        path: String,
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        sha: Option<String>,
    },
    #[serde(rename = "npm")]
    Npm {
        package: String,
        version: Option<String>,
        registry: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MarketplaceManifest {
    Catalog {
        name: Option<String>,
        plugins: Vec<MarketplacePlugin>,
    },
    List(Vec<MarketplacePlugin>),
}

#[derive(Clone, Debug)]
struct LoadedMarketplace {
    name: String,
    alias: String,
    source: String,
    root: Option<PathBuf>,
    plugins: Vec<MarketplacePlugin>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PluginState {
    #[serde(default)]
    marketplaces: Vec<String>,
    #[serde(default)]
    installed: Vec<InstalledPlugin>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct InstalledPlugin {
    name: String,
    marketplace: String,
    entry: PluginEntryConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default)]
    contributions: PluginContributions,
}

pub struct PluginMutationResult {
    pub load: PluginLoadResult,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct LoadedPlugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub root: PathBuf,
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PluginCommand {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub plugin: String,
}

impl PluginCommand {
    pub fn expand(&self, arguments: &str) -> String {
        let mut prompt = self.prompt.replace("$ARGUMENTS", arguments);
        for (index, argument) in arguments.split_whitespace().enumerate() {
            prompt = prompt.replace(&format!("${}", index + 1), argument);
        }
        if !arguments.is_empty()
            && !self.prompt.contains("$ARGUMENTS")
            && !(1..=9).any(|index| self.prompt.contains(&format!("${index}")))
        {
            prompt.push_str("\n\nArguments: ");
            prompt.push_str(arguments);
        }
        prompt
    }
}

#[derive(Clone, Debug)]
pub struct PluginDocument {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub plugin: String,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PluginHook {
    pub event: HookEvent,
    pub command: String,
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(default = "default_hook_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub plugin: String,
    #[serde(skip)]
    pub root: Option<PathBuf>,
    #[serde(skip)]
    pub settings: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart,
    SessionEnd,
    PromptSubmit,
    BeforeModelCall,
    AfterModelCall,
    BeforeToolCall,
    AfterToolCall,
    BeforeCompact,
    AfterCompact,
    AgentStart,
    AgentEnd,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginManifest {
    name: String,
    #[serde(default = "default_plugin_version")]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    commands: Option<ResourcePaths>,
    #[serde(default)]
    skills: Option<ResourcePaths>,
    #[serde(default)]
    agents: Option<ResourcePaths>,
    #[serde(default)]
    hooks: Option<ResourcePaths>,
    #[serde(default)]
    mcp_servers: Option<ResourcePaths>,
    #[serde(default)]
    lsp_servers: Option<ResourcePaths>,
    #[serde(default)]
    settings: Option<ResourcePaths>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ResourcePaths {
    One(String),
    Many(Vec<String>),
}

impl ResourcePaths {
    fn values(&self) -> Vec<&str> {
        match self {
            Self::One(path) => vec![path],
            Self::Many(paths) => paths.iter().map(String::as_str).collect(),
        }
    }
}

pub struct PluginLoadResult {
    pub catalog: ExtensionCatalog,
    pub mcp: McpConfig,
    pub lsp: LspConfig,
}

pub struct PluginManager;

impl PluginManager {
    pub fn with_progress<T>(
        reporter: ProgressReporter,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let previous = PROGRESS_REPORTER.with(|current| current.replace(Some(reporter)));
        let result = operation();
        PROGRESS_REPORTER.with(|current| {
            current.replace(previous);
        });
        result
    }

    pub fn load(
        config: &PluginsConfig,
        mcp: McpConfig,
        lsp: LspConfig,
        cwd: &Path,
    ) -> Result<PluginLoadResult> {
        let state = load_plugin_state(config, cwd)?;
        load_with_state(config, &state, mcp, lsp, cwd)
    }

    pub fn add_marketplace(
        config: &PluginsConfig,
        mcp: McpConfig,
        lsp: LspConfig,
        cwd: &Path,
        source: &str,
    ) -> Result<PluginMutationResult> {
        let mut state = load_plugin_state(config, cwd)?;
        let marketplace = load_marketplace(source, config, cwd)?;
        let configured = load_marketplaces(config, &state, cwd)?;
        if let Some(existing) = configured
            .iter()
            .find(|existing| existing.name == marketplace.name)
        {
            if existing.source == marketplace.source {
                return Ok(PluginMutationResult {
                    load: load_with_state(config, &state, mcp, lsp, cwd)?,
                    message: format!("Marketplace `{}` is already configured.", marketplace.name),
                });
            }
            bail!(
                "marketplace '{}' is already configured from '{}'",
                marketplace.name,
                existing.source
            );
        }
        state.marketplaces.push(source.to_owned());
        let load = load_with_state(config, &state, mcp, lsp, cwd)?;
        save_plugin_state(config, cwd, &state)?;
        Ok(PluginMutationResult {
            load,
            message: format!("Added marketplace `{}` from `{source}`.", marketplace.name),
        })
    }

    pub fn remove_marketplace(
        config: &PluginsConfig,
        mcp: McpConfig,
        lsp: LspConfig,
        cwd: &Path,
        name: &str,
    ) -> Result<PluginMutationResult> {
        let mut state = load_plugin_state(config, cwd)?;
        let marketplaces = load_marketplaces(config, &state, cwd)?;
        let marketplace = marketplaces
            .iter()
            .find(|marketplace| marketplace.name == name || marketplace.alias == name)
            .with_context(|| format!("unknown marketplace '{name}'"))?;
        if config
            .marketplaces
            .iter()
            .any(|source| source == &marketplace.source)
        {
            bail!("marketplace '{name}' is declared in config.yaml and must be removed there");
        }
        state
            .marketplaces
            .retain(|source| source != &marketplace.source);
        let removed = state.installed.len();
        state
            .installed
            .retain(|plugin| plugin.marketplace != marketplace.name);
        let removed = removed - state.installed.len();
        let load = load_with_state(config, &state, mcp, lsp, cwd)?;
        save_plugin_state(config, cwd, &state)?;
        Ok(PluginMutationResult {
            load,
            message: format!("Removed marketplace `{name}` and {removed} installed plugin(s)."),
        })
    }

    pub fn install(
        config: &PluginsConfig,
        mcp: McpConfig,
        lsp: LspConfig,
        cwd: &Path,
        spec: &str,
    ) -> Result<PluginMutationResult> {
        let mut state = load_plugin_state(config, cwd)?;
        let marketplaces = load_marketplaces(config, &state, cwd)?;
        let (marketplace, plugin) = find_marketplace_plugin(&marketplaces, spec)?;
        if state.installed.iter().any(|installed| {
            installed.name == plugin.name && installed.marketplace == marketplace.name
        }) {
            bail!(
                "plugin '{}@{}' is already installed",
                plugin.name,
                marketplace.name
            );
        }
        let entry = marketplace_plugin_entry(marketplace, plugin)?;
        let root = resolve_entry(&entry, config, cwd)?
            .context("marketplace plugin unexpectedly resolved as disabled")?;
        validate_marketplace_plugin_root(&root, plugin)?;
        state.installed.push(InstalledPlugin {
            name: plugin.name.clone(),
            marketplace: marketplace.name.clone(),
            entry,
            version: Some(plugin.version.clone()),
            description: Some(plugin.description.clone()),
            contributions: PluginContributions::default(),
        });
        let load = load_with_state(config, &state, mcp, lsp, cwd)?;
        cache_installed_status(
            state
                .installed
                .last_mut()
                .expect("installed plugin was just appended"),
            &load.catalog,
        );
        save_plugin_state(config, cwd, &state)?;
        Ok(PluginMutationResult {
            load,
            message: format!(
                "Installed `{}@{}` from {}.",
                plugin.name,
                marketplace.name,
                root.display()
            ),
        })
    }

    pub fn uninstall(
        config: &PluginsConfig,
        mcp: McpConfig,
        lsp: LspConfig,
        cwd: &Path,
        spec: &str,
    ) -> Result<PluginMutationResult> {
        let mut state = load_plugin_state(config, cwd)?;
        let index = find_installed_plugin(&state, spec)?;
        let plugin = state.installed.remove(index);
        let load = load_with_state(config, &state, mcp, lsp, cwd)?;
        save_plugin_state(config, cwd, &state)?;
        Ok(PluginMutationResult {
            load,
            message: format!("Uninstalled `{}@{}`.", plugin.name, plugin.marketplace),
        })
    }

    pub fn set_enabled(
        config: &PluginsConfig,
        mcp: McpConfig,
        lsp: LspConfig,
        cwd: &Path,
        spec: &str,
        enabled: bool,
    ) -> Result<PluginMutationResult> {
        let mut state = load_plugin_state(config, cwd)?;
        let index = find_installed_plugin(&state, spec)?;
        if !enabled && entry_enabled(&state.installed[index].entry) {
            let current = load_with_cached_sources(config, &state, mcp.clone(), lsp.clone(), cwd)?;
            cache_installed_status(&mut state.installed[index], &current.catalog);
        }
        set_entry_enabled(&mut state.installed[index].entry, enabled);
        let name = state.installed[index].name.clone();
        let marketplace = state.installed[index].marketplace.clone();
        let load = load_with_cached_sources(config, &state, mcp, lsp, cwd)?;
        if enabled {
            cache_installed_status(&mut state.installed[index], &load.catalog);
        }
        save_plugin_state(config, cwd, &state)?;
        Ok(PluginMutationResult {
            load,
            message: format!(
                "{} `{}@{}`.",
                if enabled { "Enabled" } else { "Disabled" },
                name,
                marketplace
            ),
        })
    }

    pub fn refresh(
        config: &PluginsConfig,
        mcp: McpConfig,
        lsp: LspConfig,
        cwd: &Path,
    ) -> Result<PluginMutationResult> {
        let state = load_plugin_state(config, cwd)?;
        Ok(PluginMutationResult {
            load: load_with_state(config, &state, mcp, lsp, cwd)?,
            message: "Reloaded plugins and refreshed marketplaces.".to_owned(),
        })
    }
}

fn load_with_state(
    config: &PluginsConfig,
    state: &PluginState,
    mcp: McpConfig,
    lsp: LspConfig,
    cwd: &Path,
) -> Result<PluginLoadResult> {
    load_with_source_mode(config, state, mcp, lsp, cwd, GitSourceMode::Refresh)
}

fn load_with_cached_sources(
    config: &PluginsConfig,
    state: &PluginState,
    mcp: McpConfig,
    lsp: LspConfig,
    cwd: &Path,
) -> Result<PluginLoadResult> {
    load_with_source_mode(config, state, mcp, lsp, cwd, GitSourceMode::Cached)
}

fn load_with_source_mode(
    config: &PluginsConfig,
    state: &PluginState,
    mut mcp: McpConfig,
    mut lsp: LspConfig,
    cwd: &Path,
    source_mode: GitSourceMode,
) -> Result<PluginLoadResult> {
    let mut catalog = ExtensionCatalog::default();
    let marketplaces = load_marketplaces_with_mode(config, state, cwd, source_mode)?;
    let installed = state
        .installed
        .iter()
        .map(|plugin| (plugin.marketplace.as_str(), plugin.name.as_str()))
        .collect::<BTreeSet<_>>();
    for marketplace in &marketplaces {
        catalog.marketplaces.push(ConfiguredMarketplace {
            name: marketplace.name.clone(),
            alias: marketplace.alias.clone(),
            source: marketplace.source.clone(),
            root: marketplace.root.clone(),
        });
        catalog
            .marketplace_plugins
            .extend(marketplace.plugins.iter().cloned().map(|mut plugin| {
                plugin.installed =
                    installed.contains(&(marketplace.name.as_str(), plugin.name.as_str()));
                plugin
            }));
    }
    for entry in &config.entries {
        let Some(root) = resolve_entry_with_mode(entry, config, cwd, source_mode)? else {
            continue;
        };
        load_plugin(&root, &mut catalog, &mut mcp, &mut lsp)?;
        let loaded = catalog
            .plugins
            .last()
            .context("plugin loader did not register the loaded plugin")?;
        catalog.installed_plugins.push(InstalledPluginStatus {
            name: loaded.name.clone(),
            version: loaded.version.clone(),
            description: loaded.description.clone(),
            marketplace: None,
            enabled: true,
            config_managed: true,
            root: Some(loaded.root.clone()),
            contributions: collect_plugin_contributions(&catalog, &mcp, &lsp, &loaded.name),
        });
    }
    for installed_plugin in &state.installed {
        let enabled = entry_enabled(&installed_plugin.entry);
        if enabled {
            let root = resolve_entry_with_mode(&installed_plugin.entry, config, cwd, source_mode)?
                .context("enabled installed plugin unexpectedly resolved as disabled")?;
            load_plugin(&root, &mut catalog, &mut mcp, &mut lsp)?;
            let loaded = catalog
                .plugins
                .last()
                .context("plugin loader did not register the installed plugin")?;
            catalog.installed_plugins.push(InstalledPluginStatus {
                name: loaded.name.clone(),
                version: loaded.version.clone(),
                description: loaded.description.clone(),
                marketplace: Some(installed_plugin.marketplace.clone()),
                enabled: true,
                config_managed: false,
                root: Some(loaded.root.clone()),
                contributions: collect_plugin_contributions(&catalog, &mcp, &lsp, &loaded.name),
            });
            continue;
        }

        let marketplace_plugin = catalog.marketplace_plugins.iter().find(|plugin| {
            plugin.name == installed_plugin.name
                && plugin.marketplace == installed_plugin.marketplace
        });
        let cached_plugin = cached_installed_plugin(installed_plugin, config, cwd).ok();
        catalog.installed_plugins.push(InstalledPluginStatus {
            name: installed_plugin.name.clone(),
            version: installed_plugin
                .version
                .clone()
                .or_else(|| {
                    cached_plugin
                        .as_ref()
                        .map(|(_, manifest)| manifest.version.clone())
                })
                .or_else(|| marketplace_plugin.map(|plugin| plugin.version.clone()))
                .unwrap_or_else(default_plugin_version),
            description: installed_plugin
                .description
                .clone()
                .or_else(|| {
                    cached_plugin
                        .as_ref()
                        .map(|(_, manifest)| manifest.description.clone())
                })
                .or_else(|| marketplace_plugin.map(|plugin| plugin.description.clone()))
                .unwrap_or_default(),
            marketplace: Some(installed_plugin.marketplace.clone()),
            enabled: false,
            config_managed: false,
            root: cached_plugin.as_ref().map(|(root, _)| root.clone()),
            contributions: installed_plugin.contributions.clone(),
        });
    }
    catalog
        .plugins
        .sort_by(|left, right| left.name.cmp(&right.name));
    catalog.installed_plugins.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.marketplace.cmp(&right.marketplace))
    });
    validate_plugin_dependencies(&catalog.plugins)?;
    catalog
        .commands
        .sort_by(|left, right| left.name.cmp(&right.name));
    mcp.validate()?;
    Ok(PluginLoadResult { catalog, mcp, lsp })
}

fn cache_installed_status(installed: &mut InstalledPlugin, catalog: &ExtensionCatalog) {
    let Some(status) = catalog.installed_plugins.iter().find(|status| {
        status.name == installed.name
            && status.marketplace.as_deref() == Some(installed.marketplace.as_str())
    }) else {
        return;
    };
    installed.version = Some(status.version.clone());
    installed.description = Some(status.description.clone());
    installed.contributions = status.contributions.clone();
}

fn cached_installed_plugin(
    installed: &InstalledPlugin,
    config: &PluginsConfig,
    cwd: &Path,
) -> Result<(PathBuf, PluginManifest)> {
    let mut entry = installed.entry.clone();
    set_entry_enabled(&mut entry, true);
    let root = resolve_entry_with_mode(&entry, config, cwd, GitSourceMode::Cached)?
        .context("installed plugin source is not cached")?;
    let manifest_path = plugin_manifest_path(&root)?;
    let manifest: PluginManifest = serde_json::from_str(
        &fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    if manifest.name != installed.name {
        bail!(
            "installed plugin '{}' resolved to manifest for '{}'",
            installed.name,
            manifest.name
        );
    }
    Ok((root, manifest))
}

fn collect_plugin_contributions(
    catalog: &ExtensionCatalog,
    mcp: &McpConfig,
    lsp: &LspConfig,
    plugin: &str,
) -> PluginContributions {
    let prefix = format!("{plugin}:");
    PluginContributions {
        commands: catalog
            .commands
            .iter()
            .filter(|command| command.plugin == plugin)
            .map(|command| format!("/{}", command.name))
            .collect(),
        skills: catalog
            .skills
            .iter()
            .filter(|skill| skill.plugin == plugin)
            .map(|skill| skill.name.clone())
            .collect(),
        agents: catalog
            .agents
            .iter()
            .filter(|agent| agent.plugin == plugin)
            .map(|agent| agent.name.clone())
            .collect(),
        hooks: catalog
            .hooks
            .iter()
            .filter(|hook| hook.plugin == plugin)
            .map(|hook| match hook.matcher.as_deref() {
                Some(matcher) => format!("{:?} ({matcher})", hook.event),
                None => format!("{:?}", hook.event),
            })
            .collect(),
        mcp_servers: mcp
            .servers
            .keys()
            .filter(|name| name.starts_with(&prefix))
            .cloned()
            .collect(),
        lsp_servers: lsp
            .servers
            .keys()
            .filter(|name| name.starts_with(&prefix))
            .cloned()
            .collect(),
        settings: catalog.settings.contains_key(plugin),
    }
}

impl ExtensionCatalog {
    pub fn agent_prompt(&self, name: &str, task: &str) -> Result<String> {
        let agent = self
            .agents
            .iter()
            .find(|agent| agent.name == name)
            .with_context(|| format!("unknown plugin agent '{name}'"))?;
        Ok(format!(
            "Use the `{name}` plugin agent definition for this task.\n\n<plugin-agent-definition>\n{}\n</plugin-agent-definition>\n\n<task>\n{}\n</task>",
            agent.body, task
        ))
    }

    pub fn system_prompt_fragment(&self) -> String {
        let mut lines = Vec::new();
        if !self.skills.is_empty() {
            lines.push(
                "Available plugin skills (read the referenced SKILL.md before using one):"
                    .to_owned(),
            );
            lines.extend(self.skills.iter().map(|skill| {
                format!(
                    "- {} [{}]: {} ({})",
                    skill.name,
                    skill.plugin,
                    skill.description,
                    skill.path.display()
                )
            }));
        }
        if !self.agents.is_empty() {
            lines.push(
                "Available plugin agent definitions (select one with the Subagent tool's agent field):"
                    .to_owned(),
            );
            lines.extend(self.agents.iter().map(|agent| {
                format!(
                    "- {} [{}]: {} ({})",
                    agent.name,
                    agent.plugin,
                    agent.description,
                    agent.path.display()
                )
            }));
        }
        lines.join("\n")
    }

    pub fn plugin_status(&self) -> String {
        if self.plugins.is_empty() && self.marketplace_plugins.is_empty() {
            return "No plugins loaded.".to_owned();
        }
        let mut sections = self
            .plugins
            .iter()
            .map(|plugin| {
                format!(
                    "{} {} — {}\n  {}",
                    plugin.name,
                    plugin.version,
                    plugin.description,
                    plugin.root.display()
                )
            })
            .collect::<Vec<_>>();
        if !self.commands.is_empty() {
            sections.push(format!(
                "Commands:\n{}",
                self.commands
                    .iter()
                    .map(|command| format!(
                        "  /{} [{}] — {}",
                        command.name, command.plugin, command.description
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        if !self.marketplace_plugins.is_empty() {
            sections.push(format!(
                "Available from marketplaces:\n{}",
                self.marketplace_plugins
                    .iter()
                    .map(|plugin| format!(
                        "  {}{} {} — {} ({}; {})",
                        if plugin.installed { "[installed] " } else { "" },
                        plugin.name,
                        plugin.version,
                        plugin.description,
                        plugin.marketplace,
                        plugin.source.label()
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        sections.join("\n")
    }
}

impl MarketplacePluginSource {
    pub fn label(&self) -> String {
        match self {
            Self::Relative(path) => path.clone(),
            Self::Detailed(MarketplaceSourceSpec::Github { repo, .. }) => {
                format!("github:{repo}")
            }
            Self::Detailed(MarketplaceSourceSpec::Url { url, .. }) => url.clone(),
            Self::Detailed(MarketplaceSourceSpec::GitSubdir { url, path, .. }) => {
                format!("{url}#{path}")
            }
            Self::Detailed(MarketplaceSourceSpec::Npm {
                package, version, ..
            }) => format!("npm:{package}@{}", version.as_deref().unwrap_or("latest")),
        }
    }
}

fn load_marketplaces(
    config: &PluginsConfig,
    state: &PluginState,
    cwd: &Path,
) -> Result<Vec<LoadedMarketplace>> {
    load_marketplaces_with_mode(config, state, cwd, GitSourceMode::Refresh)
}

fn load_marketplaces_with_mode(
    config: &PluginsConfig,
    state: &PluginState,
    cwd: &Path,
    source_mode: GitSourceMode,
) -> Result<Vec<LoadedMarketplace>> {
    let mut seen_sources = BTreeSet::new();
    let mut marketplaces: Vec<LoadedMarketplace> = Vec::new();
    for source in config.marketplaces.iter().chain(&state.marketplaces) {
        if !seen_sources.insert(source.as_str()) {
            continue;
        }
        let marketplace = load_marketplace_with_mode(source, config, cwd, source_mode)?;
        if let Some(existing) = marketplaces
            .iter()
            .find(|existing| existing.name == marketplace.name)
        {
            bail!(
                "duplicate marketplace name '{}' from '{}' and '{}'",
                marketplace.name,
                existing.source,
                marketplace.source
            );
        }
        marketplaces.push(marketplace);
    }
    marketplaces.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(marketplaces)
}

fn load_marketplace(source: &str, config: &PluginsConfig, cwd: &Path) -> Result<LoadedMarketplace> {
    load_marketplace_with_mode(source, config, cwd, GitSourceMode::Refresh)
}

fn load_marketplace_with_mode(
    source: &str,
    config: &PluginsConfig,
    cwd: &Path,
    source_mode: GitSourceMode,
) -> Result<LoadedMarketplace> {
    let local = resolve_user_path(Path::new(source), cwd);
    let (content, root) = if local.exists() {
        let manifest_path = marketplace_manifest_path(&local)?;
        let root = marketplace_root(&manifest_path)?;
        (
            fs::read_to_string(&manifest_path).with_context(|| {
                format!(
                    "failed to read plugin marketplace {}",
                    manifest_path.display()
                )
            })?,
            Some(root),
        )
    } else if is_remote_marketplace_file(source) {
        report_progress(format!("http: downloading marketplace {source}"));
        let content = ureq::get(source)
            .call()
            .with_context(|| format!("failed to download plugin marketplace '{source}'"))?
            .into_string()
            .with_context(|| format!("failed to read plugin marketplace '{source}'"))?;
        (content, None)
    } else {
        let (repository_source, git_ref) = split_marketplace_git_ref(source);
        let repository =
            resolve_source_with_mode(repository_source, git_ref, None, config, cwd, source_mode)?;
        let manifest_path = marketplace_manifest_path(&repository)?;
        (
            fs::read_to_string(&manifest_path).with_context(|| {
                format!(
                    "failed to read plugin marketplace {}",
                    manifest_path.display()
                )
            })?,
            Some(marketplace_root(&manifest_path)?),
        )
    };
    let manifest: MarketplaceManifest = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse plugin marketplace '{source}'"))?;
    let (name, mut plugins) = match manifest {
        MarketplaceManifest::Catalog { name, plugins } => (
            name.unwrap_or_else(|| marketplace_name_from_source(source)),
            plugins,
        ),
        MarketplaceManifest::List(plugins) => (marketplace_name_from_source(source), plugins),
    };
    validate_plugin_name(&name)?;
    for plugin in &mut plugins {
        validate_plugin_name(&plugin.name)?;
        plugin.marketplace = name.clone();
        plugin.installed = false;
    }
    Ok(LoadedMarketplace {
        name,
        alias: marketplace_alias_from_source(source),
        source: source.to_owned(),
        root,
        plugins,
    })
}

fn resolve_entry(
    entry: &PluginEntryConfig,
    config: &PluginsConfig,
    cwd: &Path,
) -> Result<Option<PathBuf>> {
    resolve_entry_with_mode(entry, config, cwd, GitSourceMode::Refresh)
}

fn resolve_entry_with_mode(
    entry: &PluginEntryConfig,
    config: &PluginsConfig,
    cwd: &Path,
    source_mode: GitSourceMode,
) -> Result<Option<PathBuf>> {
    match entry {
        PluginEntryConfig::Source(source) => {
            resolve_source_with_mode(source, None, None, config, cwd, source_mode).map(Some)
        }
        PluginEntryConfig::Detailed {
            source,
            enabled,
            git_ref,
            subdir,
        } => {
            if !enabled {
                return Ok(None);
            }
            resolve_source_with_mode(
                source,
                git_ref.as_deref(),
                subdir.as_deref(),
                config,
                cwd,
                source_mode,
            )
            .map(Some)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GitSourceMode {
    Refresh,
    Cached,
}

fn resolve_source_with_mode(
    source: &str,
    git_ref: Option<&str>,
    subdir: Option<&str>,
    config: &PluginsConfig,
    cwd: &Path,
    source_mode: GitSourceMode,
) -> Result<PathBuf> {
    if let Some(subdir) = subdir {
        validate_subdir(subdir)?;
    }
    let local = resolve_user_path(Path::new(source), cwd);
    if local.exists() {
        let root = local
            .canonicalize()
            .with_context(|| format!("failed to resolve plugin path {}", local.display()))?;
        return resolve_subdir(&root, subdir);
    }
    let repository_source = git_repository_url(source)
        .with_context(|| format!("plugin source '{source}' does not exist"))?;

    let cache_root = config
        .cache_dir
        .clone()
        .map(|path| resolve_user_path(&path, cwd))
        .unwrap_or_else(default_plugin_cache_dir);
    fs::create_dir_all(&cache_root)
        .with_context(|| format!("failed to create plugin cache {}", cache_root.display()))?;
    let cache_identity = format!(
        "{}#{}#{}",
        repository_source,
        git_ref.unwrap_or("HEAD"),
        subdir.unwrap_or("")
    );
    let cache_name = format!(
        "{}-{:016x}",
        sanitize_name(source).chars().take(80).collect::<String>(),
        stable_hash(&cache_identity)
    );
    let destination = cache_root.join(cache_name);
    let existed = destination.exists();
    if source_mode == GitSourceMode::Cached {
        if !existed {
            bail!(
                "plugin source '{source}' is not cached; reinstall or refresh it before enabling"
            );
        }
        let root = destination
            .canonicalize()
            .context("failed to resolve cached plugin path")?;
        return resolve_subdir(&root, subdir);
    }
    if existed {
        run_git(
            Command::new("git").args([
                "-C",
                destination.to_string_lossy().as_ref(),
                "fetch",
                "--all",
                "--prune",
                "--progress",
            ]),
            &format!("update plugin source '{source}'"),
        )?;
    } else {
        let mut clone = Command::new("git");
        clone.args(["clone", "--progress"]);
        if subdir.is_some() {
            clone.args(["--filter=blob:none", "--sparse"]);
        }
        clone.args([
            "--",
            repository_source.as_str(),
            destination.to_string_lossy().as_ref(),
        ]);
        run_git(&mut clone, &format!("clone plugin source '{source}'"))?;
    }
    if let Some(subdir) = subdir {
        run_git(
            Command::new("git").args([
                "-C",
                destination.to_string_lossy().as_ref(),
                "sparse-checkout",
                "init",
                "--cone",
            ]),
            &format!("initialize sparse checkout for '{subdir}'"),
        )?;
        run_git(
            Command::new("git").args([
                "-C",
                destination.to_string_lossy().as_ref(),
                "sparse-checkout",
                "set",
                "--",
                subdir,
            ]),
            &format!("select plugin subdirectory '{subdir}'"),
        )?;
    }
    if let Some(git_ref) = git_ref {
        if git_ref.starts_with('-') {
            bail!("plugin revision must not start with '-'");
        }
        let remote_ref = format!("refs/remotes/origin/{git_ref}");
        let remote_exists = Command::new("git")
            .args([
                "-C",
                destination.to_string_lossy().as_ref(),
                "rev-parse",
                "--verify",
                "--quiet",
                &remote_ref,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .context("failed to resolve plugin revision")?
            .status
            .success();
        let target = if remote_exists {
            remote_ref.as_str()
        } else {
            git_ref
        };
        run_git(
            Command::new("git").args([
                "-C",
                destination.to_string_lossy().as_ref(),
                "checkout",
                "--detach",
                "--force",
                target,
            ]),
            &format!("checkout plugin revision '{git_ref}'"),
        )?;
    } else if existed {
        fast_forward_default_branch(&destination, source)?;
    }
    let root = destination
        .canonicalize()
        .context("failed to resolve cached plugin path")?;
    resolve_subdir(&root, subdir)
}

fn load_plugin_state(config: &PluginsConfig, cwd: &Path) -> Result<PluginState> {
    let path = plugin_state_path(config, cwd);
    if !path.exists() {
        return Ok(PluginState::default());
    }
    serde_json::from_str(
        &fs::read_to_string(&path)
            .with_context(|| format!("failed to read plugin state {}", path.display()))?,
    )
    .with_context(|| format!("failed to parse plugin state {}", path.display()))
}

fn save_plugin_state(config: &PluginsConfig, cwd: &Path, state: &PluginState) -> Result<()> {
    let path = plugin_state_path(config, cwd);
    let parent = path
        .parent()
        .context("plugin state path did not have a parent")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create plugin state directory {}",
            parent.display()
        )
    })?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let content =
        serde_json::to_string_pretty(state).context("failed to serialize plugin state")?;
    fs::write(&temporary, format!("{content}\n"))
        .with_context(|| format!("failed to write plugin state {}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .with_context(|| format!("failed to replace plugin state {}", path.display()))
}

fn plugin_state_path(config: &PluginsConfig, cwd: &Path) -> PathBuf {
    config
        .cache_dir
        .clone()
        .map(|path| resolve_user_path(&path, cwd).join("state.json"))
        .unwrap_or_else(default_plugin_state_path)
}

fn marketplace_manifest_path(path: &Path) -> Result<PathBuf> {
    if path.is_file() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to resolve marketplace {}", path.display()));
    }
    [
        path.join(".glint-plugin/marketplace.json"),
        path.join(".claude-plugin/marketplace.json"),
        path.join("marketplace.json"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
    .with_context(|| format!("{} has no supported marketplace manifest", path.display()))?
    .canonicalize()
    .context("failed to resolve marketplace manifest")
}

fn marketplace_root(manifest_path: &Path) -> Result<PathBuf> {
    let parent = manifest_path
        .parent()
        .context("marketplace manifest did not have a parent")?;
    let root = if matches!(
        parent.file_name().and_then(|name| name.to_str()),
        Some(".claude-plugin" | ".glint-plugin")
    ) {
        parent
            .parent()
            .context("marketplace metadata directory did not have a parent")?
    } else {
        parent
    };
    root.canonicalize()
        .context("failed to resolve marketplace root")
}

fn marketplace_name_from_source(source: &str) -> String {
    let source = source.split('#').next().unwrap_or(source);
    let source = source.trim_end_matches('/');
    let last = source.rsplit('/').next().unwrap_or("marketplace");
    let last = last.strip_suffix(".json").unwrap_or(last);
    let last = last.strip_suffix(".git").unwrap_or(last);
    let name = sanitize_name(last).trim_matches('-').to_owned();
    if name.is_empty() {
        "marketplace".to_owned()
    } else {
        name
    }
}

fn marketplace_alias_from_source(source: &str) -> String {
    let (source, _) = split_marketplace_git_ref(source);
    if !source.contains("://")
        && !source.starts_with("git@")
        && let Some((owner, repository)) = source.split_once('/')
        && !owner.is_empty()
        && !repository.is_empty()
        && !repository.contains('/')
    {
        return sanitize_name(&format!(
            "{}-{}",
            owner,
            repository.strip_suffix(".git").unwrap_or(repository)
        ))
        .trim_matches('-')
        .to_owned();
    }
    marketplace_name_from_source(source)
}

fn split_marketplace_git_ref(source: &str) -> (&str, Option<&str>) {
    match source.rsplit_once('#') {
        Some((repository, git_ref)) if !repository.is_empty() && !git_ref.is_empty() => {
            (repository, Some(git_ref))
        }
        _ => (source, None),
    }
}

fn is_remote_marketplace_file(source: &str) -> bool {
    let path = source.split(['?', '#']).next().unwrap_or(source);
    (source.starts_with("https://") || source.starts_with("http://")) && path.ends_with(".json")
}

fn find_marketplace_plugin<'a>(
    marketplaces: &'a [LoadedMarketplace],
    spec: &str,
) -> Result<(&'a LoadedMarketplace, &'a MarketplacePlugin)> {
    let (name, marketplace_name) = split_plugin_spec(spec)?;
    let matches = marketplaces
        .iter()
        .flat_map(|marketplace| {
            marketplace
                .plugins
                .iter()
                .map(move |plugin| (marketplace, plugin))
        })
        .filter(|(marketplace, plugin)| {
            plugin.name == name
                && marketplace_name.is_none_or(|expected| {
                    marketplace.name == expected || marketplace.alias == expected
                })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => bail!("plugin '{spec}' was not found in configured marketplaces"),
        [found] => Ok(*found),
        _ => bail!("plugin '{name}' exists in multiple marketplaces; use name@marketplace"),
    }
}

fn find_installed_plugin(state: &PluginState, spec: &str) -> Result<usize> {
    let (name, marketplace) = split_plugin_spec(spec)?;
    let matches = state
        .installed
        .iter()
        .enumerate()
        .filter(|(_, plugin)| {
            plugin.name == name && marketplace.is_none_or(|expected| plugin.marketplace == expected)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => bail!("plugin '{spec}' is not installed"),
        [index] => Ok(*index),
        _ => bail!("plugin '{name}' is installed from multiple marketplaces; use name@marketplace"),
    }
}

fn split_plugin_spec(spec: &str) -> Result<(&str, Option<&str>)> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("plugin name must not be empty");
    }
    let (name, marketplace) = match spec.rsplit_once('@') {
        Some((name, marketplace)) if !name.is_empty() && !marketplace.is_empty() => {
            (name, Some(marketplace))
        }
        Some(_) => bail!("plugin spec must be name or name@marketplace"),
        None => (spec, None),
    };
    validate_plugin_name(name)?;
    if let Some(marketplace) = marketplace {
        validate_plugin_name(marketplace)?;
    }
    Ok((name, marketplace))
}

fn marketplace_plugin_entry(
    marketplace: &LoadedMarketplace,
    plugin: &MarketplacePlugin,
) -> Result<PluginEntryConfig> {
    let (source, git_ref, subdir) = match &plugin.source {
        MarketplacePluginSource::Relative(path) => {
            let root = marketplace.root.as_ref().with_context(|| {
                format!(
                    "marketplace '{}' was loaded from a remote JSON file, so relative plugin source '{}' cannot be resolved",
                    marketplace.name, path
                )
            })?;
            let subdir = path.strip_prefix("./").unwrap_or(path);
            validate_subdir(subdir)?;
            (
                root.to_string_lossy().into_owned(),
                None,
                Some(subdir.to_owned()),
            )
        }
        MarketplacePluginSource::Detailed(MarketplaceSourceSpec::Github { repo, git_ref, sha }) => {
            (
                format!("https://github.com/{repo}.git"),
                sha.clone().or_else(|| git_ref.clone()),
                None,
            )
        }
        MarketplacePluginSource::Detailed(MarketplaceSourceSpec::Url { url, git_ref, sha }) => {
            (url.clone(), sha.clone().or_else(|| git_ref.clone()), None)
        }
        MarketplacePluginSource::Detailed(MarketplaceSourceSpec::GitSubdir {
            url,
            path,
            git_ref,
            sha,
        }) => (
            url.clone(),
            sha.clone().or_else(|| git_ref.clone()),
            Some(path.clone()),
        ),
        MarketplacePluginSource::Detailed(MarketplaceSourceSpec::Npm {
            package, registry, ..
        }) => {
            let registry = registry
                .as_deref()
                .map(|registry| format!(" from {registry}"))
                .unwrap_or_default();
            bail!(
                "npm marketplace source '{package}'{registry} is not supported; use a git or git-subdir source"
            );
        }
    };
    Ok(PluginEntryConfig::Detailed {
        source,
        enabled: true,
        git_ref,
        subdir,
    })
}

fn validate_marketplace_plugin_root(root: &Path, plugin: &MarketplacePlugin) -> Result<()> {
    let manifest_path = plugin_manifest_path(root)?;
    let manifest: PluginManifest = serde_json::from_str(
        &fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    validate_plugin_name(&manifest.name)?;
    if manifest.name != plugin.name {
        bail!(
            "marketplace plugin '{}' resolved to manifest for '{}'",
            plugin.name,
            manifest.name
        );
    }
    Ok(())
}

fn set_entry_enabled(entry: &mut PluginEntryConfig, enabled: bool) {
    match entry {
        PluginEntryConfig::Source(source) => {
            *entry = PluginEntryConfig::Detailed {
                source: std::mem::take(source),
                enabled,
                git_ref: None,
                subdir: None,
            };
        }
        PluginEntryConfig::Detailed {
            enabled: current, ..
        } => *current = enabled,
    }
}

fn entry_enabled(entry: &PluginEntryConfig) -> bool {
    match entry {
        PluginEntryConfig::Source(_) => true,
        PluginEntryConfig::Detailed { enabled, .. } => *enabled,
    }
}

fn git_repository_url(source: &str) -> Option<String> {
    if source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("ssh://")
        || source.starts_with("git@")
        || source.starts_with("file://")
    {
        return Some(source.to_owned());
    }
    let mut parts = source.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(repository), None)
            if !owner.is_empty() && !repository.is_empty() && owner != "." && owner != ".." =>
        {
            Some(format!("https://github.com/{owner}/{repository}.git"))
        }
        _ => None,
    }
}

fn validate_subdir(subdir: &str) -> Result<()> {
    let path = Path::new(subdir);
    if subdir.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("plugin subdir must be a non-empty relative path without '..'");
    }
    Ok(())
}

fn resolve_subdir(root: &Path, subdir: Option<&str>) -> Result<PathBuf> {
    let Some(subdir) = subdir else {
        return Ok(root.to_path_buf());
    };
    let joined = root.join(subdir);
    let resolved = joined
        .canonicalize()
        .with_context(|| format!("plugin subdirectory {} does not exist", joined.display()))?;
    if !resolved.starts_with(root) {
        bail!("plugin subdirectory '{subdir}' escapes the repository root");
    }
    Ok(resolved)
}

fn fast_forward_default_branch(destination: &Path, source: &str) -> Result<()> {
    let output = run_git(
        Command::new("git").args([
            "-C",
            destination.to_string_lossy().as_ref(),
            "symbolic-ref",
            "--short",
            "refs/remotes/origin/HEAD",
        ]),
        &format!("resolve the default branch for plugin source '{source}'"),
    )?;
    let remote_branch =
        String::from_utf8(output.stdout).context("plugin default branch is not valid UTF-8")?;
    let branch = remote_branch
        .trim()
        .strip_prefix("origin/")
        .context("plugin default branch is not an origin branch")?;
    run_git(
        Command::new("git").args([
            "-C",
            destination.to_string_lossy().as_ref(),
            "checkout",
            "--force",
            branch,
        ]),
        &format!("select the default branch for plugin source '{source}'"),
    )?;
    run_git(
        Command::new("git").args([
            "-C",
            destination.to_string_lossy().as_ref(),
            "pull",
            "--progress",
            "--ff-only",
            "origin",
            branch,
        ]),
        &format!("fast-forward plugin source '{source}'"),
    )?;
    Ok(())
}

fn run_git(command: &mut Command, action: &str) -> Result<Output> {
    report_progress(format!("git: {action}"));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = if progress_reporter_active() {
        run_git_streaming(command, action)?
    } else {
        command
            .output()
            .with_context(|| format!("failed to start Git while trying to {action}"))?
    };
    if output.status.success() {
        report_progress(format!("git: completed {action}"));
        return Ok(output);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if details.is_empty() {
        bail!("Git failed while trying to {action} ({})", output.status);
    }
    bail!("Git failed while trying to {action}: {details}")
}

fn run_git_streaming(command: &mut Command, action: &str) -> Result<Output> {
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start Git while trying to {action}"))?;
    let stdout = child.stdout.take().context("Git stdout was not captured")?;
    let stderr = child.stderr.take().context("Git stderr was not captured")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_pending = String::new();
    let mut stderr_pending = String::new();

    std::thread::scope(|scope| {
        let stdout_sender = sender.clone();
        scope.spawn(move || read_git_stream(stdout, false, stdout_sender));
        let stderr_sender = sender.clone();
        scope.spawn(move || read_git_stream(stderr, true, stderr_sender));
        drop(sender);
        for (is_stderr, chunk) in receiver {
            if is_stderr {
                stderr_bytes.extend_from_slice(&chunk);
                report_git_chunk(&mut stderr_pending, &chunk);
            } else {
                stdout_bytes.extend_from_slice(&chunk);
                report_git_chunk(&mut stdout_pending, &chunk);
            }
        }
    });
    flush_git_progress(&mut stdout_pending);
    flush_git_progress(&mut stderr_pending);
    let status = child
        .wait()
        .with_context(|| format!("failed to wait for Git while trying to {action}"))?;
    Ok(Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}

fn read_git_stream(
    mut stream: impl Read,
    is_stderr: bool,
    sender: std::sync::mpsc::Sender<(bool, Vec<u8>)>,
) {
    let mut buffer = [0_u8; 1024];
    while let Ok(count) = stream.read(&mut buffer) {
        if count == 0 || sender.send((is_stderr, buffer[..count].to_vec())).is_err() {
            break;
        }
    }
}

fn report_git_chunk(pending: &mut String, chunk: &[u8]) {
    pending.push_str(&String::from_utf8_lossy(chunk));
    while let Some(index) = pending.find(['\r', '\n']) {
        let line = pending[..index].trim().to_owned();
        pending.drain(..=index);
        if !line.is_empty() {
            report_progress(line);
        }
    }
}

fn flush_git_progress(pending: &mut String) {
    let line = std::mem::take(pending);
    let line = line.trim();
    if !line.is_empty() {
        report_progress(line.to_owned());
    }
}

fn report_progress(message: String) {
    PROGRESS_REPORTER.with(|reporter| {
        if let Some(reporter) = reporter.borrow().as_ref() {
            reporter(message);
        }
    });
}

fn progress_reporter_active() -> bool {
    PROGRESS_REPORTER.with(|reporter| reporter.borrow().is_some())
}

fn load_plugin(
    root: &Path,
    catalog: &mut ExtensionCatalog,
    mcp: &mut McpConfig,
    lsp: &mut LspConfig,
) -> Result<()> {
    let manifest_path = plugin_manifest_path(root)?;
    let mut manifest: PluginManifest = serde_json::from_str(
        &fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    apply_convention_defaults(root, &mut manifest);
    validate_plugin_name(&manifest.name)?;
    if catalog
        .plugins
        .iter()
        .any(|plugin| plugin.name == manifest.name)
    {
        bail!("duplicate plugin name '{}'", manifest.name);
    }

    load_documents(
        root,
        &manifest.name,
        manifest.commands.as_ref(),
        false,
        |document| {
            if catalog
                .commands
                .iter()
                .any(|command| command.name == document.name)
            {
                bail!("duplicate plugin command '/{}'", document.name);
            }
            catalog.commands.push(PluginCommand {
                name: document.name,
                description: document.description,
                prompt: document.body,
                plugin: manifest.name.clone(),
            });
            Ok(())
        },
    )?;
    load_documents(
        root,
        &manifest.name,
        manifest.skills.as_ref(),
        true,
        |document| {
            catalog
                .skills
                .push(document.into_plugin_document(&manifest.name));
            Ok(())
        },
    )?;
    load_documents(
        root,
        &manifest.name,
        manifest.agents.as_ref(),
        false,
        |document| {
            catalog
                .agents
                .push(document.into_plugin_document(&manifest.name));
            Ok(())
        },
    )?;
    load_settings(root, &manifest, catalog)?;
    load_hooks(root, &manifest, catalog)?;
    load_mcp_servers(root, &manifest, mcp)?;
    load_lsp_servers(root, &manifest, lsp)?;

    catalog.plugins.push(LoadedPlugin {
        name: manifest.name,
        version: manifest.version,
        description: manifest.description,
        root: root.to_path_buf(),
        dependencies: manifest.dependencies,
    });
    Ok(())
}

fn plugin_manifest_path(root: &Path) -> Result<PathBuf> {
    [
        root.join(".glint-plugin/plugin.json"),
        root.join(".claude-plugin/plugin.json"),
        root.join(".codex-plugin/plugin.json"),
        root.join("plugin.json"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .with_context(|| format!("plugin {} has no supported plugin manifest", root.display()))
}

fn validate_plugin_dependencies(plugins: &[LoadedPlugin]) -> Result<()> {
    let loaded = plugins
        .iter()
        .map(|plugin| plugin.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for plugin in plugins {
        for dependency in &plugin.dependencies {
            if !loaded.contains(dependency.as_str()) {
                bail!(
                    "plugin '{}' requires missing plugin '{}'",
                    plugin.name,
                    dependency
                );
            }
        }
    }
    Ok(())
}

fn apply_convention_defaults(root: &Path, manifest: &mut PluginManifest) {
    set_default_resource(root, &mut manifest.commands, "commands");
    set_default_resource(root, &mut manifest.skills, "skills");
    set_default_resource(root, &mut manifest.agents, "agents");
    set_default_resource(root, &mut manifest.hooks, "hooks/hooks.json");
    set_default_resource(root, &mut manifest.mcp_servers, ".mcp.json");
    set_default_resource(root, &mut manifest.lsp_servers, ".lsp.json");
    set_default_resource(root, &mut manifest.settings, "settings/settings.json");
}

fn set_default_resource(root: &Path, slot: &mut Option<ResourcePaths>, relative: &str) {
    if slot.is_none() && root.join(relative).exists() {
        *slot = Some(ResourcePaths::One(relative.to_owned()));
    }
}

struct ParsedDocument {
    name: String,
    description: String,
    body: String,
    path: PathBuf,
}

impl ParsedDocument {
    fn into_plugin_document(self, plugin: &str) -> PluginDocument {
        PluginDocument {
            name: self.name,
            description: self.description,
            path: self.path,
            plugin: plugin.to_owned(),
            body: self.body,
        }
    }
}

fn load_documents(
    root: &Path,
    plugin: &str,
    paths: Option<&ResourcePaths>,
    skills: bool,
    mut consume: impl FnMut(ParsedDocument) -> Result<()>,
) -> Result<()> {
    let Some(paths) = paths else {
        return Ok(());
    };
    for resource in paths.values() {
        let path = resolve_resource(root, resource)?;
        let files = if path.is_dir() {
            collect_markdown_files(&path, skills)?
        } else {
            vec![path]
        };
        for file in files {
            let content = fs::read_to_string(&file)
                .with_context(|| format!("failed to read plugin document {}", file.display()))?;
            let (description, body) = parse_frontmatter(&content)?;
            let stem = if skills {
                file.parent().and_then(Path::file_name)
            } else {
                file.file_stem()
            }
            .and_then(|name| name.to_str())
            .context("plugin document name is not valid UTF-8")?;
            consume(ParsedDocument {
                name: format!("{plugin}:{stem}"),
                description: description.unwrap_or_else(|| format!("Provided by plugin {plugin}")),
                body,
                path: file,
            })?;
        }
    }
    Ok(())
}

fn collect_markdown_files(path: &Path, skills: bool) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let candidate = if skills && entry.path().is_dir() {
            entry.path().join("SKILL.md")
        } else {
            entry.path()
        };
        if candidate.is_file()
            && candidate.extension().and_then(|value| value.to_str()) == Some("md")
        {
            files.push(candidate);
        }
    }
    files.sort();
    Ok(files)
}

fn parse_frontmatter(content: &str) -> Result<(Option<String>, String)> {
    let Some(rest) = content.strip_prefix("---\n") else {
        return Ok((None, content.trim().to_owned()));
    };
    let Some((header, body)) = rest.split_once("\n---\n") else {
        bail!("plugin markdown has an unterminated YAML frontmatter block");
    };
    #[derive(Deserialize)]
    struct Frontmatter {
        description: Option<String>,
    }
    let frontmatter: Frontmatter = serde_yaml::from_str(header)?;
    Ok((frontmatter.description, body.trim().to_owned()))
}

fn load_hooks(
    root: &Path,
    manifest: &PluginManifest,
    catalog: &mut ExtensionCatalog,
) -> Result<()> {
    let Some(paths) = manifest.hooks.as_ref() else {
        return Ok(());
    };
    for resource in paths.values() {
        let path = resolve_resource(root, resource)?;
        let value: Value = serde_json::from_str(&fs::read_to_string(&path)?)
            .with_context(|| format!("failed to parse hooks {}", path.display()))?;
        let mut hooks = parse_hooks(value)?;
        for hook in &mut hooks {
            hook.plugin = manifest.name.clone();
            hook.root = Some(root.to_path_buf());
            hook.settings = catalog.settings.get(&manifest.name).cloned();
        }
        catalog.hooks.extend(hooks);
    }
    Ok(())
}

fn parse_hooks(value: Value) -> Result<Vec<PluginHook>> {
    if value.is_array() {
        return serde_json::from_value(value).context("failed to parse plugin hook list");
    }
    #[derive(Deserialize)]
    struct HookGroup {
        matcher: Option<String>,
        hooks: Vec<CommandHook>,
    }
    #[derive(Deserialize)]
    struct CommandHook {
        #[serde(rename = "type")]
        kind: String,
        command: String,
        timeout: Option<u64>,
        timeout_ms: Option<u64>,
    }
    let groups: BTreeMap<String, Vec<HookGroup>> = serde_json::from_value(value)
        .context("plugin hooks must be an array or an event-to-hook mapping")?;
    let mut hooks = Vec::new();
    for (event, groups) in groups {
        let event = hook_event_name(&event)?;
        for group in groups {
            for hook in group.hooks {
                if hook.kind != "command" {
                    bail!("unsupported plugin hook type '{}'", hook.kind);
                }
                hooks.push(PluginHook {
                    event,
                    command: hook.command,
                    matcher: group.matcher.clone(),
                    timeout_ms: hook
                        .timeout_ms
                        .or_else(|| hook.timeout.map(|seconds| seconds.saturating_mul(1_000)))
                        .unwrap_or_else(default_hook_timeout_ms),
                    plugin: String::new(),
                    root: None,
                    settings: None,
                });
            }
        }
    }
    Ok(hooks)
}

fn hook_event_name(name: &str) -> Result<HookEvent> {
    match name {
        "SessionStart" | "session_start" => Ok(HookEvent::SessionStart),
        "SessionEnd" | "session_end" => Ok(HookEvent::SessionEnd),
        "UserPromptSubmit" | "PromptSubmit" | "prompt_submit" => Ok(HookEvent::PromptSubmit),
        "BeforeModelCall" | "before_model_call" => Ok(HookEvent::BeforeModelCall),
        "AfterModelCall" | "after_model_call" => Ok(HookEvent::AfterModelCall),
        "PreToolUse" | "BeforeToolCall" | "before_tool_call" => Ok(HookEvent::BeforeToolCall),
        "PostToolUse" | "AfterToolCall" | "after_tool_call" => Ok(HookEvent::AfterToolCall),
        "PreCompact" | "BeforeCompact" | "before_compact" => Ok(HookEvent::BeforeCompact),
        "AfterCompact" | "after_compact" => Ok(HookEvent::AfterCompact),
        "SubagentStart" | "AgentStart" | "agent_start" => Ok(HookEvent::AgentStart),
        "SubagentStop" | "Stop" | "AgentEnd" | "agent_end" => Ok(HookEvent::AgentEnd),
        _ => bail!("unknown plugin hook event '{name}'"),
    }
}

fn load_mcp_servers(root: &Path, manifest: &PluginManifest, mcp: &mut McpConfig) -> Result<()> {
    let Some(paths) = manifest.mcp_servers.as_ref() else {
        return Ok(());
    };
    for resource in paths.values() {
        let path = resolve_resource(root, resource)?;
        let value: Value = serde_json::from_str(&fs::read_to_string(&path)?)
            .with_context(|| format!("failed to parse MCP config {}", path.display()))?;
        let servers = value.get("mcpServers").unwrap_or(&value);
        let servers = servers
            .as_object()
            .context("plugin MCP config must be an object")?;
        for (name, server) in servers {
            let qualified = format!("{}:{}", manifest.name, name);
            if mcp.servers.contains_key(&qualified) {
                bail!("duplicate MCP server '{qualified}'");
            }
            mcp.servers
                .insert(qualified, parse_plugin_mcp_server(server, root)?);
        }
    }
    Ok(())
}

fn parse_plugin_mcp_server(value: &Value, root: &Path) -> Result<McpServerConfig> {
    #[derive(Deserialize)]
    struct RawServer {
        command: Option<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        env_vars: Vec<String>,
        cwd: Option<String>,
        url: Option<String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        bearer_token_env: Option<String>,
        oauth: Option<McpOAuthConfig>,
        #[serde(default = "enabled_by_default")]
        enabled: bool,
        #[serde(default = "default_startup_timeout_ms")]
        startup_timeout_ms: u64,
        #[serde(default = "default_tool_timeout_ms")]
        tool_timeout_ms: u64,
        #[serde(default)]
        approval: McpApprovalPolicy,
        #[serde(default)]
        tool_approval: BTreeMap<String, McpApprovalPolicy>,
        #[serde(default)]
        enabled_tools: Option<Vec<String>>,
        #[serde(default)]
        disabled_tools: Vec<String>,
    }
    let raw: RawServer = serde_json::from_value(value.clone())?;
    let transport = match (raw.command, raw.url) {
        (Some(command), None) => McpTransportConfig::Stdio {
            command,
            args: raw.args,
            env: raw.env,
            env_vars: raw.env_vars,
            cwd: Some(
                raw.cwd
                    .map(PathBuf::from)
                    .map(|cwd| {
                        if cwd.is_absolute() {
                            cwd
                        } else {
                            root.join(cwd)
                        }
                    })
                    .unwrap_or_else(|| root.to_path_buf())
                    .to_string_lossy()
                    .into_owned(),
            ),
        },
        (None, Some(url)) => McpTransportConfig::StreamableHttp {
            url,
            headers: raw.headers,
            bearer_token_env: raw.bearer_token_env,
            oauth: raw.oauth,
        },
        _ => bail!("plugin MCP server must define exactly one of command or url"),
    };
    Ok(McpServerConfig {
        enabled: raw.enabled,
        startup_timeout_ms: raw.startup_timeout_ms,
        tool_timeout_ms: raw.tool_timeout_ms,
        approval: raw.approval,
        tool_approval: raw.tool_approval,
        enabled_tools: raw.enabled_tools,
        disabled_tools: raw.disabled_tools,
        transport,
    })
}

fn load_lsp_servers(root: &Path, manifest: &PluginManifest, lsp: &mut LspConfig) -> Result<()> {
    let Some(paths) = manifest.lsp_servers.as_ref() else {
        return Ok(());
    };
    for resource in paths.values() {
        let path = resolve_resource(root, resource)?;
        let servers: BTreeMap<String, LspServerConfig> =
            serde_json::from_str(&fs::read_to_string(&path)?)
                .with_context(|| format!("failed to parse LSP config {}", path.display()))?;
        for (name, server) in servers {
            let qualified = format!("{}:{}", manifest.name, name);
            if lsp.servers.insert(qualified.clone(), server).is_some() {
                bail!("duplicate LSP server '{qualified}'");
            }
        }
    }
    Ok(())
}

fn load_settings(
    root: &Path,
    manifest: &PluginManifest,
    catalog: &mut ExtensionCatalog,
) -> Result<()> {
    let Some(paths) = manifest.settings.as_ref() else {
        return Ok(());
    };
    for resource in paths.values() {
        let path = resolve_resource(root, resource)?;
        let value = serde_json::from_str(&fs::read_to_string(&path)?)
            .with_context(|| format!("failed to parse plugin settings {}", path.display()))?;
        catalog.settings.insert(manifest.name.clone(), value);
    }
    Ok(())
}

fn resolve_resource(root: &Path, resource: &str) -> Result<PathBuf> {
    let joined = root.join(resource);
    let canonical = joined
        .canonicalize()
        .with_context(|| format!("plugin resource {} does not exist", joined.display()))?;
    if !canonical.starts_with(root) {
        bail!("plugin resource '{}' escapes the plugin root", resource);
    }
    Ok(canonical)
}

fn validate_plugin_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        bail!("invalid plugin name '{name}'");
    }
    Ok(())
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn stable_hash(value: &str) -> u64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
}

fn resolve_user_path(path: &Path, cwd: &Path) -> PathBuf {
    if let Some(value) = path.to_str()
        && let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
    {
        if value == "~" {
            return home;
        }
        if let Some(relative) = value.strip_prefix("~/") {
            return home.join(relative);
        }
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn default_plugin_cache_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".glint/plugins/cache")
}

fn default_plugin_state_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".glint/plugins/state.json")
}

fn enabled_by_default() -> bool {
    true
}

fn default_plugin_version() -> String {
    "0.0.0".to_owned()
}

fn default_hook_timeout_ms() -> u64 {
    10_000
}

fn default_startup_timeout_ms() -> u64 {
    20_000
}

fn default_tool_timeout_ms() -> u64 {
    60_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_git_failure_output() {
        let mut command = Command::new("git");
        command.arg("glint-command-that-does-not-exist");
        let error = run_git(&mut command, "run the regression check").unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("Git failed while trying to run the regression check"));
        assert!(message.contains("glint-command-that-does-not-exist"));
    }

    #[test]
    fn loads_all_declared_plugin_contributions() {
        let root = test_dir("full-plugin");
        fs::create_dir_all(root.join(".glint-plugin")).unwrap();
        fs::create_dir_all(root.join("commands")).unwrap();
        fs::create_dir_all(root.join("skills/explain")).unwrap();
        fs::create_dir_all(root.join("agents")).unwrap();
        fs::write(
            root.join(".glint-plugin/plugin.json"),
            r#"{
                "name":"demo",
                "version":"1.2.3",
                "description":"Demo plugin",
                "commands":"./commands",
                "skills":"./skills",
                "agents":"./agents",
                "hooks":"./hooks.json",
                "mcpServers":"./mcp.json",
                "lspServers":"./lsp.json",
                "settings":"./settings.json"
            }"#,
        )
        .unwrap();
        fs::write(
            root.join("commands/review.md"),
            "---\ndescription: Review the workspace\n---\nReview all current changes.",
        )
        .unwrap();
        fs::write(
            root.join("skills/explain/SKILL.md"),
            "---\ndescription: Explain code\n---\nRead the requested code.",
        )
        .unwrap();
        fs::write(
            root.join("agents/researcher.md"),
            "---\ndescription: Research code\n---\nInvestigate the repository.",
        )
        .unwrap();
        fs::write(root.join("hooks.json"), "[]").unwrap();
        fs::write(
            root.join("mcp.json"),
            r#"{"echo":{"command":"echo-server","args":["--stdio"]}}"#,
        )
        .unwrap();
        fs::write(root.join("lsp.json"), "{}").unwrap();
        fs::write(root.join("settings.json"), r#"{"theme":"dark"}"#).unwrap();

        let result = PluginManager::load(
            &PluginsConfig {
                entries: vec![PluginEntryConfig::Source(root.display().to_string())],
                cache_dir: Some(root.join(".cache")),
                ..Default::default()
            },
            McpConfig::default(),
            LspConfig::default(),
            &root,
        )
        .unwrap();

        assert_eq!(result.catalog.plugins[0].name, "demo");
        assert_eq!(result.catalog.installed_plugins.len(), 1);
        assert!(result.catalog.installed_plugins[0].config_managed);
        assert!(result.catalog.installed_plugins[0].enabled);
        assert_eq!(
            result.catalog.installed_plugins[0].contributions.commands,
            ["/demo:review"]
        );
        assert_eq!(
            result.catalog.installed_plugins[0].contributions.skills,
            ["demo:explain"]
        );
        assert_eq!(
            result.catalog.installed_plugins[0]
                .contributions
                .mcp_servers,
            ["demo:echo"]
        );
        assert_eq!(result.catalog.commands[0].name, "demo:review");
        assert_eq!(result.catalog.skills[0].name, "demo:explain");
        assert_eq!(result.catalog.agents[0].name, "demo:researcher");
        let agent_prompt = result
            .catalog
            .agent_prompt("demo:researcher", "Find the parser")
            .unwrap();
        assert!(agent_prompt.contains("Investigate the repository."));
        assert!(agent_prompt.contains("Find the parser"));
        assert!(result.catalog.settings.contains_key("demo"));
        let server = &result.mcp.servers["demo:echo"];
        assert!(matches!(server.transport, McpTransportConfig::Stdio { .. }));
        if let McpTransportConfig::Stdio { cwd, .. } = &server.transport {
            assert_eq!(cwd.as_deref(), Some(root.to_string_lossy().as_ref()));
        }

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn installs_relative_marketplace_plugin_and_persists_lifecycle() {
        let root = test_dir("marketplace-install");
        let marketplace = root.join("marketplace");
        let plugin = marketplace.join("plugins/demo");
        write_test_plugin(&plugin, "demo");
        fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{
                "name":"demo",
                "version":"1.0.0",
                "description":"Description from plugin manifest"
            }"#,
        )
        .unwrap();
        fs::create_dir_all(marketplace.join(".claude-plugin")).unwrap();
        fs::write(
            marketplace.join(".claude-plugin/marketplace.json"),
            r#"{
                "name":"demo-market",
                "plugins":[{
                    "name":"demo",
                    "description":"Demo marketplace plugin",
                    "source":"./plugins/demo"
                }]
            }"#,
        )
        .unwrap();
        let config = PluginsConfig {
            cache_dir: Some(root.join("cache")),
            ..Default::default()
        };

        let added = PluginManager::add_marketplace(
            &config,
            McpConfig::default(),
            LspConfig::default(),
            &root,
            marketplace.to_string_lossy().as_ref(),
        )
        .unwrap();
        assert_eq!(added.load.catalog.marketplace_plugins.len(), 1);
        assert!(!added.load.catalog.marketplace_plugins[0].installed);

        let installed = PluginManager::install(
            &config,
            McpConfig::default(),
            LspConfig::default(),
            &root,
            "demo@demo-market",
        )
        .unwrap();
        assert_eq!(installed.load.catalog.plugins[0].name, "demo");
        assert!(installed.load.catalog.marketplace_plugins[0].installed);
        assert_eq!(installed.load.catalog.marketplaces[0].name, "demo-market");
        assert!(installed.load.catalog.installed_plugins[0].enabled);
        assert_eq!(installed.load.catalog.installed_plugins[0].version, "1.0.0");
        assert_eq!(
            installed.load.catalog.installed_plugins[0].description,
            "Description from plugin manifest"
        );
        assert_eq!(
            installed.load.catalog.installed_plugins[0]
                .contributions
                .skills,
            ["demo:demo"]
        );

        let reloaded =
            PluginManager::load(&config, McpConfig::default(), LspConfig::default(), &root)
                .unwrap();
        assert_eq!(reloaded.catalog.plugins[0].name, "demo");

        let disabled = PluginManager::set_enabled(
            &config,
            McpConfig::default(),
            LspConfig::default(),
            &root,
            "demo@demo-market",
            false,
        )
        .unwrap();
        assert!(disabled.load.catalog.plugins.is_empty());
        assert!(disabled.load.catalog.marketplace_plugins[0].installed);
        assert!(!disabled.load.catalog.installed_plugins[0].enabled);
        assert_eq!(disabled.load.catalog.installed_plugins[0].version, "1.0.0");
        assert_eq!(
            disabled.load.catalog.installed_plugins[0].description,
            "Description from plugin manifest"
        );
        assert_eq!(
            disabled.load.catalog.installed_plugins[0].root.as_deref(),
            Some(plugin.canonicalize().unwrap().as_path())
        );
        assert_eq!(
            disabled.load.catalog.installed_plugins[0]
                .contributions
                .skills,
            ["demo:demo"]
        );

        let reloaded_disabled =
            PluginManager::load(&config, McpConfig::default(), LspConfig::default(), &root)
                .unwrap();
        assert!(!reloaded_disabled.catalog.installed_plugins[0].enabled);
        assert_eq!(
            reloaded_disabled.catalog.installed_plugins[0].version,
            "1.0.0"
        );
        assert_eq!(
            reloaded_disabled.catalog.installed_plugins[0].description,
            "Description from plugin manifest"
        );

        let uninstalled = PluginManager::uninstall(
            &config,
            McpConfig::default(),
            LspConfig::default(),
            &root,
            "demo@demo-market",
        )
        .unwrap();
        assert!(uninstalled.load.catalog.plugins.is_empty());
        assert!(!uninstalled.load.catalog.marketplace_plugins[0].installed);

        let removed = PluginManager::remove_marketplace(
            &config,
            McpConfig::default(),
            LspConfig::default(),
            &root,
            "demo-market",
        )
        .unwrap();
        assert!(removed.load.catalog.marketplace_plugins.is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn disabled_legacy_install_uses_cached_manifest_metadata() {
        let root = test_dir("legacy-disabled-metadata");
        let marketplace = root.join("marketplace");
        let plugin = marketplace.join("plugins/demo");
        write_test_plugin(&plugin, "demo");
        fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{
                "name":"demo",
                "version":"1.2.3",
                "description":"Description from cached manifest"
            }"#,
        )
        .unwrap();
        fs::create_dir_all(marketplace.join(".claude-plugin")).unwrap();
        fs::write(
            marketplace.join(".claude-plugin/marketplace.json"),
            r#"{
                "name":"demo-market",
                "plugins":[{
                    "name":"demo",
                    "description":"Different marketplace description",
                    "source":"./plugins/demo"
                }]
            }"#,
        )
        .unwrap();
        let config = PluginsConfig {
            marketplaces: vec![marketplace.to_string_lossy().into_owned()],
            cache_dir: Some(root.join("cache")),
            ..Default::default()
        };
        let state = PluginState {
            installed: vec![InstalledPlugin {
                name: "demo".to_owned(),
                marketplace: "demo-market".to_owned(),
                entry: PluginEntryConfig::Detailed {
                    source: marketplace.to_string_lossy().into_owned(),
                    enabled: false,
                    git_ref: None,
                    subdir: Some("plugins/demo".to_owned()),
                },
                version: None,
                description: None,
                contributions: PluginContributions::default(),
            }],
            ..Default::default()
        };
        save_plugin_state(&config, &root, &state).unwrap();

        let loaded =
            PluginManager::load(&config, McpConfig::default(), LspConfig::default(), &root)
                .unwrap();

        assert!(!loaded.catalog.installed_plugins[0].enabled);
        assert_eq!(loaded.catalog.installed_plugins[0].version, "1.2.3");
        assert_eq!(
            loaded.catalog.installed_plugins[0].description,
            "Description from cached manifest"
        );
        assert_eq!(
            loaded.catalog.installed_plugins[0].root.as_deref(),
            Some(plugin.canonicalize().unwrap().as_path())
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn installs_git_subdir_plugin_from_marketplace() {
        let root = test_dir("git-subdir-install");
        let repository = root.join("monorepo");
        write_test_plugin(&repository.join("packages/demo"), "demo");
        init_git_repository(&repository);
        let marketplace = root.join("marketplace.json");
        fs::write(
            &marketplace,
            format!(
                r#"{{
                    "name":"git-market",
                    "plugins":[{{
                        "name":"demo",
                        "version":"1.0.0",
                        "source":{{
                            "source":"git-subdir",
                            "url":"file://{}",
                            "path":"packages/demo"
                        }}
                    }}]
                }}"#,
                repository.display()
            ),
        )
        .unwrap();
        let config = PluginsConfig {
            marketplaces: vec![marketplace.to_string_lossy().into_owned()],
            cache_dir: Some(root.join("cache")),
            ..Default::default()
        };

        let installed = PluginManager::install(
            &config,
            McpConfig::default(),
            LspConfig::default(),
            &root,
            "demo@git-market",
        )
        .unwrap();
        let loaded = &installed.load.catalog.plugins[0];
        assert_eq!(loaded.name, "demo");
        assert!(loaded.root.ends_with("packages/demo"));
        let git_file = loaded
            .root
            .ancestors()
            .find(|path| path.join(".git").exists())
            .unwrap();
        let sparse = fs::read_to_string(git_file.join(".git/info/sparse-checkout")).unwrap();
        assert!(sparse.contains("/packages/demo/"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn adds_git_marketplace_and_installs_relative_monorepo_plugin() {
        let root = test_dir("git-marketplace-install");
        let repository = root.join("marketplace-repo");
        write_test_plugin(&repository.join("plugins/demo"), "demo");
        fs::create_dir_all(repository.join(".claude-plugin")).unwrap();
        fs::write(
            repository.join(".claude-plugin/marketplace.json"),
            r#"{
                "name":"demo-market",
                "plugins":[{"name":"demo","source":"./plugins/demo"}]
            }"#,
        )
        .unwrap();
        init_git_repository(&repository);
        let config = PluginsConfig {
            cache_dir: Some(root.join("cache")),
            ..Default::default()
        };
        let source = format!("file://{}", repository.display());

        let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&progress);
        PluginManager::with_progress(
            Arc::new(move |message| captured.lock().unwrap().push(message)),
            || {
                PluginManager::add_marketplace(
                    &config,
                    McpConfig::default(),
                    LspConfig::default(),
                    &root,
                    &source,
                )
            },
        )
        .unwrap();
        assert!(
            progress
                .lock()
                .unwrap()
                .iter()
                .any(|message| message.starts_with("git: clone plugin source"))
        );
        let installed = PluginManager::install(
            &config,
            McpConfig::default(),
            LspConfig::default(),
            &root,
            "demo@demo-market",
        )
        .unwrap();

        assert_eq!(installed.load.catalog.plugins[0].name, "demo");
        assert!(
            installed.load.catalog.plugins[0]
                .root
                .ends_with("plugins/demo")
        );

        let toggle_progress = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&toggle_progress);
        let disabled = PluginManager::with_progress(
            Arc::new(move |message| captured.lock().unwrap().push(message)),
            || {
                PluginManager::set_enabled(
                    &config,
                    McpConfig::default(),
                    LspConfig::default(),
                    &root,
                    "demo@demo-market",
                    false,
                )
            },
        )
        .unwrap();
        assert!(!disabled.load.catalog.installed_plugins[0].enabled);
        assert!(
            toggle_progress
                .lock()
                .unwrap()
                .iter()
                .all(|message| !message.starts_with("git:"))
        );
        toggle_progress.lock().unwrap().clear();
        let captured = Arc::clone(&toggle_progress);
        let enabled = PluginManager::with_progress(
            Arc::new(move |message| captured.lock().unwrap().push(message)),
            || {
                PluginManager::set_enabled(
                    &config,
                    McpConfig::default(),
                    LspConfig::default(),
                    &root,
                    "demo@demo-market",
                    true,
                )
            },
        )
        .unwrap();
        assert!(enabled.load.catalog.installed_plugins[0].enabled);
        assert!(
            toggle_progress
                .lock()
                .unwrap()
                .iter()
                .all(|message| !message.starts_with("git:"))
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recognizes_github_marketplace_shorthand_and_alias() {
        assert_eq!(
            git_repository_url("anthropics/claude-code").as_deref(),
            Some("https://github.com/anthropics/claude-code.git")
        );
        assert_eq!(
            marketplace_alias_from_source("anthropics/claude-code"),
            "anthropics-claude-code"
        );
        assert_eq!(
            split_marketplace_git_ref("anthropics/claude-code#v1"),
            ("anthropics/claude-code", Some("v1"))
        );
    }

    #[test]
    fn rejects_resources_that_escape_plugin_root() {
        let parent = test_dir("escape-plugin");
        let root = parent.join("plugin");
        fs::create_dir_all(root.join(".glint-plugin")).unwrap();
        fs::write(parent.join("outside.md"), "outside").unwrap();
        fs::write(
            root.join(".glint-plugin/plugin.json"),
            r#"{"name":"escape","version":"1","commands":"../outside.md"}"#,
        )
        .unwrap();
        let error = PluginManager::load(
            &PluginsConfig {
                entries: vec![PluginEntryConfig::Source(root.display().to_string())],
                cache_dir: Some(parent.join(".cache")),
                ..Default::default()
            },
            McpConfig::default(),
            LspConfig::default(),
            &parent,
        )
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("escapes the plugin root"));
        fs::remove_dir_all(parent).ok();
    }

    fn test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("glint-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn write_test_plugin(root: &Path, name: &str) {
        fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        fs::create_dir_all(root.join("skills/demo")).unwrap();
        fs::write(
            root.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
        fs::write(
            root.join("skills/demo/SKILL.md"),
            "---\ndescription: Demo skill\n---\nUse the demo skill.",
        )
        .unwrap();
    }

    fn init_git_repository(root: &Path) {
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=Glint Test",
                "-c",
                "user.email=glint@example.invalid",
                "commit",
                "-m",
                "test plugin",
            ])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
    }
}
