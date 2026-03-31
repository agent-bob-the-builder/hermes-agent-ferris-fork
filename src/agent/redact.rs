//! Regex-based secret redaction for logs and tool output.
//!
//! Applies pattern matching to mask API keys, tokens, and credentials
//! before they reach log files, verbose output, or gateway logs.
//!
//! Short tokens (< 18 chars) are fully masked. Longer tokens preserve
//! the first 6 and last 4 characters for debuggability.

use once_cell::sync::Lazy;
use regex::{Regex, RegexBuilder};
use std::env;

/// Mask a token, preserving prefix for long tokens.
fn mask_token(token: &str) -> String {
    if token.len() < 18 {
        "***".to_string()
    } else {
        format!("{}...{}", &token[..6], &token[token.len() - 4..])
    }
}

// ---------------------------------------------------------------------------
// Known API key prefixes — match the prefix + contiguous token chars
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
    r"sk_[A-Za-z0-9_]{10,}",            // ElevenLabs TTS key (sk_ underscore, not sk- dash)
    r"tvly-[A-Za-z0-9]{10,}",           // Tavily search API key
    r"exa_[A-Za-z0-9]{10,}",            // Exa search API key
];

/// ENV assignment patterns: KEY=value where KEY contains a secret-like name.
/// Uses `A...TH` as a heuristic — catches API_KEY, SECRET_KEY, AUTH_TOKEN, etc.
static ENV_ASSIGN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)([A-Z_]*API[A-Z_]*|[A-Z_]*SECRET[A-Z_]*|[A-Z_]*TOKEN[A-Z_]*|[A-Z_]*PASSWORD[A-Z_]*|[A-Z_]*AUTH[A-Z_]*)\s*=\s*(['\"]?)(\S+)\2")
        .expect("invalid ENV_ASSIGN_RE regex")
});

/// JSON field patterns: "apiKey": "***", "token": "***", etc.
static JSON_FIELD_RE: Lazy<Regex> = Lazy::new(|| {
    let key_names = r"(?i)\"api_?[Kk]ey\"|\"token\"|\"secret\"|\"password\"|\"access_token\"|\"refresh_token\"|\"auth_token\"|\"bearer\"|\"secret_value\"|\"raw_secret\"|\"secret_input\"|\"key_material\"";
    Regex::new(&format!(r#"({})\s*:\s*"([^"]+)""#, key_names)).expect("invalid JSON_FIELD_RE regex")
});

/// Authorization headers: Bearer tokens
static AUTH_HEADER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(Authorization:\s*Bearer\s+)(\S+)").expect("invalid AUTH_HEADER_RE regex")
});

/// Telegram bot tokens: bot<digits>:<token> or <digits>:<token>
static TELEGRAM_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(bot)?(\d{8,}):([-A-Za-z0-9_]{30,})").expect("invalid TELEGRAM_RE regex")
});

/// Private key blocks: [REDACTED PRIVATE KEY]
static PRIVATE_KEY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"-----BEGIN[A-Z ]*PRIVATE KEY-----[\s\S]*?-----END[A-Z ]*PRIVATE KEY-----")
        .expect("invalid PRIVATE_KEY_RE regex")
});

/// Database connection strings: protocol://user:PASSWORD@host
static DB_CONNSTR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"((?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|amqp)://[^:]+:)([^@]+)(@)")
        .expect("invalid DB_CONNSTR_RE regex")
});

/// E.164 phone numbers: +<country><number>, 7–15 digits.
/// Negative lookahead prevents matching hex strings or identifiers.
static SIGNAL_PHONE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(\+[1-9]\d{6,14})(?![A-Za-z0-9])").expect("invalid SIGNAL_PHONE_RE regex")
});

/// Known prefix patterns — compiled into one alternation with word boundaries.
static PREFIX_RE: Lazy<Regex> = Lazy::new(|| {
    let alternation = PREFIX_PATTERNS.join("|");
    let pattern = format!(r"(?<![A-Za-z0-9_-])({})(?![A-Za-z0-9_-])", alternation);
    RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .expect("invalid PREFIX_RE regex")
});

