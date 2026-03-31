//! PyO3 bindings for the Rust secret redaction library.
//!
//! Applies pattern matching to mask API keys, tokens, and credentials
//! before they reach log files, verbose output, or gateway logs.
//!
//! Short tokens (< 18 chars) are fully masked. Longer tokens preserve
//! the first 6 and last 4 characters for debuggability.

use once_cell::sync::Lazy;
use regex::Regex;
use std::env;

// ---------------------------------------------------------------------------
// Token masking
// ---------------------------------------------------------------------------

/// Mask a token, preserving prefix for long tokens.
fn mask_token(token: &str) -> String {
    if token.len() < 18 {
        "***".to_string()
    } else {
        format!("{}...{}", &token[..6], &token[token.len() - 4..])
    }
}

// ---------------------------------------------------------------------------
// Known API key prefixes
// ---------------------------------------------------------------------------

const PREFIX_PATTERNS: &[&str] = &[
    r"sk-[A-Za-z0-9_-]{10,}",            // OpenAI / OpenRouter / Anthropic (sk-ant-*)
    r"ghp_[A-Za-z0-9]{10,}",             // GitHub PAT (classic)
    r"github_pat_[A-Za-z0-9_]{10,}",     // GitHub PAT (fine-grained)
    r"xox[baprs]-[A-Za-z0-9-]{10,}",     // Slack tokens
    r"AIza[A-Za-z0-9_-]{30,}",           // Google API keys
    r"pplx-[A-Za-z0-9]{10,}",            // Perplexity
    r"fal_[A-Za-z0-9_-]{10,}",           // Fal.ai
    r"fc-[A-Za-z0-9]{10,}",              // Firecrawl
    r"bb_live_[A-Za-z0-9_-]{10,}",       // BrowserBase
    r"gAAAA[A-Za-z0-9_=-]{20,}",         // Codex encrypted tokens
    r"AKIA[A-Z0-9]{16}",                 // AWS Access Key ID
    r"sk_live_[A-Za-z0-9]{10,}",         // Stripe secret key (live)
    r"sk_test_[A-Za-z0-9]{10,}",         // Stripe secret key (test)
    r"rk_live_[A-Za-z0-9]{10,}",         // Stripe restricted key
    r"SG\.[A-Za-z0-9_-]{10,}",           // SendGrid API key
    r"hf_[A-Za-z0-9]{10,}",              // HuggingFace token
    r"r8_[A-Za-z0-9]{10,}",              // Replicate API token
    r"npm_[A-Za-z0-9]{10,}",             // npm access token
    r"pypi-[A-Za-z0-9_-]{10,}",          // PyPI API token
    r"dop_v1_[A-Za-z0-9]{10,}",          // DigitalOcean PAT
    r"doo_v1_[A-Za-z0-9]{10,}",          // DigitalOcean OAuth
    r"am_[A-Za-z0-9_-]{10,}",            // AgentMail API key
    r"sk_[A-Za-z0-9_]{10,}",             // ElevenLabs TTS key
    r"tvly-[A-Za-z0-9]{10,}",            // Tavily search API key
    r"exa_[A-Za-z0-9]{10,}",             // Exa search API key
];

/// ENV assignment patterns: KEY=value where KEY contains a secret-like name.
/// Rust regex does not support lookbehind/lookahead, so we match broadly
/// and rely on the distinctive secret-like KEY names to keep false positives low.
static ENV_ASSIGN_RE: Lazy<Regex> = Lazy::new(|| {
    // (?i)                     case-insensitive
    // ([A-Z_]*(?:API|SECRET|TOKEN|PASSWORD|AUTH)[A-Z_]*)  capture group 1: env var name
    // \s*=\s*                  equals with optional whitespace
    // (\S+)                    capture group 2: the value (unquoted)
    let unquoted = concat!(
        r"(?i)",
        r"([A-Z_]*(?:API|SECRET|TOKEN|PASSWORD|AUTH)[A-Z_]*)",
        r"\s*=\s*",
        r"(\S+)"
    );
    Regex::new(unquoted).expect("invalid ENV_ASSIGN_RE regex")
});

