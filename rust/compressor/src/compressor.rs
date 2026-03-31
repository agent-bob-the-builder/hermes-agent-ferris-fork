//! Core context compression logic — the Rust equivalent of Python's ContextCompressor.
//!
//! Algorithm:
//!   1. Prune old tool results (cheap pre-pass, no LLM call)
//!   2. Protect head messages (system prompt + first exchange)
//!   3. Protect tail by token budget (~20K recent tokens)
//!   4. Summarize middle turns via LLM call
//!   5. On re-compression, iteratively update the previous summary
use serde_json::{json, Value};
use std::collections::HashSet;

use super::summarizer::{generate_summary, with_summary_prefix};
use super::tokenizer::count_tokens_fallible as tiktoken_count;

// ---------------------------------------------------------------------------
// Constants (mirror Python)
// ---------------------------------------------------------------------------

const CHARS_PER_TOKEN: usize = 4;
const PRUNED_PLACEHOLDER: &str = "[Old tool output cleared to save context space]";
const LEGACY_SUMMARY_PREFIX: &str = "[CONTEXT SUMMARY]:";
const SUMMARY_PREFIX: &str =
    "[CONTEXT COMPACTION] Earlier turns in this conversation were compacted \
     to save context space. The summary below describes work that was \
     already completed, and the current session state may still reflect \
     that work (for example, files may already be changed). Use the summary \
     and the current state to continue from where things left off, and \
     avoid repeating work:";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Token-budget tail protection — walks backward accumulating tokens until
/// budget is exhausted. Mirrors `_find_tail_cut_by_tokens` in Python.
pub fn find_tail_cut(
    messages: &[Value],
    head_end: usize,
    token_budget: usize,
    protect_last_n: usize,
) -> usize {
    let n = messages.len();
    let mut accumulated = 0;
    let mut cut_idx = n;

    for i in (head_end..n).rev() {
        let msg = &messages[i];
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        // Use tiktoken for accurate token counting instead of char-count heuristic
        let mut msg_tokens = tiktoken_count(content);

        // Include tool call arguments in estimate (still char-count — arguments are typically compact)
        if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                {
                    msg_tokens += tiktoken_count(args);
                }
            }
        }

        if accumulated + msg_tokens > token_budget && (n - i) >= protect_last_n {
            break;
        }
        accumulated += msg_tokens;
        cut_idx = i;
    }

    // Ensure we protect at least protect_last_n messages
    let fallback_cut = n.saturating_sub(protect_last_n);
    if cut_idx > fallback_cut {
        cut_idx = fallback_cut;
    }

    // If budget would protect everything, fall back to fixed count
    if cut_idx <= head_end {
        cut_idx = fallback_cut;
    }

    // Align backward to avoid splitting tool_call/result groups
    cut_idx = align_boundary_backward(messages, cut_idx);
    cut_idx.max(head_end + 1)
}

/// Prune old tool results — replace content >200 chars with placeholder.
/// Mirrors `_prune_old_tool_results` in Python.
pub fn prune_old_tool_results(
    messages: &[Value],
    protect_tail_count: usize,
) -> (Vec<Value>, usize) {
    if messages.is_empty() {
        return (messages.to_vec(), 0);
    }

    let mut result: Vec<Value> = Vec::with_capacity(messages.len());
    let mut pruned = 0;
    let prune_boundary = messages.len().saturating_sub(protect_tail_count);

    for (i, msg) in messages.iter().enumerate() {
        if i < prune_boundary && msg.get("role").and_then(|v| v.as_str()) == Some("tool") {
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if !content.is_empty() && content != PRUNED_PLACEHOLDER && content.len() > 200 {
                let mut pruned_msg = msg.clone();
                pruned_msg["content"] = json!(PRUNED_PLACEHOLDER);
                result.push(pruned_msg);
                pruned += 1;
            } else {
                result.push(msg.clone());
            }
        } else {
            result.push(msg.clone());
        }
    }

    (result, pruned)
}

/// Align compress_start forward past orphan tool results.
/// Mirrors `_align_boundary_forward` in Python.
pub fn align_boundary_forward(messages: &[Value], mut idx: usize) -> usize {
    while idx < messages.len()
        && messages[idx].get("role").and_then(|v| v.as_str()) == Some("tool")
    {
        idx += 1;
    }
    idx
}

/// Align compress_end backward to avoid splitting tool groups.
/// Mirrors `_align_boundary_backward` in Python.
pub fn align_boundary_backward(messages: &[Value], mut idx: usize) -> usize {
    if idx == 0 || idx >= messages.len() {
        return idx;
    }

    let mut check = idx.saturating_sub(1);
    while check > 0 && messages[check].get("role").and_then(|v| v.as_str()) == Some("tool") {
        check = check.saturating_sub(1);
    }

    if check > 0
        && messages[check].get("role").and_then(|v| v.as_str()) == Some("assistant")
        && messages[check]
            .get("tool_calls")
            .is_some()
    {
        idx = check;
    }

    idx
}