/// Fast-path check — skip full regex when no escape-like bytes are present.
static HAS_ESCAPE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[\x1b\x80-\x9f]").expect("invalid HAS_ESCAPE regex")
});

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Apply all redaction patterns to a block of text.
///
/// Safe to call on any string — non-matching text passes through unchanged.
/// Returns `None` if input is `None`.
pub fn redact_sensitive_text(text: Option<&str>) -> Option<String> {
    let text = text?;

    if text.is_empty() {
        return Some(text.to_string());
    }

    // Allow disabling redaction via env var
    if env::var("HERMES_REDACT_SECRETS")
        .map(|v| v.to_lowercase())
        .map(|v| matches!(v.as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(false)
    {
        return Some(text.to_string());
    }

    let mut result = text.to_string();

    // Known prefixes (sk-, ghp_, etc.)
    result = PREFIX_RE.replace_all(&result, |caps: &regex::Captures| {
        mask_token(caps.get(1).map(|m| m.as_str()).unwrap_or(""))
    }).to_string();

    // ENV assignments: OPENAI_API_KEY=***
    result = ENV_ASSIGN_RE.replace_all(&result, |caps: &regex::Captures| {
        let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let quote = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let value = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        format!("{}={}{}{}", name, quote, mask_token(value), quote)
    }).to_string();

    // JSON fields: "apiKey": "***"
    result = JSON_FIELD_RE.replace_all(&result, |caps: &regex::Captures| {
        let key = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let value = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        format!(r#"{}: "{}""#, key, mask_token(value))
    }).to_string();

    // Authorization headers
    result = AUTH_HEADER_RE.replace_all(&result, |caps: &regex::Captures| {
        let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let token = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        format!("{}{}", prefix, mask_token(token))
    }).to_string();

    // Telegram bot tokens
    result = TELEGRAM_RE.replace_all(&result, |caps: &regex::Captures| {
        let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let digits = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        format!("{}{}:***", prefix, digits)
    }).to_string();

    // Private key blocks
    result = PRIVATE_KEY_RE.replace_all(&result, "[REDACTED PRIVATE KEY]").to_string();

    // Database connection string passwords
    result = DB_CONNSTR_RE.replace_all(&result, |caps: &regex::Captures| {
        let before = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let _after = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        format!("{}***{}", before, "@")
    }).to_string();

    // E.164 phone numbers (Signal, WhatsApp)
    result = SIGNAL_PHONE_RE.replace_all(&result, |caps: &regex::Captures| {
        let phone = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        if phone.len() <= 8 {
            format!("{}****{}", &phone[..2], &phone[phone.len() - 2..])
        } else {
            format!("{}****{}", &phone[..4], &phone[phone.len() - 4..])
        }
    }).to_string();

    Some(result)
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
        assert_eq!(mask_token("sk-ant-api03-verylongtoken"), "sk-ant...oken");
    }

    #[test]
    fn test_openai_key_redacted() {
        let input = "API key is sk-abcdefghij1234567890abcdefghij";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("sk-abc...ghij"));
        assert!(!result.contains("sk-abcdefghij1234567890abcdefghij"));
    }

    #[test]
    fn test_github_pat_redacted() {
        let input = "ghp_abcdefghij1234567890abcdefghijklmn";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("ghp_abc...mn"));
        assert!(!result.contains("ghp_abcdefghij1234567890abcdefghijklmn"));
    }

    #[test]
    fn test_json_api_key_redacted() {
        let input = r#""apiKey": "sk-abcdefghij1234567890abcdefghij""#;
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("apiKey"));
        assert!(!result.contains("sk-abcdefghij1234567890abcdefghij"));
    }

    #[test]
    fn test_auth_header_redacted() {
        let input = "Authorization: Bearer ghp_abcdefghij1234567890abcdefghijklmn";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("Bearer"));
        assert!(!result.contains("ghp_abcdefghij1234567890abcdefghijklmn"));
    }

    #[test]
    fn test_private_key_redacted() {
        let input = "-----BEGIN PRIVATE KEY-----\nsecretdata\n-----END PRIVATE KEY-----";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert_eq!(result, "[REDACTED PRIVATE KEY]");
    }

    #[test]
    fn test_db_password_redacted() {
        let input = "postgres://user:password123@localhost/db";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("***"));
        assert!(!result.contains("password123"));
        assert!(result.contains("postgres://user:"));
    }

    #[test]
    fn test_phone_number_redacted() {
        let input = "Call me at +12025551234";
        let result = redact_sensitive_text(Some(input)).unwrap();
        assert!(result.contains("+120****1234"));
        assert!(!result.contains("+12025551234"));
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
        assert!(result.contains("***"));
        assert!(!result.contains("ABCDEFGHIJabcdefghijABCDEFGHIJ1234567890abcdef"));
    }

    #[test]
    fn test_env_var_disables_redaction() {
        temp_env::with_var("HERMES_REDACT_SECRETS", Some("false"), || {
            let input = "API key is sk-abcdefghij1234567890abcdefghij";
            let result = redact_sensitive_text(Some(input)).unwrap();
            assert!(result.contains("sk-abcdefghij1234567890abcdefghij"));
        });
    }
}
