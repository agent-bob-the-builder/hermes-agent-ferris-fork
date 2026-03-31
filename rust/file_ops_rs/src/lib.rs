//! file_ops_rs — Rust-native file operation helpers for hermes-agent.

use pyo3::prelude::*;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;

static BINARY_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".ico", ".tiff", ".tif",
    ".svg", ".mp3", ".mp4", ".wav", ".avi", ".mov", ".mkv", ".flac", ".ogg",
    ".webm", ".zip", ".tar", ".gz", ".bz2", ".xz", ".7z", ".rar", ".pdf",
    ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".exe", ".dll", ".so",
    ".dylib", ".o", ".a", ".pyc", ".pyo", ".class", ".wasm", ".bin", ".ttf",
    ".otf", ".woff", ".woff2", ".eot", ".db", ".sqlite", ".sqlite3",
];

static IMAGE_EXTENSIONS: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".ico"];

static HIDDEN_EXCLUDE: &[&str] = &[".git", "node_modules", "__pycache__", ".hub", ".venv", "venv"];

const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINES: usize = 2000;

#[inline]
fn is_binary_ext(ext: &str) -> bool {
    BINARY_EXTENSIONS.contains(&ext)
}

#[inline]
fn is_image_ext(ext: &str) -> bool {
    IMAGE_EXTENSIONS.contains(&ext)
}

fn is_likely_binary(path_ext: &str, sample: &str) -> bool {
    if is_binary_ext(path_ext) {
        return true;
    }
    if sample.is_empty() {
        return false;
    }
    let non_printable: usize = sample
        .chars()
        .take(1000)
        .filter(|c| {
            let cp = *c as u32;
            cp < 32 && *c != '\n' && *c != '\r' && *c != '\t'
        })
        .count();
    non_printable * 10 > 3 * usize::min(sample.len(), 1000)
}

#[inline]
fn is_image_extension(path_ext: &str) -> bool {
    is_image_ext(path_ext)
}

fn add_line_numbers(content: &str, start_line: usize) -> String {
    let num_lines = content.lines().count();
    let mut result = String::with_capacity(content.len() + num_lines * 8);
    for (i, line) in content.lines().enumerate() {
        let line_num = start_line + i;
        let display = if line.len() > MAX_LINE_LENGTH {
            format!("{}... [truncated]", &line[..MAX_LINE_LENGTH])
        } else {
            line.to_string()
        };
        use std::fmt::Write;
        let _ = write!(&mut result, "{:6}|{}\n", line_num, display);
    }
    result
}

fn native_expand_path(path: &str) -> String {
    if path.is_empty() {
        return path.to_string();
    }
    if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().into_owned();
        }
        return path.to_string();
    }
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.to_string_lossy(), &path[1..]);
        }
    }
    path.to_string()
}

fn escape_shell_arg(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\"'\"'"))
}

fn unified_diff(old_content: &str, new_content: &str, filename: &str) -> String {
    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();
    let m = old_lines.len();
    let n = new_lines.len();

    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if old_lines[i - 1] == new_lines[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    let mut lcs_pairs: Vec<(usize, usize)> = Vec::new();
    let mut i = m;
    let mut j = n;
    while i > 0 && j > 0 {
        if old_lines[i - 1] == new_lines[j - 1] {
            lcs_pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] > dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    lcs_pairs.reverse();

    let mut result = format!("--- a/{}\n+++ b/{}\n", filename, filename);
    let mut old_idx: usize = 0;
    let mut new_idx: usize = 0;

    for (lcs_old, lcs_new) in lcs_pairs {
        while old_idx < lcs_old {
            let h_start = old_idx.saturating_sub(3);
            result.push_str(&format!(
                "@@ -{},{} +{},{} @@\n", h_start + 1,
                lcs_old - old_idx + lcs_new - new_idx,
                new_idx.saturating_sub(3) + 1,
                lcs_old - old_idx + lcs_new - new_idx
            ));
            result.push_str(&format!("-{}\n", old_lines[old_idx]));
            old_idx += 1;
        }
        while new_idx < lcs_new {
            let h_start = old_idx.saturating_sub(3);
            result.push_str(&format!(
                "@@ -{},{} +{},{} @@\n", h_start + 1,
                lcs_old - old_idx + lcs_new - new_idx,
                new_idx.saturating_sub(3) + 1,
                lcs_old - old_idx + lcs_new - new_idx
            ));
            result.push_str(&format!("+{}\n", new_lines[new_idx]));
            new_idx += 1;
        }
        result.push_str(&format!(" {}\n", old_lines[lcs_old]));
        old_idx = lcs_old + 1;
        new_idx = lcs_new + 1;
    }

    while old_idx < m {
        result.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_idx.saturating_sub(3) + 1, m - old_idx,
            new_idx.saturating_sub(3) + 1, n - new_idx
        ));
        result.push_str(&format!("-{}\n", old_lines[old_idx]));
        old_idx += 1;
    }
    while new_idx < n {
        result.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_idx.saturating_sub(3) + 1, m - old_idx,
            new_idx.saturating_sub(3) + 1, n - new_idx
        ));
        result.push_str(&format!("+{}\n", new_lines[new_idx]));
        new_idx += 1;
    }

    result
}

