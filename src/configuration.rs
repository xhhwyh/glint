#[cfg(test)]
pub use crate::chatgpt::ReasoningLevel;
pub use crate::chatgpt::ReasoningOptions;
use std::{
    collections::HashSet,
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    config::{
        CHATGPT_PROVIDER_ID, CHATGPT_PROVIDER_NAME, ChatGptConfig, Config, CustomProviderConfig,
        DEFAULT_SYSTEM_PROMPT, LlmConfig, LlmProviderConfig, ModelCatalog, ModelCatalogEntry,
        ProviderCatalogEntry, RuntimeExtensions, UserConfig, UserLlmConfig,
    },
    credentials::{CredentialId, CredentialStore, CredentialStoreStatus, open_credential_store},
    paths::GlintPaths,
    persistence::{UserConfigRepository, UserConfigStore},
    plugins::PluginManager,
    provider_catalog::{ModelMetadata, PromptCacheConfig, ProviderCatalog},
    services::mcp::{McpServerConfig, upsert_mcp_server_value},
};

const DEFAULT_TEMPERATURE: f32 = 0.7;
const DEFAULT_MAX_TOKENS: u32 = 8196;

#[derive(Clone, Debug, PartialEq)]
pub struct AvailableProvider {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub base_url: String,
    pub models: Vec<AvailableModel>,
    pub prompt_cache: PromptCacheConfig,
    pub custom: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AvailableModel {
    pub name: String,
    pub metadata: ModelMetadata,
}

pub struct ModelRuntimeConfig {
    pub llm: LlmConfig,
    pub model_catalog: ModelCatalog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationMutationErrorKind {
    ProviderNameRequired,
    ProviderNameCollision,
    InvalidBaseUrl,
    ModelRequired,
    DuplicateModel,
    CredentialRequired,
    CredentialUnavailable,
    Persistence,
    ProviderNotConfigured,
    InvalidProvider,
}

impl ConfigurationMutationErrorKind {
    pub fn user_message(self) -> &'static str {
        match self {
            Self::ProviderNameRequired => "Enter a provider name.",
            Self::ProviderNameCollision => {
                "Choose a unique provider name; it matches an existing provider."
            }
            Self::InvalidBaseUrl => "Enter a valid HTTP or HTTPS base URL.",
            Self::ModelRequired => "Enter at least one model name.",
            Self::DuplicateModel => "Model names must be unique.",
            Self::CredentialRequired => "Enter an API key for this provider.",
            Self::CredentialUnavailable => {
                "Credential storage is unavailable. Re-enter the API key to repair it, or try again when the system credential store is available."
            }
            Self::Persistence => {
                "Could not write Glint configuration. Check that ~/.glint is writable and try again."
            }
            Self::ProviderNotConfigured => {
                "That provider is no longer configured. Return to the provider list and try again."
            }
            Self::InvalidProvider => {
                "That provider is not available. Return to the provider list and try again."
            }
        }
    }
}

pub struct ConfigurationMutationError {
    kind: ConfigurationMutationErrorKind,
    source: Option<anyhow::Error>,
}

impl ConfigurationMutationError {
    fn new(kind: ConfigurationMutationErrorKind) -> Self {
        Self { kind, source: None }
    }

    fn with_source(kind: ConfigurationMutationErrorKind, source: anyhow::Error) -> Self {
        Self {
            kind,
            source: Some(source),
        }
    }

    pub fn kind(&self) -> ConfigurationMutationErrorKind {
        self.kind
    }

    pub fn user_message(&self) -> &'static str {
        self.kind.user_message()
    }
}

impl fmt::Debug for ConfigurationMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigurationMutationError")
            .field("kind", &self.kind)
            .field("source", &self.source.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl fmt::Display for ConfigurationMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.user_message())
    }
}

impl Error for ConfigurationMutationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(|source| source.as_ref())
    }
}

pub type ConfigurationMutationResult<T> = std::result::Result<T, ConfigurationMutationError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderStatus {
    pub id: String,
    pub display_name: String,
    pub builtin: bool,
    pub configured: bool,
    pub needs_credential: bool,
    pub model_count: usize,
}

pub struct ConfigurationManager {
    paths: GlintPaths,
    workspace: PathBuf,
    catalog: ProviderCatalog,
    repository: Box<dyn UserConfigRepository>,
    credentials: Box<dyn CredentialStore>,
    user: UserConfig,
}

impl ConfigurationManager {
    pub fn discover(paths: GlintPaths, workspace: &Path) -> Result<Self> {
        let repository: Box<dyn UserConfigRepository> =
            Box::new(UserConfigStore::new(paths.clone()));
        let user = repository.load()?.unwrap_or_default();
        let catalog = ProviderCatalog::embedded()?;
        validate_loaded_user(&user, &catalog).with_context(|| {
            format!(
                "invalid Glint configuration at {}",
                repository.path().display()
            )
        })?;
        let has_configured_providers =
            !user.configured_providers.is_empty() || !user.custom_providers.is_empty();
        let credentials = open_credential_store(&paths, has_configured_providers)?;
        Ok(Self {
            paths,
            workspace: workspace.to_path_buf(),
            catalog,
            repository,
            credentials,
            user,
        })
    }

    pub fn new(
        paths: GlintPaths,
        workspace: PathBuf,
        catalog: ProviderCatalog,
        repository: Box<dyn UserConfigRepository>,
        credentials: Box<dyn CredentialStore>,
    ) -> Result<Self> {
        let user = repository.load()?.unwrap_or_default();
        validate_loaded_user(&user, &catalog).with_context(|| {
            format!(
                "invalid Glint configuration at {}",
                repository.path().display()
            )
        })?;
        Ok(Self {
            paths,
            workspace,
            catalog,
            repository,
            credentials,
            user,
        })
    }

