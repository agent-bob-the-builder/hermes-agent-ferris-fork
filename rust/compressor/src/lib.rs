//! PyO3 0.28 bindings for the Rust ContextCompressor.
//!
//! Key patterns:
//! - `Bound<'_, T>` for references into Python's memory
//! - `.into_pyobject(py)` → `Py<T>`, then `.into_any().into()` → `Py<PyAny>`
//! - `IntoPyObjectExt::into_bound_py_any(py)` for primitive → Python objects
//! - `unsafe { Python::assume_attached() }` for GIL when no messages available
//! - `thread::scope()` + `spawn` for running async from sync Python

use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;

mod compressor;
mod summarizer;
mod tokenizer;

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
        return dict_to_json(&dict);
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
            dict.into_any().into_pyobject(py).expect("dict into_pyobject failed").into()
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
fn align_boundary_forward(
    messages: Vec<Bound<'_, PyDict>>,
    idx: usize,
) -> PyResult<usize> {
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
    Ok(compressor::align_boundary_forward(&json_msgs, idx))
}

/// Align compress_end backward to avoid splitting tool groups.
#[pyfunction]
fn align_boundary_backward(
    messages: Vec<Bound<'_, PyDict>>,
    idx: usize,
) -> PyResult<usize> {
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
fn sanitize_tool_pairs_py(
    messages: Vec<Bound<'_, PyDict>>,
) -> PyResult<Vec<Py<PyAny>>> {
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
    let json_msgs: Vec<Value> = turns_to_summarize
        .iter()
        .map(|d| dict_to_json(d))
        .collect();
    Ok(summarizer::compute_summary_budget(&json_msgs, context_length))
}

/// Serialize conversation turns for the summarizer.
#[pyfunction]
fn serialize_turns(
    messages: Vec<Bound<'_, PyDict>>,
) -> PyResult<String> {
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
fn estimate_messages_tokens(
    messages: Vec<Bound<'_, PyDict>>,
) -> PyResult<usize> {
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();
    Ok(tokenizer::estimate_messages_tokens(&json_msgs))
}

/// Synchronous compress — blocks the calling thread while awaiting the LLM.
/// Uses thread::scope so the GIL can be released while waiting on I/O.
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
) -> PyResult<Option<Vec<Py<PyAny>>>> {
    let json_msgs: Vec<Value> = messages.iter().map(|d| dict_to_json(d)).collect();

    let result = std::thread::scope(|s| {
        s.spawn(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(compressor::compress(
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
            ))
        })
        .join()
        .unwrap()
    });

    match result {
        Some(compressed) => Ok(Some(json_msgs_to_py(py, compressed))),
        None => Ok(None),
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
        self.last_prompt_tokens = get_field(&dict, "prompt_tokens");
        self.last_completion_tokens = get_field(&dict, "completion_tokens");
        self.last_total_tokens = get_field(&dict, "total_tokens");
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
            Some(compressed) => {
                self.compression_count += 1;
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
            self.previous_summary = Some(s.strip_prefix(prefix).map(|s| s.trim().to_string()).unwrap_or(s));
        } else {
            self.previous_summary = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract content string from a message dict without JSON serialization.
/// Returns the string content of the "content" field, or "" if missing.
#[allow(dead_code)]
fn extract_content(msg: &Bound<'_, PyDict>) -> String {
    msg.get_item("content")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_default()
}

/// Estimate token count for a single message dict using direct PyDict access.
/// Matches Python's `len(str(msg)) // 4` but avoids the Python str() call.
#[allow(dead_code)]
fn estimate_msg_tokens_fast(msg: &Bound<'_, PyDict>) -> usize {
    let content_len = extract_content(msg).len();
    // Rough: content chars / 4  +  10 tokens overhead for role/metadata framing
    content_len / 4 + 10
}

/// Estimate token count for a list of message dicts using direct PyDict access.
/// This is the hot path for should_compress_preflight.
#[allow(dead_code)]
fn estimate_msgs_tokens_fast(messages: &[Bound<'_, PyDict>]) -> usize {
    messages.iter().map(estimate_msg_tokens_fast).sum()
}

/// Safely get Python token from an optional message dict, or assume attached.
/// SAFETY: GIL is held by Python when calling into a #[pyfunction] or #[pymethod].
fn unwrap_py<'a>(msg: Option<&'a Bound<'a, PyDict>>) -> Python<'a> {
    msg.map(|m| m.py())
        .unwrap_or_else(|| unsafe { Python::assume_attached() })
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

#[pymodule]
fn rust_compressor(m: &Bound<'_, PyModule>) -> PyResult<()> {
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
    m.add_function(wrap_pyfunction!(compress_async, m)?)?;
    m.add_class::<PyContextCompressor>()?;
    Ok(())
}



