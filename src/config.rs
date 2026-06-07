use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone)]
pub struct Config {
    pub llm: LlmConfig,
}

#[derive(Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub api_key: String,
}

#[derive(Deserialize)]
struct FileConfig {
    llm: FileLlmConfig,
}

#[derive(Deserialize)]
struct FileLlmConfig {
    base_url: String,
    model: String,
    api_key_env: String,
    temperature: f32,
    max_tokens: u32,
}

impl Config {
    pub fn load() -> Result<Self> {
        let file = std::fs::read_to_string("config.toml").context("failed to read config.toml")?;
        let config: FileConfig = toml::from_str(&file).context("failed to parse config.toml")?;
        let api_key = std::env::var(&config.llm.api_key_env)
            .with_context(|| format!("{} must be set", config.llm.api_key_env))?;

        Ok(Self {
            llm: LlmConfig {
                base_url: config.llm.base_url.trim_end_matches('/').to_owned(),
                model: config.llm.model,
                temperature: config.llm.temperature,
                max_tokens: config.llm.max_tokens,
                api_key,
            },
        })
    }
}
