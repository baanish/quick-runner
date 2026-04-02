use std::{env, time::Duration};

use anyhow::{Context, Result, anyhow};
use reqwest::{StatusCode, blocking::Client};
use serde_json::{Value, json};

use crate::ai::providers::{AiProtocol, ProviderConfig};

#[derive(Debug, Clone)]
pub struct AiClient {
    http: Client,
    primary: ProviderConfig,
    fallback: Option<ProviderConfig>,
}

#[derive(Debug, Clone)]
pub struct AiResponse {
    pub output_text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: f64,
    pub provider_label: String,
}

impl AiClient {
    pub fn new(primary: ProviderConfig, fallback: Option<ProviderConfig>) -> Self {
        Self::with_timeout(primary, fallback, Duration::from_secs(10))
    }

    pub fn with_timeout(
        primary: ProviderConfig,
        fallback: Option<ProviderConfig>,
        timeout: Duration,
    ) -> Self {
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client should build");
        Self {
            http,
            primary,
            fallback,
        }
    }

    pub fn primary_provider_label(&self) -> String {
        provider_label(&self.primary)
    }

    pub fn fallback_provider_label(&self) -> Option<String> {
        self.fallback.as_ref().map(provider_label)
    }

    pub fn execute_prompt(&self, system_prompt: &str, user_message: &str) -> Result<AiResponse> {
        match self.send_prompt(&self.primary, system_prompt, user_message) {
            Ok(response) => Ok(response),
            Err(primary_error) => {
                if let Some(fallback) = &self.fallback {
                    self.send_prompt(fallback, system_prompt, user_message)
                        .map_err(|fallback_error| {
                            anyhow!(
                                "Primary provider failed: {primary_error}. Fallback provider failed: {fallback_error}"
                            )
                        })
                } else {
                    Err(primary_error)
                }
            }
        }
    }

    fn send_prompt(
        &self,
        provider: &ProviderConfig,
        system_prompt: &str,
        user_message: &str,
    ) -> Result<AiResponse> {
        let api_key = env::var(&provider.api_key_env)
            .with_context(|| format!("Missing API key in {}", provider.api_key_env))?;
        let url = endpoint_url(provider);

        let request = match provider.protocol {
            AiProtocol::OpenAi => self
                .http
                .post(url)
                .bearer_auth(api_key)
                .json(&json!({
                    "model": provider.model,
                    "messages": [
                        {"role": "system", "content": system_prompt},
                        {"role": "user", "content": user_message}
                    ]
                })),
            AiProtocol::Anthropic => self
                .http
                .post(url)
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&json!({
                    "model": provider.model,
                    "system": system_prompt,
                    "max_tokens": 1024,
                    "messages": [
                        {"role": "user", "content": user_message}
                    ]
                })),
        };

        let response = request.send().context("Failed to reach AI provider")?;
        let status = response.status();
        let body = response.text().context("Failed to read AI response body")?;

        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow!("AI provider rate limited the request"));
        }
        if !status.is_success() {
            return Err(anyhow!(
                "AI provider returned HTTP {}: {}",
                status.as_u16(),
                extract_error_message(&body)
            ));
        }

        let json: Value = serde_json::from_str(&body).context("Failed to parse AI response")?;
        parse_response(provider, &json)
    }
}

fn endpoint_url(provider: &ProviderConfig) -> String {
    let base = provider.base_url.trim_end_matches('/');
    match provider.protocol {
        AiProtocol::OpenAi => format!("{base}/chat/completions"),
        AiProtocol::Anthropic => format!("{base}/messages"),
    }
}

