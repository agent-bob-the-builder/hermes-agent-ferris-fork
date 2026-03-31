//! PyO3 bindings for fast context-reference parsing and text cleanup.
//!
//! Covers:
//! - `parse_context_references`: regex-based @-reference extraction
//! - `remove_reference_tokens`: strip @ tokens and clean whitespace
//! - `estimate_tokens_rough`: fast char/4 heuristic
//!
//! The pure-Python original lives in `agent/context_references.py`.
//! This Rust module is a drop-in accelerator.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use regex::Regex;

// ---------------------------------------------------------------------------
// Regex patterns — compiled once at static init
// ---------------------------------------------------------------------------

// Matches @diff, @staged, @file:..., @folder:..., @git:..., @url:...
static REFERENCE_RE: Regex = Regex::new(
    r"(?<![\w/])(?:@(?P<simple>diff|staged)\b|(?P<kind>file|folder|git|url):(?P<value>\S+))"
).unwrap();

// Range suffix on file refs: path:123 or path:123-456
static FILE_RANGE_RE: Regex = Regex::new(
    r"^(?P<path>.+?):(?P<start>\d+)(?:-(?P<end>\d+))?$"
).unwrap();

// Whitespace normalisation after token removal
static WHITESPACE_COLLAPSE_RE: Regex = Regex::new(r"\s{2,}").unwrap();
static TRAILING_PUNCT_RE: Regex = Regex::new(r"\s+([,.;!?])").unwrap();

// Trailing punctuation characters to strip from reference targets.
const TRAILING_PUNCT_CHARS: &str = ",.;!?";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip trailing punctuation characters from a reference target.
/// Handles balanced bracket pairs (e.g. "(foo)" → "foo").
fn strip_trailing_punctuation(value: &str) -> String {
    let mut s = value.trim_end_matches(TRAILING_PUNCT_CHARS);
    while s.ends_with(')') || s.ends_with(']') || s.ends_with('}') {
        let closer = s.chars().last().unwrap();
        let opener = match closer {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => break,
        };
        if s.matches(opener).count() < s.matches(closer).count() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s.to_string()
}

/// Build a Python dict for a simple @diff / @staged reference.
fn build_simple_ref(py: Python<'_>, raw_text: &str, simple_kind: &str, start: usize, end: usize) -> Py<PyAny> {
    let dict = PyDict::new(py);
    dict.set_item("raw", raw_text).unwrap();
    dict.set_item("kind", simple_kind).unwrap();
    dict.set_item("target", "").unwrap();
    dict.set_item("start", start as i64).unwrap();
    dict.set_item("end", end as i64).unwrap();
    dict.set_item("line_start", py.None()).unwrap();
    dict.set_item("line_end", py.None()).unwrap();
    dict.into_any().unbind()
}

/// Build a Python dict for a typed @kind:value reference.
fn build_typed_ref(
    py: Python<'_>,
    raw_text: &str,
    kind: &str,
    target: &str,
    start: usize,
    end: usize,
    line_start: Option<i64>,
    line_end: Option<i64>,
) -> Py<PyAny> {
    let dict = PyDict::new(py);
    dict.set_item("raw", raw_text).unwrap();
    dict.set_item("kind", kind).unwrap();
    dict.set_item("target", target).unwrap();
    dict.set_item("start", start as i64).unwrap();
    dict.set_item("end", end as i64).unwrap();
    dict.set_item("line_start", line_start.map(|v| v as i64).unwrap_or(py.None())).unwrap();
    dict.set_item("line_end", line_end.map(|v| v as i64).unwrap_or(py.None())).unwrap();
    dict.into_any().unbind()
}

// ---------------------------------------------------------------------------
// Python-callable functions
// ---------------------------------------------------------------------------

/// Parse all @-references from a message string.
///
/// Returns a list of dicts with keys: raw, kind, target, start, end,
/// line_start, line_end.
#[pyfunction]
fn parse_context_references(message: &str) -> PyResult<Vec<Py<PyAny>>> {
    let py = Python::acquire_gil();
    let py = py.python();
    let mut results: Vec<Py<PyAny>> = Vec::new();

    for caps in REFERENCE_RE.captures_iter(message) {
        let m = caps.get(0).unwrap();
        let start = m.start();
        let end = m.end();
        let raw_text = m.as_str();

        if let Some(simple_match) = caps.name("simple") {
            let dict = build_simple_ref(py, raw_text, simple_match.as_str(), start, end);
            results.push(dict);
            continue;
        }

        let kind = caps.name("kind").map(|m| m.as_str()).unwrap_or("");
        let value = caps.name("value").map(|m| m.as_str()).unwrap_or("");
        let stripped = strip_trailing_punctuation(value);

        if kind == "file" {
            if let Some(range_caps) = FILE_RANGE_RE.captures(&stripped) {
                let path = range_caps.name("path").map(|m| m.as_str()).unwrap_or(&stripped);
                let line_start: i64 = range_caps.name("start")
                    .map(|m| m.as_str().parse().unwrap_or(1))
                    .unwrap_or(1);
                let line_end: i64 = range_caps.name("end")
                    .map(|m| m.as_str().parse::<i64>().unwrap_or(line_start))
                    .unwrap_or(line_start);
                let dict = build_typed_ref(py, raw_text, kind, path, start, end, Some(line_start), Some(line_end));
                results.push(dict);
            } else {
                let dict = build_typed_ref(py, raw_text, kind, &stripped, start, end, None, None);
                results.push(dict);
            }
        } else {
            let dict = build_typed_ref(py, raw_text, kind, &stripped, start, end, None, None);
            results.push(dict);
        }
    }

    Ok(results)
}

/// Remove all @-reference tokens from a message and normalise whitespace.
///
/// `refs_json` is the JSON-encoded list returned by `parse_context_references`.
#[pyfunction]
fn remove_reference_tokens(message: &str, refs_json: &str) -> PyResult<String> {
    let refs: Vec<serde_json::Value> = serde_json::from_str(refs_json)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid refs JSON: {}", e)))?;

    let mut result = message.to_string();
    // Iterate in reverse so byte offsets stay valid after each replacement.
    for item in refs.iter().rev() {
        let start = item.get("start").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
        let end = item.get("end").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
        if end > start && end <= result.len() {
            result = format!("{}{}", &result[..start], &result[end..]);
        }
    }

    // Whitespace normalisation
    result = WHITESPACE_COLLAPSE_RE.replace_all(&result, " ").to_string();
    result = TRAILING_PUNCT_RE.replace_all(&result, "$1").to_string();
    result = result.trim().to_string();

    Ok(result)
}

/// Fast rough token estimate: `len(text) // 4`.
#[pyfunction]
fn estimate_tokens_rough(text: &str) -> PyResult<usize> {
    Ok(if text.is_empty() { 0 } else { text.len() / 4 })
}

// ---------------------------------------------------------------------------
// PyO3 module
// ---------------------------------------------------------------------------

#[pymodule]
fn context_refs_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_context_references, m)?)?;
    m.add_function(wrap_pyfunction!(remove_reference_tokens, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_tokens_rough, m)?)?;
    Ok(())
}