fn suggest_similar_files(path: &str) -> Vec<String> {
    let dir_path = Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    let filename = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    if filename.is_empty() {
        return vec![];
    }

    let mut similar = Vec::new();
    let filename_lower: HashSet<char> = filename.to_lowercase().chars().collect();

    if let Ok(entries) = std::fs::read_dir(&dir_path) {
        for entry in entries.flatten() {
            if let Ok(name) = entry.file_name().into_string() {
                let name_lower: HashSet<char> = name.to_lowercase().chars().collect();
                let common = filename_lower.intersection(&name_lower).count();
                if common >= filename.len() / 2 {
                    similar.push(entry.path().to_string_lossy().into_owned());
                }
            }
        }
    }
    similar.truncate(5);
    similar
}

fn matches_glob(filename: &str, pattern: &str) -> bool {
    let regex_pattern = pattern.replace('*', ".*").replace('?', ".");
    Regex::new(&format!("^{}$", regex_pattern))
        .map(|re| re.is_match(filename))
        .unwrap_or(false)
}

fn walk_and_search(
    root: &Path,
    re: &Regex,
    file_glob: Option<&str>,
    limit: usize,
) -> std::io::Result<Vec<(String, usize, String)>> {
    let hidden: HashSet<&str> = HIDDEN_EXCLUDE.iter().copied().collect();
    let mut matches = Vec::new();

    for entry in walkdir::WalkDir::new(root)
        .max_depth(256)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.') && !hidden.contains(name.as_ref())
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let filename = entry.file_name().to_string_lossy();
        if filename.starts_with('.') {
            continue;
        }
        if let Some(glob_pat) = file_glob {
            if !matches_glob(&filename, glob_pat) {
                continue;
            }
        }
        if let Some(ext) = entry.path().extension() {
            if is_binary_ext(&ext.to_string_lossy().to_lowercase()) {
                continue;
            }
        }
        let filepath = entry.path();
        if let Ok(content) = std::fs::read_to_string(filepath) {
            for (lineno, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    let trimmed = line
                        .trim_end_matches(['\n', '\r'])
                        .chars()
                        .take(500)
                        .collect::<String>();
                    matches.push((filepath.to_string_lossy().into_owned(), lineno + 1, trimmed));
                    if matches.len() >= limit {
                        return Ok(matches);
                    }
                }
            }
        }
    }
    Ok(matches)
}

fn search_native(
    pattern: &str,
    path_str: &str,
    file_glob: Option<&str>,
    limit: usize,
    offset: usize,
    output_mode: &str,
    _context: usize,
) -> Option<String> {
    let path = Path::new(path_str);
    if !path.is_dir() {
        return None;
    }
    let re = Regex::new(pattern).ok()?;
    let limit_plus_offset = limit.saturating_add(offset);

    let matches = walk_and_search(path, &re, file_glob, limit_plus_offset).ok()?;

    let total = matches.len();

    let result = if output_mode == "files_only" {
        let mut files: Vec<String> = matches.iter().map(|(p, _, _)| p.clone()).collect();
        files.sort();
        files.dedup();
        let total_files = files.len();
        let page: Vec<String> = files.into_iter().skip(offset).take(limit).collect();
        serde_json::json!({
            "files": page,
            "total_count": total_files,
            "truncated": total > limit + offset
        })
    } else if output_mode == "count" {
        let mut counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for (p, _, _) in &matches {
            *counts.entry(p.clone()).or_insert(0) += 1;
        }
        serde_json::json!({"counts": counts, "total_count": total})
    } else {
        let page: Vec<serde_json::Value> = matches
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(path, lineno, content)| {
                serde_json::json!({"path": path, "line": lineno, "content": content})
            })
            .collect();
        serde_json::json!({
            "matches": page,
            "total_count": total,
            "truncated": total > limit + offset
        })
    };

    Some(result.to_string())
}