/// JSON field patterns: "apiKey": "***", "token": "***", etc.
/// Key names built as a simple &'static str to avoid concat!/escape issues.
static JSON_FIELD_RE: Lazy<Regex> = Lazy::new(|| {
    // (?i)                                    case-insensitive
    // (?:api_?[Kk]ey|token|secret|password|...)  non-capturing group of key names
    // \s*:\s*                                 colon with optional whitespace
    // "([^"]+)"                                quoted value — captured in group 2
    let key_names = "(?i)(?:api_?[Kk]ey|token|secret|password|access_token|refresh_token|auth_token|bearer|secret_value|raw_secret|secret_input|key_material)";
    let pattern = format!(r#"({})\s*:\s*"([^"]+)""#, key_names);
    Regex::new(&pattern).expect("invalid JSON_FIELD_RE regex")
});

/// Authorization headers: Bearer tokens.
static AUTH_HEADER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(Authorization:\s*Bearer\s+)(\S+)").expect("invalid AUTH_HEADER_RE regex")
});

/// Telegram bot tokens.
static TELEGRAM_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(bot)?(\d{8,}):([-A-Za-z0-9_]{30,})").expect("invalid TELEGRAM_RE regex")
});

/// Private key blocks.
static PRIVATE_KEY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"-----BEGIN[A-Z ]*PRIVATE KEY-----[\s\S]*?-----END[A-Z ]*PRIVATE KEY-----")
        .expect("invalid PRIVATE_KEY_RE regex")
});

/// Database connection strings: protocol://user:PASSWORD@host.
static DB_CONNSTR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"((?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|amqp)://[^:]+:)([^@]+)(@)")
        .expect("invalid DB_CONNSTR_RE regex")
});

/// E.164 phone numbers.
/// No lookahead since Rust's regex crate doesn't support it.
/// The distinctive +<country><number> format keeps false positives rare.
static SIGNAL_PHONE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(\+[1-9]\d{6,14})").expect("invalid SIGNAL_PHONE_RE regex")
});

/// Known prefix patterns — compiled into one alternation.
/// No lookbehind/lookahead since Rust's regex crate doesn't support them.
/// The distinct token prefixes (sk-, ghp_, etc.) are distinctive enough
/// that the risk of false-positive redaction is acceptable.
static PREFIX_RE: Lazy<Regex> = Lazy::new(|| {
    let alternation = PREFIX_PATTERNS.join("|");
    Regex::new(&alternation).expect("invalid PREFIX_RE regex")
});

// ---------------------------------------------------------------------------
// Core redaction logic
// ---------------------------------------------------------------------------

/// Apply all redaction patterns to a block of text.
///
/// Safe to call on any string -- non-matching text passes through unchanged.
/// Returns `None` if input is `None`.
pub fn redact_sensitive_text(text: Option<&str>) -> Option<String> {
    let text = text?;

    if text.is_empty() {
        return Some(text.to_string());
    }

    if env::var("HERMES_REDACT_SECRETS")
        .map(|v| {
            let lo = v.to_lowercase();
            matches!(lo.as_str(), "0" | "false" | "no" | "off")
        })
        .unwrap_or(false)
    {
        return Some(text.to_string());
    }

    let mut result = text.to_string();

    // Known prefixes (sk-, ghp_, etc.)
    result = PREFIX_RE
        .replace_all(&result, |_caps: &regex::Captures| {
            // Group 0 is the full match; the alternation has no outer group
            mask_token(_caps.get(0).map(|m| m.as_str()).unwrap_or(""))
        })
        .to_string();

    // ENV assignments: NAME=value or NAME="value" or NAME='value'
    result = ENV_ASSIGN_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let value = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!("{}={}", name, mask_token(value))
        })
        .to_string();

    // JSON fields
    result = JSON_FIELD_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let key = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let value = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!(r#"{}: "{}""#, key, mask_token(value))
        })
        .to_string();

    // Authorization headers
    result = AUTH_HEADER_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let token = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!("{}{}", prefix, mask_token(token))
        })
        .to_string();

    // Telegram bot tokens
    result = TELEGRAM_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let digits = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!("{}{}:***", prefix, digits)
        })
        .to_string();

    // Private key blocks
    result = PRIVATE_KEY_RE
        .replace_all(&result, "[REDACTED PRIVATE KEY]")
        .to_string();

    // Database connection string passwords
    result = DB_CONNSTR_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let before = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            format!("{}***{}", before, "@")
        })
        .to_string();

    // E.164 phone numbers
    result = SIGNAL_PHONE_RE
        .replace_all(&result, |caps: &regex::Captures| {
            let phone = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if phone.len() <= 8 {
                format!("{}****{}", &phone[..2], &phone[phone.len() - 2..])
            } else {
                format!("{}****{}", &phone[..4], &phone[phone.len() - 4..])
            }
        })
        .to_string();

    Some(result)
}

