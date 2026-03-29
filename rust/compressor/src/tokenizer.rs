//! Simple character-based token estimation.
//! Matches the Python `_CHARS_PER_TOKEN=4` heuristic used throughout hermes-agent.

/// Rough token estimate: each token ≈ 4 characters.
/// Add ~10 tokens overhead per message for role/metadata framing.
pub fn estimate_message_tokens(content: &str, tool_calls: Option<&[serde_json::Value]>) -> usize {
    let base = content.len() / 4;
    let overhead = 10;
    let tc_overhead = tool_calls
        .map(|tcs| {
            tcs.iter()
                .filter_map(|tc| tc.get("function"))
                .filter_map(|f| f.get("arguments"))
                .filter_map(|a| a.as_str())
                .map(|args| args.len() / 4)
                .sum::<usize>()
        })
        .unwrap_or(0);
    base + overhead + tc_overhead
}

/// Rough token estimate for a message dict (matches Python logic).
pub fn estimate_message_dict_tokens(msg: &serde_json::Value) -> usize {
    let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let tool_calls = msg
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice());
    estimate_message_tokens(content, tool_calls)
}

/// Rough token estimate for a list of message dicts.
pub fn estimate_messages_tokens(messages: &[serde_json::Value]) -> usize {
    messages.iter().map(estimate_message_dict_tokens).sum()
}

// ---------------------------------------------------------------------------
// Fast path: takes a JSON string, counts tokens without Python object overhead
// ---------------------------------------------------------------------------
// Used by agent/model_metadata.py as the primary token estimator.
// Input: a JSON array string like '[{"role":"user","content":"..."},...]'
// This is what json.dumps already produces in the Python callers.

use serde_json::Value;

/// Estimate tokens for a pre-serialized JSON string of a messages array.
/// This avoids the dict→Value conversion overhead in the PyDict path.
pub fn estimate_messages_tokens_from_json(json_str: &str) -> usize {
    let msgs: Vec<Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    estimate_messages_tokens(&msgs)
}

/// Estimate tokens for a single message dict from a JSON string.
/// Returns (content_tokens, tool_calls_tokens).
pub fn estimate_message_from_json(json_str: &str) -> (usize, usize) {
    let msg: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return (0, 0),
    };
    let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let base = content.len() / 4;
    let overhead = 10;
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
    (base + overhead, tc_tokens)
}
