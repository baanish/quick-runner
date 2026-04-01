use anyhow::{Result, bail};
use reqwest::Client;

use crate::ai::providers::{AiProtocol, ProviderConfig};

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AiClient {
    http: Client,
    primary: ProviderConfig,
    fallback: Option<ProviderConfig>,
}

#[allow(dead_code)]
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
        Self {
            http: Client::new(),
            primary,
            fallback,
        }
    }

    pub fn primary_provider_label(&self) -> String {
        provider_label(&self.primary)
    }

    #[allow(dead_code)]
    pub fn fallback_provider_label(&self) -> Option<String> {
        self.fallback.as_ref().map(provider_label)
    }

    #[allow(dead_code)]
    pub fn execute_prompt(&self, _prompt: &str) -> Result<AiResponse> {
        let _ = &self.http;
        bail!("AI-backed commands are not part of QuickRunner v1")
    }
}

fn provider_label(provider: &ProviderConfig) -> String {
    match provider.protocol {
        AiProtocol::OpenAi => format!("OpenAI-compatible ({})", provider.base_url),
        AiProtocol::Anthropic => format!("Anthropic-compatible ({})", provider.base_url),
    }
}