/// Fix orphaned tool_call / tool_result pairs after compression.
/// Mirrors `_sanitize_tool_pairs` in Python.
pub fn sanitize_tool_pairs(messages: &mut Vec<Value>) {
    // Collect surviving call IDs from assistant tool_calls
    let surviving_call_ids: HashSet<String> = messages
        .iter()
        .filter(|m| m.get("role").and_then(|v| v.as_str()) == Some("assistant"))
        .flat_map(|m| {
            m.get("tool_calls")
                .and_then(|v| v.as_array())
                .map(|arr| arr.to_vec())
                .unwrap_or_default()
        })
        .filter_map(|tc| {
            tc.get("id")
                .or_else(|| tc.get("function")?.get("name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    // Collect call IDs present in tool results
    let result_call_ids: HashSet<String> = messages
        .iter()
        .filter(|m| m.get("role").and_then(|v| v.as_str()) == Some("tool"))
        .filter_map(|m| m.get("tool_call_id").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .collect();

    let orphaned_results: HashSet<String> = result_call_ids.difference(&surviving_call_ids).cloned().collect();
    let missing_results: HashSet<String> = surviving_call_ids.difference(&result_call_ids).cloned().collect();

    // 1. Remove orphaned tool results
    if !orphaned_results.is_empty() {
        let before = messages.len();
        messages.retain(|m| {
            !(m.get("role").and_then(|v| v.as_str()) == Some("tool")
                && m.get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .map(|s| orphaned_results.contains(s))
                    .unwrap_or(false))
        });
        let removed = before - messages.len();
        if removed > 0 {
            eprintln!("Compression sanitizer: removed {} orphaned tool result(s)", removed);
        }
    }

    // 2. Add stub results for orphaned assistant tool_calls
    if !missing_results.is_empty() {
        let mut patched: Vec<Value> = Vec::with_capacity(messages.len() + missing_results.len());
        for msg in messages.iter() {
            patched.push(msg.clone());
            if msg.get("role").and_then(|v| v.as_str()) == Some("assistant") {
                if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        if let Some(cid) = tc.get("id").and_then(|v| v.as_str()) {
                            if missing_results.contains(cid) {
                                patched.push(json!({
                                    "role": "tool",
                                    "content": "[Result from earlier conversation — see context summary above]",
                                    "tool_call_id": cid
                                }));
                            }
                        }
                    }
                }
            }
        }
        *messages = patched;
        eprintln!(
            "Compression sanitizer: added {} stub tool result(s)",
            missing_results.len()
        );
    }
}

/// Compress a message list — the main entry point.
/// Returns the compressed messages, or None if no compression was performed.
pub async fn compress(
    messages: &[Value],
    _model: &str,
    context_length: usize,
    threshold_percent: f64,
    protect_first_n: usize,
    protect_last_n: usize,
    summary_target_ratio: f64,
    summary_model: Option<&str>,
    provider: &str,
    base_url: &str,
    api_key: &str,
    previous_summary: Option<&str>,
    compression_count: usize,
    quiet: bool,
) -> Option<(Vec<Value>, Option<String>)> {
    let threshold_tokens = (context_length as f64 * threshold_percent) as usize;
    let tail_token_budget = (context_length as f64 * summary_target_ratio * threshold_percent) as usize;
    let n_messages = messages.len();
    let min_required = protect_first_n + protect_last_n + 1;

    if n_messages <= min_required {
        if !quiet {
            eprintln!(
                "Cannot compress: only {} messages (need > {})",
                n_messages, min_required
            );
        }
        return None;
    }

    // Phase 1: Prune old tool results
    let (messages, pruned_count) = prune_old_tool_results(messages, protect_last_n * 3);
    if pruned_count > 0 && !quiet {
        eprintln!("Pre-compression: pruned {} old tool result(s)", pruned_count);
    }

    // Phase 2: Determine boundaries
    let compress_start = align_boundary_forward(&messages, protect_first_n);
    let compress_end =
        find_tail_cut(&messages, compress_start, tail_token_budget, protect_last_n);

    if compress_start >= compress_end {
        return None;
    }

    let turns_to_summarize = &messages[compress_start..compress_end];
    let tail_msgs = n_messages - compress_end;

    if !quiet {
        eprintln!(
            "Context compression triggered (threshold={} tokens)",
            threshold_tokens
        );
        eprintln!(
            "Model context limit: {} tokens ({:.0}% = {})",
            context_length,
            threshold_percent * 100.0,
            threshold_tokens
        );
        eprintln!(
            "Summarizing turns {}-{} ({} turns), protecting {} head + {} tail messages",
            compress_start + 1,
            compress_end,
            turns_to_summarize.len(),
            compress_start,
            tail_msgs
        );
    }

    // Phase 3: Generate structured summary via LLM
    let summary = generate_summary(
        turns_to_summarize,
        context_length,
        provider,
        base_url,
        api_key,
        summary_model,
        previous_summary,
    )
    .await;

    // Phase 4: Assemble compressed message list
    let mut compressed: Vec<Value> = Vec::with_capacity(n_messages - turns_to_summarize.len() + 1);

    // Head messages
    for i in 0..compress_start {
        let mut msg = messages[i].clone();
        if i == 0
            && msg.get("role").and_then(|v| v.as_str()) == Some("system")
            && compression_count == 0
        {
            let existing = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            msg["content"] = json!(format!(
                "{}\n\n[Note: Some earlier conversation turns have been compacted into a handoff summary to preserve context space. The current session state may still reflect earlier work, so build on that summary and state rather than re-doing work.]",
                existing
            ));
        }
        compressed.push(msg);
    }

    // Summary insertion with role collision avoidance
    let mut merge_into_tail = false;
    #[allow(unused_assignments)]
    let mut summary_role = "user".to_string();

    if let Some(ref summ) = summary {
        let last_head_role = messages
            .get(compress_start.saturating_sub(1))
            .and_then(|m| m.get("role").and_then(|v| v.as_str()))
            .unwrap_or("user");
        let first_tail_role = messages
            .get(compress_end)
            .and_then(|m| m.get("role").and_then(|v| v.as_str()))
            .unwrap_or("user");

        // Pick a role avoiding consecutive same-role
        if last_head_role == "assistant" || last_head_role == "tool" {
            summary_role = "user".to_string();
        } else {
            summary_role = "assistant".to_string();
        }

        if summary_role == first_tail_role {
            // Try flipping
            let flipped = if summary_role == "user" { "assistant" } else { "user" };
            if flipped != last_head_role {
                summary_role = flipped.to_string();
                merge_into_tail = false;
            } else {
                // Both would collide — merge into first tail message
                merge_into_tail = true;
            }
        } else {
            merge_into_tail = false;
        }

        if !merge_into_tail {
            let prefixed = with_summary_prefix(summ);
            compressed.push(json!({ "role": summary_role, "content": prefixed }));
        }
    }

    // Tail messages
    for i in compress_end..n_messages {
        let mut msg = messages[i].clone();
        if merge_into_tail {
            if let Some(ref summ) = summary {
                let existing = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let prefixed = with_summary_prefix(summ);
                msg["content"] = json!(format!("{}\n\n{}", prefixed, existing));
            }
            merge_into_tail = false; // only do this once
        }
        compressed.push(msg);
    }

    // Phase 5: Sanitize orphaned tool pairs
    sanitize_tool_pairs(&mut compressed);

    if !quiet {
        let saved = n_messages * 50 - compressed.len() * 50; // rough chars→tokens
        eprintln!(
            "Compressed: {} -> {} messages (~{} tokens saved)",
            n_messages,
            compressed.len(),
            saved.max(0)
        );
    }

    Some((compressed, summary))
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Normalize summary text to the current compaction prefix format.
pub fn normalize_summary_prefix(text: &str) -> String {
    let text = text.trim();
    let text = text.strip_prefix(LEGACY_SUMMARY_PREFIX).unwrap_or(text);
    let text = text.strip_prefix(SUMMARY_PREFIX).unwrap_or(text);
    let text = text.trim();
    if text.is_empty() {
        SUMMARY_PREFIX.to_string()
    } else {
        format!("{}\n{}", SUMMARY_PREFIX, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prune_old_tool_results() {
        let msgs: Vec<Value> = vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "tool", "content": "x".repeat(300), "tool_call_id": "call_1"}),
            json!({"role": "tool", "content": "short", "tool_call_id": "call_2"}),
            json!({"role": "tool", "content": "y".repeat(300), "tool_call_id": "call_3"}),
            json!({"role": "assistant", "content": "done"}),
        ];
        let (pruned, count) = prune_old_tool_results(&msgs, 1);
        assert_eq!(count, 2);
        assert_eq!(
            pruned[1].get("content").and_then(|v| v.as_str()).unwrap(),
            PRUNED_PLACEHOLDER
        );
        assert_eq!(
            pruned[2].get("content").and_then(|v| v.as_str()).unwrap(),
            "short"
        );
    }

    #[test]
    fn test_normalize_summary_prefix() {
        let input = "[CONTEXT SUMMARY]: foo bar";
        let out = normalize_summary_prefix(input);
        assert!(out.starts_with("[CONTEXT COMPACTION]"));
        assert!(out.contains("foo bar"));
    }

    #[test]
    fn test_align_boundary_forward() {
        let msgs: Vec<Value> = vec![
            json!({"role": "user", "content": "a"}),
            json!({"role": "tool", "content": "t1"}),
            json!({"role": "tool", "content": "t2"}),
            json!({"role": "assistant", "content": "b"}),
        ];
        assert_eq!(align_boundary_forward(&msgs, 1), 3);
    }
}

