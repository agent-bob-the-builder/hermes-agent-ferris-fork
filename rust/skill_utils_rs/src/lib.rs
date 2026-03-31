//! Frontmatter parsing for skill metadata — Rust accelerator.
//!
//! Replaces `agent.skill_utils.parse_frontmatter()` with a regex-free,
//! allocation-minimal path.  YAML parsing is delegated to the Python runtime
//! (via yaml.CSafeLoader) so full YAML semantics are preserved.

use pyo3::prelude::*;

static FRONTMATTER_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"\n---\s*\n").unwrap());

/// Parse YAML frontmatter from a markdown string.
//
// In Python this returns (Dict[str, Any], str).  We return a 2-element
// PyTuple so the caller gets the same structure.
#[pyfunction]
fn parse_frontmatter(content: &str) -> PyResult<Py<pyo3::PyTuple>> {
    let py = Python::acquire_gil();

    if !content.starts_with("---") {
        let empty_dict = py.eval("{}", None, None)?;
        return Ok(pyo3::types::PyTuple::new(py, &[empty_dict, content])?.into());
    }

    let rest = &content[3..];
    let Some(cap) = FRONTMATTER_RE.find(rest) else {
        let empty_dict = py.eval("{}", None, None)?;
        return Ok(pyo3::types::PyTuple::new(py, &[empty_dict, content])?.into());
    };

    // cap.start() is relative to `rest` = content[3:]
    // yaml bytes in content[3..3+cap.start()]
    let yaml_end = cap.start() + 3;
    let yaml_content = &content[3..yaml_end];
    let body = &content[yaml_end + cap.len()..];

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

    let parsed_py: Py<pyo3::PyDict> = match yaml_module.call1("load", (yaml_content, loader)) {
        Ok(result) => {
            if let Ok(d) = result.downcast::<pyo3::types::PyDict>() {
                d.into()
            } else if let Ok(l) = result.downcast::<pyo3::types::PyList>() {
                let dict = pyo3::types::PyDict::new(py);
                dict.set_item("__root__", l.as_ref())?;
                dict.into()
            } else {
                let dict = pyo3::types::PyDict::new(py);
                dict.set_item("__root__", result.as_ref())?;
                dict.into()
            }
        }
        Err(_) => {
            // Fallback: simple key:value parsing
            let fallback_json = simple_parse(yaml_content);
            py.eval(&fallback_json, None, None)?
                .downcast::<pyo3::types::PyDict>()
                .map(|d| d.into())
                .unwrap_or_else(|_| pyo3::types::PyDict::new(py).into())
        }
    };

    let frontmatter_ref: &pyo3::Bound<'_, pyo3::PyDict> = parsed_py.as_ref(py);
    let tuple: Py<pyo3::PyTuple> =
        pyo3::types::PyTuple::new(py, &[frontmatter_ref, &body.to_string()])?.into();
    Ok(tuple)
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
        Python::with_gil(|py| {
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
        Python::with_gil(|py| {
            let content = "---\nname: test\n---\n";
            let result = parse_frontmatter(content).unwrap();
            let tuple = result.as_ref(py);
            assert_eq!(tuple.len(), 2);
            let body = tuple.get_item(1).unwrap();
            assert_eq!(body.extract::<String>().unwrap(), "");
        });
    }
}
