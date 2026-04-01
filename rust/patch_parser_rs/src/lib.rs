use lazy_static::lazy_static;
use pyo3::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};

// Operation type constants
pub const OP_ADD: &str = "add";
pub const OP_UPDATE: &str = "update";
pub const OP_DELETE: &str = "delete";
pub const OP_MOVE: &str = "move";

lazy_static! {
    static ref RE_UPDATE_FILE: Regex = Regex::new(r"^\*{3}\s*Update\s+File:\s*(.+)$").unwrap();
    static ref RE_ADD_FILE: Regex = Regex::new(r"^\*{3}\s*Add\s+File:\s*(.+)$").unwrap();
    static ref RE_DELETE_FILE: Regex = Regex::new(r"^\*{3}\s*Delete\s+File:\s*(.+)$").unwrap();
    static ref RE_MOVE_FILE: Regex =
        Regex::new(r"^\*{3}\s*Move\s+File:\s*(.+?)\s*->\s*(.+)$").unwrap();
    static ref RE_HUNK_MARKER: Regex = Regex::new(r"@@\s*(.+?)\s*@@").unwrap();
    static ref RE_LINE_NUM: Regex = Regex::new(r"^\s*\d+\|").unwrap();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkLine {
    pub prefix: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_hint: Option<String>,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchOperation {
    pub operation: String,
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_path: Option<String>,
    #[serde(default)]
    pub hunks: Vec<Hunk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

fn finalize_hunk(op: &mut PatchOperation, current_hunk: &mut Option<Hunk>) {
    if let Some(ref mut hunk) = current_hunk {
        if !hunk.lines.is_empty() {
            op.hunks.push(current_hunk.take().unwrap());
        }
    }
    *current_hunk = None;
}

pub fn parse_v4a_patch_impl(patch_content: &str) -> Vec<PatchOperation> {
    let lines: Vec<&str> = patch_content.split('\n').collect();
    let mut operations: Vec<PatchOperation> = Vec::new();

    // Find patch boundaries
    let mut start_idx: isize = -1;
    let mut end_idx = lines.len();

    for (i, line) in lines.iter().enumerate() {
        if line.contains("*** Begin Patch") || line.contains("***Begin Patch") {
            start_idx = i as isize;
        } else if line.contains("*** End Patch") || line.contains("***End Patch") {
            end_idx = i;
            break;
        }
    }

    if start_idx == -1 {
        start_idx = -1;
    }

    let mut i = (start_idx + 1) as usize;
    let mut current_op: Option<PatchOperation> = None;
    let mut current_hunk: Option<Hunk> = None;

    while i < end_idx {
        let line = lines[i];

        // Check for file operation markers
        if let Some(caps) = RE_UPDATE_FILE.captures(line) {
            if let Some(ref mut op) = current_op {
                finalize_hunk(op, &mut current_hunk);
                if !op.hunks.is_empty() || !op.content.as_ref().is_some_and(|c| c.is_empty()) {
                    operations.push(current_op.take().unwrap());
                }
            }

            current_op = Some(PatchOperation {
                operation: OP_UPDATE.to_string(),
                file_path: caps
                    .get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default(),
                new_path: None,
                hunks: Vec::new(),
                content: None,
            });
            current_hunk = None;
        } else if let Some(caps) = RE_ADD_FILE.captures(line) {
            if let Some(ref mut op) = current_op {
                finalize_hunk(op, &mut current_hunk);
                if !op.hunks.is_empty() || !op.content.as_ref().is_some_and(|c| c.is_empty()) {
                    operations.push(current_op.take().unwrap());
                }
            }

            current_op = Some(PatchOperation {
                operation: OP_ADD.to_string(),
                file_path: caps
                    .get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default(),
                new_path: None,
                hunks: Vec::new(),
                content: None,
            });
            current_hunk = Some(Hunk {
                context_hint: None,
                lines: Vec::new(),
            });
        } else if let Some(caps) = RE_DELETE_FILE.captures(line) {
            if let Some(ref mut op) = current_op {
                finalize_hunk(op, &mut current_hunk);
                if !op.hunks.is_empty() || !op.content.as_ref().is_some_and(|c| c.is_empty()) {
                    operations.push(current_op.take().unwrap());
                }
            }

            let delete_op = PatchOperation {
                operation: OP_DELETE.to_string(),
                file_path: caps
                    .get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default(),
                new_path: None,
                hunks: Vec::new(),
                content: None,
            };
            operations.push(delete_op);
            current_op = None;
            current_hunk = None;
        } else if let Some(caps) = RE_MOVE_FILE.captures(line) {
            if let Some(ref mut op) = current_op {
                finalize_hunk(op, &mut current_hunk);
                if !op.hunks.is_empty() || !op.content.as_ref().is_some_and(|c| c.is_empty()) {
                    operations.push(current_op.take().unwrap());
                }
            }

            let move_op = PatchOperation {
                operation: OP_MOVE.to_string(),
                file_path: caps
                    .get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default(),
                new_path: caps.get(2).map(|m| m.as_str().trim().to_string()),
                hunks: Vec::new(),
                content: None,
            };
            operations.push(move_op);
            current_op = None;
            current_hunk = None;
        } else if line.starts_with("@@") {
            if let Some(ref mut op) = current_op {
                finalize_hunk(op, &mut current_hunk);

                let hint = RE_HUNK_MARKER
                    .captures(line)
                    .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()));

                current_hunk = Some(Hunk {
                    context_hint: hint,
                    lines: Vec::new(),
                });
            }
        } else if current_op.is_some() && !line.is_empty() {
            if current_hunk.is_none() {
                current_hunk = Some(Hunk {
                    context_hint: None,
                    lines: Vec::new(),
                });
            }

            if let Some(ref mut hunk) = current_hunk {
                if line.starts_with('+') {
                    hunk.lines.push(HunkLine {
                        prefix: '+'.to_string(),
                        content: line[1..].to_string(),
                    });
                } else if line.starts_with('-') {
                    hunk.lines.push(HunkLine {
                        prefix: '-'.to_string(),
                        content: line[1..].to_string(),
                    });
                } else if line.starts_with(' ') {
                    hunk.lines.push(HunkLine {
                        prefix: ' '.to_string(),
                        content: line[1..].to_string(),
                    });
                } else if line.starts_with('\\') {
                    // "\ No newline at end of file" marker - skip
                } else {
                    // Treat as context line (implicit space prefix)
                    hunk.lines.push(HunkLine {
                        prefix: ' '.to_string(),
                        content: line.to_string(),
                    });
                }
            }
        }

        i += 1;
    }

    // Don't forget the last operation
    if let Some(ref mut op) = current_op {
        finalize_hunk(op, &mut current_hunk);
        if !op.hunks.is_empty() || !op.content.as_ref().is_some_and(|c| c.is_empty()) {
            operations.push(current_op.take().unwrap());
        }
    }

    operations
}

#[pyfunction]
pub fn parse_v4a_patch(patch_content: &str) -> PyResult<String> {
    let operations = parse_v4a_patch_impl(patch_content);
    serde_json::to_string(&operations)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
}

#[pymodule]
fn _patch_parser_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_v4a_patch, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_update() {
        let patch = r#"*** Begin Patch
*** Update File: test.py
@@ context hint @@
 context line
-removed line
+added line
*** End Patch"#;

        let ops = parse_v4a_patch_impl(patch);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, OP_UPDATE);
        assert_eq!(ops[0].file_path, "test.py");
        assert_eq!(ops[0].hunks.len(), 1);
        assert_eq!(
            ops[0].hunks[0].context_hint,
            Some("context hint".to_string())
        );
        assert_eq!(ops[0].hunks[0].lines.len(), 3);
    }

    #[test]
    fn test_parse_delete() {
        let patch = r#"*** Begin Patch
*** Delete File: old.py
*** End Patch"#;

        let ops = parse_v4a_patch_impl(patch);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, OP_DELETE);
        assert_eq!(ops[0].file_path, "old.py");
    }

    #[test]
    fn test_parse_move() {
        let patch = r#"*** Begin Patch
*** Move File: old.py -> new.py
*** End Patch"#;

        let ops = parse_v4a_patch_impl(patch);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, OP_MOVE);
        assert_eq!(ops[0].file_path, "old.py");
        assert_eq!(ops[0].new_path, Some("new.py".to_string()));
    }
}
