use std::{sync::mpsc::Sender, thread};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::LlmConfig;

use super::AgentEvent;

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

pub fn spawn_agent_loop(prompt: String, config: LlmConfig, tx: Sender<AgentEvent>) {
    thread::spawn(move || {
        tx.send(AgentEvent::Started).ok();

        match complete_chat(prompt, config) {
            Ok(content) => {
                tx.send(AgentEvent::AssistantDelta(content)).ok();
                tx.send(AgentEvent::AssistantFinished).ok();
            }
            Err(error) => {
                tx.send(AgentEvent::Failed(format!("LLM error: {error:#}")))
                    .ok();
            }
        }
    });
}

fn complete_chat(prompt: String, config: LlmConfig) -> Result<String> {
    let request = ChatRequest {
        model: config.model,
        messages: vec![ChatMessage {
            role: "user",
            content: prompt,
        }],
        temperature: config.temperature,
        max_tokens: config.max_tokens,
    };

    let response: ChatResponse = ureq::post(&format!("{}/chat/completions", config.base_url))
        .set("Authorization", &format!("Bearer {}", config.api_key))
        .set("Content-Type", "application/json")
        .send_json(serde_json::to_value(request)?)
        .context("request failed")?
        .into_json()
        .context("invalid response")?;

    response
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content)
        .context("response did not include a message")
}
