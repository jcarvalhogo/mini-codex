use crate::types::ChatMessage;
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::io::Write;
use std::time::Duration;
use tokio::time::MissedTickBehavior;

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    content: String,
}

pub(crate) async fn call_ollama(
    client: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<String> {
    print!("\nmodel thinking");
    std::io::stdout().flush()?;

    let request = call_ollama_request(client, ollama_url, model, messages);
    tokio::pin!(request);
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticks.tick().await;

    let result = loop {
        tokio::select! {
            result = &mut request => break result,
            _ = ticks.tick() => {
                print!(".");
                std::io::stdout().flush()?;
            }
        }
    };

    println!(" done");
    result
}

async fn call_ollama_request(
    client: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<String> {
    let request = ChatRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        stream: false,
    };

    let response = client
        .post(ollama_url)
        .json(&request)
        .send()
        .await
        .context("failed to call Ollama")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Ollama returned {status}: {body}");
    }

    let response = response
        .json::<ChatResponse>()
        .await
        .context("failed to parse Ollama response")?;

    Ok(response.message.content.trim().to_string())
}
