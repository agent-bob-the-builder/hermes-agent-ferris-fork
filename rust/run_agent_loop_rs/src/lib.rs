//! run_agent_loop_rs — Rust-owned agent loop for Hermes.
//!
//! ## Architecture: Foreign-loop pattern
//!
//! Python's `AIAgent` owns all initialization state (config, env, clients, tools).
//! When `run_conversation` is called, it hands off to this loop driver. Rust owns
//! the iteration state machine and calls back into Python only for:
//!
//!   1. `_build_system_prompt(...)`       — needs full AIAgent state
//!   2. `_interruptible_streaming_api_call(...)` — SDK has complex SSE handling
//!   3. `handle_function_call(...)`        — Python tool registry
//!   4. `_execute_tool_calls_sequential(...)`  — interactive/clarify tools
//!   5. `_compress_context(...)`          — needs AIAgent state + model metadata
//!
//! The Rust side owns:
//!   - Iteration budget tracking
//!   - Message list construction and mutation
//!   - Retry counter, continuation counter, fallback index
//!   - Final result assembly
//!
//! ## GIL management
//!
//! Python callables are passed as `Py<PyAny>` handles. We acquire the GIL using
//! `Python::attach` (pyo3 0.28.2 API) for each callback. The GIL is released
//! between calls, preventing Rust code from starving the Python interpreter.

use once_cell::sync::Lazy;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

// ─── Constants ────────────────────────────────────────────────────────────────

const MAX_ITERATIONS_DEFAULT: usize = 90;
const MAX_RETRIES: u8 = 3;
const MAX_LENGTH_CONTINUATIONS: u8 = 3;
const MAX_COMPRESSION_ATTEMPTS: u8 = 3;
const MAX_TOOL_RESULT_CHARS: usize = 100_000;

// ─── Config types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub model: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    pub base_url: String,
    pub api_key: String,
    pub api_mode: String,
    pub provider: String,
    pub reasoning_config: Option<HashMap<String, Value>>,
    pub max_tokens: Option<usize>,
    pub enabled_toolsets: Option<Vec<String>>,
    pub disabled_toolsets: Option<Vec<String>>,
    #[serde(default)]
    pub save_trajectories: bool,
    #[serde(default)]
    pub verbose_logging: bool,
    #[serde(default)]
    pub quiet_mode: bool,
    pub platform: Option<String>,
    pub session_id: String,
    #[serde(default)]
    pub fallback_chain: Vec<FallbackProvider>,
    #[serde(default = "default_caution_threshold")]
    pub budget_caution_threshold: f64,
    #[serde(default = "default_warning_threshold")]
    pub budget_warning_threshold: f64,
    #[serde(default)]
    pub use_prompt_caching: bool,
    pub cache_ttl: Option<String>,
}

fn default_max_iterations() -> usize {
    MAX_ITERATIONS_DEFAULT
}

fn default_caution_threshold() -> f64 {
    0.70
}

fn default_warning_threshold() -> f64 {
    0.90
}

#[derive(Debug, Clone, Deserialize)]
pub struct FallbackProvider {
    pub provider: String,
    pub model: String,
}

// ─── Tool types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallInput {
    pub id: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub index: usize,
    pub tool_call_id: String,
    pub content: String,
    pub duration_secs: f64,
    pub is_error: bool,
}

// ─── Persistent state ────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LoopState {
    pub iteration: usize,
    pub retry_count: u8,
    pub compression_count: u8,
    pub fallback_index: usize,
    pub length_continuation_count: u8,
    pub last_error: Option<String>,
    pub using_fallback: bool,
    pub interrupted: bool,
    pub interrupted_message: Option<String>,
}

// ─── Tool safety classification ─────────────────────────────────────────────

static NEVER_PARALLEL_TOOLS: Lazy<std::collections::HashSet<&'static str>> =
    Lazy::new(|| std::collections::HashSet::from(["clarify"]));

