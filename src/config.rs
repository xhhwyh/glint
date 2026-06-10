use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Clone)]
pub struct Config {
    pub llm: LlmConfig,
    pub model_catalog: ModelCatalog,
    pub system_prompt: String,
}

#[derive(Clone)]
pub struct LlmConfig {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub providers: Vec<LlmProviderConfig>,
    pub temperature: f32,
    pub max_tokens: u32,
    pub context_window: Option<u64>,
    pub api_key: String,
}

#[derive(Clone)]
pub struct LlmProviderConfig {
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub api_key_env: String,
}

#[derive(Clone, Default, Deserialize)]
pub struct ModelCatalog {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderCatalogEntry>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelCatalogEntry>,
}

#[derive(Clone, Default, Deserialize)]
pub struct ProviderCatalogEntry {
    #[serde(default)]
    pub summary: String,
}

#[derive(Clone, Default, Deserialize)]
pub struct ModelCatalogEntry {
    #[serde(default)]
    pub positioning: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub price: String,
}

#[derive(Deserialize)]
struct FileConfig {
    llm: FileLlmConfig,
}

#[derive(Deserialize)]
struct FileLlmConfig {
    provider: String,
    model: String,
    providers: BTreeMap<String, FileProviderConfig>,
    temperature: f32,
    max_tokens: u32,
    context_window: Option<u64>,
}

#[derive(Deserialize)]
struct FileProviderConfig {
    base_url: String,
    models: Vec<String>,
    api_key_env: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let file = std::fs::read_to_string("config.toml").context("failed to read config.toml")?;
        let config: FileConfig = toml::from_str(&file).context("failed to parse config.toml")?;
        let model_catalog = ModelCatalog::load("model-info.toml")?;
        let system_prompt = std::fs::read_to_string("prompts/system.md")
            .context("failed to read prompts/system.md")?;

        Ok(Self {
            llm: config
                .llm
                .into_runtime_config(|api_key_env| std::env::var(api_key_env).ok())?,
            model_catalog,
            system_prompt,
        })
    }
}

impl ModelCatalog {
    fn load(path: &str) -> Result<Self> {
        let file =
            std::fs::read_to_string(path).with_context(|| format!("failed to read {path}"))?;
        toml::from_str(&file).with_context(|| format!("failed to parse {path}"))
    }
}

impl FileLlmConfig {
    fn into_runtime_config(
        self,
        resolve_api_key: impl Fn(&str) -> Option<String>,
    ) -> Result<LlmConfig> {
        let selected_provider = self.provider;
        let selected_model = self.model;
        let mut providers = Vec::new();
        for (name, provider) in self.providers {
            if provider.models.is_empty() {
                bail!("llm provider '{name}' does not define any models");
            }
            providers.push(LlmProviderConfig {
                name,
                base_url: provider.base_url.trim_end_matches('/').to_owned(),
                models: provider.models,
                api_key_env: provider.api_key_env,
            });
        }

        let mut config = LlmConfig {
            provider: String::new(),
            base_url: String::new(),
            model: String::new(),
            providers,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            context_window: self.context_window,
            api_key: String::new(),
        };
        config.switch_model(&selected_provider, &selected_model, resolve_api_key)?;
        Ok(config)
    }
}

impl LlmConfig {
    pub fn switch_model(
        &mut self,
        provider_name: &str,
        model: &str,
        resolve_api_key: impl Fn(&str) -> Option<String>,
    ) -> Result<()> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.name == provider_name)
            .cloned()
            .with_context(|| format!("llm provider '{provider_name}' is not defined"))?;
        if provider.models.is_empty() {
            bail!("llm provider '{provider_name}' does not define any models");
        }
        if !provider.models.iter().any(|candidate| candidate == model) {
            bail!("model '{model}' is not defined for provider '{provider_name}'");
        }

        let api_key = resolve_api_key(&provider.api_key_env)
            .with_context(|| format!("{} must be set", provider.api_key_env))?;

        self.provider = provider.name;
        self.base_url = provider.base_url;
        self.model = model.to_owned();
        self.api_key = api_key;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_selected_provider_with_global_defaults() {
        let config: FileConfig = toml::from_str(
            r#"
            [llm]
            provider = "deepseek"
            model = "deepseek-chat"
            temperature = 0.7
            max_tokens = 8196

            [llm.providers.deepseek]
            base_url = "https://api.deepseek.com/"
            models = ["deepseek-chat", "deepseek-reasoner"]
            api_key_env = "TEST_API_KEY"
            "#,
        )
        .unwrap();

        let llm = config.llm.into_runtime_config(fake_api_key).unwrap();

        assert_eq!(llm.base_url, "https://api.deepseek.com");
        assert_eq!(llm.provider, "deepseek");
        assert_eq!(llm.model, "deepseek-chat");
        assert_eq!(llm.providers[0].models, ["deepseek-chat", "deepseek-reasoner"]);
        assert_eq!(llm.temperature, 0.7);
        assert_eq!(llm.max_tokens, 8196);
        assert_eq!(llm.api_key, "secret");
    }

    #[test]
    fn reports_missing_selected_provider() {
        let config: FileConfig = toml::from_str(
            r#"
            [llm]
            provider = "missing"
            model = "deepseek-chat"
            temperature = 0.7
            max_tokens = 8196

            [llm.providers.deepseek]
            base_url = "https://api.deepseek.com"
            models = ["deepseek-chat"]
            api_key_env = "TEST_API_KEY"
            "#,
        )
        .unwrap();

        let error = match config.llm.into_runtime_config(fake_api_key) {
            Ok(_) => panic!("missing provider should fail"),
            Err(error) => error,
        };

        assert!(format!("{error:#}").contains("llm provider 'missing' is not defined"));
    }

    #[test]
    fn reports_missing_selected_model() {
        let config: FileConfig = toml::from_str(
            r#"
            [llm]
            provider = "deepseek"
            model = "missing"
            temperature = 0.7
            max_tokens = 8196

            [llm.providers.deepseek]
            base_url = "https://api.deepseek.com"
            models = ["deepseek-chat"]
            api_key_env = "TEST_API_KEY"
            "#,
        )
        .unwrap();

        let error = match config.llm.into_runtime_config(fake_api_key) {
            Ok(_) => panic!("missing model should fail"),
            Err(error) => error,
        };

        assert!(format!("{error:#}").contains("model 'missing' is not defined"));
    }

    #[test]
    fn failed_switch_preserves_current_model() {
        let config: FileConfig = toml::from_str(
            r#"
            [llm]
            provider = "deepseek"
            model = "deepseek-chat"
            temperature = 0.7
            max_tokens = 8196

            [llm.providers.deepseek]
            base_url = "https://api.deepseek.com"
            models = ["deepseek-chat"]
            api_key_env = "TEST_API_KEY"

            [llm.providers.other]
            base_url = "https://example.com"
            models = ["other-model"]
            api_key_env = "MISSING_API_KEY"
            "#,
        )
        .unwrap();

        let mut llm = config.llm.into_runtime_config(fake_api_key).unwrap();
        let error = llm
            .switch_model("other", "other-model", fake_api_key)
            .unwrap_err();

        assert!(format!("{error:#}").contains("MISSING_API_KEY must be set"));
        assert_eq!(llm.provider, "deepseek");
        assert_eq!(llm.model, "deepseek-chat");
        assert_eq!(llm.api_key, "secret");
    }

    fn fake_api_key(api_key_env: &str) -> Option<String> {
        (api_key_env == "TEST_API_KEY").then(|| "secret".to_owned())
    }
}
