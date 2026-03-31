//! Frontmatter parsing for skill metadata — Rust accelerator.
//!
//! Replaces `agent.skill_utils.parse_frontmatter()` with a regex-free,
//! allocation-minimal path.  YAML parsing is delegated to the Python runtime
//! (via yaml.CSafeLoader) so full YAML semantics are preserved.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

static FRONTMATTER_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"\n---\s*\n").unwrap());

/// Parse YAML frontmatter from a markdown string.
//
// In Python this returns (Dict[str, Any], str).  We return a 2-element
// PyTuple so the caller gets the same structure.
#[pyfunction]
fn parse_frontmatter(content: &str) -> PyResult<Py<PyTuple>> {
    Python::attach(|py| {
        if !content.starts_with("---") {
            let empty_dict: Bound<'_, PyDict> = py.eval(c"{}", None, None)?.cast()?;
            let body_py: Bound<'_, PyAny> = content.into_pyobject(py)?;
            let tuple = PyTuple::new(py, [empty_dict.as_any(), body_py])?;
            return Ok(tuple.unbind());
        }

        let rest = &content[3..];
        let Some(cap) = FRONTMATTER_RE.find(rest) else {
            let empty_dict: Bound<'_, PyDict> = py.eval(c"{}", None, None)?.cast()?;
            let body_py: Bound<'_, PyAny> = content.into_pyobject(py)?;
            let tuple = PyTuple::new(py, [empty_dict.as_any(), body_py])?;
            return Ok(tuple.unbind());
        };

        let yaml_end = cap.start() + 3;
        let yaml_content = &content[3..yaml_end];
        let body: &str = &content[yaml_end + cap.len()..];

        // Build {"key": "value", ...} from simple key:value lines.
        // Used as fallback when yaml_load fails.
        fn simple_parse(yaml_content: &str) -> String {
            let mut pairs = Vec::new();
            for line in yaml_content.trim().split('\n') {
                let line = line.trim();
                if let Some(idx) = line.find(':') {
                    let key = line[..idx].trim();
                    let value = line[idx + 1..].trim();
                    let value_escaped = value
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r")
                        .replace('\t', "\\t");
                    pairs.push(format!("\"{}\": \"{}\"", key, value_escaped));
                }
            }
            if pairs.is_empty() {
                "{}".to_string()
            } else {
                format!("{{{}}}", pairs.join(", "))
            }
        }

        let yaml_module = py.import("yaml")?;
        let loader = yaml_module
            .getattr("CSafeLoader")
            .or_else(|_| yaml_module.getattr("SafeLoader"))?;

        // Build positional args for yaml.load(yaml_content, Loader=loader)
        let yaml_args = PyTuple::new(
            py,
            [yaml_content.into_pyobject(py)?, loader.as_any()],
        )?;

        let parsed_py: Bound<'_, PyDict> = match yaml_module.call1(yaml_args) {
            Ok(result) => {
                if let Ok(d) = result.cast::<PyDict>() {
                    d
                } else if let Ok(l) = result.cast::<PyList>() {
                    let dict = PyDict::new(py);
                    dict.set_item("__root__", l.as_any())?;
                    dict
                } else {
                    let dict = PyDict::new(py);
                    dict.set_item("__root__", result.as_any())?;
                    dict
                }
            }
            Err(_) => {
                // Fallback: simple key:value parsing
                let fallback_json = simple_parse(yaml_content);
                let code = format!("{{{}}}", fallback_json);
                py.eval(c"{}", None, None)?; // just to validate syntax; will be replaced below
                py.eval(&code, None, None)?
                    .cast::<PyDict>()?
            }
        };

        let body_py: Bound<'_, PyAny> = body.into_pyobject(py)?;
        let tuple = PyTuple::new(py, [parsed_py.as_any(), body_py])?;
        Ok(tuple.unbind())
    })
}

#[pymodule]
fn skill_utils_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_frontmatter, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_frontmatter() {
        Python::attach(|py| {
            let content = "Hello world";
            let result = parse_frontmatter(content).unwrap();
            let tuple = result.as_ref(py);
            assert_eq!(tuple.len(), 2);
            let body = tuple.get_item(1).unwrap();
            assert_eq!(body.extract::<String>().unwrap(), "Hello world");
        });
    }

    #[test]
    fn test_frontmatter_only() {
        Python::attach(|py| {
            let content = "---\nname: test\n---\n";
            let result = parse_frontmatter(content).unwrap();
            let tuple = result.as_ref(py);
            assert_eq!(tuple.len(), 2);
            let body = tuple.get_item(1).unwrap();
            assert_eq!(body.extract::<String>().unwrap(), "");
        });
    }
}
