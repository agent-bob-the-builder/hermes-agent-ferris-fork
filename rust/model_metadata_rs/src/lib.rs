use pyo3::prelude::*;
use regex::Regex;
use std::net::IpAddr;
use std::str::FromStr;

const PROVIDER_PREFIXES: &[&str] = &[
    "openrouter", "nous", "openai-codex", "copilot", "copilot-acp",
    "zai", "kimi-coding", "minimax", "minimax-cn", "anthropic", "deepseek",
    "opencode-zen", "opencode-go", "ai-gateway", "kilocode", "alibaba",
    "custom", "local", "glm", "z-ai", "z.ai", "zhipu", "github",
    "github-copilot", "github-models", "kimi", "moonshot", "claude",
    "deep-seek", "opencode", "zen", "go", "vercel", "kilo", "dashscope",
    "aliyun", "qwen",
];

lazy_static::lazy_static! {
    static ref OLLAMA_TAG_PATTERN: Regex = Regex::new(
        r"^(\d+\.?\d*b|latest|stable|q\d|fp?\d|instruct|chat|coder|vision|text)"
    ).unwrap();
}

#[pyfunction]
pub fn strip_provider_prefix(model: &str) -> String {
    if !model.contains(':') || model.starts_with("http") {
        return model.to_string();
    }
    if let Some((prefix, suffix)) = model.split_once(':') {
        let prefix_lower = prefix.trim().to_lowercase();
        if PROVIDER_PREFIXES.contains(&prefix_lower.as_str()) {
            let suffix_stripped = suffix.trim();
            if OLLAMA_TAG_PATTERN.is_match(suffix_stripped) {
                return model.to_string();
            }
            return suffix_stripped.to_string();
        }
    }
    model.to_string()
}

const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1", "0.0.0.0"];

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

fn parse_host_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    let url = if !url.contains("://") { format!("http://{}", url) } else { url.to_string() };
    match url::Url::parse(&url) {
        Ok(parsed) => parsed.host_str().map(|s| s.to_string()),
        Err(_) => None,
    }
}

#[pyfunction]
pub fn is_local_endpoint(base_url: &str) -> bool {
    let normalized = normalize_base_url(base_url);
    if normalized.is_empty() { return false; }
    let host = match parse_host_from_url(&normalized) {
        Some(h) => h,
        None => return false,
    };
    if LOCAL_HOSTS.contains(&host.as_str()) { return true; }
    if let Ok(addr) = IpAddr::from_str(&host) {
        return match addr {
            IpAddr::V4(ipv4) => ipv4.is_private() || ipv4.is_loopback(),
            IpAddr::V6(_) => addr.is_loopback(),
        };
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() == 4 {
        if let (Ok(a), Ok(b)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) {
            if a == 10 { return true; }
            if a == 172 && (16..=31).contains(&b) { return true; }
            if a == 192 && b == 168 { return true; }
        }
    }
    false
}

const CONTEXT_LENGTH_KEYS: &[&str] = &[
    "context_length", "context_window", "max_context_length",
    "max_position_embeddings", "max_model_len", "max_input_tokens",
    "max_sequence_length", "max_seq_len", "n_ctx_train", "n_ctx",
];
const MAX_COMPLETION_KEYS: &[&str] = &[
    "max_completion_tokens", "max_output_tokens", "max_tokens",
];

fn coerce_int(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.trim().replace(',', "").parse::<i64>().ok(),
        _ => None,
    }
}

fn search_nested(value: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    if let serde_json::Value::Object(map) = value {
        for key in keys {
            let key_lower = key.to_lowercase();
            for (k, v) in map {
                if k.to_lowercase() == key_lower {
                    if let Some(n) = coerce_int(v) {
                        if (1024..=10_000_000).contains(&n) { return Some(n); }
                    }
                }
            }
        }
    }
    if let serde_json::Value::Array(arr) = value {
        for item in arr { if let Some(f) = search_nested(item, keys) { return Some(f); } }
    }
    if let serde_json::Value::Object(map) = value {
        for v in map.values() { if let Some(f) = search_nested(v, keys) { return Some(f); } }
    }
    None
}

#[pyfunction]
pub fn extract_context_length(json_payload: &str) -> Option<i64> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_payload) else { return None; };
    search_nested(&value, CONTEXT_LENGTH_KEYS)
}

#[pyfunction]
pub fn extract_max_completion_tokens(json_payload: &str) -> Option<i64> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_payload) else { return None; };
    search_nested(&value, MAX_COMPLETION_KEYS)
}