    pub fn paths(&self) -> &GlintPaths {
        &self.paths
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn user_config(&self) -> &UserConfig {
        &self.user
    }

    pub fn credential_store_status(&self) -> CredentialStoreStatus {
        self.credentials.status()
    }

    pub fn available_providers(&self) -> Result<Vec<AvailableProvider>> {
        available_providers(&self.catalog, &self.user, self.credentials.as_ref())
    }

    pub fn provider_statuses(&self) -> Result<Vec<ProviderStatus>> {
        let mut statuses = Vec::new();
        for provider in self.catalog.providers() {
            let configured = self
                .user
                .configured_providers
                .iter()
                .any(|id| id == &provider.id);
            let needs_credential = configured
                && !credential_is_present(
                    self.credentials.as_ref(),
                    &CredentialId::builtin(&provider.id),
                )?;
            statuses.push(ProviderStatus {
                id: provider.id.clone(),
                display_name: provider.name.clone(),
                builtin: true,
                configured,
                needs_credential,
                model_count: provider.models.len(),
            });
        }

        let mut custom = self.user.custom_providers.iter().collect::<Vec<_>>();
        custom.sort_by(|(left, _), (right, _)| compare_custom_provider_names(left, right));
        for (name, provider) in custom {
            statuses.push(ProviderStatus {
                id: name.clone(),
                display_name: name.clone(),
                builtin: false,
                configured: true,
                needs_credential: !credential_is_present(
                    self.credentials.as_ref(),
                    &CredentialId::custom(name),
                )?,
                model_count: provider.models.len(),
            });
        }
        Ok(statuses)
    }

    pub fn repair_selection(&mut self) -> Result<()> {
        let mut staged = self.user.clone();
        if !repair_selection_in(&self.catalog, self.credentials.as_ref(), &mut staged)? {
            return Ok(());
        }
        self.repository.save(&staged)?;
        self.user = staged;
        Ok(())
    }

    pub fn build_runtime(&self) -> Result<Config> {
        let model_runtime = self.build_model_runtime()?;
        let runtime_extensions = RuntimeExtensions::from_user_config(&self.user)?;
        let base_lsp = runtime_extensions.lsp;
        let base_mcp = runtime_extensions.mcp;
        let plugins = runtime_extensions.plugins;
        let plugin_result = PluginManager::load(
            &plugins,
            base_mcp.clone(),
            base_lsp.clone(),
            self.paths.root(),
        )?;
        let base_system_prompt = DEFAULT_SYSTEM_PROMPT.to_owned();
        let extension_prompt = plugin_result.catalog.system_prompt_fragment();
        let system_prompt = if extension_prompt.is_empty() {
            base_system_prompt.clone()
        } else {
            format!("{base_system_prompt}\n\n{extension_prompt}")
        };
        Ok(Config {
            config_path: self.paths.config(),
            llm: model_runtime.llm,
            lsp: plugin_result.lsp,
            mcp: plugin_result.mcp,
            extensions: plugin_result.catalog,
            model_catalog: model_runtime.model_catalog,
            system_prompt,
            plugins,
            base_lsp,
            base_mcp,
            base_system_prompt,
        })
    }

    pub fn upsert_mcp_server(&mut self, name: &str, server: &McpServerConfig) -> Result<()> {
        let mut staged = self.repository.load()?.unwrap_or_default();
        validate_loaded_user(&staged, &self.catalog).with_context(|| {
            format!(
                "invalid Glint configuration at {}",
                self.repository.path().display()
            )
        })?;
        staged.mcp = Some(upsert_mcp_server_value(staged.mcp.as_ref(), name, server)?);
        self.repository.save(&staged)?;
        self.user = staged;
        Ok(())
    }

    pub fn build_model_runtime(&self) -> Result<ModelRuntimeConfig> {
        let providers = self.available_providers()?;
        let selection =
            self.user.llm.as_ref().context(
                "no model is configured; run `glint` in an interactive terminal to add one",
            )?;
        let provider = providers
            .iter()
            .find(|provider| provider.id == selection.provider)
            .with_context(|| {
                format!(
                    "configured provider '{}' is unavailable",
                    selection.provider
                )
            })?;
        if !provider
            .models
            .iter()
            .any(|model| model.name == selection.model)
        {
            bail!(
                "configured model '{}' is unavailable for provider '{}'",
                selection.model,
                selection.provider
            );
        }
        let api_key = provider_api_key(self.credentials.as_ref(), provider)?;
        let model_catalog = runtime_model_catalog(&self.catalog, &providers);
        let mut llm = llm_from_available(
            &self.user,
            providers,
            &selection.provider,
            &selection.model,
            api_key,
        )?;
        llm.reasoning_effort =
            self.saved_model_reasoning_effort(&selection.provider, &selection.model);
        Ok(ModelRuntimeConfig { llm, model_catalog })
    }

    pub fn save_chatgpt(
        &mut self,
        models: Vec<String>,
        default_model: Option<String>,
    ) -> ConfigurationMutationResult<()> {
        validate_chatgpt_models(&models)?;
        let mut staged = self.user.clone();
        let reasoning_efforts = staged
            .chatgpt
            .as_ref()
            .map(|config| {
                config
                    .reasoning_efforts
                    .iter()
                    .filter(|(model, effort)| {
                        models.contains(model)
                            && self
                                .reasoning_options(model)
                                .levels
                                .iter()
                                .any(|level| &level.effort == *effort)
                    })
                    .map(|(model, effort)| (model.clone(), effort.clone()))
                    .collect()
            })
            .unwrap_or_default();
        staged.chatgpt = Some(ChatGptConfig {
            models,
            reasoning_efforts,
        });
        let selection_valid = staged.llm.as_ref().is_some_and(|selection| {
            selection.provider != CHATGPT_PROVIDER_ID
                || staged
                    .chatgpt
                    .as_ref()
                    .is_some_and(|config| config.models.contains(&selection.model))
        });
        if !selection_valid {
            let config = staged.chatgpt.as_ref().expect("staged above");
            let model = default_model
                .filter(|model| config.models.contains(model))
                .unwrap_or_else(|| config.models[0].clone());
            let (temperature, max_tokens) = staged
                .llm
                .as_ref()
                .map(|selection| (selection.temperature, selection.max_tokens))
                .unwrap_or((DEFAULT_TEMPERATURE, DEFAULT_MAX_TOKENS));
            staged.llm = Some(UserLlmConfig {
                provider: CHATGPT_PROVIDER_ID.into(),
                model,
                temperature,
                max_tokens,
            });
        }
        self.persist_chatgpt_change(staged)
    }

    fn persist_chatgpt_change(
        &mut self,
        mut staged: UserConfig,
    ) -> ConfigurationMutationResult<()> {
        repair_selection_in(&self.catalog, self.credentials.as_ref(), &mut staged).map_err(
            |error| {
                ConfigurationMutationError::with_source(
                    ConfigurationMutationErrorKind::CredentialUnavailable,
                    error,
                )
            },
        )?;
        self.repository.save(&staged).map_err(|error| {
            ConfigurationMutationError::with_source(
                ConfigurationMutationErrorKind::Persistence,
                error,
            )
        })?;
        self.user = staged;
        Ok(())
    }

    pub fn save_builtin(
        &mut self,
        provider_id: &str,
        api_key: Option<&str>,
    ) -> ConfigurationMutationResult<()> {
        let provider = self.catalog.builtin(provider_id).ok_or_else(|| {
            ConfigurationMutationError::new(ConfigurationMutationErrorKind::InvalidProvider)
        })?;
        let provider_id = provider.id.clone();
        let credential_id = CredentialId::builtin(&provider_id);
        let old_credential = self.credentials.get(&credential_id).map_err(|error| {
            ConfigurationMutationError::with_source(
                ConfigurationMutationErrorKind::CredentialUnavailable,
                error,
            )
        })?;
        let new_credential = nonblank_key(api_key);
        if new_credential.is_none() && !has_nonblank_credential(old_credential.as_deref()) {
            return Err(ConfigurationMutationError::new(
                ConfigurationMutationErrorKind::CredentialRequired,
            ));
        }

        let mut staged = self.user.clone();
        if !staged
            .configured_providers
            .iter()
            .any(|configured| configured == &provider_id)
        {
            staged.configured_providers.push(provider_id);
        }
        self.persist_provider_change(staged, &credential_id, old_credential, new_credential)
    }

    pub fn save_custom(
        &mut self,
        name: &str,
        base_url: &str,
        api_key: Option<&str>,
        models: Vec<String>,
    ) -> ConfigurationMutationResult<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ConfigurationMutationError::new(
                ConfigurationMutationErrorKind::ProviderNameRequired,
            ));
        }
        if reserved_chatgpt_name(name)
            || self.catalog.providers().iter().any(|provider| {
                provider.id.eq_ignore_ascii_case(name) || provider.name.eq_ignore_ascii_case(name)
            })
        {
            return Err(ConfigurationMutationError::new(
                ConfigurationMutationErrorKind::ProviderNameCollision,
            ));
        }
        if self
            .user
            .custom_providers
            .keys()
            .any(|existing| existing != name && existing.eq_ignore_ascii_case(name))
        {
            return Err(ConfigurationMutationError::new(
                ConfigurationMutationErrorKind::ProviderNameCollision,
            ));
        }

        let base_url = validate_base_url(base_url)?;
        let models = normalize_models(models)?;
        let credential_id = CredentialId::custom(name);
        let old_credential = self.credentials.get(&credential_id).map_err(|error| {
            ConfigurationMutationError::with_source(
                ConfigurationMutationErrorKind::CredentialUnavailable,
                error,
            )
        })?;
        let new_credential = nonblank_key(api_key);
        if new_credential.is_none() && !has_nonblank_credential(old_credential.as_deref()) {
            return Err(ConfigurationMutationError::new(
                ConfigurationMutationErrorKind::CredentialRequired,
            ));
        }

        let mut staged = self.user.clone();
        staged
            .custom_providers
            .insert(name.to_owned(), CustomProviderConfig { base_url, models });
        self.persist_provider_change(staged, &credential_id, old_credential, new_credential)
    }

    pub fn delete_provider(&mut self, provider_id: &str) -> ConfigurationMutationResult<()> {
        if provider_id == CHATGPT_PROVIDER_ID {
            let mut staged = self.user.clone();
            if staged.chatgpt.take().is_none() {
                return Err(ConfigurationMutationError::new(
                    ConfigurationMutationErrorKind::ProviderNotConfigured,
                ));
            }
            return self.persist_chatgpt_change(staged);
        }
        let (credential_id, mut staged) = if self.catalog.builtin(provider_id).is_some() {
            let mut staged = self.user.clone();
            let original_len = staged.configured_providers.len();
            staged
                .configured_providers
                .retain(|configured| configured != provider_id);
            if original_len == staged.configured_providers.len() {
                return Err(ConfigurationMutationError::new(
                    ConfigurationMutationErrorKind::ProviderNotConfigured,
                ));
            }
            (CredentialId::builtin(provider_id), staged)
        } else if self.user.custom_providers.contains_key(provider_id) {
            let mut staged = self.user.clone();
            staged.custom_providers.remove(provider_id);
            (CredentialId::custom(provider_id), staged)
        } else {
            return Err(ConfigurationMutationError::new(
                ConfigurationMutationErrorKind::ProviderNotConfigured,
            ));
        };

        staged.reasoning_efforts.remove(provider_id);
        let old_credential = self.credentials.get(&credential_id).map_err(|error| {
            ConfigurationMutationError::with_source(
                ConfigurationMutationErrorKind::CredentialUnavailable,
                error,
            )
        })?;
        self.credentials.delete(&credential_id).map_err(|error| {
            ConfigurationMutationError::with_source(
                ConfigurationMutationErrorKind::CredentialUnavailable,
                error,
            )
        })?;
        if let Err(error) =
            repair_selection_in(&self.catalog, self.credentials.as_ref(), &mut staged)
        {
            return Err(rollback_credential_mutation_error(
                self.credentials.as_ref(),
                &credential_id,
                old_credential.as_deref(),
                ConfigurationMutationError::with_source(
                    ConfigurationMutationErrorKind::CredentialUnavailable,
                    error,
                ),
            ));
        }
        if let Err(error) = self.repository.save(&staged) {
            return Err(rollback_credential_mutation_error(
                self.credentials.as_ref(),
                &credential_id,
                old_credential.as_deref(),
                ConfigurationMutationError::with_source(
                    ConfigurationMutationErrorKind::Persistence,
                    error,
                ),
            ));
        }
        self.user = staged;
        Ok(())
    }

    pub fn model_reasoning_options(&self, provider: &str, model: &str) -> ReasoningOptions {
        if provider == CHATGPT_PROVIDER_ID {
            self.reasoning_options(model)
        } else {
            crate::reasoning::options(provider, model)
        }
    }

    pub fn saved_model_reasoning_effort(&self, provider: &str, model: &str) -> Option<String> {
        if provider == CHATGPT_PROVIDER_ID {
            return self.saved_reasoning_effort(model);
        }
        let effort = self.user.reasoning_efforts.get(provider)?.get(model)?;
        self.model_reasoning_options(provider, model)
            .levels
            .iter()
            .any(|level| &level.effort == effort)
            .then(|| effort.clone())
    }

    pub fn select_model_with_reasoning(
        &mut self,
        provider: &str,
        model: &str,
        effort: Option<&str>,
    ) -> Result<ModelRuntimeConfig> {
        if provider == CHATGPT_PROVIDER_ID {
            return self.select_chatgpt_model_with_effort(model, effort);
        }
        if let Some(effort) = effort {
            anyhow::ensure!(
                self.model_reasoning_options(provider, model)
                    .levels
                    .iter()
                    .any(|level| level.effort == effort),
                "reasoning effort '{effort}' is not supported for {provider}/{model}"
            );
        }
        self.select_model_with_effort(provider, model, Some(effort))
    }

    pub fn reasoning_options(&self, model: &str) -> ReasoningOptions {
        crate::chatgpt::cached_reasoning_options(self.paths.root(), model)
    }
    pub fn saved_reasoning_effort(&self, model: &str) -> Option<String> {
        let config = self.user.chatgpt.as_ref()?;
        if !config.models.iter().any(|candidate| candidate == model) {
            return None;
        }
        let effort = config.reasoning_efforts.get(model)?;
        self.reasoning_options(model)
            .levels
            .iter()
            .any(|level| &level.effort == effort)
            .then(|| effort.clone())
    }
    pub fn select_chatgpt_model_with_effort(
        &mut self,
        model: &str,
        effort: Option<&str>,
    ) -> Result<ModelRuntimeConfig> {
        if let Some(effort) = effort
            && !self
                .reasoning_options(model)
                .levels
                .iter()
                .any(|level| level.effort == effort)
        {
            bail!("reasoning effort '{effort}' is not supported for ChatGPT model '{model}'");
        }
        self.select_model_with_effort(CHATGPT_PROVIDER_ID, model, Some(effort))
    }
    pub fn select_model(&mut self, provider_id: &str, model: &str) -> Result<ModelRuntimeConfig> {
        self.select_model_with_effort(provider_id, model, None)
    }
    fn select_model_with_effort(
        &mut self,
        provider_id: &str,
        model: &str,
        effort: Option<Option<&str>>,
    ) -> Result<ModelRuntimeConfig> {
        let providers = self.available_providers()?;
        let provider = providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .with_context(|| format!("provider '{provider_id}' is not available"))?;
        if !provider
            .models
            .iter()
            .any(|candidate| candidate.name == model)
        {
            bail!("model '{model}' is not defined for provider '{provider_id}'");
        }
        let api_key = provider_api_key(self.credentials.as_ref(), provider)?;
        let mut staged = self.user.clone();
        let (temperature, max_tokens) = staged
            .llm
            .as_ref()
            .map(|selection| (selection.temperature, selection.max_tokens))
            .unwrap_or((DEFAULT_TEMPERATURE, DEFAULT_MAX_TOKENS));
        staged.llm = Some(UserLlmConfig {
            provider: provider_id.to_owned(),
            model: model.to_owned(),
            temperature,
            max_tokens,
        });
        let selected_effort = effort
            .map(|effort| effort.map(str::to_owned))
            .unwrap_or_else(|| self.saved_model_reasoning_effort(provider_id, model));
        if provider_id == CHATGPT_PROVIDER_ID {
            if let Some(chatgpt) = staged.chatgpt.as_mut() {
                if let Some(effort) = &selected_effort {
                    chatgpt
                        .reasoning_efforts
                        .insert(model.to_owned(), effort.clone());
                } else {
                    chatgpt.reasoning_efforts.remove(model);
                }
            }
        } else if let Some(effort) = &selected_effort {
            staged
                .reasoning_efforts
                .entry(provider_id.to_owned())
                .or_default()
                .insert(model.to_owned(), effort.clone());
        } else if let Some(models) = staged.reasoning_efforts.get_mut(provider_id) {
            models.remove(model);
            if models.is_empty() {
                staged.reasoning_efforts.remove(provider_id);
            }
        }
        let model_catalog = runtime_model_catalog(&self.catalog, &providers);
        let mut llm = llm_from_available(&staged, providers, provider_id, model, api_key)?;
        let selected_api_key = llm.api_key.clone();
        llm.switch_model(provider_id, model, Some(selected_api_key))?;
        llm.reasoning_effort = selected_effort;
        self.repository.save(&staged)?;
        self.user = staged;
        Ok(ModelRuntimeConfig { llm, model_catalog })
    }

    fn persist_provider_change(
        &mut self,
        mut staged: UserConfig,
        credential_id: &CredentialId,
        old_credential: Option<String>,
        new_credential: Option<&str>,
    ) -> ConfigurationMutationResult<()> {
        if let Some(api_key) = new_credential {
            self.credentials
                .set(credential_id, api_key)
                .map_err(|error| {
                    ConfigurationMutationError::with_source(
                        ConfigurationMutationErrorKind::CredentialUnavailable,
                        error,
                    )
                })?;
        }
        if let Err(error) =
            repair_selection_in(&self.catalog, self.credentials.as_ref(), &mut staged)
        {
            if new_credential.is_some() {
                return Err(rollback_credential_mutation_error(
                    self.credentials.as_ref(),
                    credential_id,
                    old_credential.as_deref(),
                    ConfigurationMutationError::with_source(
                        ConfigurationMutationErrorKind::CredentialUnavailable,
                        error,
                    ),
                ));
            }
            return Err(ConfigurationMutationError::with_source(
                ConfigurationMutationErrorKind::CredentialUnavailable,
                error,
            ));
        }
        if let Err(error) = self.repository.save(&staged) {
            if new_credential.is_some() {
                return Err(rollback_credential_mutation_error(
                    self.credentials.as_ref(),
                    credential_id,
                    old_credential.as_deref(),
                    ConfigurationMutationError::with_source(
                        ConfigurationMutationErrorKind::Persistence,
                        error,
                    ),
                ));
            }
            return Err(ConfigurationMutationError::with_source(
                ConfigurationMutationErrorKind::Persistence,
                error,
            ));
        }
        self.user = staged;
        Ok(())
    }
}

