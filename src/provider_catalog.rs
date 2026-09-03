use std::collections::HashSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer};

const EMBEDDED_PROVIDERS: &str = include_str!("../assets/providers.yaml");

#[derive(Clone, Debug, Deserialize)]
struct CatalogFile {
    providers: Vec<ProviderDefinition>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ProviderDefinition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub base_url: String,
    pub default_model: String,
    #[serde(default)]
    pub unit: String,
    pub models: Vec<ModelDefinition>,
    #[serde(default)]
    pub prompt_cache: PromptCacheConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ModelDefinition {
    pub name: String,
    #[serde(flatten)]
    pub metadata: ModelMetadata,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ModelMetadata {
    #[serde(default)]
    pub positioning: String,
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    pub context: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_display_string")]
    pub max_tokens: String,
    #[serde(default, deserialize_with = "deserialize_display_string")]
    pub price: String,
    #[serde(default, deserialize_with = "deserialize_display_string")]
    pub input: String,
    #[serde(default, deserialize_with = "deserialize_display_string")]
    pub output: String,
    #[serde(default, deserialize_with = "deserialize_display_string")]
    pub cache_read: String,
    #[serde(default, deserialize_with = "deserialize_display_string")]
    pub cache_write: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct PromptCacheConfig {
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub retention: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ProviderCatalog {
    providers: Vec<ProviderDefinition>,
}

impl ProviderCatalog {
    pub fn embedded() -> Result<Self> {
        Self::parse(EMBEDDED_PROVIDERS)
    }

    fn parse(yaml: &str) -> Result<Self> {
        let file: CatalogFile = serde_yaml::from_str(yaml)?;
        validate_catalog(&file.providers)?;
        Ok(Self {
            providers: file.providers,
        })
    }

    pub fn providers(&self) -> &[ProviderDefinition] {
        &self.providers
    }

    pub fn builtin(&self, id: &str) -> Option<&ProviderDefinition> {
        self.providers.iter().find(|provider| provider.id == id)
    }
}

fn validate_catalog(providers: &[ProviderDefinition]) -> Result<()> {
    let mut provider_ids = HashSet::new();
    for provider in providers {
        if provider.id.trim().is_empty() {
            bail!("provider id must not be empty");
        }
        if !provider_ids.insert(provider.id.to_ascii_lowercase()) {
            bail!("duplicate provider id '{}' (case-insensitive)", provider.id);
        }
        if provider.name.trim().is_empty() {
            bail!("provider '{}' has an empty name", provider.id);
        }
        if provider.base_url.trim().is_empty() {
            bail!("provider '{}' has an empty base URL", provider.id);
        }
        let url = reqwest::Url::parse(&provider.base_url).map_err(|error| {
            anyhow::anyhow!(
                "provider '{}' has an invalid base URL: {error}",
                provider.id
            )
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("provider '{}' base URL must use http or https", provider.id);
        }
        if provider.models.is_empty() {
            bail!("provider '{}' does not define any models", provider.id);
        }
        let mut model_names = HashSet::new();
        for model in &provider.models {
            if model.name.trim().is_empty() {
                bail!("provider '{}' has an empty model name", provider.id);
            }
            if !model_names.insert(model.name.to_ascii_lowercase()) {
                bail!(
                    "duplicate model '{}' for provider '{}', case-insensitive",
                    model.name,
                    provider.id
                );
            }
        }
        if !provider
            .models
            .iter()
            .any(|model| model.name == provider.default_model)
        {
            bail!(
                "provider '{}' default model '{}' is not declared",
                provider.id,
                provider.default_model
            );
        }
    }
    Ok(())
}

fn deserialize_display_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_yaml::Value::String(value) => value,
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::Bool(value) => value.to_string(),
        value => serde_yaml::to_string(&value)
            .map_err(serde::de::Error::custom)?
            .trim()
            .to_owned(),
    })
}

fn deserialize_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_yaml::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        serde_yaml::Value::Number(value) => value.as_u64(),
        serde_yaml::Value::String(value) => value.parse().ok(),
        _ => None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_contains_every_shipped_provider() {
        let catalog = ProviderCatalog::embedded().expect("embedded catalog");
        let ids = catalog
            .providers()
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            ids,
            [
                "deepseek",
                "volcengine",
                "zhipu",
                "kimi",
                "dashscope",
                "openrouter"
            ]
        );
    }

    #[test]
    fn every_builtin_default_is_a_declared_model() {
        let catalog = ProviderCatalog::embedded().expect("embedded catalog");

        for provider in catalog.providers() {
            assert!(
                provider
                    .models
                    .iter()
                    .any(|model| model.name == provider.default_model),
                "{} has an invalid default",
                provider.id
            );
        }
    }

    #[test]
    fn minimax_m3_has_no_max_tokens_metadata() {
        let catalog = ProviderCatalog::embedded().expect("embedded catalog");
        let model = catalog
            .builtin("dashscope")
            .expect("dashscope provider")
            .models
            .iter()
            .find(|model| model.name == "minimax-m3")
            .expect("minimax-m3 model");

        assert!(model.metadata.max_tokens.is_empty());
    }

    #[test]
    fn catalog_rejects_case_insensitive_provider_collisions() {
        let yaml = r#"
providers:
  - id: demo
    name: Demo
    base_url: https://one.example/v1
    default_model: one
    models: [{ name: one }]
  - id: DEMO
    name: Other
    base_url: https://two.example/v1
    default_model: two
    models: [{ name: two }]
"#;

        let error = ProviderCatalog::parse(yaml).unwrap_err();
        assert!(format!("{error:#}").contains("duplicate provider"));
    }
}
