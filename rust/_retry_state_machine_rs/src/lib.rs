//! retry_state_machine_rs — Finite-state machine for Hermes agent API retry logic.
//!
//! Replaces the nested retry loops in Python's `run_conversation` with a clean
//! state-machine model. Each state produces a transition based on the API
//! response or error, and the machine drives until `Done`, `Error`, or
//! `MaxRetriesExceeded`.
//!
//! ## States
//!
//! Request → AwaitingResponse
//!   ├──[finish_reason=stop]───────────────────────────────► Done
//!   ├──[finish_reason=tool_calls]────────────────────────► Tools
//!   ├──[finish_reason=length]──► LengthContinuation ───────┤
//!   └──[error]────────► RetryOrFallback ───────────────────┤
//!                         │                                  │
//!                         ├──[retries < max]────► Request   │
//!                         ├──[rate limit]────► Fallback ──────┤
//!                         ├──[413 / ctx overflow]──► Compress│
//!                         │                  compress ─► Request
//!                         └──[max retries]──────► Error ──────┘

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── Constants ───────────────────────────────────────────────────────────────

const MAX_RETRIES: u8 = 3;
const MAX_COMPRESSION_ATTEMPTS: u8 = 3;
const MAX_LENGTH_CONTINUATIONS: u8 = 3;

// ─── API mode ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiMode {
    ChatCompletions,
    CodexResponses,
    AnthropicMessages,
}

impl ApiMode {
    fn from_str(s: &str) -> Self {
        match s {
            "codex_responses" => ApiMode::CodexResponses,
            "anthropic_messages" => ApiMode::AnthropicMessages,
            _ => ApiMode::ChatCompletions,
        }
    }
}

// ─── State ─────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MachineState {
    pub retry_count: u8,
    pub compression_count: u8,
    pub fallback_index: usize,
    pub length_continuation_count: u8,
    pub last_error: Option<String>,
    pub using_fallback: bool,
}

// ─── Commands for Python ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum AgentCommand {
    LengthContinuation { messages_json: String },
    CompressAndRequest {
        messages_json: String,
        compressed_messages_json: String,
    },
    FallbackAndRequest {
        messages_json: String,
        provider: String,
        model: String,
    },
    InjectErrorAndReturn { error: String },
    Status {
        message: String,
        spinner_label: Option<String>,
    },
    ToolCalls,
}

// ─── Machine result ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MachineResult {
    Command(AgentCommand),
    Done {
        final_response_json: String,
        tool_calls_present: bool,
    },
    Failed {
        error: String,
        messages_json: String,
        partial: bool,
    },
}

// ─── Response evaluation ───────────────────────────────────────────────────

struct ResponseEval {
    finish_reason: String,
    is_empty: bool,
    is_length: bool,
    is_tool_calls: bool,
    is_stop: bool,
    error_code: Option<String>,
    error_message: Option<String>,
    http_status: Option<u16>,
}

fn evaluate_response(mode: ApiMode, finish_reason: &str, response_json: &str) -> ResponseEval {
    let mut eval = ResponseEval {
        finish_reason: finish_reason.to_string(),
        is_empty: false,
        is_length: false,
        is_tool_calls: false,
        is_stop: false,
        error_code: None,
        error_message: None,
        http_status: None,
    };

    if let Ok(v) = serde_json::from_str::<Value>(response_json) {
        match mode {
            ApiMode::CodexResponses => {
                let status = v.get("status").and_then(|s| s.as_str());
                let incomplete = v.get("incomplete_details");
                let incomplete_reason =
                    incomplete.and_then(|d| d.get("reason")).and_then(|r| r.as_str());

                if status == Some("incomplete")
                    && incomplete_reason
                        .map(|r| r == "max_output_tokens" || r == "length")
                        .unwrap_or(false)
                {
                    eval.is_length = true;
                } else {
                    eval.is_stop = true;
                }
            }
            ApiMode::AnthropicMessages => match finish_reason {
                "end_turn" | "stop_sequence" => eval.is_stop = true,
                "tool_use" => eval.is_tool_calls = true,
                "max_tokens" => eval.is_length = true,
                _ => eval.is_stop = true,
            },
            ApiMode::ChatCompletions => match finish_reason {
                "stop" | "stop_sequence" => eval.is_stop = true,
                "tool_calls" | "function_call" => eval.is_tool_calls = true,
                "length" => eval.is_length = true,
                _ => eval.is_stop = true,
            },
        }

        if let Some(err) = v.get("error").or_else(|| v.get("message")) {
            eval.error_message = err.as_str().map(String::from);
        }
        if let Some(code) = v.get("code").or_else(|| v.get("type")) {
            eval.error_code = code.as_str().map(String::from);
        }
        if let Some(status) = v.get("status_code").and_then(|s| s.as_u64()) {
            eval.http_status = Some(status as u16);
        }
    }

    eval
}