fn runtime_model_catalog(
    catalog: &ProviderCatalog,
    providers: &[AvailableProvider],
) -> ModelCatalog {
    let mut runtime = ModelCatalog::default();
    for provider in providers {
        let unit = catalog
            .builtin(&provider.id)
            .map(|definition| definition.unit.clone())
            .unwrap_or_default();
        runtime.providers.insert(
            provider.id.clone(),
            ProviderCatalogEntry {
                description: provider.description.clone(),
                unit,
            },
        );
        let models = provider
            .models
            .iter()
            .filter_map(|model| {
                let entry = metadata_catalog_entry(model.metadata.clone());
                (!catalog_entry_is_empty(&entry)).then_some((model.name.clone(), entry))
            })
            .collect();
        runtime.models.insert(provider.id.clone(), models);
    }
    runtime
}

fn validate_loaded_user(user: &UserConfig, catalog: &ProviderCatalog) -> Result<()> {
    if user.version != 1 {
        bail!("unsupported configuration version {}", user.version);
    }
    if let Some(chatgpt) = &user.chatgpt {
        validate_chatgpt_models(&chatgpt.models).context("invalid ChatGPT models")?;
    }
    validate_persisted_custom_providers(user, catalog)
}

fn validate_persisted_custom_providers(user: &UserConfig, catalog: &ProviderCatalog) -> Result<()> {
    let mut custom_names = HashSet::new();
    for (name, provider) in &user.custom_providers {
        if name.is_empty() {
            bail!("custom provider name must not be empty");
        }
        if name.trim() != name {
            bail!("custom provider name '{name}' must not have surrounding whitespace");
        }
        if reserved_chatgpt_name(name)
            || catalog.providers().iter().any(|builtin| {
                builtin.id.eq_ignore_ascii_case(name) || builtin.name.eq_ignore_ascii_case(name)
            })
        {
            bail!("custom provider '{name}' collides with a built-in provider");
        }
        if !custom_names.insert(name.to_ascii_lowercase()) {
            bail!("duplicate custom provider name '{name}' (case-insensitive)");
        }
        validate_persisted_custom_provider(name, provider)?;
    }
    Ok(())
}

fn validate_persisted_custom_provider(name: &str, provider: &CustomProviderConfig) -> Result<()> {
    if provider.base_url.trim().is_empty() {
        bail!("custom provider '{name}' base URL must not be empty");
    }
    if provider.base_url.trim() != provider.base_url {
        bail!("custom provider '{name}' base URL must not have surrounding whitespace");
    }
    let url = reqwest::Url::parse(&provider.base_url)
        .with_context(|| format!("custom provider '{name}' base URL is invalid"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("custom provider '{name}' base URL must use http or https");
    }
    if provider.models.is_empty() {
        bail!("custom provider '{name}' must define at least one model");
    }

    let mut model_names = HashSet::new();
    for model in &provider.models {
        if model.trim().is_empty() {
            bail!("custom provider '{name}' model name must not be empty");
        }
        if model.trim() != model {
            bail!("custom provider '{name}' model name must not have surrounding whitespace");
        }
        if !model_names.insert(model) {
            bail!("custom provider '{name}' has duplicate model '{model}'");
        }
    }
    Ok(())
}

fn repair_selection_in(
    catalog: &ProviderCatalog,
    credentials: &dyn CredentialStore,
    user: &mut UserConfig,
) -> Result<bool> {
    let providers = available_providers(catalog, user, credentials)?;
    let selection_is_valid = user.llm.as_ref().is_some_and(|selection| {
        providers.iter().any(|provider| {
            provider.id == selection.provider
                && provider
                    .models
                    .iter()
                    .any(|model| model.name == selection.model)
        })
    });
    if selection_is_valid {
        return Ok(false);
    }

    let (temperature, max_tokens) = user
        .llm
        .as_ref()
        .map(|selection| (selection.temperature, selection.max_tokens))
        .unwrap_or((DEFAULT_TEMPERATURE, DEFAULT_MAX_TOKENS));
    let repaired = providers.first().and_then(|provider| {
        let model = if provider.custom {
            provider.models.first()
        } else {
            catalog
                .builtin(&provider.id)
                .and_then(|definition| {
                    provider
                        .models
                        .iter()
                        .find(|model| model.name == definition.default_model)
                })
                .or_else(|| provider.models.first())
        }?;
        Some(UserLlmConfig {
            provider: provider.id.clone(),
            model: model.name.clone(),
            temperature,
            max_tokens,
        })
    });
    if user.llm == repaired {
        return Ok(false);
    }
    user.llm = repaired;
    Ok(true)
}

fn llm_from_available(
    user: &UserConfig,
    providers: Vec<AvailableProvider>,
    selected_provider: &str,
    selected_model: &str,
    api_key: String,
) -> Result<LlmConfig> {
    let selection = user.llm.as_ref().context("no model is configured")?;
    let selected = providers
        .iter()
        .find(|provider| provider.id == selected_provider)
        .with_context(|| format!("provider '{selected_provider}' is unavailable"))?;
    let selected_metadata = selected
        .models
        .iter()
        .find(|model| model.name == selected_model)
        .with_context(|| {
            format!("model '{selected_model}' is unavailable for provider '{selected_provider}'")
        })?;
    let base_url = selected.base_url.clone();
    let context_window = selected_metadata.metadata.context;
    let prompt_cache = selected.prompt_cache.clone();
    let providers = providers
        .into_iter()
        .map(|provider| LlmProviderConfig {
            credential_id: credential_id(&provider),
            name: provider.id,
            base_url: provider.base_url,
            models: provider
                .models
                .iter()
                .map(|model| model.name.clone())
                .collect(),
            model_context_windows: provider
                .models
                .into_iter()
                .filter_map(|model| model.metadata.context.map(|context| (model.name, context)))
                .collect(),
            prompt_cache: provider.prompt_cache,
        })
        .collect();
    Ok(LlmConfig {
        reasoning_effort: None,
        provider: selected_provider.to_owned(),
        base_url,
        model: selected_model.to_owned(),
        providers,
        temperature: selection.temperature,
        max_tokens: selection.max_tokens,
        context_window,
        api_key,
        default_context_window: None,
        prompt_cache,
    })
}

fn credential_id(provider: &AvailableProvider) -> CredentialId {
    if provider.custom {
        CredentialId::custom(&provider.id)
    } else {
        CredentialId::builtin(&provider.id)
    }
}

fn reserved_chatgpt_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(CHATGPT_PROVIDER_ID) || name.eq_ignore_ascii_case("ChatGPT (Codex)")
}

fn validate_chatgpt_models(models: &[String]) -> ConfigurationMutationResult<()> {
    if models.is_empty()
        || models.iter().any(|model| {
            model.is_empty()
                || model.trim() != model
                || model.chars().any(char::is_whitespace)
                || model.chars().any(char::is_control)
        })
    {
        return Err(ConfigurationMutationError::new(
            ConfigurationMutationErrorKind::ModelRequired,
        ));
    }
    let mut seen = HashSet::new();
    if models.iter().any(|model| !seen.insert(model)) {
        return Err(ConfigurationMutationError::new(
            ConfigurationMutationErrorKind::DuplicateModel,
        ));
    }
    Ok(())
}

