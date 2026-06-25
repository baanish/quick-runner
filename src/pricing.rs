//! AI token-price table sourced from [models.dev](https://models.dev).
//!
//! The full catalog (~2.3 MB, 144 providers) is fetched at `qr init` and via
//! `qr cost --refresh`, then distilled into a slim local snapshot
//! (`prices.json`) so cost estimation needs no network at command time.
//!
//! Prices are USD per **million** tokens. A model is often carried by many
//! providers at slightly different prices. To pick one without a hardcoded
//! provider list, we use the model slug's namespace: when a carrier keys the
//! model as `maker/model` (e.g. `zai/glm-5.2`) and `maker` is itself a provider,
//! that maker's own price wins. Otherwise we take the **median** across all
//! carriers (resellers cluster around the real price; outliers are rejected).
//! Subscription/"coding plan" entries priced 0/0 are skipped.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// Per-million-token price for a model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Price {
    /// USD per million input tokens.
    pub input: f64,
    /// USD per million output tokens.
    pub output: f64,
}

impl Price {
    /// Estimated cost in USD for the given token counts.
    pub fn cost(self, input_tokens: u64, output_tokens: u64) -> f64 {
        (input_tokens as f64 * self.input + output_tokens as f64 * self.output) / 1_000_000.0
    }
}

/// Slim, normalized-model-id → price table.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PriceTable {
    models: HashMap<String, Price>,
}

impl PriceTable {
    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Look up a configured model name (case-insensitive; any `provider/` prefix
    /// is stripped, so `zai/GLM-5.2` and `GLM-5.2` both resolve to `glm-5.2`).
    pub fn get(&self, model: &str) -> Option<Price> {
        self.models.get(&normalize_model(model)).copied()
    }

    /// Distill a slim table from the full models.dev JSON: maker-from-slug-prefix
    /// where resolvable, else the median across carriers.
    fn from_models_dev(json: &serde_json::Value) -> Self {
        let Some(providers) = json.as_object() else {
            return Self::default();
        };

        // provider id -> (normalized model id -> price)
        let mut by_provider: HashMap<String, HashMap<String, Price>> = HashMap::new();
        // normalized model id -> every carrier's price
        let mut carriers: HashMap<String, Vec<Price>> = HashMap::new();
        // normalized model id -> "maker" hints (first slug segment of prefixed keys)
        let mut maker_hints: HashMap<String, HashSet<String>> = HashMap::new();

        for (provider_id, provider) in providers {
            let Some(catalog) = provider.get("models").and_then(|m| m.as_object()) else {
                continue;
            };
            for (raw_id, model) in catalog {
                let (Some(input), Some(output)) = (
                    model
                        .pointer("/cost/input")
                        .and_then(serde_json::Value::as_f64),
                    model
                        .pointer("/cost/output")
                        .and_then(serde_json::Value::as_f64),
                ) else {
                    continue;
                };
                // Skip malformed prices (negative or non-finite) and free /
                // subscription-plan placeholders priced 0/0, so a bad upstream
                // entry can never surface as a negative or non-finite cost.
                if !input.is_finite()
                    || !output.is_finite()
                    || input < 0.0
                    || output < 0.0
                    || (input == 0.0 && output == 0.0)
                {
                    continue;
                }
                let price = Price { input, output };
                let norm = normalize_model(raw_id);
                by_provider
                    .entry(provider_id.to_ascii_lowercase())
                    .or_default()
                    .insert(norm.clone(), price);
                carriers.entry(norm.clone()).or_default().push(price);
                if let Some((maker, _)) = raw_id.split_once('/') {
                    maker_hints
                        .entry(norm)
                        .or_default()
                        .insert(maker.to_ascii_lowercase());
                }
            }
        }

        let mut models = HashMap::with_capacity(carriers.len());
        for (norm, all) in &carriers {
            let maker_prices: Vec<Price> = maker_hints
                .get(norm)
                .into_iter()
                .flatten()
                .filter_map(|maker| by_provider.get(maker)?.get(norm).copied())
                .collect();
            let pool = if maker_prices.is_empty() {
                all
            } else {
                &maker_prices
            };
            models.insert(norm.clone(), median_price(pool));
        }

        Self { models }
    }
}

/// Normalize a model id for lookup: drop any `provider/` prefix and lowercase.
fn normalize_model(model: &str) -> String {
    model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase()
}

/// Element-wise median of input and output prices across a non-empty pool.
fn median_price(prices: &[Price]) -> Price {
    Price {
        input: median(prices.iter().map(|p| p.input)),
        output: median(prices.iter().map(|p| p.output)),
    }
}

fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut sorted: Vec<f64> = values.collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    match sorted.len() {
        0 => 0.0,
        n if n % 2 == 1 => sorted[n / 2],
        // Halve each side before summing so two large finite prices can't
        // overflow to infinity (e.g. f64::MAX + f64::MAX).
        n => sorted[n / 2 - 1] / 2.0 + sorted[n / 2] / 2.0,
    }
}