// ─── Error classification ───────────────────────────────────────────────────

struct ErrorReason {
    code: String,
    detail: String,
    http_status: Option<u16>,
}

fn classify_error(
    error_json: &str,
    fallback_chain_len: usize,
    fallback_index: usize,
) -> Option<ErrorReason> {
    let err_val: Value = serde_json::from_str(error_json).ok()?;

    let msg = err_val
        .get("error")
        .and_then(|e| e.as_str())
        .or_else(|| err_val.get("message").and_then(|m| m.as_str()))
        .unwrap_or("")
        .to_lowercase();

    let status = err_val
        .get("status_code")
        .and_then(|s| s.as_u64())
        .map(|s| s as u16);

    if status == Some(429)
        || msg.contains("rate limit")
        || msg.contains("too many requests")
        || msg.contains("usage limit")
        || msg.contains("quota")
    {
        return Some(ErrorReason {
            code: "rate_limited".to_string(),
            detail: format!("Rate limited (HTTP {:?})", status),
            http_status: status,
        });
    }

    if status == Some(413)
        || msg.contains("payload too large")
        || msg.contains("request entity too large")
        || msg.contains("error code: 413")
    {
        return Some(ErrorReason {
            code: "ctx_overflow".to_string(),
            detail: "Request payload too large".to_string(),
            http_status: status,
        });
    }

    let ctx_keywords = [
        "context length",
        "context size",
        "maximum context",
        "token limit",
        "too many tokens",
        "reduce the length",
        "exceeds the limit",
        "context window",
        "prompt is too long",
        "prompt exceeds max length",
    ];
    if ctx_keywords.iter().any(|k| msg.contains(k)) {
        return Some(ErrorReason {
            code: "ctx_overflow".to_string(),
            detail: "Context length exceeded".to_string(),
            http_status: status,
        });
    }

    if let Some(s) = status {
        if (400..500).contains(&s) && s != 429 && s != 413 {
            return Some(ErrorReason {
                code: "client_error".to_string(),
                detail: format!("HTTP {} client error", s),
                http_status: Some(s),
            });
        }
    }

    if fallback_index < fallback_chain_len {
        return Some(ErrorReason {
            code: "try_fallback".to_string(),
            detail: "Trying fallback provider".to_string(),
            http_status: status,
        });
    }

    Some(ErrorReason {
        code: "other_error".to_string(),
        detail: msg,
        http_status: status,
    })
}

// ─── Content extraction ────────────────────────────────────────────────────

