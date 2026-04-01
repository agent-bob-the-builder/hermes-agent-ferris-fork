//! PyO3 0.28 bindings for the Rust ContextCompressor.
//!
//! Key patterns:
//! - `Bound<'_, T>` for references into Python's memory
//! - `.into_pyobject(py)` → `Py<T>`, then `.into_any().into()` → `Py<PyAny>`
//! - `IntoPyObjectExt::into_bound_py_any(py)` for primitive → Python objects
//! - `unsafe { Python::assume_attached() }` for GIL when no messages available
//! - `thread::scope()` + `spawn` for running async from sync Python

use once_cell::sync::Lazy;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::runtime::Runtime;

/// Static Tokio runtime shared across all compress calls.
/// Created once; `spawn_blocking` is used for CPU-bound compression work
/// so the runtime's async I/O workers remain available for HTTP calls.
static RUNTIME: Lazy<Runtime> = Lazy::new(|| Runtime::new().unwrap());

// ---------------------------------------------------------------------
// Job state machine for non-blocking compression
// ---------------------------------------------------------------------

enum JobState {
    Running,
    Completed(Vec<Value>, Option<String>),
    #[allow(dead_code)]
    Failed(String), // reserved for future error propagation
    Cancelled,
}

static JOB_STORE: Mutex<Option<HashMap<usize, JobState>>> = Mutex::new(None);
static JOB_COUNTER: Mutex<usize> = Mutex::new(0);

fn get_job_store() -> &'static Mutex<Option<HashMap<usize, JobState>>> {
    &JOB_STORE
}

fn next_job_id() -> usize {
    let mut counter = JOB_COUNTER.lock().unwrap();
    *counter += 1;
    *counter
}

mod compressor;
mod summarizer;
mod tokenizer;
pub use tokenizer::is_tiktoken_available;

pub use compressor::{compress, sanitize_tool_pairs};
pub use summarizer::generate_summary;

// ---------------------------------------------------------------------------
pub fn val_to_py(py: Python<'_>, v: &Value) -> Py<PyAny> {
    use pyo3::IntoPyObjectExt;
    match v {
        Value::Null => py.None(),
        Value::Bool(b) => (*b).into_py_any(py).unwrap(),
        Value::Number(n) => n.to_string().into_pyobject(py).unwrap().into_any().into(),
        Value::String(s) => s.clone().into_pyobject(py).unwrap().into_any().into(),
        Value::Array(arr) => arr
            .iter()
            .map(|x| val_to_py(py, x))
            .collect::<Vec<_>>()
            .into_pyobject(py)
            .unwrap()
            .into_any()
            .into(),
        Value::Object(obj) => {
            let dict = PyDict::new(py);
            for (k, val) in obj {
                dict.set_item(k.as_str(), val_to_py(py, val)).unwrap();
            }
            dict.into_any().into()
        }
    }
}

fn dict_to_json(dict: &Bound<'_, PyDict>) -> Value {
    let mut map = serde_json::Map::new();
    for (k, v) in dict.iter() {
        let key = k.extract::<String>().unwrap_or_default();
        let val = py_to_val(dict.py(), &v);
        map.insert(key, val);
    }
    Value::Object(map)
}

pub fn py_to_val(py: Python<'_>, v: &Bound<'_, PyAny>) -> Value {
    if v.is_none() {
        return Value::Null;
    }
    if let Ok(b) = v.extract::<bool>() {
        return Value::Bool(b);
    }
    if let Ok(i) = v.extract::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = v.extract::<f64>() {
        return serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null);
    }
    if let Ok(s) = v.extract::<String>() {
        return Value::String(s);
    }
    if let Ok(arr) = v.extract::<Vec<Bound<'_, PyAny>>>() {
        return Value::Array(arr.iter().map(|x| py_to_val(py, x)).collect());
    }
    if let Ok(dict) = v.cast::<PyDict>() {
        return dict_to_json(dict);
    }
    Value::Null
}

fn json_to_dict<'py>(py: Python<'py>, v: &Value) -> Bound<'py, PyDict> {
    let dict = PyDict::new(py);
    if let Value::Object(obj) = v {
        for (k, val) in obj {
            dict.set_item(k.as_str(), val_to_py(py, val))
                .expect("dict set_item failed");
        }
    }
    dict
}

