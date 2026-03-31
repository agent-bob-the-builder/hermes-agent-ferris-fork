//! Pure-Rust computation for usage pricing.
//!
//! Core functions (no network I/O):
//! - resolve_billing_route
//! - normalize_usage
//! - estimate_usage_cost
//! - has_known_pricing
//! - format_duration_compact
//! - format_token_count_compact

use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use pyo3::prelude::*;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ZERO: Decimal = Decimal::ZERO;
const ONE_MILLION: Decimal = Decimal::const_new(1_000_000, rust_decimal::prelude::Scale::new(0).unwrap());

fn dec(s: &str) -> Decimal {
    s.parse().unwrap_or(ZERO)
}

fn opt_dec(s: &str) -> Option<Decimal> {
    Some(dec(s))
}

// ---------------------------------------------------------------------------
// Data structures (Python-facing with pyclass)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[pyclass]
pub struct CanonicalUsage {
    #[pyo3(get, set)]
    pub input_tokens: i64,
    #[pyo3(get, set)]
    pub output_tokens: i64,
    #[pyo3(get, set)]
    pub cache_read_tokens: i64,
    #[pyo3(get, set)]
    pub cache_write_tokens: i64,
    #[pyo3(get, set)]
    pub reasoning_tokens: i64,
    #[pyo3(get, set)]
    pub request_count: i64,
}