static PARALLEL_SAFE_TOOLS: Lazy<std::collections::HashSet<&'static str>> = Lazy::new(|| {
    std::collections::HashSet::from([
        "ha_get_state",
        "ha_list_entities",
        "ha_list_services",
        "honcho_context",
        "honcho_profile",
        "honcho_search",
        "read_file",
        "search_files",
        "session_search",
        "skill_view",
        "skills_list",
        "vision_analyze",
        "web_extract",
        "web_search",
    ])
});

static PATH_SCOPED_TOOLS: Lazy<std::collections::HashSet<&'static str>> =
    Lazy::new(|| std::collections::HashSet::from(["read_file", "write_file", "patch"]));

fn expand_path(raw: &str) -> PathBuf {
    if raw.starts_with("~/") {
        PathBuf::from(
            std::env::var("HOME")
                .map(|h| format!("{}{}", h, &raw[1..]))
                .unwrap_or_else(|_| raw.to_string()),
        )
    } else {
        PathBuf::from(raw)
    }
}

fn paths_share_ancestor(left: &PathBuf, right: &PathBuf) -> bool {
    let lparts = left.components().collect::<Vec<_>>();
    let rparts = right.components().collect::<Vec<_>>();
    let common = std::cmp::min(lparts.len(), rparts.len());
    lparts[..common] == rparts[..common]
}

fn should_parallelize_calls(calls: &[ToolCallInput]) -> bool {
    if calls.len() <= 1 {
        return false;
    }
    let names: Vec<&str> = calls.iter().map(|c| c.function.name.as_str()).collect();
    if names.iter().any(|n| NEVER_PARALLEL_TOOLS.contains(n)) {
        return false;
    }
    let mut reserved: Vec<PathBuf> = Vec::new();
    for call in calls {
        let name = call.function.name.as_str();
        if PATH_SCOPED_TOOLS.contains(name) {
            let Ok(args) = serde_json::from_str::<Value>(&call.function.arguments) else {
                return false;
            };
            let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if raw_path.trim().is_empty() {
                return false;
            }
            let path = expand_path(raw_path);
            if reserved.iter().any(|p| paths_share_ancestor(p, &path)) {
                return false;
            }
            reserved.push(path);
        } else if !PARALLEL_SAFE_TOOLS.contains(name) {
            return false;
        }
    }
    true
}

// ─── Python callbacks ─────────────────────────────────────────────────────────

/// Call a Python callable with no args and extract String.
/// Python::attach returns Result<T> in pyo3 0.28.2.
fn py_call_0_str(
    py: pyo3::Python<'_>,
    callable: &pyo3::Py<pyo3::types::PyAny>,
) -> PyResult<String> {
    let bound = callable.bind(py);
    bound.call0()?.extract()
}

/// Call a Python callable with 1 string arg and extract String.
fn py_call_1_str(
    py: pyo3::Python<'_>,
    callable: &pyo3::Py<pyo3::types::PyAny>,
    arg: &str,
) -> PyResult<String> {
    let bound = callable.bind(py);
    bound.call1((arg,))?.extract()
}

/// Invoke a Python tool handler — (function_name, args_json, task_id) -> String.
/// In pyo3 0.28.2, `Bound::call1` takes individual args and returns
/// PyResult<Bound<PyAny>>. We pass three string args as positional arguments
/// to avoid the PyTuple Result ambiguity in call1's PyCallArgs impl.
fn py_invoke_tool(
    py: pyo3::Python<'_>,
    tool_invoke_fn: &pyo3::Py<pyo3::types::PyAny>,
    function_name: &str,
    args_json: &str,
    task_id: &str,
) -> PyResult<String> {
    let py_fn = tool_invoke_fn.bind(py);
    let result = py_fn.call1((function_name, args_json, task_id))?;
    result.extract()
}

// ─── Tool execution ─────────────────────────────────────────────────────────