/// Convert a JSON message list to a Python list of dicts.
fn json_msgs_to_py(py: Python<'_>, msgs: Vec<Value>) -> Vec<Py<PyAny>> {
    msgs.into_iter()
        .map(|v: Value| {
            let dict: Bound<'_, PyDict> = json_to_dict(py, &v);
            dict.into_any()
                .into_pyobject(py)
                .expect("dict into_pyobject failed")
                .into()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Python-callable wrappers
// ---------------------------------------------------------------------------

/// Prune old tool results (cheap, no LLM).
#[pyfunction]
fn prune_old_tool_results(
    messages: Vec<Bound<'_, PyDict>>,
    protect_tail_count: usize,
) -> PyResult<(Vec<Py<PyAny>>, usize)> {
    let py = unwrap_py(messages.first());
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
    let (pruned, count) = compressor::prune_old_tool_results(&json_msgs, protect_tail_count);
    Ok((json_msgs_to_py(py, pruned), count))
}

/// Align compress_start forward past orphan tool results.
#[pyfunction]
fn align_boundary_forward(messages: Vec<Bound<'_, PyDict>>, idx: usize) -> PyResult<usize> {
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
    Ok(compressor::align_boundary_forward(&json_msgs, idx))
}

/// Align compress_end backward to avoid splitting tool groups.
#[pyfunction]
fn align_boundary_backward(messages: Vec<Bound<'_, PyDict>>, idx: usize) -> PyResult<usize> {
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
    Ok(compressor::align_boundary_backward(&json_msgs, idx))
}

/// Find tail cut by token budget.
#[pyfunction]
fn find_tail_cut(
    messages: Vec<Bound<'_, PyDict>>,
    head_end: usize,
    token_budget: usize,
    protect_last_n: usize,
) -> PyResult<usize> {
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
    Ok(compressor::find_tail_cut(
        &json_msgs,
        head_end,
        token_budget,
        protect_last_n,
    ))
}

/// Sanitize orphaned tool_call / tool_result pairs.
#[pyfunction]
fn sanitize_tool_pairs_py(messages: Vec<Bound<'_, PyDict>>) -> PyResult<Vec<Py<PyAny>>> {
    let py = unwrap_py(messages.first());
    let mut json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
    compressor::sanitize_tool_pairs(&mut json_msgs);
    Ok(json_msgs_to_py(py, json_msgs))
}

/// Compute summary token budget.
#[pyfunction]
fn compute_summary_budget(
    turns_to_summarize: Vec<Bound<'_, PyDict>>,
    context_length: usize,
) -> PyResult<usize> {
    let json_msgs: Vec<Value> = turns_to_summarize.iter().map(|d| dict_to_json(d)).collect();
    Ok(summarizer::compute_summary_budget(
        &json_msgs,
        context_length,
    ))
}

/// Serialize conversation turns for the summarizer.
#[pyfunction]
fn serialize_turns(messages: Vec<Bound<'_, PyDict>>) -> PyResult<String> {
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
    Ok(summarizer::serialize_turns(&json_msgs))
}

/// Normalize summary to current prefix format.
#[pyfunction]
fn normalize_summary_prefix(text: &str) -> PyResult<String> {
    Ok(compressor::normalize_summary_prefix(text))
}

/// Token estimate for a message dict.
#[pyfunction]
fn estimate_message_tokens(msg: Bound<'_, PyDict>) -> PyResult<usize> {
    let json_msg = dict_to_json(&msg);
    Ok(tokenizer::estimate_message_dict_tokens(&json_msg))
}

/// Token estimate for a list of message dicts.
#[pyfunction]
fn estimate_messages_tokens(messages: Vec<Bound<'_, PyDict>>) -> PyResult<usize> {
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
    Ok(tokenizer::estimate_messages_tokens(&json_msgs))
}

/// Non-blocking compress — runs compression in a background thread via
/// `spawn_blocking` on the static `RUNTIME` and waits for the result.
///
/// The GIL is released automatically by PyO3 when blocking on `rx.recv()`,
/// allowing other Python threads (including the agent loop) to run while
/// compression is in progress. The GIL is re-acquired before the match
/// below executes.
#[pyfunction]
fn compress_async(
    py: Python<'_>,
    messages: Vec<Bound<'_, PyDict>>,
    model: String,
    context_length: usize,
    threshold_percent: f64,
    protect_first_n: usize,
    protect_last_n: usize,
    summary_target_ratio: f64,
    summary_model: Option<String>,
    provider: String,
    base_url: String,
    api_key: String,
    previous_summary: Option<String>,
    compression_count: usize,
    quiet: bool,
) -> PyResult<(Option<Vec<Py<PyAny>>>, Option<String>)> {
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();

    let (tx, rx) = std::sync::mpsc::channel();

    // Use the static runtime's `spawn_blocking` so the compression work
    // (CPU-bound summarization + any HTTP I/O) runs on the blocking-thread
    // pool without blocking the async worker threads.
    RUNTIME.spawn_blocking(move || {
        // json_msgs is owned by this closure for the duration of compression.
        let result = RUNTIME.block_on(compressor::compress(
            &json_msgs,
            &model,
            context_length,
            threshold_percent,
            protect_first_n,
            protect_last_n,
            summary_target_ratio,
            summary_model.as_deref(),
            &provider,
            &base_url,
            &api_key,
            previous_summary.as_deref(),
            compression_count,
            quiet,
        ));
        let _ = tx.send(result);
    });

    // The GIL is released automatically by PyO3 at this suspend-point — the
    // `#[pyfunction]` calling convention means `py` is implicit, and PyO3
    // releases the GIL when blocking on `rx.recv()`. The GIL is re-acquired
    // when `rx.recv()` returns (before the match below executes).
    let result = rx.recv();

    match result {
        Ok(Some((compressed, summary_text))) => {
            // GIL is held again here — safe to build Python objects.
            Ok((Some(json_msgs_to_py(py, compressed)), summary_text))
        }
        Ok(None) => Ok((None, None)),
        Err(_) => Ok((None, None)),
    }
}

// ---------------------------------------------------------------------------
// PyContextCompressor
// ---------------------------------------------------------------------------

#[pyclass]
struct PyContextCompressor {
    model: String,
    context_length: usize,
    threshold_percent: f64,
    protect_first_n: usize,
    protect_last_n: usize,
    summary_target_ratio: f64,
    summary_model: Option<String>,
    provider: String,
    base_url: String,
    api_key: String,
    compression_count: usize,
    quiet: bool,
    #[pyo3(get)]
    previous_summary: Option<String>,
    #[pyo3(get)]
    last_prompt_tokens: usize,
    #[pyo3(get)]
    last_completion_tokens: usize,
    #[pyo3(get)]
    last_total_tokens: usize,
}

#[pymethods]
impl PyContextCompressor {
    #[new]
    #[pyo3(signature = (
        model,
        context_length,
        threshold_percent = 0.50,
        protect_first_n = 3,
        protect_last_n = 20,
        summary_target_ratio = 0.20,
        summary_model = None,
        provider = None,
        base_url = None,
        api_key = None,
        quiet = false,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        model: String,
        context_length: usize,
        threshold_percent: f64,
        protect_first_n: usize,
        protect_last_n: usize,
        summary_target_ratio: f64,
        summary_model: Option<String>,
        provider: Option<String>,
        base_url: Option<String>,
        api_key: Option<String>,
        quiet: bool,
    ) -> Self {
        Self {
            model,
            context_length,
            threshold_percent,
            protect_first_n,
            protect_last_n,
            summary_target_ratio,
            summary_model,
            provider: provider.unwrap_or_default(),
            base_url: base_url.unwrap_or_default(),
            api_key: api_key.unwrap_or_default(),
            compression_count: 0,
            quiet,
            previous_summary: None,
            last_prompt_tokens: 0,
            last_completion_tokens: 0,
            last_total_tokens: 0,
        }
    }

    fn update_from_response(&mut self, response: &Bound<'_, PyAny>) -> PyResult<()> {
        let dict = response.cast::<PyDict>()?;
        fn get_field(dict: &Bound<'_, PyDict>, key: &str) -> usize {
            match dict.get_item(key) {
                Ok(Some(v)) => v.extract::<usize>().unwrap_or(0),
                _ => 0,
            }
        }
        self.last_prompt_tokens = get_field(dict, "prompt_tokens");
        self.last_completion_tokens = get_field(dict, "completion_tokens");
        self.last_total_tokens = get_field(dict, "total_tokens");
        Ok(())
    }

    fn should_compress(&self, prompt_tokens: Option<usize>) -> bool {
        let tokens = prompt_tokens.unwrap_or(self.last_prompt_tokens);
        let threshold = (self.context_length as f64 * self.threshold_percent) as usize;
        tokens >= threshold
    }

    fn should_compress_preflight(&self, messages: Vec<Bound<'_, PyDict>>) -> PyResult<bool> {
        let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
        let rough = tokenizer::estimate_messages_tokens(&json_msgs);
        let threshold = (self.context_length as f64 * self.threshold_percent) as usize;
        Ok(rough >= threshold)
    }

    fn get_status<'a>(&self, py: Python<'a>) -> PyResult<Bound<'a, PyDict>> {
        use pyo3::IntoPyObjectExt;
        let dict = PyDict::new(py);
        dict.set_item(
            "last_prompt_tokens",
            self.last_prompt_tokens.into_bound_py_any(py)?,
        )?;
        dict.set_item(
            "threshold_tokens",
            ((self.context_length as f64 * self.threshold_percent) as usize)
                .into_bound_py_any(py)?,
        )?;
        dict.set_item("context_length", self.context_length.into_bound_py_any(py)?)?;
        dict.set_item(
            "usage_percent",
            ((self.last_prompt_tokens as f64 / self.context_length as f64 * 100.0).min(100.0))
                .into_bound_py_any(py)?,
        )?;
        dict.set_item(
            "compression_count",
            self.compression_count.into_bound_py_any(py)?,
        )?;
        Ok(dict)
    }

    fn compress(&mut self, messages: Vec<Bound<'_, PyDict>>) -> PyResult<Option<Vec<Py<PyAny>>>> {
        let py = unwrap_py(messages.first());
        let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();

        let result = std::thread::scope(|s| {
            s.spawn(|| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(compressor::compress(
                    &json_msgs,
                    &self.model,
                    self.context_length,
                    self.threshold_percent,
                    self.protect_first_n,
                    self.protect_last_n,
                    self.summary_target_ratio,
                    self.summary_model.as_deref(),
                    &self.provider,
                    &self.base_url,
                    &self.api_key,
                    self.previous_summary.as_deref(),
                    self.compression_count,
                    self.quiet,
                ))
            })
            .join()
            .unwrap()
        });

        match result {
            Some((compressed, summary_text)) => {
                self.compression_count += 1;
                if let Some(s) = summary_text {
                    self.previous_summary = Some(s);
                }
                Ok(Some(json_msgs_to_py(py, compressed)))
            }
            None => Ok(None),
        }
    }

    fn set_previous_summary(&mut self, summary: Option<String>) {
        if let Some(s) = summary {
            let prefix = "[CONTEXT COMPACTION] Earlier turns in this conversation were compacted \
             to save context space. The summary below describes work that was \
             already completed, and the current session state may still reflect \
             that work (for example, files may already be changed). Use the summary \
             and the current state to continue from where things left off, and \
             avoid repeating work:";
            self.previous_summary = Some(
                s.strip_prefix(prefix)
                    .map(|s| s.trim().to_string())
                    .unwrap_or(s),
            );
        } else {
            self.previous_summary = None;
        }
    }
}

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

