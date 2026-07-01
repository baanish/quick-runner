use std::{env, time::Duration};

use anyhow::{Context, Result, anyhow};
use reqwest::{StatusCode, blocking::Client};
use serde_json::{Value, json};

use crate::ai::providers::{AiProtocol, ProviderConfig};

/// Cap on the AI HTTP response body. Classification replies are a few KB at most
/// (`max_tokens` is 1024), so 8 MiB is generous while still bounding memory
/// against a malicious or buggy provider streaming an unbounded body.
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

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
        match self.send_prompt(&self.primary, "primary", system_prompt, user_message) {
            Ok(response) => Ok(response),
            Err(primary_error) => {
                if let Some(fallback) = &self.fallback {
                    self.send_prompt(fallback, "fallback", system_prompt, user_message)
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
        keychain_role: &str,
        system_prompt: &str,
        user_message: &str,
    ) -> Result<AiResponse> {
        let api_key = resolve_api_key(provider, keychain_role)?;
        let url = endpoint_url(provider);

        let request = match provider.protocol {
            AiProtocol::OpenAi => self.http.post(url).bearer_auth(api_key).json(&json!({
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
        let body = read_capped(response, MAX_RESPONSE_BYTES)?;

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

fn resolve_api_key(provider: &ProviderConfig, keychain_role: &str) -> Result<String> {
    // 1. explicit per-provider env var (highest-precedence override)
    if !provider.api_key_env.trim().is_empty() {
        if let Ok(value) = env::var(&provider.api_key_env) {
            if !value.trim().is_empty() {
                return Ok(value);
            }
        }
    }

    // 2. well-known env var for the protocol
    let well_known_env = provider.protocol.default_api_key_env();
    if let Ok(value) = env::var(well_known_env) {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }

    // 3. config file (only when the key was explicitly stored there)
    if !provider.api_key.trim().is_empty() {
        return Ok(provider.api_key.clone());
    }

    // 4. OS keychain — where `qr init` stores the key when you opt in. Checked
    //    last because it is only populated when the key is NOT in env or config,
    //    so the common paths never touch the keychain backend.
    if let Some(value) =
        crate::secret::get_for_role(keychain_role, &provider.api_key_env, well_known_env)
    {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }

    Err(anyhow!(
        "Missing API key. Checked {}, {}, config.toml, then the OS keychain",
        if provider.api_key_env.trim().is_empty() {
            "<no custom env var configured>"
        } else {
            provider.api_key_env.as_str()
        },
        well_known_env
    ))
}

/// Read an HTTP body into a `String`, buffering at most `max` bytes so a
/// malicious or buggy provider can't exhaust memory with an unbounded response.
fn read_capped(reader: impl std::io::Read, max: u64) -> Result<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    // Read one extra byte so "exactly at the cap" is distinguishable from "over".
    reader
        .take(max + 1)
        .read_to_end(&mut buf)
        .context("Failed to read AI response body")?;
    if buf.len() as u64 > max {
        anyhow::bail!("AI provider response exceeded the {max}-byte limit");
    }
    String::from_utf8(buf).context("AI response body was not valid UTF-8")
}

/// Parse a usage token count, accepting an integer or a finite float and
/// clamping anything negative or non-numeric to 0. The result is capped at
/// `i64::MAX` so a huge value (e.g. `1e100`, which saturates the float→u64 cast
/// to `u64::MAX`) can't later wrap negative when stored as a SQLite i64. A wrong
/// count only skews the non-critical cost estimate, so this never errors.
fn token_count(json: &Value, pointer: &str) -> u64 {
    json.pointer(pointer)
        .and_then(|value| {
            value.as_u64().or_else(|| {
                value
                    .as_f64()
                    .filter(|f| f.is_finite())
                    .map(|f| f.max(0.0) as u64)
            })
        })
        .map(|count| count.min(i64::MAX as u64))
        .unwrap_or(0)
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
            token_count(json, "/usage/prompt_tokens"),
            token_count(json, "/usage/completion_tokens"),
        ),
        AiProtocol::Anthropic => (
            collect_text_blocks(
                json.get("content")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            ),
            token_count(json, "/usage/input_tokens"),
            token_count(json, "/usage/output_tokens"),
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
    value.as_array().map(|items| collect_text_blocks(items))
}

/// Concatenate the `text` of every `{"type":"text", …}` block in a content array
/// (the shape used by Anthropic responses and OpenAI array-form message content).
fn collect_text_blocks(items: &[Value]) -> String {
    items
        .iter()
        .filter_map(|item| {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                item.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect()
}

fn extract_error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|json| {
            json.pointer("/error/message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    json.get("error")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
        })
        .unwrap_or_else(|| {
            // Non-JSON error page: surface a bounded, char-boundary-safe excerpt
            // rather than echoing a potentially huge body.
            let trimmed = body.trim();
            let excerpt: String = trimmed.chars().take(500).collect();
            if excerpt.len() < trimmed.len() {
                format!("{excerpt}…")
            } else {
                excerpt
            }
        })
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
    use crate::test_env_lock;

    fn clear_test_env() {
        for key in [
            "QR_TEST_AI_KEY",
            "CUSTOM_QR_TEST_AI_KEY",
            "CUSTOM_QR_TEST_ANTHROPIC_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
        ] {
            unsafe {
                std::env::remove_var(key);
            }
        }
    }

    #[test]
    fn openai_client_parses_text_response() {
        let _guard = test_env_lock().lock().unwrap();
        clear_test_env();
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
                api_key: String::new(),
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
        let _guard = test_env_lock().lock().unwrap();
        clear_test_env();
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
                api_key: String::new(),
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
        let _guard = test_env_lock().lock().unwrap();
        clear_test_env();
        let server = spawn_server(429, r#"{"error":{"message":"slow down"}}"#);
        unsafe {
            std::env::set_var("QR_TEST_AI_KEY", "token");
        }

        let client = AiClient::new(
            ProviderConfig {
                protocol: AiProtocol::OpenAi,
                base_url: server,
                model: "demo".into(),
                api_key: String::new(),
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

    #[test]
    fn custom_env_var_overrides_well_known_and_config_key() {
        let _guard = test_env_lock().lock().unwrap();
        clear_test_env();
        unsafe {
            std::env::set_var("CUSTOM_QR_TEST_AI_KEY", "custom-token");
            std::env::set_var("OPENAI_API_KEY", "well-known-token");
        }

        let api_key = resolve_api_key(
            &ProviderConfig {
                protocol: AiProtocol::OpenAi,
                base_url: "https://example.test/v1".into(),
                model: "demo".into(),
                api_key: "config-token".into(),
                api_key_env: "CUSTOM_QR_TEST_AI_KEY".into(),
            },
            "primary",
        )
        .unwrap();
        assert_eq!(api_key, "custom-token");

        unsafe {
            std::env::remove_var("CUSTOM_QR_TEST_AI_KEY");
            std::env::remove_var("OPENAI_API_KEY");
        }
    }

    #[test]
    fn well_known_env_var_is_used_when_custom_env_var_is_missing() {
        let _guard = test_env_lock().lock().unwrap();
        clear_test_env();
        unsafe {
            std::env::remove_var("CUSTOM_QR_TEST_ANTHROPIC_KEY");
            std::env::set_var("ANTHROPIC_API_KEY", "well-known-token");
        }

        let api_key = resolve_api_key(
            &ProviderConfig {
                protocol: AiProtocol::Anthropic,
                base_url: "https://example.test".into(),
                model: "claude-demo".into(),
                api_key: "config-token".into(),
                api_key_env: "CUSTOM_QR_TEST_ANTHROPIC_KEY".into(),
            },
            "primary",
        )
        .unwrap();
        assert_eq!(api_key, "well-known-token");

        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
    }

    #[test]
    fn config_key_is_used_when_no_env_vars_are_available() {
        let _guard = test_env_lock().lock().unwrap();
        clear_test_env();
        unsafe {
            std::env::remove_var("CUSTOM_QR_TEST_AI_KEY");
            std::env::remove_var("OPENAI_API_KEY");
        }

        let api_key = resolve_api_key(
            &ProviderConfig {
                protocol: AiProtocol::OpenAi,
                base_url: "https://example.test/v1".into(),
                model: "demo".into(),
                api_key: "config-token".into(),
                api_key_env: "CUSTOM_QR_TEST_AI_KEY".into(),
            },
            "primary",
        )
        .unwrap();
        assert_eq!(api_key, "config-token");
    }

    #[test]
    fn read_capped_rejects_oversized_body() {
        use std::io::Cursor;
        let big = vec![b'a'; 100];
        let err = read_capped(Cursor::new(big), 50).unwrap_err();
        assert!(err.to_string().contains("exceeded"));
    }

    #[test]
    fn read_capped_accepts_body_within_cap() {
        use std::io::Cursor;
        let body = read_capped(Cursor::new(b"hello".to_vec()), 50).unwrap();
        assert_eq!(body, "hello");
    }

    #[test]
    fn token_count_accepts_int_float_and_clamps_negative() {
        let json: Value =
            serde_json::from_str(r#"{"i":12,"f":34.0,"neg":-5,"str":"x","huge":1e100}"#).unwrap();
        assert_eq!(token_count(&json, "/i"), 12);
        assert_eq!(token_count(&json, "/f"), 34);
        assert_eq!(token_count(&json, "/neg"), 0);
        assert_eq!(token_count(&json, "/str"), 0);
        assert_eq!(token_count(&json, "/missing"), 0);
        // A huge float must clamp to i64::MAX, never u64::MAX (which would wrap
        // negative when stored as a SQLite i64).
        assert_eq!(token_count(&json, "/huge"), i64::MAX as u64);
    }

    #[test]
    fn error_message_excerpt_is_bounded() {
        let body = "x".repeat(5000);
        let message = extract_error_message(&body);
        assert!(message.chars().count() <= 501, "message not truncated");
        assert!(message.ends_with('…'));
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
