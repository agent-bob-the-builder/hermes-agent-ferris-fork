//! tool_dispatch_rs — High-performance concurrent tool dispatcher for Hermes Agent.
//!
//! Replaces Python's `_execute_tool_calls_concurrent` with a Rayon-based parallel
//! executor that preserves ordering, detects path overlaps for file tools, and
//! calls back into Python for the actual tool invocation.

use once_cell::sync::Lazy;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ─── Constants ────────────────────────────────────────────────────────────────

static PARALLEL_SAFE_TOOLS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    HashSet::from([
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

static PATH_SCOPED_TOOLS: Lazy<HashSet<&'static str>> =
    Lazy::new(|| HashSet::from(["read_file", "write_file", "patch"]));

static NEVER_PARALLEL_TOOLS: Lazy<HashSet<&'static str>> = Lazy::new(|| HashSet::from(["clarify"]));

// ─── Serialized types ─────────────────────────────────────────────────────────

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
    pub content: String,
    pub tool_call_id: String,
    pub duration_secs: f64,
    pub is_error: bool,
}

// ─── Path helpers ────────────────────────────────────────────────────────────

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

fn paths_share_ancestor(left: &Path, right: &Path) -> bool {
    let lparts = left.components().collect::<Vec<_>>();
    let rparts = right.components().collect::<Vec<_>>();
    let common = std::cmp::min(lparts.len(), rparts.len());
    lparts[..common] == rparts[..common]
}

// ─── Decision logic ───────────────────────────────────────────────────────────

/// Returns `true` if the given tool call batch is safe to run concurrently.
#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn should_parallelize(tool_calls_json: &str) -> bool {
    let Ok(calls) = serde_json::from_str::<Vec<ToolCallInput>>(tool_calls_json) else {
        return false;
    };
    if calls.len() <= 1 {
        return false;
    }
    let names: Vec<&str> = calls.iter().map(|c| c.function.name.as_str()).collect();

    if names.iter().any(|n| NEVER_PARALLEL_TOOLS.contains(n)) {
        return false;
    }

    let mut reserved_paths: Vec<PathBuf> = Vec::new();
    for call in &calls {
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
            if reserved_paths
                .iter()
                .any(|p| paths_share_ancestor(p, &path))
            {
                return false;
            }
            reserved_paths.push(path);
        } else if !PARALLEL_SAFE_TOOLS.contains(name) {
            return false;
        }
    }
    true
}

// ─── Tool invocation ──────────────────────────────────────────────────────────

/// Call Python to execute a single tool.
/// `invoke_py` is model_tools_rs.rs_dispatch: (function_name, args_json, task_id) -> str
fn invoke_single(
    py: pyo3::Python<'_>,
    invoke_py: &pyo3::Py<pyo3::types::PyAny>,
    function_name: &str,
    args_json: &str,
    task_id: Option<&str>,
) -> String {
    let py_fn = invoke_py.bind(py);
    let tid_str = task_id.unwrap_or("");
    let args_tuple = pyo3::types::PyTuple::new(py, [function_name, args_json, tid_str])
        .expect("hardcoded args, PyTuple::new should not fail");
    match py_fn.call1(args_tuple) {
        Ok(result) => result
            .extract::<String>()
            .unwrap_or_else(|_| serde_json::json!({ "error": "extract failed" }).to_string()),
        Err(e) => serde_json::json!({ "error": format!("call error: {}", e) }).to_string(),
    }
}

// ─── Parallel execution via Rayon ─────────────────────────────────────────────
//
// The GIL is released between tool invocations. Each rayon thread acquires
// the GIL independently via Python::attach.

/// Run tool calls concurrently with Rayon. Results are returned in original order.
#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn run_concurrent_tool_batch(
    tool_calls_json: &str,
    invoke_py: pyo3::Py<pyo3::types::PyAny>,
    task_id: Option<&str>,
) -> Result<String, String> {
    let calls: Vec<ToolCallInput> =
        serde_json::from_str(tool_calls_json).map_err(|e| format!("parse error: {}", e))?;
    if calls.is_empty() {
        return Ok("[]".to_string());
    }

    let task_id_owned = task_id.map(String::from);

    // Pre-validate args and build owned copies for rayon
    let parsed: Vec<(usize, String, String)> = calls
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let args_json = &c.function.arguments;
            serde_json::from_str::<Value>(args_json).ok()?;
            Some((i, c.function.name.clone(), args_json.clone()))
        })
        .collect();

    // Each rayon thread acquires GIL via Python::attach for its Python call.
    let results: Vec<ToolResult> = parsed
        .par_iter()
        .map(|(i, name, args_json)| {
            let content = pyo3::Python::attach(|py| {
                invoke_single(py, &invoke_py, name, args_json, task_id_owned.as_deref())
            });
            let is_error = content.contains("\"error\"") || content.starts_with("Error:");
            // Use the captured index to look up tool_call_id — avoids wrong-result
            // when multiple calls share the same function name.
            let tool_call_id = calls.get(*i).map(|c| c.id.clone()).unwrap_or_default();
            ToolResult {
                index: *i,
                content,
                tool_call_id,
                duration_secs: 0.0,
                is_error,
            }
        })
        .collect();

    // Re-index from original positions
    let tool_results: Vec<ToolResult> = calls
        .iter()
        .enumerate()
        .map(|(i, call)| {
            let result = results.iter().find(|r| r.tool_call_id == call.id).cloned();
            result.unwrap_or_else(|| {
                let name = call.function.name.as_str();
                ToolResult {
                    index: i,
                    content: format!("{{\"error\": \"tool '{}' result not found\"}}", name),
                    tool_call_id: call.id.clone(),
                    duration_secs: 0.0,
                    is_error: true,
                }
            })
        })
        .collect();

    serde_json::to_string(&tool_results).map_err(|e| format!("serialize error: {}", e))
}

// ─── PyO3 bindings ────────────────────────────────────────────────────────────

use pyo3::prelude::*;

#[pyfunction]
fn rs_should_parallelize(tool_calls_json: &str) -> bool {
    should_parallelize(tool_calls_json)
}

#[pyfunction]
fn rs_run_concurrent_tool_batch(
    tool_calls_json: &str,
    invoke_py: pyo3::Py<pyo3::types::PyAny>,
    task_id: Option<String>,
) -> PyResult<String> {
    run_concurrent_tool_batch(tool_calls_json, invoke_py, task_id.as_deref())
        .map_err(pyo3::exceptions::PyValueError::new_err)
}

#[pymodule]
fn _tool_dispatch_rs(_py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(rs_should_parallelize, module)?)?;
    module.add_function(wrap_pyfunction!(rs_run_concurrent_tool_batch, module)?)?;
    module.add(
        "__doc__",
        "Concurrent tool dispatcher with Rayon — Rust backend for Hermes tool execution.",
    )?;
    Ok(())
}