/// Safely get Python token from an optional message dict, or assume attached.
/// SAFETY: GIL is held by Python when calling into a #[pyfunction] or #[pymethod].
fn unwrap_py<'a>(msg: Option<&'a Bound<'a, PyDict>>) -> Python<'a> {
    msg.map(|m| m.py())
        .unwrap_or_else(|| unsafe { Python::assume_attached() })
}

// ---------------------------------------------------------------------------
// compress_trajectory_rs — Rust engine for Python trajectory_compressor.py
// ---------------------------------------------------------------------------

/// Convert a Python trajectory dict (with "from"/"value") to Rust Value format
/// (with "role"/"content"), preserving other fields as-is.
fn py_trajectory_to_rust_value(dict: &Bound<'_, PyDict>) -> Value {
    let mut map = serde_json::Map::new();
    for (k, v) in dict.iter() {
        let key = k.extract::<String>().unwrap_or_default();
        let val = py_to_val(dict.py(), &v);
        // Remap Python field names to Rust internal format
        let key = match key.as_str() {
            "from" => "role".to_string(),
            "value" => "content".to_string(),
            other => other.to_string(),
        };
        map.insert(key, val);
    }
    Value::Object(map)
}

/// Metrics struct matching Python TrajectoryMetrics.to_dict() format.
#[derive(serde::Serialize)]
struct TrajectoryMetricsJson {
    original_tokens: usize,
    compressed_tokens: usize,
    tokens_saved: usize,
    compression_ratio: f64,
    original_turns: usize,
    compressed_turns: usize,
    turns_removed: usize,
    #[serde(rename = "turns_compressed_start_idx")]
    turns_compressed_start_idx: isize,
    #[serde(rename = "turns_compressed_end_idx")]
    turns_compressed_end_idx: isize,
    #[serde(rename = "turns_in_compressed_region")]
    turns_in_compressed_region: usize,
    #[serde(rename = "was_compressed")]
    was_compressed: bool,
    #[serde(rename = "still_over_limit")]
    still_over_limit: bool,
    #[serde(rename = "skipped_under_target")]
    skipped_under_target: bool,
    #[serde(rename = "summarization_api_calls")]
    summarization_api_calls: usize,
    #[serde(rename = "summarization_errors")]
    summarization_errors: usize,
}