fn price_table_path(config_dir: &Path) -> PathBuf {
    config_dir.join("prices.json")
}

/// Fetch models.dev, distill the slim table, and save it. Returns the model
/// count written.
pub fn refresh(config_dir: &Path) -> Result<usize> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let body = client
        .get(MODELS_DEV_URL)
        .send()
        .context("Failed to reach models.dev")?
        .text()
        .context("Failed to read models.dev response")?;
    let json: serde_json::Value =
        serde_json::from_str(&body).context("Failed to parse models.dev response")?;
    let table = PriceTable::from_models_dev(&json);
    crate::atomic::write(&price_table_path(config_dir), &serde_json::to_vec(&table)?)?;
    Ok(table.len())
}

/// Load the saved price table, if any.
pub fn load(config_dir: &Path) -> Option<PriceTable> {
    let raw = std::fs::read(price_table_path(config_dir)).ok()?;
    serde_json::from_slice(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "openai": { "models": { "gpt-4o-mini": { "cost": { "input": 0.15, "output": 0.6 } } } },
      "zai": { "models": { "glm-5.2": { "cost": { "input": 1.4, "output": 4.4 } } } },
      "vercel": { "models": { "zai/glm-5.2": { "cost": { "input": 1.5, "output": 4.5 } } } },
      "some-plan": { "models": { "glm-5.2": { "cost": { "input": 0, "output": 0 } } } },
      "p1": { "models": { "widget": { "cost": { "input": 1.0, "output": 2.0 } } } },
      "p2": { "models": { "widget": { "cost": { "input": 3.0, "output": 6.0 } } } },
      "p3": { "models": { "widget": { "cost": { "input": 5.0, "output": 10.0 } } } }
    }"#;

    fn sample_table() -> PriceTable {
        PriceTable::from_models_dev(&serde_json::from_str(SAMPLE).unwrap())
    }

    #[test]
    fn slug_prefix_resolves_to_the_maker_price() {
        // vercel keys it `zai/glm-5.2` -> maker `zai` is a provider, so zai's own
        // price (1.4/4.4) wins over vercel's 1.5/4.5 and the 0/0 plan, regardless
        // of case or a prefix on the configured model.
        let table = sample_table();
        let zai = Price {
            input: 1.4,
            output: 4.4,
        };
        assert_eq!(table.get("GLM-5.2"), Some(zai));
        assert_eq!(table.get("zai/glm-5.2"), Some(zai));
    }

    #[test]
    fn median_is_used_without_a_resolvable_maker() {
        // `widget` is carried unprefixed by three providers: median of inputs
        // (1,3,5)=3 and outputs (2,6,10)=6.
        assert_eq!(
            sample_table().get("widget"),
            Some(Price {
                input: 3.0,
                output: 6.0
            })
        );
        assert_eq!(sample_table().get("nonexistent-model"), None);
    }

    #[test]
    fn malformed_negative_price_is_skipped() {
        // A one-sided negative cost is malformed and must not surface as a
        // negative cost estimate; the model is dropped from the table.
        let json = serde_json::from_str(
            r#"{"evil":{"models":{"bad":{"cost":{"input":-10.0,"output":1.0}}}}}"#,
        )
        .unwrap();
        let table = PriceTable::from_models_dev(&json);
        assert_eq!(table.get("bad"), None);
    }

    #[test]
    fn even_median_of_huge_prices_stays_finite() {
        // Two carriers each at f64::MAX must not average to infinity (which would
        // serialize as JSON null and make the saved table unloadable).
        let json = serde_json::from_str(
            r#"{
              "p1":{"models":{"huge":{"cost":{"input":1.7976931348623157e308,"output":1.0}}}},
              "p2":{"models":{"huge":{"cost":{"input":1.7976931348623157e308,"output":1.0}}}}
            }"#,
        )
        .unwrap();
        let price = PriceTable::from_models_dev(&json).get("huge").unwrap();
        assert!(
            price.input.is_finite(),
            "median overflowed to {}",
            price.input
        );
        assert_eq!(price.input, f64::MAX);
    }

    #[test]
    fn cost_is_priced_per_million_tokens() {
        // Pinned: gpt-4o-mini at $0.15/$0.60 per Mtok, 1000 in + 500 out.
        let price = sample_table().get("gpt-4o-mini").unwrap();
        let cost = price.cost(1_000, 500);
        assert!(
            (cost - 0.00045).abs() < 1e-9,
            "expected 0.00045, got {cost}"
        );
    }
}