/// Execute tool calls in parallel or sequential, return in original order.
fn execute_tools(
    calls: &[ToolCallInput],
    tool_invoke_fn: &pyo3::Py<pyo3::types::PyAny>,
    session_id: &str,
) -> Vec<ToolResult> {
    let parallel = should_parallelize_calls(calls);
    if !parallel {
        // Sequential
        let results: Vec<ToolResult> = calls
            .iter()
            .enumerate()
            .map(|(i, call)| {
                let start = std::time::Instant::now();
                // py_invoke_tool returns PyResult<String>; attach() returns Result<T>
                let content: String = match pyo3::Python::attach(|py| {
                    py_invoke_tool(
                        py,
                        tool_invoke_fn,
                        &call.function.name,
                        &call.function.arguments,
                        session_id,
                    )
                }) {
                    Ok(s) => s,
                    Err(_) => serde_json::json!({ "error": "GIL error" }).to_string(),
                };
                let is_error = content.contains("\"error\"") || content.starts_with("Error:");
                ToolResult {
                    index: i,
                    tool_call_id: call.id.clone(),
                    content,
                    duration_secs: start.elapsed().as_secs_f64(),
                    is_error,
                }
            })
            .collect();
        return results;
    }

    // Parallel via Rayon
    let task_id_owned = session_id.to_string();
    let calls_owned: Vec<_> = calls
        .iter()
        .map(|c| ToolCallInput {
            id: c.id.clone(),
            function: ToolFunction {
                name: c.function.name.clone(),
                arguments: c.function.arguments.clone(),
            },
        })
        .collect();

    let results: Vec<ToolResult> = calls_owned
        .par_iter()
        .enumerate()
        .map(|(i, call)| {
            let start = std::time::Instant::now();
            let content: String = match pyo3::Python::attach(|py| {
                py_invoke_tool(
                    py,
                    tool_invoke_fn,
                    &call.function.name,
                    &call.function.arguments,
                    &task_id_owned,
                )
            }) {
                Ok(s) => s,
                Err(_) => serde_json::json!({ "error": "GIL error" }).to_string(),
            };
            let is_error = content.contains("\"error\"") || content.starts_with("Error:");
            ToolResult {
                index: i,
                tool_call_id: call.id.clone(),
                content,
                duration_secs: start.elapsed().as_secs_f64(),
                is_error,
            }
        })
        .collect();

    // Reorder to original call order by matching tool_call_id
    calls
        .iter()
        .enumerate()
        .map(|(i, call)| {
            results
                .iter()
                .find(|r| r.tool_call_id == call.id)
                .cloned()
                .unwrap_or_else(|| ToolResult {
                    index: i,
                    tool_call_id: call.id.clone(),
                    content: serde_json::json!({ "error": "result not found" }).to_string(),
                    duration_secs: 0.0,
                    is_error: true,
                })
        })
        .collect()
}

// ─── Response parsing helpers ─────────────────────────────────────────────────

fn extract_finish_reason(response: &Value, api_mode: &str) -> String {
    match api_mode {
        "anthropic_messages" => response
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("stop")
            .to_string(),
        "codex_responses" => {
            let status = response
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("complete");
            if status == "incomplete" {
                "length".to_string()
            } else {
                "stop".to_string()
            }
        }
        _ => response
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|choice| choice.get("finish_reason"))
            .and_then(|fr| fr.as_str())
            .unwrap_or("stop")
            .to_string(),
    }
}

fn extract_tool_calls(response: &Value, api_mode: &str) -> Option<Vec<ToolCallInput>> {
    match api_mode {
        "anthropic_messages" => {
            let arr = response.get("content")?.as_array()?;
            extract_anthropic_tool_calls(arr)
        }
        "codex_responses" => {
            let output_arr = response.get("output")?.as_array()?;
            let first = output_arr.first()?;
            let tc_arr = first.get("tool_calls")?.as_array()?;
            extract_codex_tool_calls(tc_arr)
        }
        _ => {
            // response["choices"][0]["message"]["tool_calls"]
            let choices = response.get("choices")?.as_array()?;
            let first = choices.first()?;
            let msg = first.get("message")?;
            let tc_arr = msg.get("tool_calls")?.as_array()?;
            extract_generic_tool_calls(tc_arr)
        }
    }
}