/// Compress a trajectory — the Rust engine for Python trajectory_compressor.py.
///
/// Accepts Python-format dicts with `from`/`value` fields, converts internally,
/// runs the full compression pipeline, returns the compressible region for Python
/// to summarize via LLM.
///
/// Returns (compressible_start, compressible_end, original_tokens, middle_content_tokens, metrics_json_str)
/// - If compression not needed: (None, None, total_tokens, 0, metrics_json)
/// - If compression done: (start_idx, end_idx, total_tokens, middle_tokens, metrics_json)
///   where middle content (Python-format turns[start:end]) needs LLM summarization in Python.
#[pyfunction]
fn compress_trajectory_rs(
    _py: Python<'_>,
    messages: Vec<Bound<'_, PyDict>>,
    target_max_tokens: usize,
    summary_target_tokens: usize,
    protect_first_n: usize,
    protect_last_n: usize,
) -> PyResult<(Option<usize>, Option<usize>, usize, usize, String)> {
    // Step 1: Convert Python dicts to Rust Value format (from→role, value→content)
    let rust_messages: Vec<Value> = messages
        .iter()
        .map(|d| py_trajectory_to_rust_value(d))
        .collect();

    let n = rust_messages.len();

    // Step 2: Token counting
    let total_tokens = tokenizer::estimate_messages_tokens(&rust_messages);

    // Step 3: Early exit if compression not needed
    if total_tokens <= target_max_tokens {
        let metrics = TrajectoryMetricsJson {
            original_tokens: total_tokens,
            compressed_tokens: total_tokens,
            tokens_saved: 0,
            compression_ratio: 1.0,
            original_turns: n,
            compressed_turns: n,
            turns_removed: 0,
            turns_compressed_start_idx: -1,
            turns_compressed_end_idx: -1,
            turns_in_compressed_region: 0,
            was_compressed: false,
            still_over_limit: false,
            skipped_under_target: true,
            summarization_api_calls: 0,
            summarization_errors: 0,
        };
        let metrics_json = serde_json::to_string(&metrics).unwrap_or_default();
        return Ok((None, None, total_tokens, 0, metrics_json));
    }

    // Step 4: Find protected indices (same logic as Python's _find_protected_indices)
    let mut protected = std::collections::HashSet::new();
    let mut first_system = None;
    let mut first_human = None;
    let mut first_gpt = None;
    let mut first_tool = None;

    for (i, turn) in rust_messages.iter().enumerate() {
        let role = turn.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "system" && first_system.is_none() {
            first_system = Some(i);
        } else if role == "human" && first_human.is_none() {
            first_human = Some(i);
        } else if role == "gpt" && first_gpt.is_none() {
            first_gpt = Some(i);
        } else if role == "tool" && first_tool.is_none() {
            first_tool = Some(i);
        }
    }

    // Protect first N occurrences (per config) — protect_first_n means protect the
    // first occurrence of each role type if protect_first_n > 0
    if protect_first_n > 0 {
        if let Some(idx) = first_system {
            protected.insert(idx);
        }
        if let Some(idx) = first_human {
            protected.insert(idx);
        }
        if let Some(idx) = first_gpt {
            protected.insert(idx);
        }
        if let Some(idx) = first_tool {
            protected.insert(idx);
        }
    }

    // Protect last N turns
    let tail_start = n.saturating_sub(protect_last_n);
    for i in tail_start..n {
        protected.insert(i);
    }

    // Determine compressible region
    let mid = n / 2;
    let head_protected: Vec<usize> = protected.iter().filter(|&&i| i < mid).copied().collect();
    let tail_protected: Vec<usize> = protected.iter().filter(|&&i| i >= mid).copied().collect();

    let compressible_start = head_protected.iter().max().copied().unwrap_or(0) + 1;
    let compressible_end = tail_protected.iter().min().copied().unwrap_or(n);

    // If nothing to compress, return early
    if compressible_start >= compressible_end {
        let metrics = TrajectoryMetricsJson {
            original_tokens: total_tokens,
            compressed_tokens: total_tokens,
            tokens_saved: 0,
            compression_ratio: 1.0,
            original_turns: n,
            compressed_turns: n,
            turns_removed: 0,
            turns_compressed_start_idx: -1,
            turns_compressed_end_idx: -1,
            turns_in_compressed_region: 0,
            was_compressed: false,
            still_over_limit: total_tokens > target_max_tokens,
            skipped_under_target: false,
            summarization_api_calls: 0,
            summarization_errors: 0,
        };
        let metrics_json = serde_json::to_string(&metrics).unwrap_or_default();
        return Ok((None, None, total_tokens, 0, metrics_json));
    }

    // Step 5: Token accumulation (same as Python)
    let tokens_to_save = total_tokens - target_max_tokens;
    let target_tokens_to_compress = tokens_to_save + summary_target_tokens;

    let mut accumulated = 0;
    let mut compress_until = compressible_start;

    for i in compressible_start..compressible_end {
        accumulated += tokenizer::estimate_message_dict_tokens(&rust_messages[i]);
        compress_until = i + 1;
        if accumulated >= target_tokens_to_compress {
            break;
        }
    }

    // If we still don't have enough savings, compress the entire compressible region
    if accumulated < target_tokens_to_compress && compress_until < compressible_end {
        compress_until = compressible_end;
        accumulated = (compressible_start..compressible_end)
            .map(|i| tokenizer::estimate_message_dict_tokens(&rust_messages[i]))
            .sum();
    }

    // Step 6: Build metrics JSON
    let turns_in_region = compress_until - compressible_start;
    let metrics = TrajectoryMetricsJson {
        original_tokens: total_tokens,
        compressed_tokens: total_tokens, // Python will update after summarization
        tokens_saved: 0,                 // Python will update after summarization
        compression_ratio: 1.0,          // Python will update after summarization
        original_turns: n,
        compressed_turns: n,             // Python will update after summarization
        turns_removed: 0,                // Python will update after summarization
        turns_compressed_start_idx: compressible_start as isize,
        turns_compressed_end_idx: compress_until as isize,
        turns_in_compressed_region: turns_in_region,
        was_compressed: true,
        still_over_limit: false,
        skipped_under_target: false,
        summarization_api_calls: 0,
        summarization_errors: 0,
    };
    let metrics_json = serde_json::to_string(&metrics).unwrap_or_default();

    // Step 7: Return compressible region — Python will handle summarization
    Ok((Some(compressible_start), Some(compress_until), total_tokens, accumulated, metrics_json))
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Fast-path estimators — JSON string input, no dict→Value conversion overhead
// ---------------------------------------------------------------------------

#[pyfunction]
fn estimate_messages_tokens_from_json(json_str: &str) -> PyResult<usize> {
    Ok(tokenizer::estimate_messages_tokens_from_json(json_str))
}

#[pyfunction]
fn estimate_message_from_json(json_str: &str) -> PyResult<(usize, usize)> {
    Ok(tokenizer::estimate_message_from_json(json_str))
}

#[pyfunction]
fn is_tiktoken_available_py() -> PyResult<bool> {
    Ok(tokenizer::is_tiktoken_available())
}

// ---------------------------------------------------------------------
// Non-blocking compression API
// ---------------------------------------------------------------------

/// Start a background compression job and return immediately.
/// Returns a job ID that Python uses to poll/check/cancel.
///
/// Algorithm:
///   1. Prune old tool results (cheap pre-pass, no LLM call)
///   2. Protect head messages (system prompt + first exchange)
///   3. Find tail boundary by token budget
///   4. Summarize middle turns via LLM call (in background thread)
///   5. On re-compression, iteratively update the previous summary
///
/// The GIL is released for the entire duration of the background thread,
/// including the HTTP I/O for the LLM summarization call.
#[pyfunction]
fn compress_start(
    messages: Vec<Bound<'_, PyDict>>,
    model: String,
    context_length: usize,
    threshold_percent: f64,
    protect_first_n: usize,
    protect_last_n: usize,
    summary_target_ratio: f64,
    summary_model: Option<String>,
    provider: String,
    base_url: String,
    api_key: String,
    previous_summary: Option<String>,
    compression_count: usize,
    quiet: bool,
) -> PyResult<usize> {
    // Initialize job store if first call
    {
        let mut store = get_job_store().lock().unwrap();
        if store.is_none() {
            *store = Some(HashMap::new());
        }
    }

    let job_id = next_job_id();
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();

    // Register Running state immediately
    {
        let mut store = get_job_store().lock().unwrap();
        store.as_mut().unwrap().insert(job_id, JobState::Running);
    }

    // Reuse the static RUNTIME via spawn_blocking — no per-call runtime allocation.
    RUNTIME.spawn_blocking(move || {
        let result = RUNTIME.block_on(compressor::compress(
            &json_msgs,
            &model,
            context_length,
            threshold_percent,
            protect_first_n,
            protect_last_n,
            summary_target_ratio,
            summary_model.as_deref(),
            &provider,
            &base_url,
            &api_key,
            previous_summary.as_deref(),
            compression_count,
            quiet,
        ));

        let state = match result {
            Some((compressed, summary)) => JobState::Completed(compressed, summary),
            None => JobState::Cancelled,
        };

        let mut store = get_job_store().lock().unwrap();
        if let Some(ref mut m) = *store {
            m.insert(job_id, state);
        }
    });

    Ok(job_id)
}

/// Poll a compression job — returns immediately without blocking.
/// Returns:
///   (0, None, None)         — still running
///   (1, compressed, summary) — completed successfully
///   (2, None, error_msg)    — failed
///   (3, None, None)         — cancelled / not found
#[pyfunction]
fn compress_check(job_id: usize) -> PyResult<(u8, Option<Vec<Py<PyAny>>>, Option<String>)> {
    let py = unsafe { Python::assume_attached() };
    let store = get_job_store().lock().unwrap();

    match store.as_ref().and_then(|m| m.get(&job_id)) {
        None | Some(JobState::Running) => Ok((0, None, None)),
        Some(JobState::Completed(compressed, summary)) => {
            let py_compressed = json_msgs_to_py(py, compressed.clone());
            Ok((1, Some(py_compressed), summary.clone()))
        }
        Some(JobState::Failed(err)) => Ok((2, None, Some(err.clone()))),
        Some(JobState::Cancelled) => Ok((3, None, None)),
    }
}

/// Cancel a running compression job and remove it from the store.
/// No-op if job already completed or not found.
#[pyfunction]
fn compress_cancel(job_id: usize) -> PyResult<bool> {
    let mut store = get_job_store().lock().unwrap();
    let store = &mut *store;
    if let Some(ref mut m) = *store {
        if matches!(m.get(&job_id), Some(JobState::Running)) {
            m.remove(&job_id);
            return Ok(true);
        }
    }
    Ok(false)
}

#[pymodule]
fn compressor_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(prune_old_tool_results, m)?)?;
    m.add_function(wrap_pyfunction!(align_boundary_forward, m)?)?;
    m.add_function(wrap_pyfunction!(align_boundary_backward, m)?)?;
    m.add_function(wrap_pyfunction!(find_tail_cut, m)?)?;
    m.add_function(wrap_pyfunction!(sanitize_tool_pairs_py, m)?)?;
    m.add_function(wrap_pyfunction!(compute_summary_budget, m)?)?;
    m.add_function(wrap_pyfunction!(serialize_turns, m)?)?;
    m.add_function(wrap_pyfunction!(normalize_summary_prefix, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_message_tokens, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_messages_tokens, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_messages_tokens_from_json, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_message_from_json, m)?)?;
    m.add_function(wrap_pyfunction!(is_tiktoken_available_py, m)?)?;

    m.add_function(wrap_pyfunction!(compress_async, m)?)?;
    m.add_function(wrap_pyfunction!(compress_start, m)?)?;
    m.add_function(wrap_pyfunction!(compress_check, m)?)?;
    m.add_function(wrap_pyfunction!(compress_cancel, m)?)?;
    m.add_function(wrap_pyfunction!(compress_trajectory_rs, m)?)?;
    m.add_class::<PyContextCompressor>()?;
    Ok(())
}
