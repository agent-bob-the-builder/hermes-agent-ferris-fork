//! LLM-based summarization for context compaction.
//! Uses reqwest to call the configured provider's chat completions API.

use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

const SUMMARY_RATIO: f64 = 0.20;
const MIN_SUMMARY_TOKENS: usize = 250;
const SUMMARY_TOKENS_CEILING: usize = 6000;

/// Serialized conversation turn for the summarizer.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SerializedTurn {
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
}

/// Serialize a message dict into a labeled string for the summarizer.
/// Mirrors `_serialize_for_summary` in the Python version.
pub fn serialize_turn(msg: &serde_json::Value) -> SerializedTurn {
    let role = msg
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mut content = msg
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let tool_calls = msg.get("tool_calls").and_then(|v| v.as_array());

    // Truncate long content
    let tool_calls_str = if let Some(tcs) = tool_calls {
        let parts: Vec<String> = tcs
            .iter()
            .filter_map(|tc| {
                let fn_obj = tc.get("function")?;
                let name = fn_obj.get("name")?.as_str()?;
                let args = fn_obj
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or("{}");
                let args_display = if args.len() > 500 {
                    format!("{}...", &args[..400])
                } else {
                    args.to_string()
                };
                Some(format!("  {name}({args_display})"))
            })
            .collect();
        if !parts.is_empty() {
            let tc_str = format!("[Tool calls:\n{}\n]", parts.join("\n"));
            content.push_str(&format!("\n{tc_str}"));
            Some(tc_str)
        } else {
            None
        }
    } else {
        None
    };

    // Truncate very long messages
    if content.len() > 3000 {
        let keep_start = 2000;
        let keep_end = 800;
        content = format!(
            "{}\n...[truncated]...\n{}",
            &content[..keep_start],
            &content[content.len() - keep_end..]
        );
    }

    SerializedTurn {
        role,
        content,
        tool_calls: tool_calls_str,
    }
}