const PRICING_ALIASES: &[(&str, &[&str])] = &[
    ("prompt", &["prompt", "input", "input_cost_per_token", "prompt_token_cost"]),
    ("completion", &["completion", "output", "output_cost_per_token", "completion_token_cost"]),
    ("request", &["request", "request_cost"]),
    ("cache_read", &["cache_read", "cached_prompt", "input_cache_read", "cache_read_cost_per_token"]),
    ("cache_write", &["cache_write", "cache_creation", "input_cache_write", "cache_write_cost_per_token"]),
];

#[pyfunction]
pub fn extract_pricing(json_payload: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_payload) else { return "{}".to_string(); };
    let mut pricing = serde_json::Map::new();
    fn search(v: &serde_json::Value, pricing: &mut serde_json::Map<String, serde_json::Value>) -> bool {
        if let serde_json::Value::Object(map) = v {
            let normalized: std::collections::HashMap<String, &serde_json::Value> =
                map.iter().map(|(k, val)| (k.to_lowercase(), val)).collect();
            let mut found_any = false;
            for (target, aliases) in PRICING_ALIASES {
                for alias in *aliases {
                    if let Some(val) = normalized.get(*alias) {
                        if !val.is_null() {
                            let is_empty_str = val.as_str().map(|s| s.is_empty()).unwrap_or(false);
                            if !is_empty_str {
                                pricing.insert(target.to_string(), (*val).clone());
                                found_any = true;
                                break;
                            }
                        }
                    }
                }
            }
            if found_any { return true; }
        }
        if let serde_json::Value::Array(arr) = v {
            for item in arr { if search(item, pricing) { return true; } }
        }
        if let serde_json::Value::Object(map) = v {
            for val in map.values() { if search(val, pricing) { return true; } }
        }
        false
    }
    search(&value, &mut pricing);
    serde_json::to_string(&pricing).unwrap_or_else(|_| "{}".to_string())
}

lazy_static::lazy_static! {
    static ref CONTEXT_ERROR_PATTERNS: Vec<Regex> = vec![
        Regex::new(r"(?i)(?:max(?:imum)?|limit)\s*(?:context\s*)?(?:length|size|window)?\s*(?:is|of|:)?\s*(\d{4,})").unwrap(),
        Regex::new(r"(?i)context\s*(?:length|size|window)\s*(?:is|of|:)?\s*(\d{4,})").unwrap(),
        Regex::new(r"(?i)(\d{4,})\s*(?:token)?\s*(?:context|limit)").unwrap(),
        Regex::new(r"(?i)>\s*(\d{4,})\s*(?:max|limit|token)").unwrap(),
        Regex::new(r"(?i)(\d{4,})\s*(?:max(?:imum)?)\b").unwrap(),
    ];
}

#[pyfunction]
pub fn parse_context_limit_from_error(error_msg: &str) -> Option<i64> {
    for pattern in CONTEXT_ERROR_PATTERNS.iter() {
        if let Some(caps) = pattern.captures(error_msg) {
            if let Some(m) = caps.get(1) {
                if let Ok(n) = m.as_str().parse::<i64>() {
                    if (1024..=10_000_000).contains(&n) { return Some(n); }
                }
            }
        }
    }
    None
}

#[pyfunction]
pub fn model_id_matches(candidate_id: &str, lookup_model: &str) -> bool {
    if candidate_id == lookup_model { return true; }
    if let Some((_, bare)) = candidate_id.rsplit_once('/') { return bare == lookup_model; }
    false
}

const CONTEXT_PROBE_TIERS: [i64; 5] = [128_000, 64_000, 32_000, 16_000, 8_000];

#[pyfunction]
pub fn get_next_probe_tier(current_length: i64) -> Option<i64> {
    CONTEXT_PROBE_TIERS.iter().find(|&&t| t < current_length).copied()
}

#[pyfunction]
pub fn estimate_tokens_rough(text: &str) -> i64 {
    if text.is_empty() { return 0; }
    (text.len() / 4) as i64
}

#[pymodule]
pub fn model_metadata_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(strip_provider_prefix, m)?)?;
    m.add_function(wrap_pyfunction!(is_local_endpoint, m)?)?;
    m.add_function(wrap_pyfunction!(extract_context_length, m)?)?;
    m.add_function(wrap_pyfunction!(extract_max_completion_tokens, m)?)?;
    m.add_function(wrap_pyfunction!(extract_pricing, m)?)?;
    m.add_function(wrap_pyfunction!(parse_context_limit_from_error, m)?)?;
    m.add_function(wrap_pyfunction!(model_id_matches, m)?)?;
    m.add_function(wrap_pyfunction!(get_next_probe_tier, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_tokens_rough, m)?)?;
    Ok(())
}