fn extract_anthropic_tool_calls(arr: &[Value]) -> Option<Vec<ToolCallInput>> {
    let mut calls = Vec::new();
    let mut idx = 0usize;
    for block in arr {
        if block.get("type")?.as_str()? == "tool_use" {
            let input = block.get("input")?;
            let name = block.get("name")?.as_str()?.to_string();
            let args_str = serde_json::to_string(input).ok()?;
            calls.push(ToolCallInput {
                id: format!("toolu_{}", idx),
                function: ToolFunction {
                    name,
                    arguments: args_str,
                },
            });
            idx += 1;
        }
    }
    Some(calls)
}

fn extract_codex_tool_calls(arr: &[Value]) -> Option<Vec<ToolCallInput>> {
    let mut calls = Vec::new();
    for tc in arr {
        let name = tc.get("name")?.as_str()?.to_string();
        let args_str = serde_json::to_string(tc.get("input")?).unwrap_or_default();
        calls.push(ToolCallInput {
            id: tc.get("call_id")?.as_str()?.to_string(),
            function: ToolFunction {
                name,
                arguments: args_str,
            },
        });
    }
    Some(calls)
}

fn extract_generic_tool_calls(arr: &[Value]) -> Option<Vec<ToolCallInput>> {
    let mut calls = Vec::new();
    for tc in arr {
        let fn_obj = tc.get("function")?;
        let name = fn_obj.get("name")?.as_str()?.to_string();
        let args_str = fn_obj.get("arguments")?.as_str()?.to_string();
        calls.push(ToolCallInput {
            id: tc.get("id")?.as_str()?.to_string(),
            function: ToolFunction {
                name,
                arguments: args_str,
            },
        });
    }
    Some(calls)
}

fn extract_content(response: &Value, api_mode: &str) -> String {
    match api_mode {
        "anthropic_messages" => response
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            })
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .map(String::from)
            .unwrap_or_default(),
        "codex_responses" => response
            .get("output")
            .and_then(|o| o.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|text| text.get("text"))
            .and_then(|t| t.as_str())
            .map(String::from)
            .unwrap_or_default(),
        _ => response
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|msg| msg.get("content"))
            .and_then(|t| t.as_str())
            .map(String::from)
            .unwrap_or_default(),
    }
}

fn extract_reasoning(response: &Value, api_mode: &str) -> Option<String> {
    match api_mode {
        "anthropic_messages" => response
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("thinking"))
            })
            .and_then(|b| b.get("thinking"))
            .and_then(|t| t.as_str())
            .map(String::from),
        _ => response
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|msg| msg.get("reasoning_content"))
            .and_then(|r| r.as_str())
            .map(String::from),
    }
}

// ─── Loop result ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct LoopResult {
    pub final_response: Option<String>,
    pub last_reasoning: Option<String>,
    pub messages_json: String,
    pub api_calls: usize,
    pub completed: bool,
    pub partial: bool,
    pub interrupted: bool,
    pub error: Option<String>,
    pub state_json: String,
}