impl CanonicalUsage {
    pub fn prompt_tokens(&self) -> i64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
    pub fn total_tokens(&self) -> i64 {
        self.prompt_tokens() + self.output_tokens
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[pyclass]
pub struct BillingRoute {
    #[pyo3(get, set)]
    pub provider: String,
    #[pyo3(get, set)]
    pub model: String,
    #[pyo3(get, set)]
    pub base_url: String,
    #[pyo3(get, set)]
    pub billing_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[pyclass]
pub struct PricingEntry {
    #[pyo3(get, set)]
    pub input_cost_per_million: Option<Decimal>,
    #[pyo3(get, set)]
    pub output_cost_per_million: Option<Decimal>,
    #[pyo3(get, set)]
    pub cache_read_cost_per_million: Option<Decimal>,
    #[pyo3(get, set)]
    pub cache_write_cost_per_million: Option<Decimal>,
    #[pyo3(get, set)]
    pub request_cost: Option<Decimal>,
    #[pyo3(get, set)]
    pub source: String,
    #[pyo3(get, set)]
    pub source_url: Option<String>,
    #[pyo3(get, set)]
    pub pricing_version: Option<String>,
}

impl Default for PricingEntry {
    fn default() -> Self {
        Self {
            input_cost_per_million: None,
            output_cost_per_million: None,
            cache_read_cost_per_million: None,
            cache_write_cost_per_million: None,
            request_cost: None,
            source: "none".to_string(),
            source_url: None,
            pricing_version: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[pyclass]
pub struct CostResult {
    #[pyo3(get, set)]
    pub amount_usd: Option<Decimal>,
    #[pyo3(get, set)]
    pub status: String,
    #[pyo3(get, set)]
    pub source: String,
    #[pyo3(get, set)]
    pub label: String,
    #[pyo3(get, set)]
    pub fetched_at: Option<String>,
    #[pyo3(get, set)]
    pub pricing_version: Option<String>,
    #[pyo3(get, set)]
    pub notes: Vec<String>,
}

impl CostResult {
    fn unknown(source: &str) -> Self {
        Self {
            amount_usd: None,
            status: "unknown".to_string(),
            source: source.to_string(),
            label: "n/a".to_string(),
            fetched_at: None,
            pricing_version: None,
            notes: vec![],
        }
    }
    fn included() -> Self {
        Self {
            amount_usd: Some(ZERO),
            status: "included".to_string(),
            source: "none".to_string(),
            label: "included".to_string(),
            fetched_at: None,
            pricing_version: Some("included-route".to_string()),
            notes: vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// Official docs pricing snapshot
// ---------------------------------------------------------------------------

type PricingKey = (String, String);

fn build_official_docs_pricing() -> HashMap<PricingKey, PricingEntry> {
    let mut m = HashMap::new();

    // Anthropic
    m.insert(
        ("anthropic".into(), "claude-opus-4-20250514".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("15.00"),
            output_cost_per_million: opt_dec("75.00"),
            cache_read_cost_per_million: opt_dec("1.50"),
            cache_write_cost_per_million: opt_dec("18.75"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching".into()),
            pricing_version: Some("anthropic-prompt-caching-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("anthropic".into(), "claude-sonnet-4-20250514".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("3.00"),
            output_cost_per_million: opt_dec("15.00"),
            cache_read_cost_per_million: opt_dec("0.30"),
            cache_write_cost_per_million: opt_dec("3.75"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching".into()),
            pricing_version: Some("anthropic-prompt-caching-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("anthropic".into(), "claude-3-5-sonnet-20241022".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("3.00"),
            output_cost_per_million: opt_dec("15.00"),
            cache_read_cost_per_million: opt_dec("0.30"),
            cache_write_cost_per_million: opt_dec("3.75"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching".into()),
            pricing_version: Some("anthropic-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("anthropic".into(), "claude-3-5-haiku-20241022".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("0.80"),
            output_cost_per_million: opt_dec("4.00"),
            cache_read_cost_per_million: opt_dec("0.08"),
            cache_write_cost_per_million: opt_dec("1.00"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching".into()),
            pricing_version: Some("anthropic-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("anthropic".into(), "claude-3-opus-20240229".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("15.00"),
            output_cost_per_million: opt_dec("75.00"),
            cache_read_cost_per_million: opt_dec("1.50"),
            cache_write_cost_per_million: opt_dec("18.75"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching".into()),
            pricing_version: Some("anthropic-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("anthropic".into(), "claude-3-haiku-20240307".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("0.25"),
            output_cost_per_million: opt_dec("1.25"),
            cache_read_cost_per_million: opt_dec("0.03"),
            cache_write_cost_per_million: opt_dec("0.30"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching".into()),
            pricing_version: Some("anthropic-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );

    // OpenAI
    m.insert(
        ("openai".into(), "gpt-4o".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("2.50"),
            output_cost_per_million: opt_dec("10.00"),
            cache_read_cost_per_million: opt_dec("1.25"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://openai.com/api/pricing/".into()),
            pricing_version: Some("openai-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("openai".into(), "gpt-4o-mini".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("0.15"),
            output_cost_per_million: opt_dec("0.60"),
            cache_read_cost_per_million: opt_dec("0.075"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://openai.com/api/pricing/".into()),
            pricing_version: Some("openai-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("openai".into(), "gpt-4.1".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("2.00"),
            output_cost_per_million: opt_dec("8.00"),
            cache_read_cost_per_million: opt_dec("0.50"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://openai.com/api/pricing/".into()),
            pricing_version: Some("openai-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("openai".into(), "gpt-4.1-mini".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("0.40"),
            output_cost_per_million: opt_dec("1.60"),
            cache_read_cost_per_million: opt_dec("0.10"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://openai.com/api/pricing/".into()),
            pricing_version: Some("openai-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("openai".into(), "gpt-4.1-nano".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("0.10"),
            output_cost_per_million: opt_dec("0.40"),
            cache_read_cost_per_million: opt_dec("0.025"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://openai.com/api/pricing/".into()),
            pricing_version: Some("openai-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("openai".into(), "o3".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("10.00"),
            output_cost_per_million: opt_dec("40.00"),
            cache_read_cost_per_million: opt_dec("2.50"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://openai.com/api/pricing/".into()),
            pricing_version: Some("openai-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("openai".into(), "o3-mini".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("1.10"),
            output_cost_per_million: opt_dec("4.40"),
            cache_read_cost_per_million: opt_dec("0.55"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://openai.com/api/pricing/".into()),
            pricing_version: Some("openai-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );

    // DeepSeek
    m.insert(
        ("deepseek".into(), "deepseek-chat".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("0.14"),
            output_cost_per_million: opt_dec("0.28"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://api-docs.deepseek.com/quick_start/pricing".into()),
            pricing_version: Some("deepseek-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("deepseek".into(), "deepseek-reasoner".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("0.55"),
            output_cost_per_million: opt_dec("2.19"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://api-docs.deepseek.com/quick_start/pricing".into()),
            pricing_version: Some("deepseek-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );

    // Google Gemini
    m.insert(
        ("google".into(), "gemini-2.5-pro".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("1.25"),
            output_cost_per_million: opt_dec("10.00"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://ai.google.dev/pricing".into()),
            pricing_version: Some("google-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("google".into(), "gemini-2.5-flash".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("0.15"),
            output_cost_per_million: opt_dec("0.60"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://ai.google.dev/pricing".into()),
            pricing_version: Some("google-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );
    m.insert(
        ("google".into(), "gemini-2.0-flash".into()),
        PricingEntry {
            input_cost_per_million: opt_dec("0.10"),
            output_cost_per_million: opt_dec("0.40"),
            source: "official_docs_snapshot".into(),
            source_url: Some("https://ai.google.dev/pricing".into()),
            pricing_version: Some("google-pricing-2026-03-16".into()),
            ..Default::default()
        },
    );

    m
}

static OFFICIAL_DOCS_PRICING: Lazy<HashMap<PricingKey, PricingEntry>> =
    Lazy::new(build_official_docs_pricing);

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn to_int(value: &serde_json::Value) -> i64 {
    match value {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Pure core functions
// ---------------------------------------------------------------------------

/// Resolve a BillingRoute from model name + optional provider/base_url.
/// Pure function -- no network I/O.
pub fn resolve_billing_route(
    model_name: &str,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> BillingRoute {
    let provider_name = provider.unwrap_or("").trim().to_lowercase();
    let base = base_url.unwrap_or("").trim().to_lowercase();
    let model = model_name.trim().to_string();

    let (provider_name, model) = if provider_name.is_empty() && model.contains('/') {
        let parts: Vec<&str> = model.splitn(2, '/').collect();
        let inferred = parts[0].to_lowercase();
        if ["anthropic", "openai", "google"].contains(&inferred.as_str()) {
            (inferred, parts[1].to_string())
        } else {
            (provider_name, model)
        }
    } else {
        (provider_name, model)
    };

    if provider_name == "openai-codex" {
        return BillingRoute {
            provider: "openai-codex".to_string(),
            model,
            base_url: base_url.unwrap_or("").to_string(),
            billing_mode: "subscription_included".to_string(),
        };
    }

    if provider_name == "openrouter" || base.contains("openrouter.ai") {
        return BillingRoute {
            provider: "openrouter".to_string(),
            model,
            base_url: base_url.unwrap_or("").to_string(),
            billing_mode: "official_models_api".to_string(),
        };
    }

    if provider_name == "anthropic" {
        return BillingRoute {
            provider: "anthropic".to_string(),
            model: model.split('/').last().unwrap_or(&model).to_string(),
            base_url: base_url.unwrap_or("").to_string(),
            billing_mode: "official_docs_snapshot".to_string(),
        };
    }

    if provider_name == "openai" {
        return BillingRoute {
            provider: "openai".to_string(),
            model: model.split('/').last().unwrap_or(&model).to_string(),
            base_url: base_url.unwrap_or("").to_string(),
            billing_mode: "official_docs_snapshot".to_string(),
        };
    }

    if provider_name == "custom"
        || provider_name == "local"
        || (!base.is_empty() && base.contains("localhost"))
    {
        return BillingRoute {
            provider: if provider_name.is_empty() {
                "custom".to_string()
            } else {
                provider_name
            },
            model,
            base_url: base_url.unwrap_or("").to_string(),
            billing_mode: "unknown".to_string(),
        };
    }

    BillingRoute {
        provider: if provider_name.is_empty() {
            "unknown".to_string()
        } else {
            provider_name
        },
        model: if model.is_empty() {
            String::new()
        } else {
            model.split('/').last().unwrap_or(&model).to_string()
        },
        base_url: base_url.unwrap_or("").to_string(),
        billing_mode: "unknown".to_string(),
    }
}

/// Normalize raw API response usage into CanonicalUsage.
/// Pure function -- no network I/O.
pub fn normalize_usage(
    response_usage: &serde_json::Value,
    provider: Option<&str>,
    api_mode: Option<&str>,
) -> CanonicalUsage {
    let null_val = &serde_json::Value::Null;
    if response_usage.is_null() {
        return CanonicalUsage::default();
    }

    let usage_map = match response_usage {
        serde_json::Value::Object(m) => m,
        _ => return CanonicalUsage::default(),
    };

    let provider_name = provider.unwrap_or("").trim().to_lowercase();
    let mode = api_mode.unwrap_or("").trim().to_lowercase();

    let (input_tokens, output_tokens, cache_read_tokens, cache_write_tokens) =
        if mode == "anthropic_messages" || provider_name == "anthropic" {
            (
                to_int(usage_map.get("input_tokens").unwrap_or(null_val)),
                to_int(usage_map.get("output_tokens").unwrap_or(null_val)),
                to_int(usage_map.get("cache_read_input_tokens").unwrap_or(null_val)),
                to_int(usage_map.get("cache_creation_input_tokens").unwrap_or(null_val)),
            )
        } else if mode == "codex_responses" {
            let input_total = to_int(usage_map.get("input_tokens").unwrap_or(null_val));
            let output_tokens = to_int(usage_map.get("output_tokens").unwrap_or(null_val));
            let details = usage_map.get("input_tokens_details");
            let cache_read_tokens: i64 = details
                .and_then(|d: &serde_json::Value| d.get("cached_tokens"))
                .map(|v| to_int(v))
                .unwrap_or(0);
            let cache_write_tokens: i64 = details
                .and_then(|d: &serde_json::Value| d.get("cache_creation_tokens"))
                .map(|v| to_int(v))
                .unwrap_or(0);
            let input_tokens = (input_total - cache_read_tokens - cache_write_tokens).max(0);
            (input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)
        } else {
            let prompt_total = to_int(usage_map.get("prompt_tokens").unwrap_or(null_val));
            let output_tokens = to_int(usage_map.get("completion_tokens").unwrap_or(null_val));
            let details = usage_map.get("prompt_tokens_details");
            let cache_read_tokens: i64 = details
                .and_then(|d: &serde_json::Value| d.get("cached_tokens"))
                .map(|v| to_int(v))
                .unwrap_or(0);
            let cache_write_tokens: i64 = details
                .and_then(|d: &serde_json::Value| d.get("cache_write_tokens"))
                .map(|v| to_int(v))
                .unwrap_or(0);
            let input_tokens = (prompt_total - cache_read_tokens - cache_write_tokens).max(0);
            (input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)
        };

    let output_details = usage_map.get("output_tokens_details");
    let reasoning_tokens: i64 = output_details
        .and_then(|d: &serde_json::Value| d.get("reasoning_tokens"))
        .map(|v| to_int(v))
        .unwrap_or(0);

    CanonicalUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        request_count: 1,
    }
}

/// Lookup official docs pricing entry for a billing route.
/// Pure function -- no network I/O.
pub fn lookup_official_docs_pricing(route: &BillingRoute) -> Option<PricingEntry> {
    OFFICIAL_DOCS_PRICING
        .get(&(route.provider.clone(), route.model.to_lowercase()))
        .cloned()
}

/// Check if known pricing exists for a model+route.
/// Pure function -- no network I/O.
pub fn has_known_pricing(model_name: &str, provider: Option<&str>, base_url: Option<&str>) -> bool {
    let route = resolve_billing_route(model_name, provider, base_url);
    if route.billing_mode == "subscription_included" {
        return true;
    }
    lookup_official_docs_pricing(&route).is_some()
}

/// Compute cost estimate from a CanonicalUsage and PricingEntry.
/// Pure function -- no network I/O.
pub fn compute_cost_from_usage(
    usage: &CanonicalUsage,
    entry: &PricingEntry,
) -> (Decimal, Vec<String>) {
    let mut amount = ZERO;
    let notes: Vec<String>;

    if usage.input_tokens > 0 && entry.input_cost_per_million.is_none() {
        return (Decimal::MAX, vec![]);
    }
    if usage.output_tokens > 0 && entry.output_cost_per_million.is_none() {
        return (Decimal::MAX, vec![]);
    }
    if usage.cache_read_tokens > 0 && entry.cache_read_cost_per_million.is_none() {
        notes = vec!["cache-read pricing unavailable for route".to_string()];
        return (Decimal::MAX, notes);
    }
    if usage.cache_write_tokens > 0 && entry.cache_write_cost_per_million.is_none() {
        notes = vec!["cache-write pricing unavailable for route".to_string()];
        return (Decimal::MAX, notes);
    }

    if let Some(cost) = entry.input_cost_per_million {
        amount += Decimal::from(usage.input_tokens) * cost / ONE_MILLION;
    }
    if let Some(cost) = entry.output_cost_per_million {
        amount += Decimal::from(usage.output_tokens) * cost / ONE_MILLION;
    }
    if let Some(cost) = entry.cache_read_cost_per_million {
        amount += Decimal::from(usage.cache_read_tokens) * cost / ONE_MILLION;
    }
    if let Some(cost) = entry.cache_write_cost_per_million {
        amount += Decimal::from(usage.cache_write_tokens) * cost / ONE_MILLION;
    }
    if let Some(cost) = entry.request_cost {
        amount += Decimal::from(usage.request_count) * cost;
    }

    (amount, vec![])
}

/// Estimate usage cost for a model given canonical usage and pricing entry.
/// Pure function -- no network I/O.
pub fn estimate_usage_cost(
    model_name: &str,
    usage: &CanonicalUsage,
    pricing_entry: Option<&PricingEntry>,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> CostResult {
    let route = resolve_billing_route(model_name, provider, base_url);

    if route.billing_mode == "subscription_included" {
        return CostResult::included();
    }

    let entry = match pricing_entry {
        Some(e) => e.clone(),
        None => {
            return CostResult::unknown(&route.provider);
        }
    };

    let (amount, mut notes) = compute_cost_from_usage(usage, &entry);

    if amount == Decimal::MAX {
        return CostResult::unknown(&entry.source);
    }

    let status = if entry.source == "none" && amount == ZERO {
        "included"
    } else {
        "estimated"
    };

    let label = format!("~${:.2}", amount);

    if route.provider == "openrouter" {
        notes.push("OpenRouter cost is estimated from the models API until reconciled.".to_string());
    }

    CostResult {
        amount_usd: Some(amount),
        status: status.to_string(),
        source: entry.source,
        label,
        fetched_at: None,
        pricing_version: entry.pricing_version,
        notes,
    }
}

/// Format a duration in seconds as a compact string (e.g., "1h 30m").
/// Pure function.
pub fn format_duration_compact(seconds: f64) -> String {
    if seconds < 60.0 {
        return format!("{:.0}s", seconds);
    }
    let minutes = seconds / 60.0;
    if minutes < 60.0 {
        return format!("{:.0}m", minutes);
    }
    let hours = minutes / 60.0;
    if hours < 24.0 {
        let remaining_min = (minutes % 60.0).trunc() as i64;
        if remaining_min == 0 {
            return format!("{:.0}h", hours.trunc());
        }
        return format!("{}h {}m", hours.trunc() as i64, remaining_min);
    }
    let days = hours / 24.0;
    format!("{:.1}d", days)
}

/// Format a token count as a compact string (e.g., "1.5M", "350K").
/// Pure function.
pub fn format_token_count_compact(value: i64) -> String {
    let abs_value = value.abs();
    if abs_value < 1_000 {
        return value.to_string();
    }

    let sign = if value < 0 { "-" } else { "" };
    let units: [(i64, &str); 3] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "K")];

    for (threshold, suffix) in units.iter() {
        if abs_value >= *threshold {
            let scaled = abs_value as f64 / *threshold as f64;
            let text = if scaled < 10.0 {
                format!("{:.2}", scaled)
            } else if scaled < 100.0 {
                format!("{:.1}", scaled)
            } else {
                format!("{:.0}", scaled)
            };
            let text = text.trim_end_matches('0').trim_end_matches('.');
            return format!("{}{}{}", sign, text, suffix);
        }
    }

    value.to_string()
}

// ---------------------------------------------------------------------------
// PyO3 bindings
// ---------------------------------------------------------------------------

/// Resolve a billing route (pure).
#[pyfunction]
fn py_resolve_billing_route(
    model_name: &str,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> BillingRoute {
    resolve_billing_route(model_name, provider, base_url)
}

/// Normalize usage from a JSON string or dict (pure).
#[pyfunction]
fn py_normalize_usage(
    response_usage: &str,
    provider: Option<&str>,
    api_mode: Option<&str>,
) -> CanonicalUsage {
    let value: serde_json::Value =
        serde_json::from_str(response_usage).unwrap_or(serde_json::Value::Null);
    normalize_usage(&value, provider, api_mode)
}

/// Compute cost from canonical usage and a pricing entry dict (pure).
#[pyfunction]
fn py_compute_cost_from_usage(usage: &str, pricing_entry: &str) -> (Option<String>, Vec<String>) {
    let usage_val: CanonicalUsage = serde_json::from_str(usage).unwrap_or_default();
    let entry_val: PricingEntry = serde_json::from_str(pricing_entry).unwrap_or_default();
    let (amount, notes) = compute_cost_from_usage(&usage_val, &entry_val);
    if amount == Decimal::MAX {
        (None, notes)
    } else {
        (Some(amount.to_string()), notes)
    }
}

/// Check if known pricing exists (pure).
#[pyfunction]
fn py_has_known_pricing(
    model_name: &str,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> bool {
    has_known_pricing(model_name, provider, base_url)
}

/// Estimate usage cost (pure, uses provided pricing entry).
#[pyfunction]
fn py_estimate_usage_cost(
    model_name: &str,
    usage: &str,
    pricing_entry: Option<&str>,
    provider: Option<&str>,
    base_url: Option<&str>,
) -> CostResult {
    let usage_val: CanonicalUsage = serde_json::from_str(usage).unwrap_or_default();
    let entry_val: Option<PricingEntry> =
        pricing_entry.and_then(|s| serde_json::from_str(s).ok());
    estimate_usage_cost(model_name, &usage_val, entry_val.as_ref(), provider, base_url)
}

/// Format duration compact (pure).
#[pyfunction]
fn py_format_duration_compact(seconds: f64) -> String {
    format_duration_compact(seconds)
}

/// Format token count compact (pure).
#[pyfunction]
fn py_format_token_count_compact(value: i64) -> String {
    format_token_count_compact(value)
}

/// Lookup official docs pricing for a provider+model (pure).
#[pyfunction]
fn py_lookup_official_docs_pricing(provider: &str, model: &str) -> Option<PricingEntry> {
    OFFICIAL_DOCS_PRICING
        .get(&(provider.to_lowercase(), model.to_lowercase()))
        .cloned()
}

#[pymodule]
fn usage_pricing_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_resolve_billing_route, m)?)?;
    m.add_function(wrap_pyfunction!(py_normalize_usage, m)?)?;
    m.add_function(wrap_pyfunction!(py_compute_cost_from_usage, m)?)?;
    m.add_function(wrap_pyfunction!(py_has_known_pricing, m)?)?;
    m.add_function(wrap_pyfunction!(py_estimate_usage_cost, m)?)?;
    m.add_function(wrap_pyfunction!(py_format_duration_compact, m)?)?;
    m.add_function(wrap_pyfunction!(py_format_token_count_compact, m)?)?;
    m.add_function(wrap_pyfunction!(py_lookup_official_docs_pricing, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_billing_route_anthropic() {
        let route = resolve_billing_route("claude-3-5-sonnet-20241022", Some("anthropic"), None);
        assert_eq!(route.provider, "anthropic");
        assert_eq!(route.model, "claude-3-5-sonnet-20241022");
        assert_eq!(route.billing_mode, "official_docs_snapshot");
    }

    #[test]
    fn test_resolve_billing_route_openai() {
        let route = resolve_billing_route("gpt-4o", Some("openai"), None);
        assert_eq!(route.provider, "openai");
        assert_eq!(route.billing_mode, "official_docs_snapshot");
    }

    #[test]
    fn test_resolve_billing_route_openrouter() {
        let route = resolve_billing_route("model-name", Some("openrouter"), None);
        assert_eq!(route.provider, "openrouter");
        assert_eq!(route.billing_mode, "official_models_api");
    }

    #[test]
    fn test_normalize_usage_anthropic() {
        let json = r#"{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":20,"cache_creation_input_tokens":5}"#;
        let usage = normalize_usage(&serde_json::from_str(json).unwrap(), Some("anthropic"), None);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_write_tokens, 5);
    }

    #[test]
    fn test_normalize_usage_openai() {
        let json = r#"{"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":20}}"#;
        let usage = normalize_usage(&serde_json::from_str(json).unwrap(), None, None);
        assert_eq!(usage.input_tokens, 80);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
    }

    #[test]
    fn test_lookup_official_pricing() {
        let route = resolve_billing_route("claude-3-5-sonnet-20241022", Some("anthropic"), None);
        let entry = lookup_official_docs_pricing(&route);
        assert!(entry.is_some());
        let e = entry.unwrap();
        assert_eq!(e.input_cost_per_million, Some(Decimal::from(3)));
    }

    #[test]
    fn test_compute_cost() {
        let usage = CanonicalUsage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            ..Default::default()
        };
        let entry = PricingEntry {
            input_cost_per_million: opt_dec("3.00"),
            output_cost_per_million: opt_dec("15.00"),
            ..Default::default()
        };
        let (amount, _) = compute_cost_from_usage(&usage, &entry);
        // 3.00 per million input = 3.00 for 1M tokens
        // 15.00 per million output = 7.50 for 500k tokens
        // total = 10.50
        assert_eq!(amount, dec("10.50"));
    }

    #[test]
    fn test_format_duration_compact() {
        assert_eq!(format_duration_compact(30.0), "30s");
        assert_eq!(format_duration_compact(90.0), "2m");
        assert_eq!(format_duration_compact(3600.0), "1h");
        assert_eq!(format_duration_compact(5400.0), "1h 30m");
        assert_eq!(format_duration_compact(90000.0), "1.0d");
    }

    #[test]
    fn test_format_token_count_compact() {
        assert_eq!(format_token_count_compact(500), "500");
        assert_eq!(format_token_count_compact(1500), "2K");
        assert_eq!(format_token_count_compact(1_500_000), "2M");
    }

    #[test]
    fn test_has_known_pricing_true() {
        assert!(has_known_pricing("claude-3-5-sonnet-20241022", Some("anthropic"), None));
        assert!(has_known_pricing("gpt-4o", Some("openai"), None));
    }

    #[test]
    fn test_has_known_pricing_false() {
        assert!(!has_known_pricing("unknown-model", Some("custom"), None));
    }
}
