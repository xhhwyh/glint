use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone)]
pub struct Config {
    pub llm: LlmConfig,
    pub system_prompt: String,
}

#[derive(Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub context_window: Option<u64>,
    pub api_key: String,
}

#[derive(Deserialize)]
struct FileConfig {
    llm: FileLlmConfig,
}

#[derive(Deserialize)]
struct FileLlmConfig {
    provider: String,
    providers: BTreeMap<String, FileProviderConfig>,
    temperature: f32,
    max_tokens: u32,
    context_window: Option<u64>,
}

#[derive(Deserialize)]
struct FileProviderConfig {
    base_url: String,
    model: String,
    api_key_env: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let file = std::fs::read_to_string("config.toml").context("failed to read config.toml")?;
        let config: FileConfig = toml::from_str(&file).context("failed to parse config.toml")?;
        let system_prompt = std::fs::read_to_string("prompts/system.md")
            .context("failed to read prompts/system.md")?;

        Ok(Self {
            llm: config
                .llm
                .into_runtime_config(|api_key_env| std::env::var(api_key_env).ok())?,
            system_prompt,
        })
    }
}

impl FileLlmConfig {
    fn into_runtime_config(
        self,
        resolve_api_key: impl FnOnce(&str) -> Option<String>,
    ) -> Result<LlmConfig> {
        let provider = self
            .providers
            .get(&self.provider)
            .with_context(|| format!("llm provider '{}' is not defined", self.provider))?;
        let api_key = resolve_api_key(&provider.api_key_env)
            .with_context(|| format!("{} must be set", provider.api_key_env))?;

        Ok(LlmConfig {
            base_url: provider.base_url.trim_end_matches('/').to_owned(),
            model: provider.model.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            context_window: self.context_window,
            api_key,
        })
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
            temperature = 0.7
            max_tokens = 8196

            [llm.providers.deepseek]
            base_url = "https://api.deepseek.com/"
            model = "deepseek-v4-flash"
            api_key_env = "TEST_API_KEY"
            "#,
        )
        .unwrap();

        let llm = config.llm.into_runtime_config(fake_api_key).unwrap();

        assert_eq!(llm.base_url, "https://api.deepseek.com");
        assert_eq!(llm.model, "deepseek-v4-flash");
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
            temperature = 0.7
            max_tokens = 8196

            [llm.providers.deepseek]
            base_url = "https://api.deepseek.com"
            model = "deepseek-v4-flash"
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

    fn fake_api_key(api_key_env: &str) -> Option<String> {
        (api_key_env == "TEST_API_KEY").then(|| "secret".to_owned())
    }
}