fn extract_content(mode: ApiMode, response_json: &str) -> String {
    let val: Value = match serde_json::from_str(response_json) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    match mode {
        ApiMode::CodexResponses => val
            .get("output")
            .and_then(|o| o.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .map(String::from)
            .unwrap_or_default(),
        ApiMode::AnthropicMessages => val
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
        ApiMode::ChatCompletions => val
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

fn has_tool_calls(tool_calls_json: &str) -> bool {
    if let Ok(val) = serde_json::from_str::<Value>(tool_calls_json) {
        if val.is_array() {
            return !val.as_array().unwrap().is_empty();
        }
        return !val.is_null();
    }
    false
}

// Helper: serialize a MachineResult to String, collapsing serde errors into "..."
fn serialize_result(result: &MachineResult) -> String {
    serde_json::to_string(result).unwrap_or_else(|_| String::from("{}"))
}

// ─── Main evaluate function ────────────────────────────────────────────────

/// Evaluate an API response and decide what the agent should do next.
#[no_mangle]
pub extern "C" fn evaluate(
    mode: &str,
    finish_reason: &str,
    response_json: &str,
    messages_json: &str,
    state_json: &str,
    fallback_chain_json: &str,
    tool_calls_json: &str,
) -> *mut std::os::raw::c_char {
    // Convenience: return a boxed CString and return a raw pointer.
    // Caller (PyO3) will convert to String and manage memory.
    let result = evaluate_inner(
        mode,
        finish_reason,
        response_json,
        messages_json,
        state_json,
        fallback_chain_json,
        tool_calls_json,
    );
    let cstring = std::ffi::CString::new(result).unwrap_or_else(|_| {
        std::ffi::CString::new(String::from("{\"type\":\"Failed\",\"error\":\"encoding error\",\"messages_json\":\"\",\"partial\":true}")).unwrap()
    });
    std::ffi::CString::into_raw(cstring) as *mut std::os::raw::c_char
}

fn evaluate_inner(
    mode: &str,
    finish_reason: &str,
    response_json: &str,
    messages_json: &str,
    state_json: &str,
    fallback_chain_json: &str,
    tool_calls_json: &str,
) -> String {
    let api_mode = ApiMode::from_str(mode);
    let mut state: MachineState = serde_json::from_str(state_json).unwrap_or_default();
    let fallback_chain: Vec<serde_json::Map<String, Value>> =
        serde_json::from_str(fallback_chain_json).unwrap_or_default();

    let eval = evaluate_response(api_mode, finish_reason, response_json);

    // Happy path: stop
    let has_tc = has_tool_calls(tool_calls_json);
    if eval.is_stop && !has_tc {
        let content = extract_content(api_mode, response_json);
        let final_resp = serde_json::json!({
            "final_response": content,
            "tool_calls_present": false,
        });
        let result = MachineResult::Done {
            final_response_json: serde_json::to_string(&final_resp).unwrap_or_default(),
            tool_calls_present: false,
        };
        return serialize_result(&result);
    }

    // Tool calls
    if eval.is_tool_calls || has_tc {
        let result = MachineResult::Command(AgentCommand::ToolCalls);
        return serialize_result(&result);
    }

    // Length continuation
    if eval.is_length {
        state.length_continuation_count += 1;
        if state.length_continuation_count < MAX_LENGTH_CONTINUATIONS {
            let continuation_msg = serde_json::json!({
                "role": "user",
                "content": "[System: Your previous response was truncated by the output length limit. Continue exactly where you left off. Do not restart or repeat prior text. Finish the answer directly.]"
            });
            let mut msgs: Vec<Value> = serde_json::from_str(messages_json).unwrap_or_default();
            msgs.push(continuation_msg);
            let result = MachineResult::Command(AgentCommand::LengthContinuation {
                messages_json: serde_json::to_string(&msgs).unwrap_or_default(),
            });
            return serialize_result(&result);
        } else {
            let content = extract_content(api_mode, response_json);
            if !content.trim().is_empty() {
                let result = MachineResult::Done {
                    final_response_json: serde_json::to_string(
                        &serde_json::json!({ "final_response": content, "partial": true }),
                    )
                    .unwrap_or_default(),
                    tool_calls_present: false,
                };
                return serialize_result(&result);
            }
            let result = MachineResult::Failed {
                error: "Response truncated: max length continuations exceeded".to_string(),
                messages_json: messages_json.to_string(),
                partial: true,
            };
            return serialize_result(&result);
        }
    }

    // Error path
    if let Some(reason) = classify_error(response_json, fallback_chain.len(), state.fallback_index)
    {
        state.last_error = Some(reason.detail.clone());

        if reason.code == "rate_limited" && state.fallback_index < fallback_chain.len() {
            let fb = &fallback_chain[state.fallback_index];
            let provider = fb
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let model = fb.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
            state.fallback_index += 1;
            state.using_fallback = true;
            state.retry_count = 0;
            let result = MachineResult::Command(AgentCommand::FallbackAndRequest {
                messages_json: messages_json.to_string(),
                provider: provider.to_string(),
                model: model.to_string(),
            });
            return serialize_result(&result);
        }

        if reason.code == "ctx_overflow" {
            state.compression_count += 1;
            if state.compression_count >= MAX_COMPRESSION_ATTEMPTS {
                let result = MachineResult::Failed {
                    error: format!(
                        "Request payload too large: max compression attempts ({}) reached",
                        MAX_COMPRESSION_ATTEMPTS
                    ),
                    messages_json: messages_json.to_string(),
                    partial: true,
                };
                return serialize_result(&result);
            }
            let result = MachineResult::Command(AgentCommand::Status {
                message: format!(
                    "⚠️  Request payload too large — compression attempt {}/{}...",
                    state.compression_count, MAX_COMPRESSION_ATTEMPTS
                ),
                spinner_label: Some("📦 compressing...".to_string()),
            });
            return serialize_result(&result);
        }

        state.retry_count += 1;
        if state.retry_count >= MAX_RETRIES {
            if state.fallback_index < fallback_chain.len() {
                let fb = &fallback_chain[state.fallback_index];
                let provider = fb
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let model = fb.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
                state.fallback_index += 1;
                state.using_fallback = true;
                state.retry_count = 0;
                let result = MachineResult::Command(AgentCommand::FallbackAndRequest {
                    messages_json: messages_json.to_string(),
                    provider: provider.to_string(),
                    model: model.to_string(),
                });
                return serialize_result(&result);
            }
            let result = MachineResult::Failed {
                error: format!(
                    "API call failed after {} retries: {}",
                    MAX_RETRIES, reason.detail
                ),
                messages_json: messages_json.to_string(),
                partial: false,
            };
            return serialize_result(&result);
        }

        let backoff = std::cmp::min(5 * (2_u64.pow(state.retry_count as u32 - 1)), 120);
        let result = MachineResult::Command(AgentCommand::Status {
            message: format!(
                "⚠️  API call failed (attempt {}/{}): {} — retrying in {}s",
                state.retry_count, MAX_RETRIES, reason.detail, backoff
            ),
            spinner_label: Some(format!("⏳ retrying in {}s...", backoff)),
        });
        return serialize_result(&result);
    }

    // Empty response
    let content = extract_content(api_mode, response_json);
    if content.trim().is_empty() {
        state.retry_count += 1;
        if state.retry_count >= MAX_RETRIES {
            let result = MachineResult::Failed {
                error: "Model generated empty response after max retries".to_string(),
                messages_json: messages_json.to_string(),
                partial: false,
            };
            return serialize_result(&result);
        }
        let result = MachineResult::Command(AgentCommand::Status {
            message: format!(
                "⚠️  Empty response — retrying ({}/{})...",
                state.retry_count, MAX_RETRIES
            ),
            spinner_label: Some(format!(
                "⚠️  empty, retrying {}/{}...",
                state.retry_count, MAX_RETRIES
            )),
        });
        return serialize_result(&result);
    }

    // Fallback: treat as stop
    let result = MachineResult::Done {
        final_response_json: serde_json::to_string(&serde_json::json!({ "final_response": content }))
            .unwrap_or_default(),
        tool_calls_present: false,
    };
    serialize_result(&result)
}

/// Serialize initial machine state.
#[no_mangle]
pub extern "C" fn make_state() -> *mut std::os::raw::c_char {
    let result = serde_json::to_string(&MachineState::default()).unwrap_or_default();
    let cstring =
        std::ffi::CString::new(result).unwrap_or_else(|_| std::ffi::CString::new("{}").unwrap());
    std::ffi::CString::into_raw(cstring) as *mut std::os::raw::c_char
}

// ─── PyO3 bindings ─────────────────────────────────────────────────────────

use pyo3::prelude::*;

/// Convert a raw C string returned by `evaluate` into a Python String.
/// Caller owns the pointer; we free it after converting.
fn c_string_to_py_string(ptr: *mut std::os::raw::c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe {
        let cstring = std::ffi::CString::from_raw(ptr);
        cstring.to_string_lossy().into_owned()
    }
}

#[pyfunction]
fn rs_evaluate(
    mode: &str,
    finish_reason: &str,
    response_json: &str,
    messages_json: &str,
    state_json: &str,
    fallback_chain_json: &str,
    tool_calls_json: &str,
) -> PyResult<String> {
    let ptr = evaluate(
        mode,
        finish_reason,
        response_json,
        messages_json,
        state_json,
        fallback_chain_json,
        tool_calls_json,
    );
    Ok(c_string_to_py_string(ptr))
}

#[pyfunction]
fn rs_make_state() -> String {
    let ptr = make_state();
    c_string_to_py_string(ptr)
}

#[pymodule]
fn _retry_state_machine_rs(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(rs_evaluate, module)?)?;
    module.add_function(wrap_pyfunction!(rs_make_state, module)?)?;
    module.add(
        "__doc__",
        "Retry state machine for Hermes agent API call handling — Rust backend.",
    )?;
    Ok(())
}