/// Serialize multiple turns into a single string.
pub fn serialize_turns(turns: &[serde_json::Value]) -> String {
    turns
        .iter()
        .map(|msg| {
            let turn = serialize_turn(msg);
            let label = match turn.role.as_str() {
                "tool" => {
                    let tool_id = msg
                        .get("tool_call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    format!("[TOOL RESULT {tool_id}]: {}", turn.content)
                }
                "assistant" => format!("[ASSISTANT]: {}", turn.content),
                _ => format!("[{}]: {}", turn.role.to_uppercase(), turn.content),
            };
            label
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Compute scaled summary budget based on content being summarized.
pub fn compute_summary_budget(turns_to_summarize: &[serde_json::Value], context_length: usize) -> usize {
    let content_tokens = estimate_messages_tokens(turns_to_summarize);
    let budget = (content_tokens as f64 * SUMMARY_RATIO) as usize;
    let max_tokens = (context_length as f64 * 0.05) as usize;
    let capped = max_tokens.min(SUMMARY_TOKENS_CEILING);
    budget.max(MIN_SUMMARY_TOKENS).min(capped)
}

/// Estimate tokens for a slice of message dicts.
fn estimate_messages_tokens(messages: &[serde_json::Value]) -> usize {
    messages
        .iter()
        .map(|msg| {
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let overhead = 10;
            let base = content.len() / 4;
            let tc_overhead = msg
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|tc| {
                            tc.get("function")?
                                .get("arguments")?
                                .as_str()
                        })
                        .map(|args| args.len() / 4)
                        .sum::<usize>()
                })
                .unwrap_or(0);
            base + overhead + tc_overhead
        })
        .sum()
}

// ---------------------------------------------------------------------------
// LLM call
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

/// Call the LLM to generate a summary.
/// Returns the summary text, or None on failure.
pub async fn generate_summary(
    turns_to_summarize: &[serde_json::Value],
    context_length: usize,
    provider: &str,
    base_url: &str,
    api_key: &str,
    summary_model: Option<&str>,
    previous_summary: Option<&str>,
) -> Option<String> {
    let summary_budget = compute_summary_budget(turns_to_summarize, context_length);
    let content_to_summarize = serialize_turns(turns_to_summarize);

    let model = summary_model
        .filter(|s| !s.is_empty())
        .unwrap_or("anthropic/claude-3-haiku");

    let prompt = if let Some(prev) = previous_summary {
        format!(
            r#"You are updating a context compaction summary. A previous compaction produced the summary below. New conversation turns have occurred since then and need to be incorporated.

PREVIOUS SUMMARY:
{prev}

NEW TURNS TO INCORPORATE:
{content_to_summarize}

Update the summary using this exact structure. PRESERVE all existing information that is still relevant. ADD new progress. Move items from "In Progress" to "Done" when completed. Remove information only if it is clearly obsolete.

## Goal
[What the user is trying to accomplish — preserve from previous summary, update if goal evolved]

## Constraints & Preferences
[User preferences, coding style, constraints, important decisions — accumulate across compactions]

## Progress
### Done
[Completed work — include specific file paths, commands run, results obtained]
### In Progress
[Work currently underway]
### Blocked
[Any blockers or issues encountered]

## Key Decisions
[Important technical decisions and why they were made]

## Relevant Files
[Files read, modified, or created — with brief note on each. Accumulate across compactions.]

## Next Steps
[What needs to happen next to continue the work]

## Critical Context
[Any specific values, error messages, configuration details, or data that would be lost without explicit preservation]

Target ~{summary_budget} tokens. Be specific — include file paths, command outputs, error messages, and concrete values rather than vague descriptions.

Write only the summary body. Do not include any preamble or prefix."#
        )
    } else {
        format!(
            r#"Create a structured handoff summary for a later assistant that will continue this conversation after earlier turns are compacted.

TURNS TO SUMMARIZE:
{content_to_summarize}

Use this exact structure:

## Goal
[What the user is trying to accomplish]

## Constraints & Preferences
[User preferences, coding style, constraints, important decisions]

## Progress
### Done
[Completed work — include specific file paths, commands run, results obtained]
### In Progress
[Work currently underway]
### Blocked
[Any blockers or issues encountered]

## Key Decisions
[Important technical decisions and why they were made]

## Relevant Files
[Files read, modified, or created — with brief note on each]

## Next Steps
[What needs to happen next to continue the work]

## Critical Context
[Any specific values, error messages, configuration details, or data that would be lost without explicit preservation]

Target ~{summary_budget} tokens. Be specific — include file paths, command outputs, error messages, and concrete values rather than vague descriptions. The goal is to prevent the next assistant from repeating work or losing important details.

Write only the summary body. Do not include any preamble or prefix."#
        )
    };

    let body = json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.3,
        "max_tokens": summary_budget * 2
    });

    let client = Client::new();
    let url = if base_url.is_empty() {
        "https://api.anthropic.com/v1/messages"
    } else {
        base_url
    };

    // Strip trailing slash
    let url = url.trim_end_matches('/');

    let mut request = client
        .post(format!("{url}/chat/completions"))
        .header("Content-Type", "application/json");

    if !api_key.is_empty() {
        request = request.header("Authorization", format!("Bearer {api_key}"));
    }

    // Add provider-specific headers
    match provider {
        "anthropic" => {
            request = request.header("x-api-key", api_key);
            if !api_key.is_empty() {
                request = request.header("anthropic-version", "2023-06-01");
            }
        }
        "openai" | "azure" | "ollama" | "local" | "" => {
            // Standard OpenAI-compatible
        }
        _ => {}
    }

    let resp = request.json(&body).send().await.ok()?;

    // Try chat/completions first, then messages (Anthropic)
    #[derive(Deserialize)]
    struct ChatResp {
        choices: Option<Vec<ChatChoice>>,
        content: Option<Vec<serde_json::Value>>,
    }

    if let Ok(chat_resp) = resp.json::<ChatResp>().await {
        // OpenAI-style chat/completions
        if let Some(choices) = chat_resp.choices {
            if let Some(choice) = choices.first() {
                if let Some(content) = &choice.message.content {
                    return Some(content.clone());
                }
            }
        }
        // Anthropic messages API
        if let Some(content_arr) = chat_resp.content {
            for block in content_arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        return Some(text.to_string());
                    }
                }
            }
        }
    }

    None
}

/// Wrap summary text with the compaction prefix.
pub fn with_summary_prefix(summary: &str) -> String {
    let prefix = "[CONTEXT COMPACTION] Earlier turns in this conversation were compacted \
        to save context space. The summary below describes work that was \
        already completed, and the current session state may still reflect \
        that work (for example, files may already be changed). Use the summary \
        and the current state to continue from where things left off, and \
        avoid repeating work:";
    format!("{}\n{}", prefix, summary.trim())
}