// ─── Main loop driver ────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn run_loop(
    config_json: &str,
    user_message: &str,
    conversation_history_json: &str,
    system_prompt_fn: pyo3::Py<pyo3::types::PyAny>,
    api_call_fn: pyo3::Py<pyo3::types::PyAny>,
    tool_invoke_fn: pyo3::Py<pyo3::types::PyAny>,
    interrupt_check_fn: pyo3::Py<pyo3::types::PyAny>,
    on_status_fn: pyo3::Py<pyo3::types::PyAny>,
) -> Result<String, String> {
    let config: AgentConfig =
        serde_json::from_str(config_json).map_err(|e| format!("bad config JSON: {}", e))?;

    let mut state = LoopState::default();
    let max_iters = config.max_iterations.max(1);

    // Build initial message list from history + user message
    let mut messages: Vec<Value> =
        serde_json::from_str(conversation_history_json).unwrap_or_default();
    messages.push(serde_json::json!({
        "role": "user",
        "content": user_message
    }));

    // Build system prompt (called once, cached for the session)
    let system_prompt =
        pyo3::Python::attach(|py| py_call_0_str(py, &system_prompt_fn)).unwrap_or_default();

    // ─── Main iteration loop ───────────────────────────────────────────────
    'main: while state.iteration < max_iters {
        state.iteration += 1;

        // Check interrupt — Python::attach returns Result<T>; chain to get bool
        let is_interrupted = pyo3::Python::attach(|py| {
            let bound = interrupt_check_fn.bind(py);
            bound.call0().and_then(|o| o.is_truthy())
        })
        .unwrap_or(false);

        if is_interrupted {
            state.interrupted = true;
            state.interrupted_message = Some("User interrupt".to_string());
            break 'main;
        }

        // Build API messages: system + conversation
        let mut api_messages: Vec<Value> = vec![serde_json::json!({
            "role": "system",
            "content": &system_prompt
        })];
        api_messages.extend(messages.iter().cloned());
        let api_messages_json =
            serde_json::to_string(&api_messages).map_err(|e| format!("messages JSON: {}", e))?;

        // Status update — attach returns Result<T>; map to () and discard
        let status_msg = format!(
            "Iteration {}/{}: calling API...",
            state.iteration, max_iters
        );
        let _ignored: () = pyo3::Python::attach(|py| {
            let bound = on_status_fn.bind(py);
            bound.call1((status_msg.as_str(),)).map(|_| ())
        })
        .unwrap_or(());

        // Call API
        let response_json =
            match pyo3::Python::attach(|py| py_call_1_str(py, &api_call_fn, &api_messages_json)) {
                Ok(json) => {
                    eprintln!(
                        "DEBUG API response (first 500 chars): {}",
                        &json[..json.len().min(500)]
                    );
                    json
                }
                Err(e) => {
                    state.last_error = Some(format!("API call failed: {}", e));
                    state.retry_count += 1;
                    if state.retry_count >= MAX_RETRIES {
                        break 'main;
                    }
                    continue 'main;
                }
            };

        // Parse response
        let response_val: Value = serde_json::from_str(&response_json)
            .map_err(|e| format!("bad response JSON: {}", e))?;

        let finish_reason = extract_finish_reason(&response_val, &config.api_mode);
        let tool_calls = extract_tool_calls(&response_val, &config.api_mode).unwrap_or_default();
        let content = extract_content(&response_val, &config.api_mode);
        let reasoning = extract_reasoning(&response_val, &config.api_mode);
        eprintln!(
            "DEBUG extract: finish_reason={:?}, tool_calls={}, content_len={}, reasoning_len={:?}",
            finish_reason,
            tool_calls.len(),
            content.len(),
            reasoning.as_ref().map(|s| s.len())
        );

        // ── No tool calls: return content ──────────────────────────────
        if tool_calls.is_empty() || finish_reason == "stop" {
            // Check if content is empty (model wrapped entire response in think blocks)
            let (final_response, partial, error, completed) = if content.is_empty() {
                if let Some(ref reasoning_content) = reasoning {
                    // Use reasoning as the response content
                    (Some(reasoning_content.clone()), false, None, true)
                } else {
                    // Truly empty - no content and no reasoning
                    (
                        None,
                        true,
                        Some(
                            "Model generated only think blocks with no actual response".to_string(),
                        ),
                        false,
                    )
                }
            } else {
                (Some(content), false, None, true)
            };
            let result = LoopResult {
                final_response,
                last_reasoning: reasoning,
                messages_json: serde_json::to_string(&messages).unwrap_or_default(),
                api_calls: state.iteration,
                completed,
                partial,
                interrupted: false,
                error,
                state_json: serde_json::to_string(&state).unwrap_or_default(),
            };
            return serde_json::to_string(&result).map_err(|e| e.to_string());
        }

        // ── Tool calls present: execute them ──────────────────────────
        let results = execute_tools(&tool_calls, &tool_invoke_fn, &config.session_id);

        // Append assistant message
        let assistant_msg = serde_json::json!({
            "role": "assistant",
            "tool_calls": tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "function": {
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        }
                    })
                })
                .collect::<Vec<_>>(),
            "content": serde_json::Value::Null,
        });
        messages.push(assistant_msg);

        // Append tool results in original call order
        for res in &results {
            let mut content = res.content.clone();
            if content.len() > MAX_TOOL_RESULT_CHARS {
                let original_len = content.len();
                content = format!(
                    "{}\n\n[Truncated: {} chars, limit {}]",
                    &content[..MAX_TOOL_RESULT_CHARS],
                    original_len,
                    MAX_TOOL_RESULT_CHARS
                );
            }
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": res.tool_call_id,
                "content": content,
            }));
        }

        // Budget pressure warnings
        let budget_pct = state.iteration as f64 / max_iters as f64;
        if budget_pct >= config.budget_warning_threshold {
            let msg = format!(
                "WARNING: {} iterations remaining ({}% budget used)",
                max_iters - state.iteration,
                (budget_pct * 100.0) as usize
            );
            let _: () = pyo3::Python::attach(|py| {
                let bound = on_status_fn.bind(py);
                bound.call1((msg.as_str(),)).map(|_| ())
            })
            .unwrap_or(());
        } else if budget_pct >= config.budget_caution_threshold {
            let msg = format!(
                "CAUTION: {} iterations remaining",
                max_iters - state.iteration
            );
            let _: () = pyo3::Python::attach(|py| {
                let bound = on_status_fn.bind(py);
                bound.call1((msg.as_str(),)).map(|_| ())
            })
            .unwrap_or(());
        }
    }

    // ── Max iterations or interrupt reached ───────────────────────────
    let final_response = if state.interrupted {
        state.interrupted_message.clone()
    } else {
        Some(format!(
            "Iteration budget exhausted ({} used out of {}). Consider increasing max_iterations.",
            state.iteration, max_iters
        ))
    };

    let result = LoopResult {
        final_response,
        last_reasoning: None,
        messages_json: serde_json::to_string(&messages).unwrap_or_default(),
        api_calls: state.iteration,
        completed: false,
        partial: false,
        interrupted: state.interrupted,
        error: state.last_error.clone(),
        state_json: serde_json::to_string(&state).unwrap_or_default(),
    };

    serde_json::to_string(&result).map_err(|e| e.to_string())
}