fn provider_api_key(
    credentials: &dyn CredentialStore,
    provider: &AvailableProvider,
) -> Result<String> {
    if provider.id == CHATGPT_PROVIDER_ID {
        Ok(String::new())
    } else {
        required_credential(credentials, &credential_id(provider))
    }
}

fn required_credential(
    credentials: &dyn CredentialStore,
    credential_id: &CredentialId,
) -> Result<String> {
    credentials
        .get(credential_id)?
        .filter(|credential| !credential.trim().is_empty())
        .with_context(|| format!("credential '{}' is unavailable", credential_id.as_str()))
}

fn nonblank_key(api_key: Option<&str>) -> Option<&str> {
    api_key.filter(|api_key| !api_key.trim().is_empty())
}

fn has_nonblank_credential(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|api_key| !api_key.trim().is_empty())
}

fn rollback_credential_mutation_error(
    credentials: &dyn CredentialStore,
    credential_id: &CredentialId,
    old_credential: Option<&str>,
    mut original_error: ConfigurationMutationError,
) -> ConfigurationMutationError {
    let rollback = match old_credential {
        Some(api_key) => credentials.set(credential_id, api_key),
        None => credentials.delete(credential_id),
    };
    if let Err(rollback_error) = rollback {
        let original_source = original_error.source.take();
        original_error.source = Some(match original_source {
            Some(source) => anyhow!(
                "configuration mutation failed: {source:#}; credential rollback also failed: {rollback_error:#}"
            ),
            None => anyhow!("credential rollback failed: {rollback_error:#}"),
        });
    }
    original_error
}

fn validate_base_url(base_url: &str) -> ConfigurationMutationResult<String> {
    let base_url = base_url.trim();
    if base_url.is_empty() {
        return Err(ConfigurationMutationError::new(
            ConfigurationMutationErrorKind::InvalidBaseUrl,
        ));
    }
    let parsed = reqwest::Url::parse(base_url).map_err(|error| {
        ConfigurationMutationError::with_source(
            ConfigurationMutationErrorKind::InvalidBaseUrl,
            error.into(),
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ConfigurationMutationError::new(
            ConfigurationMutationErrorKind::InvalidBaseUrl,
        ));
    }
    Ok(base_url.trim_end_matches('/').to_owned())
}

fn normalize_models(models: Vec<String>) -> ConfigurationMutationResult<Vec<String>> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for model in models {
        let model = model.trim().to_owned();
        if model.is_empty() {
            continue;
        }
        if !seen.insert(model.clone()) {
            return Err(ConfigurationMutationError::new(
                ConfigurationMutationErrorKind::DuplicateModel,
            ));
        }
        normalized.push(model);
    }
    if normalized.is_empty() {
        return Err(ConfigurationMutationError::new(
            ConfigurationMutationErrorKind::ModelRequired,
        ));
    }
    Ok(normalized)
}

fn metadata_catalog_entry(metadata: ModelMetadata) -> ModelCatalogEntry {
    ModelCatalogEntry {
        positioning: metadata.positioning,
        context: metadata
            .context
            .map(|value| value.to_string())
            .unwrap_or_default(),
        max_tokens: metadata.max_tokens,
        price: metadata.price,
        input: metadata.input,
        output: metadata.output,
        cache_read: metadata.cache_read,
        cache_write: metadata.cache_write,
    }
}

fn catalog_entry_is_empty(entry: &ModelCatalogEntry) -> bool {
    entry.positioning.is_empty()
        && entry.context.is_empty()
        && entry.max_tokens.is_empty()
        && entry.price.is_empty()
        && entry.input.is_empty()
        && entry.output.is_empty()
        && entry.cache_read.is_empty()
        && entry.cache_write.is_empty()
}

fn available_providers(
    catalog: &ProviderCatalog,
    user: &UserConfig,
    credentials: &dyn CredentialStore,
) -> Result<Vec<AvailableProvider>> {
    let mut providers = Vec::new();
    for provider in catalog.providers() {
        if !user
            .configured_providers
            .iter()
            .any(|id| id == &provider.id)
            || !credential_is_present(credentials, &CredentialId::builtin(&provider.id))?
        {
            continue;
        }
        providers.push(AvailableProvider {
            id: provider.id.clone(),
            display_name: provider.name.clone(),
            description: provider.description.clone(),
            base_url: provider.base_url.trim_end_matches('/').to_owned(),
            models: provider
                .models
                .iter()
                .map(|model| AvailableModel {
                    name: model.name.clone(),
                    metadata: model.metadata.clone(),
                })
                .collect(),
            prompt_cache: provider.prompt_cache.clone(),
            custom: false,
        });
    }

    let mut custom = user.custom_providers.iter().collect::<Vec<_>>();
    custom.sort_by(|(left, _), (right, _)| compare_custom_provider_names(left, right));
    for (name, provider) in custom {
        if !credential_is_present(credentials, &CredentialId::custom(name))? {
            continue;
        }
        providers.push(custom_available_provider(name, provider));
    }
    if let Some(chatgpt) = &user.chatgpt {
        providers.push(AvailableProvider {
            id: CHATGPT_PROVIDER_ID.into(),
            display_name: CHATGPT_PROVIDER_NAME.into(),
            description: "Use your ChatGPT subscription with Glint".into(),
            base_url: String::new(),
            models: chatgpt
                .models
                .iter()
                .map(|name| AvailableModel {
                    name: name.clone(),
                    metadata: ModelMetadata {
                        context: crate::chatgpt::cached_context_window(name),
                        ..ModelMetadata::default()
                    },
                })
                .collect(),
            prompt_cache: PromptCacheConfig::default(),
            custom: false,
        });
    }
    Ok(providers)
}

pub(crate) fn compare_custom_provider_names(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_ascii_lowercase()
        .cmp(&right.to_ascii_lowercase())
        .then_with(|| left.cmp(right))
}

fn custom_available_provider(name: &str, provider: &CustomProviderConfig) -> AvailableProvider {
    AvailableProvider {
        id: name.to_owned(),
        display_name: name.to_owned(),
        description: String::new(),
        base_url: provider.base_url.trim_end_matches('/').to_owned(),
        models: provider
            .models
            .iter()
            .map(|name| AvailableModel {
                name: name.clone(),
                metadata: ModelMetadata::default(),
            })
            .collect(),
        prompt_cache: PromptCacheConfig::default(),
        custom: true,
    }
}

