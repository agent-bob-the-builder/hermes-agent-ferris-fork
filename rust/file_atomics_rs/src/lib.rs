//! Atomic file writers — crash-safe JSON and YAML via temp file + fsync + rename.
//!
//! Pattern: write to temp file in same directory → flush + fsync → atomic rename.
//! On any error the temp file is deleted and the original `path` is untouched.

use pyo3::prelude::*;
use pyo3::types::{PyAnyMethods, PyDict, PyModuleMethods};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Atomically write `data` to `path`.
//
//  1. create parent dirs
//  2. write through a temp file in the same directory
//  3. flush + fsync
//  4. rename the temp over the target
//
// On any error the temp file is deleted and the original `path` is untouched.
fn atomic_write<F>(path: &Path, mut write_data: F) -> PyResult<()>
where
    F: FnMut(&mut BufWriter<File>) -> io::Result<()>,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            pyo3::exceptions::PyOSError::new_err(format!(
                "failed to create parent directory: {}",
                e
            ))
        })?;
    }

    // Temp file in same directory for atomic rename.
    let tmp_path = {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("data");
        let tmp_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = tmp_dir.join(format!(".{}", stem));
        tmp.set_extension("tmp");
        tmp
    };

    let result = (|| {
        let f = File::create(&tmp_path).map_err(|e| {
            pyo3::exceptions::PyOSError::new_err(format!(
                "failed to create temp file: {}",
                e
            ))
        })?;
        let mut writer = BufWriter::new(f);

        write_data(&mut writer)?;

        writer
            .flush()
            .map_err(|e| pyo3::exceptions::PyOSError::new_err(format!("flush failed: {}", e)))?;
        let f = writer
            .into_inner()
            .map_err(|e| pyo3::exceptions::PyOSError::new_err(format!("flush failed: {}", e)))?;
        f.sync_all()
            .map_err(|e| pyo3::exceptions::PyOSError::new_err(format!("fsync failed: {}", e)))?;
        drop(f);

        std::fs::rename(&tmp_path, path).map_err(|e| {
            pyo3::exceptions::PyOSError::new_err(format!("atomic rename failed: {}", e))
        })
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

// ---------------------------------------------------------------------------
// Python-callable functions
// ---------------------------------------------------------------------------

/// Write `data` as JSON to `path` atomically.
///
/// atomic_json_write(path, data, *, indent=2, default_fn=None)
#[pyfunction]
pub fn atomic_json_write(
    py: Python<'_>,
    path: &Bound<'_, PyAny>,
    data: &Bound<'_, PyAny>,
    indent: Option<u32>,
    default_fn: Option<Py<PyAny>>,
) -> PyResult<()> {
    let indent = indent.unwrap_or(2);
    let path_str: String = path.extract()?;
    let path = Path::new(&path_str);

    // Serialize data to a JSON string via Python's json.dumps.
    let json_str: String = {
        let json_mod = PyModule::import(py, "json")?;
        let dumps = json_mod.getattr("dumps")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("indent", indent as i32)?;
        kwargs.set_item("ensure_ascii", false)?;
        kwargs.set_item("default", py.None())?;
        if let Some(ref df) = default_fn {
            kwargs.set_item("default", df.as_ref())?;
        }
        dumps.call((data,), Some(&kwargs))?.extract()?
    };

    let json_bytes = json_str.as_bytes();

    atomic_write(path, |writer| {
        writer.write_all(json_bytes)?;
        writer.write_all(b"\n")?; // match Python's json.dump trailing newline
        Ok(())
    })
}

/// Write `data` as YAML to `path` atomically.
///
/// atomic_yaml_write(path, data, *, default_flow_style=False, sort_keys=False,
///                   extra_content=None)
#[pyfunction]
pub fn atomic_yaml_write(
    py: Python<'_>,
    path: &Bound<'_, PyAny>,
    data: &Bound<'_, PyAny>,
    default_flow_style: Option<bool>,
    sort_keys: Option<bool>,
    extra_content: Option<Py<PyAny>>,
) -> PyResult<()> {
    let _default_flow_style = default_flow_style.unwrap_or(false);
    let sort_keys = sort_keys.unwrap_or(false);

    let path_str: String = path.extract()?;
    let path = Path::new(&path_str);

    // Serialize via Python's json.dumps then convert to YAML.
    let json_str: String = {
        let json_mod = PyModule::import(py, "json")?;
        let dumps = json_mod.getattr("dumps")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("indent", 2)?;
        kwargs.set_item("default", py.None())?;
        dumps.call((data,), Some(&kwargs))?.extract()?
    };

    // Parse JSON → serde_json Value → YAML string.
    let json_value: serde_json::Value = serde_json::from_str(&json_str).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("data is not JSON-serializable: {}", e))
    })?;

    let yaml_bytes: Vec<u8> = {
        let yaml_str = if sort_keys {
            if let serde_json::Value::Object(map) = &json_value {
                let sorted: std::collections::BTreeMap<&str, &serde_json::Value> =
                    std::collections::BTreeMap::from_iter(
                        map.iter().map(|(k, v)| (k.as_str(), v)),
                    );
                serde_yaml::to_string(&sorted)
            } else {
                serde_yaml::to_string(&json_value)
            }
        } else {
            serde_yaml::to_string(&json_value)
        }
        .map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("YAML serialization failed: {}", e))
        })?;
        yaml_str.into_bytes()
    };

    // extra_content can be appended (used for YAML documents separator)
    let extra_bytes: Vec<u8> = if let Some(ref ec) = extra_content {
        let s: String = ec.extract(py)?;
        if !s.is_empty() {
            s.into_bytes()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    atomic_write(path, |writer| {
        writer.write_all(&yaml_bytes)?;
        if !extra_bytes.is_empty() {
            writer.write_all(&extra_bytes)?;
        }
        Ok(())
    })
}

#[pymodule]
fn file_atomics_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(atomic_json_write, m)?)?;
    m.add_function(wrap_pyfunction!(atomic_yaml_write, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_json_compact() {
        let value = serde_json::json!({"a": 1, "b": 2});
        let mut buf = Vec::new();
        serde_json::to_writer(&mut buf, &value).unwrap();
        assert!(buf.starts_with(b"{"));
        assert!(!buf.contains(&b'\n'));
    }

    #[test]
    fn test_yaml_block_style() {
        let value = serde_json::json!({"key": "value", "nested": {"a": 1}});
        let yaml = serde_yaml::to_string(&value).unwrap();
        assert!(yaml.contains("key: value"));
    }
}
