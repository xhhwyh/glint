//! Verified model capabilities; see REASONING.md for sources.
use crate::chatgpt::{ReasoningLevel, ReasoningOptions};

pub fn options(provider: &str, model: &str) -> ReasoningOptions {
    let (default, levels): (&str, &[&str]) = match (provider, model) {
        ("deepseek", "deepseek-v4-flash" | "deepseek-v4-pro") => ("high", &["low", "high", "max"]),
        ("volcengine", "doubao-seed-2-1-pro-260628" | "doubao-seed-2-1-turbo-260628") => {
            ("high", &["minimal", "low", "medium", "high"])
        }
        (
            "volcengine",
            "doubao-seed-2-0-pro-260215"
            | "doubao-seed-2-0-lite-260428"
            | "doubao-seed-2-0-mini-260428"
            | "doubao-seed-2-0-code-preview-260215",
        ) => ("medium", &["minimal", "low", "medium", "high"]),
        ("zhipu", "glm-5.2") => ("max", &["high", "max"]),
        // OpenRouter's deployed model is the 0423 revision, not the updated native API.
        ("openrouter", "deepseek/deepseek-v4-flash") => ("high", &["high", "xhigh"]),
        _ => return ReasoningOptions::default(),
    };
    ReasoningOptions {
        default_effort: Some(default.into()),
        levels: levels
            .iter()
            .map(|effort| ReasoningLevel {
                effort: (*effort).into(),
                description: String::new(),
            })
            .collect(),
    }
}

pub fn request_fields(
    provider: &str,
    model: &str,
    effort: Option<&str>,
) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let Some(effort) = effort else {
        return Ok(Default::default());
    };
    anyhow::ensure!(
        options(provider, model)
            .levels
            .iter()
            .any(|level| level.effort == effort),
        "reasoning effort '{effort}' is not supported for {provider}/{model}"
    );
    let value = if provider == "openrouter" {
        serde_json::json!({"reasoning": {"effort": effort}})
    } else {
        serde_json::json!({"reasoning_effort": effort, "thinking": {"type": if effort == "minimal" { "disabled" } else { "enabled" }}})
    };
    Ok(value.as_object().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_specific_to_provider_and_model() {
        assert_eq!(options("deepseek", "deepseek-v4-pro").levels.len(), 3);
        assert_eq!(options("zhipu", "glm-5.2").levels.len(), 2);
        for (provider, model) in [
            ("zhipu", "glm-5.1"),
            ("kimi", "kimi-k2.7-code"),
            ("dashscope", "qwen3.7-max"),
            ("custom", "deepseek-v4-pro"),
        ] {
            assert!(options(provider, model).levels.is_empty());
        }
        assert!(request_fields("deepseek", "deepseek-v4-pro", Some("ultra")).is_err());
        assert!(request_fields("openrouter", "deepseek/deepseek-v4-flash", Some("max")).is_err());
    }

    #[test]
    fn request_fields_use_the_gateway_protocol_and_omit_defaults() {
        assert!(
            request_fields("unknown", "unknown", None)
                .unwrap()
                .is_empty()
        );
        let native = request_fields("deepseek", "deepseek-v4-pro", Some("max")).unwrap();
        assert_eq!(native["reasoning_effort"], "max");
        assert_eq!(native["thinking"]["type"], "enabled");
        let gateway =
            request_fields("openrouter", "deepseek/deepseek-v4-flash", Some("xhigh")).unwrap();
        assert_eq!(gateway["reasoning"]["effort"], "xhigh");
        assert!(!gateway.contains_key("reasoning_effort"));
    }
}