fn credential_is_present(credentials: &dyn CredentialStore, id: &CredentialId) -> Result<bool> {
    Ok(credentials
        .get(id)?
        .is_some_and(|credential| !credential.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use anyhow::{Result, bail};

    use super::*;
    use crate::{
        config::{CustomProviderConfig, UserConfig, UserLlmConfig},
        credentials::{CredentialId, CredentialStore},
        paths::GlintPaths,
        persistence::{UserConfigRepository, UserConfigStore},
        provider_catalog::ProviderCatalog,
    };

    #[test]
    fn builtin_credential_exposes_every_catalog_model() {
        let mut fixture = ManagerFixture::new();
        fixture.user.configured_providers.push("deepseek".into());
        fixture.credentials.insert("builtin:deepseek", "key");

        let manager = fixture.manager();
        let providers = manager.available_providers().unwrap();

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "deepseek");
        assert_eq!(
            providers[0]
                .models
                .iter()
                .map(|model| model.name.as_str())
                .collect::<Vec<_>>(),
            ["deepseek-v4-flash", "deepseek-v4-pro"]
        );
    }

    #[test]
    fn custom_provider_requires_a_credential_and_model() {
        let mut fixture = ManagerFixture::new();
        fixture.user.custom_providers.insert(
            "Team Gateway".into(),
            CustomProviderConfig {
                base_url: "https://llm.example/v1".into(),
                models: vec!["code-large".into()],
            },
        );

        assert!(fixture.manager().available_providers().unwrap().is_empty());
    }

    #[test]
    fn invalid_persisted_custom_provider_is_rejected() {
        let mut fixture = ManagerFixture::new();
        fixture.user.custom_providers.insert(
            "Broken".into(),
            CustomProviderConfig {
                base_url: "file:///tmp/socket".into(),
                models: vec!["model".into()],
            },
        );
        fixture.credentials.insert("custom:Broken", "key");

        let error = fixture
            .try_manager()
            .err()
            .expect("invalid URL should fail");

        assert!(format!("{error:#}").contains("must use http or https"));
    }

    #[test]
    fn persisted_custom_provider_identities_are_validated_before_use() {
        let cases = [
            (
                "blank name",
                vec![("", "https://llm.example/v1", vec!["model"])],
                "name must not be empty",
            ),
            (
                "surrounding name whitespace",
                vec![(" Gateway ", "https://llm.example/v1", vec!["model"])],
                "must not have surrounding whitespace",
            ),
            (
                "built-in collision",
                vec![("DEEPSEEK", "https://llm.example/v1", vec!["model"])],
                "collides with a built-in provider",
            ),
            (
                "case-insensitive custom collision",
                vec![
                    ("Gateway", "https://one.example/v1", vec!["one"]),
                    ("gateway", "https://two.example/v1", vec!["two"]),
                ],
                "duplicate custom provider name",
            ),
        ];

        for (label, providers, expected) in cases {
            let mut fixture = ManagerFixture::new();
            for (name, base_url, models) in providers {
                fixture.user.custom_providers.insert(
                    name.into(),
                    CustomProviderConfig {
                        base_url: base_url.into(),
                        models: models.into_iter().map(str::to_owned).collect(),
                    },
                );
            }

            let error = fixture
                .try_manager()
                .err()
                .unwrap_or_else(|| panic!("{label} should be rejected"));

            assert!(
                format!("{error:#}").contains(expected),
                "{label}: {error:#}"
            );
        }
    }

    #[test]
    fn persisted_custom_provider_endpoint_and_models_are_validated_before_use() {
        let cases = [
            (
                "blank URL",
                "",
                Vec::<&str>::from(["model"]),
                "base URL must not be empty",
            ),
            (
                "non-HTTP URL",
                "file:///tmp/socket",
                vec!["model"],
                "must use http or https",
            ),
            (
                "URL whitespace",
                " https://llm.example/v1 ",
                vec!["model"],
                "base URL must not have surrounding whitespace",
            ),
            (
                "empty model list",
                "https://llm.example/v1",
                vec![],
                "at least one model",
            ),
            (
                "blank model",
                "https://llm.example/v1",
                vec![""],
                "model name must not be empty",
            ),
            (
                "whitespace-only model",
                "https://llm.example/v1",
                vec!["   "],
                "model name must not be empty",
            ),
            (
                "model whitespace",
                "https://llm.example/v1",
                vec![" model "],
                "model name must not have surrounding whitespace",
            ),
            (
                "duplicate model",
                "https://llm.example/v1",
                vec!["model", "model"],
                "duplicate model 'model'",
            ),
        ];

        for (label, base_url, models, expected) in cases {
            let mut fixture = ManagerFixture::new();
            fixture.user.custom_providers.insert(
                "Gateway".into(),
                CustomProviderConfig {
                    base_url: base_url.into(),
                    models: models.into_iter().map(str::to_owned).collect(),
                },
            );

            let error = fixture
                .try_manager()
                .err()
                .unwrap_or_else(|| panic!("{label} should be rejected"));

            assert!(
                format!("{error:#}").contains(expected),
                "{label}: {error:#}"
            );
        }
    }

    #[test]
    fn valid_persisted_custom_models_preserve_declared_order() {
        let mut fixture = ManagerFixture::new();
        fixture.user.custom_providers.insert(
            "Gateway".into(),
            CustomProviderConfig {
                base_url: "https://llm.example/v1".into(),
                models: vec!["second".into(), "first".into(), "SECOND".into()],
            },
        );
        fixture.credentials.insert("custom:Gateway", "key");

        let providers = fixture.manager().available_providers().unwrap();

        assert_eq!(
            providers[0]
                .models
                .iter()
                .map(|model| model.name.as_str())
                .collect::<Vec<_>>(),
            ["second", "first", "SECOND"]
        );
    }

    #[test]
    fn discover_reports_the_fixed_config_path_for_semantic_errors() {
        let home = std::env::temp_dir().join(format!(
            "glint-invalid-persisted-config-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = GlintPaths::from_home(home);
        let store = crate::persistence::UserConfigStore::new(paths.clone());
        let mut user = UserConfig::default();
        user.custom_providers.insert(
            " Gateway ".into(),
            CustomProviderConfig {
                base_url: "https://llm.example/v1".into(),
                models: vec!["model".into()],
            },
        );
        store.save(&user).unwrap();

        let error = ConfigurationManager::discover(paths.clone(), Path::new("/workspace"))
            .err()
            .expect("invalid persisted provider should fail discovery");
        let message = format!("{error:#}");

        assert!(message.contains(&paths.config().display().to_string()));
        assert!(message.contains("surrounding whitespace"));
    }

    #[test]
    fn unsupported_user_config_version_is_rejected() {
        let mut fixture = ManagerFixture::new();
        fixture.user.version = 2;
        fixture.repository.replace(fixture.user.clone());

        let error = ConfigurationManager::new(
            GlintPaths::from_home("/fixture"),
            PathBuf::from("/workspace"),
            ProviderCatalog::embedded().unwrap(),
            Box::new(fixture.repository.clone()),
            Box::new(fixture.credentials.clone()),
        )
        .err()
        .expect("unsupported version should fail");

        assert!(format!("{error:#}").contains("unsupported configuration version 2"));
    }

    #[test]
    fn invalid_current_model_is_repaired_to_builtin_default() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "key");
        fixture.user.llm = Some(UserLlmConfig {
            provider: "deepseek".into(),
            model: "removed-model".into(),
            temperature: 0.7,
            max_tokens: 8196,
        });

        let mut manager = fixture.manager();
        manager.repair_selection().unwrap();

        assert_eq!(
            manager.user_config().llm.as_ref().unwrap().model,
            "deepseek-v4-flash"
        );
    }

    #[test]
    fn providers_are_ordered_as_catalog_then_case_insensitive_custom_names() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("kimi", "kimi-key");
        fixture.enable_builtin("deepseek", "deepseek-key");
        for (name, model) in [("zebra", "z-model"), ("Alpha", "a-model")] {
            fixture.user.custom_providers.insert(
                name.into(),
                CustomProviderConfig {
                    base_url: "https://llm.example/v1".into(),
                    models: vec![model.into()],
                },
            );
            fixture.credentials.insert(&format!("custom:{name}"), "key");
        }

        let providers = fixture.manager().available_providers().unwrap();

        assert_eq!(
            providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            ["deepseek", "kimi", "Alpha", "zebra"]
        );
    }

    #[test]
    fn provider_status_marks_configured_missing_credential_for_repair() {
        let mut fixture = ManagerFixture::new();
        fixture.user.configured_providers.push("deepseek".into());

        let statuses = fixture.manager().provider_statuses().unwrap();
        let deepseek = statuses
            .iter()
            .find(|status| status.id == "deepseek")
            .unwrap();

        assert!(deepseek.configured);
        assert!(deepseek.needs_credential);
        assert_eq!(deepseek.model_count, 2);
    }

    #[test]
    fn save_custom_rejects_case_insensitive_custom_name_collision() {
        let mut fixture = ManagerFixture::new();
        fixture.user.custom_providers.insert(
            "Team Gateway".into(),
            CustomProviderConfig {
                base_url: "https://old.example/v1".into(),
                models: vec!["old".into()],
            },
        );
        fixture.credentials.insert("custom:Team Gateway", "key");
        let mut manager = fixture.manager();

        let error = manager
            .save_custom(
                "team gateway",
                "https://new.example/v1",
                Some("new-key"),
                vec!["new".into()],
            )
            .unwrap_err();

        assert_eq!(
            error.kind(),
            ConfigurationMutationErrorKind::ProviderNameCollision
        );
        assert_eq!(manager.user_config().custom_providers.len(), 1);
    }

    #[test]
    fn save_custom_rejects_builtin_id_and_display_name_collisions() {
        for name in ["DeepSeek", "deepseek"] {
            let fixture = ManagerFixture::new();
            let mut manager = fixture.manager();

            let error = manager
                .save_custom(
                    name,
                    "https://llm.example/v1",
                    Some("key"),
                    vec!["model".into()],
                )
                .unwrap_err();

            assert_eq!(
                error.kind(),
                ConfigurationMutationErrorKind::ProviderNameCollision
            );
        }
    }

    #[test]
    fn save_custom_rejects_non_http_url_scheme() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();

        let error = manager
            .save_custom(
                "Gateway",
                "file:///tmp/endpoint",
                Some("key"),
                vec!["model".into()],
            )
            .unwrap_err();

        assert_eq!(error.kind(), ConfigurationMutationErrorKind::InvalidBaseUrl);
    }

    #[test]
    fn save_custom_trims_models_and_ignores_blank_rows() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();

        manager
            .save_custom(
                "  Gateway  ",
                " https://llm.example/v1/ ",
                Some("key"),
                vec![" code-large ".into(), "".into(), "CODE-LARGE".into()],
            )
            .unwrap();

        let provider = &manager.user_config().custom_providers["Gateway"];
        assert_eq!(provider.base_url, "https://llm.example/v1");
        assert_eq!(provider.models, ["code-large", "CODE-LARGE"]);
    }

    #[test]
    fn save_custom_rejects_duplicate_model_names() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();

        let error = manager
            .save_custom(
                "Gateway",
                "https://llm.example/v1",
                Some("key"),
                vec![" model ".into(), "model".into()],
            )
            .unwrap_err();

        assert_eq!(error.kind(), ConfigurationMutationErrorKind::DuplicateModel);
        assert_eq!(error.user_message(), "Model names must be unique.");
    }

    #[test]
    fn save_custom_rejects_a_list_without_nonblank_models() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();

        let error = manager
            .save_custom(
                "Gateway",
                "https://llm.example/v1",
                Some("key"),
                vec![" ".into()],
            )
            .unwrap_err();

        assert!(format!("{error:#}").contains("at least one model"));
    }

    #[test]
    fn blank_key_retains_the_existing_custom_credential() {
        let mut fixture = ManagerFixture::new();
        fixture.user.custom_providers.insert(
            "Gateway".into(),
            CustomProviderConfig {
                base_url: "https://old.example/v1".into(),
                models: vec!["old".into()],
            },
        );
        fixture.credentials.insert("custom:Gateway", "old-secret");
        let credentials = fixture.credentials.clone();
        let mut manager = fixture.manager();

        manager
            .save_custom(
                "Gateway",
                "https://new.example/v1",
                Some("   "),
                vec!["new".into()],
            )
            .unwrap();

        assert_eq!(
            credentials
                .get(&CredentialId::custom("Gateway"))
                .unwrap()
                .as_deref(),
            Some("old-secret")
        );
    }

    #[test]
    fn save_rejects_missing_credential_for_a_new_provider() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();

        let error = manager.save_builtin("deepseek", None).unwrap_err();

        assert!(format!("{error:#}").contains("API key"));
        assert!(manager.user_config().configured_providers.is_empty());
    }

    #[test]
    fn first_builtin_save_selects_its_catalog_default() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();

        manager.save_builtin("deepseek", Some("secret")).unwrap();

        assert_eq!(
            manager.user_config().llm,
            Some(UserLlmConfig {
                provider: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                temperature: 0.7,
                max_tokens: 8196,
            })
        );
    }

    #[test]
    fn first_custom_save_selects_its_first_model() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();

        manager
            .save_custom(
                "Gateway",
                "https://llm.example/v1",
                Some("secret"),
                vec!["first".into(), "second".into()],
            )
            .unwrap();

        assert_eq!(manager.user_config().llm.as_ref().unwrap().model, "first");
    }

    #[test]
    fn removing_the_active_custom_model_repairs_to_the_first_available_provider() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "deepseek-key");
        fixture.user.custom_providers.insert(
            "Gateway".into(),
            CustomProviderConfig {
                base_url: "https://llm.example/v1".into(),
                models: vec!["old".into(), "new".into()],
            },
        );
        fixture.credentials.insert("custom:Gateway", "gateway-key");
        fixture.user.llm = Some(UserLlmConfig {
            provider: "Gateway".into(),
            model: "old".into(),
            temperature: 0.4,
            max_tokens: 2048,
        });
        let mut manager = fixture.manager();

        manager
            .save_custom(
                "Gateway",
                "https://llm.example/v1",
                None,
                vec!["new".into()],
            )
            .unwrap();

        assert_eq!(
            manager.user_config().llm,
            Some(UserLlmConfig {
                provider: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                temperature: 0.4,
                max_tokens: 2048,
            })
        );
    }

    #[test]
    fn build_runtime_resolves_selected_secret_and_catalog_metadata() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "runtime-secret");
        fixture.user.llm = Some(UserLlmConfig {
            provider: "deepseek".into(),
            model: "deepseek-v4-flash".into(),
            temperature: 0.6,
            max_tokens: 4096,
        });
        fixture.user.lsp = Some(serde_yaml::from_str("servers: {}").unwrap());
        let plugin_cache = std::env::temp_dir().join(format!(
            "glint-manager-plugin-cache-{}",
            uuid::Uuid::new_v4()
        ));
        fixture.user.plugins =
            Some(serde_yaml::from_str(&format!("cache_dir: {}", plugin_cache.display())).unwrap());
        let manager = fixture.manager();

        let runtime = manager.build_runtime().unwrap();

        assert_eq!(runtime.llm.api_key, "runtime-secret");
        assert_eq!(runtime.llm.base_url, "https://api.deepseek.com");
        assert_eq!(runtime.llm.context_window, Some(1_000_000));
        assert_eq!(
            runtime.llm.providers[0].credential_id.as_str(),
            "builtin:deepseek"
        );
        assert!(runtime.lsp.servers.is_empty());
        assert_eq!(
            runtime.model_catalog.models["deepseek"]["deepseek-v4-flash"].context,
            "1000000"
        );
        assert!(!runtime.system_prompt.is_empty());
    }

    #[test]
    fn mcp_updates_survive_later_model_and_provider_persistence() {
        let home = std::env::temp_dir().join(format!(
            "glint-manager-mcp-sequence-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = GlintPaths::from_home(&home);
        let workspace = home.join("workspace");
        let mut manager = ConfigurationManager::new(
            paths.clone(),
            workspace,
            ProviderCatalog::embedded().unwrap(),
            Box::new(UserConfigStore::new(paths.clone())),
            Box::new(crate::credentials::FileCredentialStore::new(paths.auth())),
        )
        .unwrap();
        manager
            .save_builtin("deepseek", Some("deepseek-key"))
            .unwrap();
        let mut server = test_mcp_server("old-mcp");
        manager.upsert_mcp_server("demo", &server).unwrap();
        server = test_mcp_server("new-mcp");
        manager.upsert_mcp_server("demo", &server).unwrap();
        let expected_mcp = manager.user_config().mcp.clone();

        manager
            .select_model("deepseek", "deepseek-v4-flash")
            .unwrap();
        assert_eq!(
            UserConfigStore::new(paths.clone())
                .load()
                .unwrap()
                .unwrap()
                .mcp,
            expected_mcp
        );
        manager
            .save_custom(
                "Gateway",
                "https://gateway.example/v1",
                Some("gateway-key"),
                vec!["gateway-model".to_owned()],
            )
            .unwrap();
        manager.delete_provider("Gateway").unwrap();

        let persisted = UserConfigStore::new(paths.clone()).load().unwrap().unwrap();
        assert_eq!(persisted.mcp, expected_mcp);
        assert!(!persisted.custom_providers.contains_key("Gateway"));
        assert_eq!(persisted.llm.unwrap().provider, "deepseek");
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn filesystem_persistence_preserves_unknown_top_level_values() {
        let home = std::env::temp_dir().join(format!(
            "glint-manager-unknown-top-level-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = GlintPaths::from_home(&home);
        fs::create_dir_all(paths.root()).unwrap();
        fs::write(
            paths.config(),
            "version: 1\nfuture_mapping:\n  nested:\n    - 1\n    - true\n    - label\nfuture_scalar: preserve-me\n",
        )
        .unwrap();
        let mut manager = ConfigurationManager::new(
            paths.clone(),
            home.join("workspace"),
            ProviderCatalog::embedded().unwrap(),
            Box::new(UserConfigStore::new(paths.clone())),
            Box::new(crate::credentials::FileCredentialStore::new(paths.auth())),
        )
        .unwrap();

        manager
            .upsert_mcp_server("demo", &test_mcp_server("demo-mcp"))
            .unwrap();
        manager
            .save_builtin("deepseek", Some("deepseek-key"))
            .unwrap();
        manager
            .select_model("deepseek", "deepseek-v4-flash")
            .unwrap();
        manager
            .save_custom(
                "Gateway",
                "https://gateway.example/v1",
                Some("gateway-key"),
                vec!["gateway-model".to_owned()],
            )
            .unwrap();

        let raw: serde_yaml::Value =
            serde_yaml::from_str(&fs::read_to_string(paths.config()).unwrap()).unwrap();
        let mapping = raw.as_mapping().unwrap();
        assert_eq!(
            mapping.get("future_mapping"),
            Some(&serde_yaml::from_str("nested: [1, true, label]").unwrap())
        );
        assert_eq!(
            mapping.get("future_scalar"),
            Some(&serde_yaml::Value::String("preserve-me".to_owned()))
        );
        assert!(mapping.contains_key("mcp"));
        assert!(mapping.contains_key("llm"));
        assert!(mapping.contains_key("custom_providers"));
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn failed_mcp_write_leaves_disk_and_manager_unchanged() {
        let fixture = ManagerFixture::new();
        let repository = fixture.repository.clone();
        let mut manager = fixture.manager();
        let before_user = manager.user_config().clone();
        let before_disk = repository.load().unwrap().unwrap();
        repository.fail_saves();

        let error = manager
            .upsert_mcp_server("demo", &test_mcp_server("demo-mcp"))
            .unwrap_err();

        assert!(format!("{error:#}").contains("user config save failure"));
        assert_eq!(manager.user_config(), &before_user);
        assert_eq!(repository.load().unwrap().unwrap(), before_disk);
    }

    #[test]
    fn mcp_upsert_loads_once_and_saves_the_exact_loaded_document() {
        let home = std::env::temp_dir().join(format!(
            "glint-manager-mcp-single-load-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = GlintPaths::from_home(&home);
        fs::create_dir_all(paths.root()).unwrap();
        let initial = UserConfig::default();
        fs::write(paths.config(), serde_yaml::to_string(&initial).unwrap()).unwrap();
        let mut exact = initial.clone();
        exact.custom_providers.insert(
            "Exact".to_owned(),
            CustomProviderConfig {
                base_url: "https://exact.example/v1".to_owned(),
                models: vec!["exact-model".to_owned()],
            },
        );
        exact.plugins = Some(serde_yaml::from_str("entries: []").unwrap());
        let mut intervening = initial;
        intervening.custom_providers.insert(
            "Intervening".to_owned(),
            CustomProviderConfig {
                base_url: "https://intervening.example/v1".to_owned(),
                models: vec!["intervening-model".to_owned()],
            },
        );
        let repository = ChangingUserConfigStore::new(paths.config(), exact.clone(), intervening);
        let mut manager = ConfigurationManager::new(
            paths.clone(),
            home.join("workspace"),
            ProviderCatalog::embedded().unwrap(),
            Box::new(repository.clone()),
            Box::new(MemoryCredentialStore::default()),
        )
        .unwrap();

        manager
            .upsert_mcp_server("demo", &test_mcp_server("demo-mcp"))
            .unwrap();

        assert_eq!(repository.load_count(), 2);
        assert_eq!(repository.save_count(), 1);
        let saved = repository.saved().unwrap();
        assert_eq!(saved.custom_providers, exact.custom_providers);
        assert_eq!(saved.plugins, exact.plugins);
        assert!(saved.mcp.is_some());
        assert_eq!(manager.user_config(), &saved);

        manager
            .save_builtin("deepseek", Some("deepseek-key"))
            .unwrap();
        let saved_after_provider = repository.saved().unwrap();
        assert_eq!(repository.load_count(), 2);
        assert_eq!(repository.save_count(), 2);
        assert_eq!(
            saved_after_provider.custom_providers,
            exact.custom_providers
        );
        assert_eq!(saved_after_provider.plugins, exact.plugins);
        assert_eq!(saved_after_provider.mcp, saved.mcp);
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn manager_keeps_the_injected_workspace() {
        let fixture = ManagerFixture::new();
        let manager = ConfigurationManager::new(
            GlintPaths::from_home("/explicit/home"),
            PathBuf::from("/injected/workspace"),
            ProviderCatalog::embedded().unwrap(),
            Box::new(fixture.repository.clone()),
            Box::new(fixture.credentials.clone()),
        )
        .unwrap();

        assert_eq!(manager.workspace(), Path::new("/injected/workspace"));
    }

    #[test]
    fn failed_yaml_save_restores_replaced_credential() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "old-secret");
        fixture.repository.fail_saves();
        let credentials = fixture.credentials.clone();
        let mut manager = fixture.manager();

        let error = manager
            .save_builtin("deepseek", Some("new-secret"))
            .unwrap_err();

        assert_eq!(error.kind(), ConfigurationMutationErrorKind::Persistence);
        assert!(!error.to_string().contains("user config save failure"));
        assert_eq!(
            credentials
                .get(&CredentialId::builtin("deepseek"))
                .unwrap()
                .as_deref(),
            Some("old-secret")
        );
    }

    #[test]
    fn failed_yaml_save_deletes_newly_staged_credential() {
        let fixture = ManagerFixture::new();
        fixture.repository.fail_saves();
        let credentials = fixture.credentials.clone();
        let mut manager = fixture.manager();

        assert!(
            manager
                .save_builtin("deepseek", Some("new-secret"))
                .is_err()
        );
        assert_eq!(
            credentials.get(&CredentialId::builtin("deepseek")).unwrap(),
            None
        );
    }

    #[test]
    fn rollback_failure_keeps_a_safe_persistence_error_without_secrets() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "old-secret");
        fixture.repository.fail_saves();
        fixture.credentials.fail_set_call(2);
        let mut manager = fixture.manager();

        let error = manager
            .save_builtin("deepseek", Some("new-secret"))
            .unwrap_err();
        let message = format!("{error:#}");

        assert_eq!(error.kind(), ConfigurationMutationErrorKind::Persistence);
        assert_eq!(
            message,
            "Could not write Glint configuration. Check that ~/.glint is writable and try again."
        );
        assert!(!message.contains("old-secret"));
        assert!(!message.contains("new-secret"));
    }

    #[test]
    fn select_model_persists_before_returning_runtime_config() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "secret");
        fixture.user.llm = Some(UserLlmConfig {
            provider: "deepseek".into(),
            model: "deepseek-v4-flash".into(),
            temperature: 0.2,
            max_tokens: 1234,
        });
        let repository = fixture.repository.clone();
        let mut manager = fixture.manager();

        let runtime = manager.select_model("deepseek", "deepseek-v4-pro").unwrap();

        assert_eq!(runtime.llm.model, "deepseek-v4-pro");
        assert_eq!(runtime.llm.api_key, "secret");
        let persisted = repository.load().unwrap().unwrap().llm.unwrap();
        assert_eq!(persisted.model, "deepseek-v4-pro");
        assert_eq!(persisted.temperature, 0.2);
        assert_eq!(persisted.max_tokens, 1234);
    }

    #[test]
    fn build_model_runtime_projects_persisted_selection_without_writing() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "secret");
        fixture.user.llm = Some(UserLlmConfig {
            provider: "deepseek".into(),
            model: "deepseek-v4-pro".into(),
            temperature: 0.2,
            max_tokens: 1234,
        });
        let repository = fixture.repository.clone();
        let manager = fixture.manager();

        let runtime = manager.build_model_runtime().unwrap();

        assert_eq!(runtime.llm.provider, "deepseek");
        assert_eq!(runtime.llm.model, "deepseek-v4-pro");
        assert!(runtime.model_catalog.providers.contains_key("deepseek"));
        assert_eq!(repository.save_count(), 0);
    }

    #[test]
    fn deleting_last_provider_clears_selection_and_credential() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "secret");
        fixture.user.llm = Some(UserLlmConfig {
            provider: "deepseek".into(),
            model: "deepseek-v4-flash".into(),
            temperature: 0.7,
            max_tokens: 8196,
        });
        let credentials = fixture.credentials.clone();
        let mut manager = fixture.manager();

        manager.delete_provider("deepseek").unwrap();

        assert!(manager.user_config().configured_providers.is_empty());
        assert!(manager.user_config().llm.is_none());
        assert_eq!(
            credentials.get(&CredentialId::builtin("deepseek")).unwrap(),
            None
        );
    }

    #[test]
    fn failed_delete_yaml_save_restores_credential_and_user_state() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "secret");
        fixture.repository.fail_saves();
        let credentials = fixture.credentials.clone();
        let mut manager = fixture.manager();

        assert!(manager.delete_provider("deepseek").is_err());

        assert_eq!(manager.user_config().configured_providers, ["deepseek"]);
        assert_eq!(
            credentials
                .get(&CredentialId::builtin("deepseek"))
                .unwrap()
                .as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn credential_delete_failure_preserves_user_configuration() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "secret");
        fixture.credentials.fail_deletes();
        let repository = fixture.repository.clone();
        let mut manager = fixture.manager();

        let error = manager.delete_provider("deepseek").unwrap_err();

        assert_eq!(
            error.kind(),
            ConfigurationMutationErrorKind::CredentialUnavailable
        );
        assert!(!error.to_string().contains("credential delete failure"));
        assert_eq!(manager.user_config().configured_providers, ["deepseek"]);
        assert_eq!(
            repository.load().unwrap().unwrap().configured_providers,
            ["deepseek"]
        );
    }

    #[test]
    fn builtin_reasoning_persists_and_never_leaks_between_models() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "secret");
        fixture.enable_builtin("zhipu", "secret");
        let mut manager = fixture.manager();
        manager
            .select_model_with_reasoning("deepseek", "deepseek-v4-pro", Some("max"))
            .unwrap();
        assert_eq!(
            manager
                .build_model_runtime()
                .unwrap()
                .llm
                .reasoning_effort
                .as_deref(),
            Some("max")
        );
        let persisted = fixture.repository.load().unwrap().unwrap();
        assert_eq!(
            persisted.reasoning_efforts["deepseek"]["deepseek-v4-pro"],
            "max"
        );
        let yaml = serde_yaml::to_string(&persisted).unwrap();
        assert_eq!(
            serde_yaml::from_str::<UserConfig>(&yaml).unwrap(),
            persisted
        );
        manager
            .select_model("deepseek", "deepseek-v4-flash")
            .unwrap();
        assert!(
            manager
                .build_model_runtime()
                .unwrap()
                .llm
                .reasoning_effort
                .is_none()
        );
        manager.select_model("deepseek", "deepseek-v4-pro").unwrap();
        assert_eq!(
            manager
                .build_model_runtime()
                .unwrap()
                .llm
                .reasoning_effort
                .as_deref(),
            Some("max")
        );
        let before = manager.user_config().clone();
        assert!(
            manager
                .select_model_with_reasoning("zhipu", "glm-5.1", Some("high"))
                .is_err()
        );
        assert!(
            manager
                .select_model_with_reasoning("deepseek", "deepseek-v4-pro", Some("ultra"))
                .is_err()
        );
        assert_eq!(manager.user_config(), &before);
        manager
            .select_model_with_reasoning("deepseek", "deepseek-v4-pro", None)
            .unwrap();
        assert!(manager.user_config().reasoning_efforts.is_empty());
    }

    #[test]
    fn chatgpt_reasoning_persists_per_model_and_isolates_api_runtime() {
        let mut fixture = ManagerFixture::new();
        fixture.user.configured_providers.push("deepseek".into());
        fixture.credentials.insert("builtin:deepseek", "key");
        let mut manager = fixture.manager();
        let home = std::env::temp_dir().join(format!("glint-reasoning-{}", uuid::Uuid::new_v4()));
        manager.paths = GlintPaths::from_home(&home);
        std::fs::create_dir_all(manager.paths.root()).unwrap();
        std::fs::write(manager.paths.root().join("chatgpt-models.json"), r#"{"models":[{"slug":"one","default_reasoning_level":"medium","supported_reasoning_levels":[{"effort":"medium","description":"Balanced"},{"effort":"ultra","description":"Most"}]},{"slug":"two","supported_reasoning_levels":[{"effort":"max","description":"Maximum"}]}]}"#).unwrap();
        manager
            .save_chatgpt(vec!["one".into(), "two".into()], None)
            .unwrap();
        assert_eq!(
            manager.reasoning_options("one").default_effort.as_deref(),
            Some("medium")
        );
        let runtime = manager
            .select_chatgpt_model_with_effort("one", Some("ultra"))
            .unwrap();
        assert_eq!(runtime.llm.reasoning_effort.as_deref(), Some("ultra"));
        assert_eq!(
            manager.saved_reasoning_effort("one").as_deref(),
            Some("ultra")
        );
        assert_eq!(
            fixture
                .repository
                .load()
                .unwrap()
                .unwrap()
                .chatgpt
                .unwrap()
                .reasoning_efforts
                .get("one")
                .map(String::as_str),
            Some("ultra")
        );
        assert!(
            manager
                .select_chatgpt_model_with_effort("two", Some("ultra"))
                .is_err()
        );
        assert!(
            manager
                .select_model("deepseek", "deepseek-v4-flash")
                .unwrap()
                .llm
                .reasoning_effort
                .is_none()
        );
        assert_eq!(
            manager
                .select_model("chatgpt", "one")
                .unwrap()
                .llm
                .reasoning_effort
                .as_deref(),
            Some("ultra")
        );
        let before = manager.user.clone();
        fixture.repository.fail_saves();
        assert!(
            manager
                .select_chatgpt_model_with_effort("one", None)
                .is_err()
        );
        assert_eq!(manager.user, before);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn chatgpt_reasoning_default_reset_and_catalog_refresh_prune_preferences() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();
        let home =
            std::env::temp_dir().join(format!("glint-effort-reset-{}", uuid::Uuid::new_v4()));
        manager.paths = GlintPaths::from_home(&home);
        std::fs::create_dir_all(manager.paths.root().join("codex")).unwrap();
        let cache = manager.paths.root().join("codex/models_cache.json");
        std::fs::write(&cache,r#"{"models":[{"slug":"one","default_reasoning_level":"max","supported_reasoning_levels":[{"effort":"max","description":"More"}]},{"slug":"two","supported_reasoning_levels":[{"effort":"ultra","description":"Most"}]}]}"#).unwrap();
        manager
            .save_chatgpt(vec!["one".into(), "two".into()], None)
            .unwrap();
        manager
            .select_chatgpt_model_with_effort("one", Some("max"))
            .unwrap();
        assert_eq!(
            manager
                .build_model_runtime()
                .unwrap()
                .llm
                .reasoning_effort
                .as_deref(),
            Some("max")
        );
        manager
            .select_chatgpt_model_with_effort("two", Some("ultra"))
            .unwrap();
        let runtime = manager
            .select_chatgpt_model_with_effort("two", None)
            .unwrap();
        assert!(runtime.llm.reasoning_effort.is_none());
        assert!(manager.saved_reasoning_effort("two").is_none());
        manager
            .select_chatgpt_model_with_effort("two", Some("ultra"))
            .unwrap();
        std::fs::write(manager.paths.root().join("chatgpt-models.json"),r#"{"models":[{"slug":"one","supported_reasoning_levels":[{"effort":"low","description":"Less"}]}]}"#).unwrap();
        assert!(manager.saved_reasoning_effort("one").is_none());
        manager.save_chatgpt(vec!["one".into()], None).unwrap();
        assert!(
            manager
                .user
                .chatgpt
                .as_ref()
                .unwrap()
                .reasoning_efforts
                .is_empty()
        );
        assert!(
            manager
                .build_model_runtime()
                .unwrap()
                .llm
                .reasoning_effort
                .is_none()
        );
        assert!(
            !serde_yaml::to_string(&manager.user)
                .unwrap()
                .contains("reasoning_efforts")
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn chatgpt_selection_needs_no_api_credentials_and_preserves_api_choice() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();
        manager
            .save_chatgpt(
                vec!["first".into(), "preferred".into()],
                Some("preferred".into()),
            )
            .unwrap();
        let runtime = manager.build_model_runtime().unwrap();
        assert_eq!(runtime.llm.provider, "chatgpt");
        assert_eq!(runtime.llm.model, "preferred");
        assert!(runtime.llm.api_key.is_empty());
        assert!(runtime.llm.base_url.is_empty());
        manager.select_model("chatgpt", "first").unwrap();
        manager.save_builtin("deepseek", Some("secret")).unwrap();
        assert_eq!(manager.user.llm.as_ref().unwrap().provider, "chatgpt");
        manager
            .select_model("deepseek", "deepseek-v4-flash")
            .unwrap();
        manager.save_chatgpt(vec!["second".into()], None).unwrap();
        assert_eq!(manager.user.llm.as_ref().unwrap().provider, "deepseek");
        manager.delete_provider("deepseek").unwrap();
        assert_eq!(manager.user.llm.as_ref().unwrap().model, "second");
        manager.delete_provider("chatgpt").unwrap();
        assert!(manager.user.llm.is_none());
        assert!(manager.user.chatgpt.is_none());
    }

    #[test]
    fn chatgpt_refresh_preserves_selection_and_removal_falls_back_to_api() {
        let mut fixture = ManagerFixture::new();
        fixture.enable_builtin("deepseek", "secret");
        let mut manager = fixture.manager();
        manager.repair_selection().unwrap();
        manager
            .save_chatgpt(vec!["one".into(), "two".into()], Some("two".into()))
            .unwrap();
        assert_eq!(manager.user.llm.as_ref().unwrap().provider, "deepseek");
        manager.select_model("chatgpt", "one").unwrap();
        manager
            .save_chatgpt(vec!["one".into(), "two".into()], Some("two".into()))
            .unwrap();
        assert_eq!(manager.user.llm.as_ref().unwrap().model, "one");
        manager
            .save_chatgpt(vec!["three".into(), "four".into()], Some("four".into()))
            .unwrap();
        assert_eq!(manager.user.llm.as_ref().unwrap().model, "four");
        manager.delete_provider("chatgpt").unwrap();
        assert_eq!(
            manager.build_model_runtime().unwrap().llm.provider,
            "deepseek"
        );
    }

    #[test]
    fn chatgpt_only_startup_never_reads_or_writes_api_credentials() {
        struct NoCredentials;
        impl CredentialStore for NoCredentials {
            fn get(&self, _: &CredentialId) -> Result<Option<String>> {
                panic!("ChatGPT must not read API credentials")
            }
            fn set(&self, _: &CredentialId, _: &str) -> Result<()> {
                panic!("ChatGPT must not write API credentials")
            }
            fn delete(&self, _: &CredentialId) -> Result<()> {
                panic!("ChatGPT must not delete API credentials")
            }
        }
        let fixture = ManagerFixture::new();
        fixture.repository.replace(serde_yaml::from_str("version: 1\nchatgpt:\n  models: [one, two]\nllm:\n  provider: chatgpt\n  model: one\n  temperature: 0.7\n  max_tokens: 8196\n").unwrap());
        let mut manager = ConfigurationManager::new(
            GlintPaths::from_home("/fixture"),
            PathBuf::from("/workspace"),
            ProviderCatalog::embedded().unwrap(),
            Box::new(fixture.repository.clone()),
            Box::new(NoCredentials),
        )
        .unwrap();
        manager.repair_selection().unwrap();
        assert_eq!(fixture.repository.save_count(), 0);
        let mut runtime = manager.build_model_runtime().unwrap();
        runtime.llm.switch_model("chatgpt", "two", None).unwrap();
        manager.provider_statuses().unwrap();
        manager.select_model("chatgpt", "two").unwrap();
        manager.save_chatgpt(vec!["two".into()], None).unwrap();
        manager.delete_provider("chatgpt").unwrap();
    }

    #[test]
    fn chatgpt_persisted_metadata_is_validated_without_secrets() {
        for models in [
            vec![],
            vec!["".to_owned()],
            vec!["a".to_owned(), "a".to_owned()],
            vec![" a".to_owned()],
        ] {
            let mut fixture = ManagerFixture::new();
            fixture.user.chatgpt = Some(ChatGptConfig {
                models,
                reasoning_efforts: Default::default(),
            });
            assert!(fixture.try_manager().is_err());
        }
        for yaml in [
            "chatgpt: {}",
            "chatgpt: {models: [one], access_token: secret}",
        ] {
            assert!(serde_yaml::from_str::<UserConfig>(yaml).is_err());
        }
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();
        manager.save_chatgpt(vec!["one".into()], None).unwrap();
        let yaml = serde_yaml::to_value(manager.user_config()).unwrap();
        let chatgpt = yaml.get("chatgpt").unwrap().as_mapping().unwrap();
        assert_eq!(chatgpt.len(), 1);
        assert!(chatgpt.contains_key("models"));
    }

    #[test]
    fn chatgpt_failed_save_and_delete_preserve_configuration() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();
        manager.save_chatgpt(vec!["one".into()], None).unwrap();
        let before = manager.user.clone();
        fixture.repository.fail_saves();
        assert_eq!(
            manager
                .save_chatgpt(vec!["two".into()], None)
                .unwrap_err()
                .kind(),
            ConfigurationMutationErrorKind::Persistence
        );
        assert_eq!(manager.user, before);
        assert!(manager.delete_provider("chatgpt").is_err());
        assert_eq!(manager.user, before);
        assert_eq!(fixture.repository.load().unwrap().unwrap(), before);
    }

    #[test]
    fn chatgpt_rejects_invalid_models_and_reserved_custom_names() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();
        for models in [
            vec![],
            vec!["".into()],
            vec!["a".into(), "a".into()],
            vec![" a".into()],
            vec!["a\nb".into()],
        ] {
            assert!(manager.save_chatgpt(models, None).is_err());
        }
        for name in ["CHATGPT", "chatgpt (codex)"] {
            assert_eq!(
                manager
                    .save_custom(name, "https://example.com", Some("key"), vec!["m".into()])
                    .unwrap_err()
                    .kind(),
                ConfigurationMutationErrorKind::ProviderNameCollision
            );
        }
        assert_eq!(fixture.repository.save_count(), 0);
    }

    struct ManagerFixture {
        user: UserConfig,
        credentials: MemoryCredentialStore,
        repository: MemoryUserConfigStore,
    }

    fn test_mcp_server(command: &str) -> McpServerConfig {
        McpServerConfig {
            enabled: true,
            startup_timeout_ms: 20_000,
            tool_timeout_ms: 60_000,
            approval: crate::services::mcp::McpApprovalPolicy::Prompt,
            tool_approval: Default::default(),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            transport: crate::services::mcp::McpTransportConfig::Stdio {
                command: command.to_owned(),
                args: Vec::new(),
                env: Default::default(),
                env_vars: Vec::new(),
                cwd: None,
            },
        }
    }

    impl ManagerFixture {
        fn new() -> Self {
            Self {
                user: UserConfig::default(),
                credentials: MemoryCredentialStore::default(),
                repository: MemoryUserConfigStore {
                    path: PathBuf::from("/fixture/.glint/config.yaml"),
                    ..MemoryUserConfigStore::default()
                },
            }
        }

        fn enable_builtin(&mut self, provider: &str, key: &str) {
            self.user.configured_providers.push(provider.to_owned());
            self.credentials.insert(&format!("builtin:{provider}"), key);
        }

        fn try_manager(&self) -> Result<ConfigurationManager> {
            self.repository.replace(self.user.clone());
            ConfigurationManager::new(
                GlintPaths::from_home("/fixture"),
                PathBuf::from("/workspace"),
                ProviderCatalog::embedded().unwrap(),
                Box::new(self.repository.clone()),
                Box::new(self.credentials.clone()),
            )
        }

        fn manager(&self) -> ConfigurationManager {
            self.try_manager().unwrap()
        }
    }

    #[derive(Clone, Default)]
    struct MemoryUserConfigStore {
        config: Arc<Mutex<Option<UserConfig>>>,
        fail_save: Arc<Mutex<bool>>,
        save_calls: Arc<std::sync::atomic::AtomicUsize>,
        path: PathBuf,
    }

    #[derive(Clone)]
    struct ChangingUserConfigStore {
        path: PathBuf,
        exact: UserConfig,
        intervening: UserConfig,
        loads: Arc<std::sync::atomic::AtomicUsize>,
        saves: Arc<std::sync::atomic::AtomicUsize>,
        saved: Arc<Mutex<Option<UserConfig>>>,
    }

    impl ChangingUserConfigStore {
        fn new(path: PathBuf, exact: UserConfig, intervening: UserConfig) -> Self {
            Self {
                path,
                exact,
                intervening,
                loads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                saves: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                saved: Arc::new(Mutex::new(None)),
            }
        }

        fn load_count(&self) -> usize {
            self.loads.load(std::sync::atomic::Ordering::Relaxed)
        }

        fn save_count(&self) -> usize {
            self.saves.load(std::sync::atomic::Ordering::Relaxed)
        }

        fn saved(&self) -> Option<UserConfig> {
            self.saved.lock().unwrap().clone()
        }
    }

    impl UserConfigRepository for ChangingUserConfigStore {
        fn path(&self) -> &Path {
            &self.path
        }

        fn load(&self) -> Result<Option<UserConfig>> {
            let call = self
                .loads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if call == 0 {
                return Ok(Some(UserConfig::default()));
            }
            if call == 1 {
                fs::write(
                    &self.path,
                    serde_yaml::to_string(&self.intervening).unwrap(),
                )
                .unwrap();
                return Ok(Some(self.exact.clone()));
            }
            bail!("unexpected extra configuration load")
        }

        fn save(&self, config: &UserConfig) -> Result<()> {
            self.saves
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *self.saved.lock().unwrap() = Some(config.clone());
            fs::write(&self.path, serde_yaml::to_string(config).unwrap()).unwrap();
            Ok(())
        }
    }

    impl MemoryUserConfigStore {
        fn replace(&self, config: UserConfig) {
            *self.config.lock().unwrap() = Some(config);
        }

        fn fail_saves(&self) {
            *self.fail_save.lock().unwrap() = true;
        }

        fn save_count(&self) -> usize {
            self.save_calls.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl UserConfigRepository for MemoryUserConfigStore {
        fn path(&self) -> &Path {
            &self.path
        }

        fn load(&self) -> Result<Option<UserConfig>> {
            Ok(self.config.lock().unwrap().clone())
        }

        fn save(&self, config: &UserConfig) -> Result<()> {
            self.save_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if *self.fail_save.lock().unwrap() {
                bail!("injected user config save failure");
            }
            *self.config.lock().unwrap() = Some(config.clone());
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct MemoryCredentialStore {
        values: Arc<Mutex<BTreeMap<String, String>>>,
        set_calls: Arc<std::sync::atomic::AtomicUsize>,
        fail_set_call: Arc<Mutex<Option<usize>>>,
        fail_delete: Arc<Mutex<bool>>,
    }

    impl MemoryCredentialStore {
        fn insert(&self, id: &str, value: &str) {
            self.values
                .lock()
                .unwrap()
                .insert(id.to_owned(), value.to_owned());
        }

        fn fail_set_call(&self, call: usize) {
            *self.fail_set_call.lock().unwrap() = Some(call);
        }

        fn fail_deletes(&self) {
            *self.fail_delete.lock().unwrap() = true;
        }
    }

    impl CredentialStore for MemoryCredentialStore {
        fn get(&self, id: &CredentialId) -> Result<Option<String>> {
            Ok(self.values.lock().unwrap().get(id.as_str()).cloned())
        }

        fn set(&self, id: &CredentialId, api_key: &str) -> Result<()> {
            let call = self
                .set_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            if *self.fail_set_call.lock().unwrap() == Some(call) {
                bail!("injected credential set failure");
            }
            self.insert(id.as_str(), api_key);
            Ok(())
        }

        fn delete(&self, id: &CredentialId) -> Result<()> {
            if *self.fail_delete.lock().unwrap() {
                bail!("injected credential delete failure");
            }
            self.values.lock().unwrap().remove(id.as_str());
            Ok(())
        }
    }
}
