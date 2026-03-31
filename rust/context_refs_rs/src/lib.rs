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
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Regex patterns — compiled once at static init via LazyLock
// ---------------------------------------------------------------------------

// Note: no lookbehind in the regex — we check the preceding character manually
// in the parsing loop to replicate Python's (?<![\w/]) behaviour.
static REFERENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@(?:(?P<simple>diff|staged)\b|(?P<kind>file|folder|git|url):(?P<value>\S+))").unwrap()
});

static FILE_RANGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<path>.+?):(?P<start>\d+)(?:-(?P<end>\d+))?$").unwrap()
});

static WHITESPACE_COLLAPSE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s{2,}").unwrap()
});

static TRAILING_PUNCT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+([,.;!?])").unwrap()
});

// Trailing punctuation characters to strip from reference targets.
const TRAILING_PUNCT_CHARS: &str = ",.;!?";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// True if ch is a word character ([a-zA-Z0-9_]) or '/'.
#[inline]
fn is_word_or_slash(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '/'
}

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

// ---------------------------------------------------------------------------
// Python-callable functions
// ---------------------------------------------------------------------------

/// Parse all @-references from a message string.
///
/// Returns a list of dicts with keys: raw, kind, target, start, end,
/// line_start, line_end.
#[pyfunction]
fn parse_context_references(py: Python<'_>, message: &str) -> PyResult<Vec<Py<PyAny>>> {
    let mut results: Vec<Py<PyAny>> = Vec::new();

    for caps in REFERENCE_RE.captures_iter(message) {
        let m = caps.get(0).unwrap();
        let start = m.start();
        let end = m.end();

        // Replicate Python's (?<![\w/]) lookbehind: skip if preceded by a
        // word character or forward slash. Position 0 always passes.
        if start > 0 {
            let prev_char = message[..start].chars().last().unwrap();
            if is_word_or_slash(prev_char) {
                continue;
            }
        }

        let raw_text = m.as_str();

        if let Some(simple_match) = caps.name("simple") {
            let dict = PyDict::new(py);
            dict.set_item("raw", raw_text).unwrap();
            dict.set_item("kind", simple_match.as_str()).unwrap();
            dict.set_item("target", "").unwrap();
            dict.set_item("start", start as i64).unwrap();
            dict.set_item("end", end as i64).unwrap();
            dict.set_item("line_start", py.None()).unwrap();
            dict.set_item("line_end", py.None()).unwrap();
            results.push(dict.into_any().unbind());
            continue;
        }

        let kind = caps.name("kind").map(|m| m.as_str()).unwrap_or("");
        let value = caps.name("value").map(|m| m.as_str()).unwrap_or("");
        let stripped = strip_trailing_punctuation(value);

        if kind == "file" {
            if let Some(range_caps) = FILE_RANGE_RE.captures(&stripped) {
                let path = range_caps.name("path").map(|m| m.as_str()).unwrap_or(&*stripped);
                let line_start: i64 = range_caps
                    .name("start")
                    .map(|m| m.as_str().parse().unwrap_or(1))
                    .unwrap_or(1);
                let line_end: i64 = range_caps
                    .name("end")
                    .map(|m| m.as_str().parse::<i64>().unwrap_or(line_start))
                    .unwrap_or(line_start);
                let dict = PyDict::new(py);
                dict.set_item("raw", raw_text).unwrap();
                dict.set_item("kind", kind).unwrap();
                dict.set_item("target", path).unwrap();
                dict.set_item("start", start as i64).unwrap();
                dict.set_item("end", end as i64).unwrap();
                dict.set_item("line_start", line_start).unwrap();
                dict.set_item("line_end", line_end).unwrap();
                results.push(dict.into_any().unbind());
            } else {
                let dict = PyDict::new(py);
                dict.set_item("raw", raw_text).unwrap();
                dict.set_item("kind", kind).unwrap();
                dict.set_item("target", &*stripped).unwrap();
                dict.set_item("start", start as i64).unwrap();
                dict.set_item("end", end as i64).unwrap();
                dict.set_item("line_start", py.None()).unwrap();
                dict.set_item("line_end", py.None()).unwrap();
                results.push(dict.into_any().unbind());
            }
        } else {
            let dict = PyDict::new(py);
            dict.set_item("raw", raw_text).unwrap();
            dict.set_item("kind", kind).unwrap();
            dict.set_item("target", &*stripped).unwrap();
            dict.set_item("start", start as i64).unwrap();
            dict.set_item("end", end as i64).unwrap();
            dict.set_item("line_start", py.None()).unwrap();
            dict.set_item("line_end", py.None()).unwrap();
            results.push(dict.into_any().unbind());
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
fn _context_refs_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_context_references, m)?)?;
    m.add_function(wrap_pyfunction!(remove_reference_tokens, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_tokens_rough, m)?)?;
    Ok(())
}