// ---------------------------------------------------------------------------
// PyO3 bindings
// ---------------------------------------------------------------------------

use pyo3::prelude::*;

/// Strip secrets from a string, returning the redacted version.
/// Returns None if input is None.
#[pyfunction]
fn redact_text(text: Option<&str>) -> Option<String> {
    redact_sensitive_text(text)
}

/// Redacting log formatter for Python's logging module.
/// Wraps a Python `logging.Formatter` instance and applies secret redaction
/// to whatever it produces.
#[pyclass]
struct RedactingFormatter {
    inner: Py<PyAny>,
}

#[pymethods]
impl RedactingFormatter {
    #[new]
    fn new(formatter: Py<PyAny>) -> Self {
        Self { inner: formatter }
    }

    fn format(&self, py: Python<'_>, record: &Bound<'_, PyAny>) -> PyResult<String> {
        let result: String = self
            .inner
            .call_method1(py, "format", (record,))?
            .extract(py)?;
        Ok(redact_sensitive_text(Some(&result)).unwrap_or(result))
    }
}

#[pymodule]
fn redact_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(redact_text, m)?)?;
    m.add_class::<RedactingFormatter>()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_token_short() {
        assert_eq!(mask_token("sk-short"), "***");
    }

    #[test]
    fn test_mask_token_long() {
        // 18+ chars: preserve first 6 + last 4
        assert_eq!(mask_token("sk_abcdefghijklmnop"), "sk_abcd...mnop");
    }

    #[test]
    fn test_openai_key_redacted() {
        let input = "API key is sk-abcdefghijklmnop";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("sk-abc...mnop"), "got: {}", result);
    }

    #[test]
    fn test_github_pat_redacted() {
        let input = "ghp_abcdefghijklmnopqrstuvwx";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("ghp_abc...vwx"), "got: {}", result);
    }

    #[test]
    fn test_json_api_key_redacted() {
        let input = r#""apiKey": "sk-abcdefghijklmnop""#;
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("apiKey"), "got: {}", result);
        assert!(result.contains("sk-abc...mnop"), "got: {}", result);
    }

    #[test]
    fn test_auth_header_redacted() {
        let input = "Authorization: Bearer ghp_abcdefghijklmnopqrstuvwx";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("Bearer"), "got: {}", result);
        assert!(result.contains("ghp_abc...vwx"), "got: {}", result);
    }

    #[test]
    fn test_private_key_redacted() {
        let input = "-----BEGIN PRIVATE KEY-----\nMIIE...\n-----END PRIVATE KEY-----";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert_eq!(result, "[REDACTED PRIVATE KEY]");
    }

    #[test]
    fn test_db_password_redacted() {
        let input = "postgres://user:secretpassword@localhost/db";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("***"), "got: {}", result);
        assert!(!result.contains("secretpassword"), "got: {}", result);
        assert!(result.contains("postgres://user:"), "got: {}", result);
    }

    #[test]
    fn test_phone_number_redacted() {
        let input = "Call me at +12025551234";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("+120****1234"), "got: {}", result);
    }

    #[test]
    fn test_none_input() {
        assert_eq!(redact_sensitive_text(None), None);
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(redact_sensitive_text(Some("")), Some("".to_string()));
    }

    #[test]
    fn test_clean_text_passthrough() {
        let input = "This is a clean log message with no secrets.";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn test_telegram_token_redacted() {
        let input = "bot12345678:ABCDEFGHIJabcdefghijABCDEFGHIJ1234567890abcdef";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("bot12345678:***"), "got: {}", result);
    }

    #[test]
    fn test_env_assignment_unquoted() {
        let input = "OPENAI_API_KEY=sk-abcdefghijklmnop";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("sk-abc...mnop"), "got: {}", result);
    }

    #[test]
    fn test_env_assignment_redaction_disabled() {
        std::env::set_var("HERMES_REDACT_SECRETS", "0");
        let input = "API key sk-abcdefghijklmnop";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("sk-abcdefghijklmnop"), "got: {}", result);
        std::env::remove_var("HERMES_REDACT_SECRETS");
    }
}
