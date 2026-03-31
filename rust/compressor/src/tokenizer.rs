//! tiktoken-based token counting for accurate OpenAI-compatible token estimation.
//!
//! Uses the `tiktoken-rs` crate — the official Rust port of OpenAI's tiktoken.
//! Fallback: character-count heuristic when tiktoken fails to load.

use serde_json::Value;
use std::sync::OnceLock;

// ─── tiktoken encoder (lazy, cached) ─────────────────────────────────────────

/// The tiktoken CoreBPE encoder type.
type TiktokenEncoder = tiktoken_rs::CoreBPE;

/// cl100k_base encoder — lazy-initialized on first use, then reused forever.
static ENCODER: OnceLock<TiktokenEncoder> = OnceLock::new();

/// Initialize cl100k_base from embedded BPE data.  Falls back to None on failure.
fn init_encoder() -> Option<&'static TiktokenEncoder> {
    tiktoken_rs::cl100k_base().ok().and_then(|enc| {
        Some(ENCODER.get_or_init(|| enc))
    })
}

/// Get (or initialize) the cl100k_base encoder.
fn get_encoder() -> Option<&'static TiktokenEncoder> {
    if let Some(enc) = ENCODER.get() {
        return Some(enc);
    }
    init_encoder()
}

/// Count tokens in a string using tiktoken.  Falls back to chars/4.
fn count_tokens_fallible(text: &str) -> usize {
    match get_encoder() {
        Some(enc) => enc.encode_ordinary(text).len(),
        None => text.len() / 4,
    }
}

// ─── Public API ────────────────────────────────────────────────────────────────

/// Token count for a single message dict.
pub fn estimate_message_dict_tokens(msg: &Value) -> usize {
    let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
    count_tokens_fallible(content)
}

/// Token count for a list of message dicts.
pub fn estimate_messages_tokens(messages: &[Value]) -> usize {
    messages.iter().map(estimate_message_dict_tokens).sum()
}

/// Estimate tokens for a pre-serialized JSON string of a messages array.
pub fn estimate_messages_tokens_from_json(json_str: &str) -> usize {
    let msgs: Vec<Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    estimate_messages_tokens(&msgs)
}

/// Estimate tokens for a single message from JSON string.
/// Returns (content_tokens, tool_calls_tokens).
pub fn estimate_message_from_json(json_str: &str) -> (usize, usize) {
    let msg: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return (0, 0),
    };
    let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let content_tokens = count_tokens_fallible(content);
    let tc_tokens = msg
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| tc.get("function"))
                .filter_map(|f| f.get("arguments"))
                .filter_map(|a| a.as_str())
                .map(|args| args.len() / 4)
                .sum::<usize>()
        })
        .unwrap_or(0);
    (content_tokens, tc_tokens)
}

/// Returns true if tiktoken encoder loaded successfully.
pub fn is_tiktoken_available() -> bool {
    get_encoder().is_some()
}