fn parse_response(provider: &ProviderConfig, json: &Value) -> Result<AiResponse> {
    let (output_text, input_tokens, output_tokens) = match provider.protocol {
        AiProtocol::OpenAi => (
            json.pointer("/choices/0/message/content")
                .and_then(parse_message_content)
                .ok_or_else(|| anyhow!("OpenAI-compatible response missing message content"))?,
            json.pointer("/usage/prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            json.pointer("/usage/completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ),
        AiProtocol::Anthropic => (
            json.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    if item.get("type").and_then(Value::as_str) == Some("text") {
                        item.get("text").and_then(Value::as_str)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(""),
            json.pointer("/usage/input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            json.pointer("/usage/output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ),
    };

    if output_text.trim().is_empty() {
        return Err(anyhow!("AI response did not contain any text output"));
    }

    Ok(AiResponse {
        output_text,
        input_tokens,
        output_tokens,
        estimated_cost_usd: 0.0,
        provider_label: provider_label(provider),
    })
}

fn parse_message_content(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }

    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(Value::as_str) == Some("text") {
                    item.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("")
    })
}

fn extract_error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|json| {
            json.pointer("/error/message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| json.get("error").and_then(Value::as_str).map(ToOwned::to_owned))
        })
        .unwrap_or_else(|| body.trim().to_string())
}

fn provider_label(provider: &ProviderConfig) -> String {
    match provider.protocol {
        AiProtocol::OpenAi => format!("OpenAI-compatible ({})", provider.base_url),
        AiProtocol::Anthropic => format!("Anthropic-compatible ({})", provider.base_url),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;
    use crate::ai::providers::AiProtocol;

    #[test]
    fn openai_client_parses_text_response() {
        let server = spawn_server(
            200,
            r#"{"choices":[{"message":{"content":"cargo test"}}],"usage":{"prompt_tokens":12,"completion_tokens":4}}"#,
        );
        unsafe {
            std::env::set_var("QR_TEST_AI_KEY", "token");
        }

        let client = AiClient::new(
            ProviderConfig {
                protocol: AiProtocol::OpenAi,
                base_url: server,
                model: "demo".into(),
                api_key_env: "QR_TEST_AI_KEY".into(),
            },
            None,
        );

        let response = client
            .execute_prompt("You classify tasks", "run tests")
            .unwrap();
        assert_eq!(response.output_text, "cargo test");
        assert_eq!(response.input_tokens, 12);
        assert_eq!(response.output_tokens, 4);

        unsafe {
            std::env::remove_var("QR_TEST_AI_KEY");
        }
    }

    #[test]
    fn anthropic_client_parses_text_response() {
        let server = spawn_server(
            200,
            r#"{"content":[{"type":"text","text":"delegate"}],"usage":{"input_tokens":7,"output_tokens":3}}"#,
        );
        unsafe {
            std::env::set_var("QR_TEST_AI_KEY", "token");
        }

        let client = AiClient::new(
            ProviderConfig {
                protocol: AiProtocol::Anthropic,
                base_url: server,
                model: "claude-demo".into(),
                api_key_env: "QR_TEST_AI_KEY".into(),
            },
            None,
        );

        let response = client
            .execute_prompt("You classify tasks", "refactor auth")
            .unwrap();
        assert_eq!(response.output_text, "delegate");
        assert_eq!(response.input_tokens, 7);
        assert_eq!(response.output_tokens, 3);

        unsafe {
            std::env::remove_var("QR_TEST_AI_KEY");
        }
    }

    #[test]
    fn rate_limit_errors_are_reported_cleanly() {
        let server = spawn_server(429, r#"{"error":{"message":"slow down"}}"#);
        unsafe {
            std::env::set_var("QR_TEST_AI_KEY", "token");
        }

        let client = AiClient::new(
            ProviderConfig {
                protocol: AiProtocol::OpenAi,
                base_url: server,
                model: "demo".into(),
                api_key_env: "QR_TEST_AI_KEY".into(),
            },
            None,
        );

        let error = client
            .execute_prompt("You classify tasks", "run tests")
            .unwrap_err();
        assert!(error.to_string().contains("rate limited"));

        unsafe {
            std::env::remove_var("QR_TEST_AI_KEY");
        }
    }

    fn spawn_server(status: u16, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0_u8; 4096];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        format!("http://{addr}/v1")
    }
}