fn search_files_native(
    pattern: &str,
    path_str: &str,
    limit: usize,
    offset: usize,
) -> Option<String> {
    let path = Path::new(path_str);
    if !path.is_dir() {
        return None;
    }

    let hidden: HashSet<&str> = HIDDEN_EXCLUDE.iter().copied().collect();
    let limit_plus_offset = limit.saturating_add(offset);
    let mut matches: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(path)
        .max_depth(256)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.') && !hidden.contains(name.as_ref())
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let filename = entry.file_name().to_string_lossy();
        if filename.starts_with('.') {
            continue;
        }

        let glob_pattern = if pattern.starts_with("**/") {
            &pattern[3..]
        } else if pattern.contains('/') {
            pattern
        } else {
            &format!("*{}*", pattern)
        };

        if matches_glob(&filename, glob_pattern) {
            matches.push(entry.path().to_string_lossy().into_owned());
            if matches.len() >= limit_plus_offset {
                break;
            }
        }
    }

    let total = matches.len();
    let page: Vec<String> = matches.into_iter().skip(offset).take(limit).collect();
    Some(
        serde_json::json!({
            "files": page,
            "total_count": total,
            "truncated": total > limit + offset
        })
        .to_string(),
    )
}

fn mime_from_ext(ext: &str) -> &'static str {
    match ext {
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".bmp" => "image/bmp",
        ".ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn call_python_fuzzy_match(
    py: Python<'_>,
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> (String, usize, Option<String>) {
    let fuzzy = match py.import("tools.fuzzy_match") {
        Ok(fm) => fm,
        Err(_) => {
            return (
                content.to_string(),
                0,
                Some("fuzzy_match module not available".to_string()),
            );
        }
    };
    let result = match fuzzy.call_method1(
        "fuzzy_find_and_replace",
        (content, old_string, new_string, replace_all),
    ) {
        Ok(r) => r,
        Err(_) => {
            return (
                content.to_string(),
                0,
                Some("fuzzy_match call failed".to_string()),
            );
        }
    };

    let new_content: String = match result.get_item(0) {
        Ok(item) => match item.extract() {
            Ok(v) => v,
            Err(_) => content.to_string(),
        },
        Err(_) => content.to_string(),
    };
    let count: usize = match result.get_item(1) {
        Ok(item) => match item.extract() {
            Ok(v) => v,
            Err(_) => 0,
        },
        Err(_) => 0,
    };
    let error: Option<String> = match result.get_item(2) {
        Ok(item) => item.extract().ok(),
        Err(_) => None,
    };

    (new_content, count, error)
}

// ---------------------------------------------------------------------------
// Python module
// ---------------------------------------------------------------------------

#[pymodule]
fn _file_ops_rust(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("MAX_LINE_LENGTH", MAX_LINE_LENGTH)?;
    m.add("MAX_LINES", MAX_LINES)?;

    m.add_function(pyo3::wrap_pyfunction!(is_likely_binary_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(is_image_extension_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(is_binary_ext_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(add_line_numbers_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(native_expand_path_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(escape_shell_arg_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(unified_diff_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(suggest_similar_files_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(search_native_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(search_files_native_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(fuzzy_find_and_replace_py, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(mime_from_ext_py, m)?)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Python wrappers
// ---------------------------------------------------------------------------

#[pyfunction]
fn is_likely_binary_py(path: &str, sample: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    is_likely_binary(&ext, sample)
}

#[pyfunction]
fn is_image_extension_py(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    is_image_extension(&ext)
}

#[pyfunction]
fn is_binary_ext_py(ext: &str) -> bool {
    is_binary_ext(ext)
}

#[pyfunction]
fn add_line_numbers_py(content: &str, start_line: usize) -> String {
    add_line_numbers(content, start_line)
}

#[pyfunction]
fn native_expand_path_py(path: &str) -> String {
    native_expand_path(path)
}

#[pyfunction]
fn escape_shell_arg_py(arg: &str) -> String {
    escape_shell_arg(arg)
}

#[pyfunction]
fn unified_diff_py(old_content: &str, new_content: &str, filename: &str) -> String {
    unified_diff(old_content, new_content, filename)
}

#[pyfunction]
fn suggest_similar_files_py(path: &str) -> Vec<String> {
    suggest_similar_files(path)
}

#[pyfunction]
fn search_native_py(
    pattern: &str,
    path: &str,
    file_glob: Option<&str>,
    limit: usize,
    offset: usize,
    output_mode: &str,
    context: usize,
) -> Option<String> {
    search_native(pattern, path, file_glob, limit, offset, output_mode, context)
}

#[pyfunction]
fn search_files_native_py(
    pattern: &str,
    path: &str,
    limit: usize,
    offset: usize,
) -> Option<String> {
    search_files_native(pattern, path, limit, offset)
}

#[pyfunction]
fn fuzzy_find_and_replace_py(
    py: Python<'_>,
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> (String, usize, Option<String>) {
    call_python_fuzzy_match(py, content, old_string, new_string, replace_all)
}

#[pyfunction]
fn mime_from_ext_py(ext: &str) -> String {
    mime_from_ext(ext).to_string()
}