// ─── PyO3 bindings ────────────────────────────────────────────────────────────

use pyo3::prelude::*;

#[pyfunction]
fn rs_run_loop(
    config_json: &str,
    user_message: &str,
    conversation_history_json: &str,
    system_prompt_fn: pyo3::Py<pyo3::types::PyAny>,
    api_call_fn: pyo3::Py<pyo3::types::PyAny>,
    tool_invoke_fn: pyo3::Py<pyo3::types::PyAny>,
    interrupt_check_fn: pyo3::Py<pyo3::types::PyAny>,
    on_status_fn: pyo3::Py<pyo3::types::PyAny>,
) -> PyResult<String> {
    run_loop(
        config_json,
        user_message,
        conversation_history_json,
        system_prompt_fn,
        api_call_fn,
        tool_invoke_fn,
        interrupt_check_fn,
        on_status_fn,
    )
    .map_err(pyo3::exceptions::PyRuntimeError::new_err)
}

#[pyfunction]
fn rs_should_parallelize(tool_calls_json: &str) -> PyResult<bool> {
    let calls: Vec<ToolCallInput> = serde_json::from_str(tool_calls_json)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    Ok(should_parallelize_calls(&calls))
}

#[pymodule]
fn run_agent_loop_rs(_py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(rs_run_loop, module)?)?;
    module.add_function(wrap_pyfunction!(rs_should_parallelize, module)?)?;
    module.add(
        "__doc__",
        "Rust-owned agent loop — foreign-loop driver for Hermes.",
    )?;
    Ok(())
}
