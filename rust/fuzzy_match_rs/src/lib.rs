use pyo3::prelude::*;
use std::collections::HashMap;
use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref RE_FTS_QUOTED: Regex = Regex::new(r#"['"].*?['"]"#).unwrap();
    static ref RE_FTS_SPECIAL: Regex = Regex::new(r"[^\w\s]").unwrap();
    static ref RE_FTS_STARS: Regex = Regex::new(r"\*+").unwrap();
}

// Unicode normalization map
fn get_unicode_map() -> HashMap<char, char> {
    let mut m = HashMap::new();
    m.insert('\u{201c}', '"');
    m.insert('\u{201d}', '"');
    m.insert('\u{2018}', '\'');
    m.insert('\u{2019}', '\'');
    m.insert('\u{2014}', '-');
    m.insert('\u{2013}', '-');
    m.insert('\u{2026}', '.');
    m.insert('\u{00a0}', ' ');
    m
}

fn _unicode_normalize(text: &str) -> String {
    let unicode_map = get_unicode_map();
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        match unicode_map.get(&ch) {
            Some(&repl) => result.push(repl),
            None => result.push(ch),
        }
    }
    result
}

// LCS-based similarity (like SequenceMatcher.ratio() = 2*LCS / (len(a) + len(b)))
fn lcs_similarity(a: &str, b: &str) -> f64 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 && b_len == 0 {
        return 1.0;
    }
    if a_len == 0 || b_len == 0 {
        return 0.0;
    }

    // DP table for LCS length
    let mut dp = vec![vec![0usize; b_len + 1]; a_len + 1];
    for i in 1..=a_len {
        for j in 1..=b_len {
            if a_chars[i - 1] == b_chars[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }
    let lcs_len = dp[a_len][b_len];
    (2.0 * lcs_len as f64) / (a_len as f64 + b_len as f64)
}

type Match = (usize, usize);

fn _apply_replacements(content: &str, matches: &[Match], new_string: &str) -> String {
    let mut sorted_matches: Vec<(usize, usize)> = matches.to_vec();
    sorted_matches.sort_by(|a, b| b.0.cmp(&a.0));

    let mut result = content.to_string();
    for &(start, end) in &sorted_matches {
        result = format!(
            "{}{}{}",
            &result[..start],
            new_string,
            &result[end..]
        );
    }
    result
}

// =============================================================================
// Matching Strategies
// =============================================================================

fn _strategy_exact(content: &str, pattern: &str) -> Vec<Match> {
    let mut matches = Vec::new();
    let mut start = 0;
    while let Some(pos) = content[start..].find(pattern) {
        let actual_pos = start + pos;
        matches.push((actual_pos, actual_pos + pattern.len()));
        start = actual_pos + 1;
    }
    matches
}

fn _calculate_line_positions(
    content_lines: &[String],
    start_line: usize,
    end_line: usize,
    content_length: usize,
) -> (usize, usize) {
    let start_pos: usize = content_lines[..start_line]
        .iter()
        .map(|line| line.len() + 1)
        .sum();
    let mut end_pos: usize = content_lines[..end_line]
        .iter()
        .map(|line| line.len() + 1)
        .sum();
    end_pos = end_pos.saturating_sub(1);
    if end_pos >= content_length {
        end_pos = content_length;
    }
    (start_pos, end_pos)
}

fn _find_normalized_matches(
    content: &str,
    content_lines: &[String],
    content_normalized_lines: &[String],
    _pattern: &str,
    pattern_normalized: &str,
) -> Vec<Match> {
    let pattern_norm_lines: Vec<&str> = pattern_normalized.split('\n').collect();
    let num_pattern_lines = pattern_norm_lines.len();

    if num_pattern_lines == 0 {
        return vec![];
    }

    let mut matches = Vec::new();
    let content_len = content.len();

    for i in 0..content_normalized_lines.len().saturating_sub(num_pattern_lines) + 1 {
        let block: String = content_normalized_lines[i..i + num_pattern_lines].join("\n");
        if block == pattern_normalized {
            let (start_pos, end_pos) =
                _calculate_line_positions(content_lines, i, i + num_pattern_lines, content_len);
            matches.push((start_pos, end_pos));
        }
    }
    matches
}

fn _map_normalized_positions(
    original: &str,
    normalized: &str,
    normalized_matches: &[Match],
) -> Vec<Match> {
    if normalized_matches.is_empty() {
        return vec![];
    }

    let orig_chars: Vec<char> = original.chars().collect();
    let norm_chars: Vec<char> = normalized.chars().collect();
    let orig_len = orig_chars.len();
    let norm_len = norm_chars.len();

    let mut orig_to_norm: Vec<usize> = Vec::with_capacity(orig_len);

    let mut orig_idx = 0;
    let mut norm_idx = 0;

    while orig_idx < orig_len && norm_idx < norm_len {
        if orig_chars[orig_idx] == norm_chars[norm_idx] {
            orig_to_norm.push(norm_idx);
            orig_idx += 1;
            norm_idx += 1;
        } else if orig_chars[orig_idx] == ' ' || orig_chars[orig_idx] == '\t' {
            if norm_idx < norm_len && norm_chars[norm_idx] == ' ' {
                orig_to_norm.push(norm_idx);
                orig_idx += 1;
                if orig_idx < orig_len && orig_chars[orig_idx] != ' ' && orig_chars[orig_idx] != '\t' {
                    norm_idx += 1;
                }
            } else {
                orig_to_norm.push(norm_idx);
                orig_idx += 1;
            }
        } else {
            orig_to_norm.push(norm_idx);
            orig_idx += 1;
        }
    }

    while orig_idx < orig_len {
        orig_to_norm.push(norm_len);
        orig_idx += 1;
    }

    let mut norm_to_orig_start: HashMap<usize, usize> = HashMap::new();
    let mut norm_to_orig_end: HashMap<usize, usize> = HashMap::new();

    for (orig_pos, &norm_pos) in orig_to_norm.iter().enumerate() {
        norm_to_orig_start.entry(norm_pos).or_insert(orig_pos);
        norm_to_orig_end.insert(norm_pos, orig_pos);
    }

    let mut original_matches = Vec::new();
    for &(norm_start, norm_end) in normalized_matches {
        let orig_start = if let Some(&pos) = norm_to_orig_start.get(&norm_start) {
            pos
        } else {
            orig_to_norm.iter().find(|&&n| n >= norm_start).copied().unwrap_or(0)
        };

        let orig_end = if let Some(&pos) = norm_to_orig_end.get(&(norm_end.saturating_sub(1))) {
            pos + 1
        } else {
            orig_start + (norm_end - norm_start)
        };

        let mut expanded_end = orig_end;
        while expanded_end < orig_len
            && (orig_chars[expanded_end] == ' ' || orig_chars[expanded_end] == '\t')
        {
            expanded_end += 1;
        }

        original_matches.push((orig_start, expanded_end.min(orig_len)));
    }

    original_matches
}

fn _strategy_line_trimmed(content: &str, pattern: &str) -> Vec<Match> {
    let pattern_lines: Vec<&str> = pattern.split('\n').collect();
    let pattern_normalized: String = pattern_lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<&str>>()
        .join("\n");

    let content_lines: Vec<&str> = content.split('\n').collect();
    let content_normalized_lines: Vec<&str> = content_lines
        .iter()
        .map(|line| line.trim())
        .collect();

    _find_normalized_matches(
        content,
        &content_lines.iter().map(|s| (*s).to_string()).collect::<Vec<String>>(),
        &content_normalized_lines
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<String>>(),
        pattern,
        &pattern_normalized,
    )
}

fn _strategy_whitespace_normalized(content: &str, pattern: &str) -> Vec<Match> {
    fn normalize(s: &str) -> String {
        let mut result = String::new();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == ' ' || chars[i] == '\t' {
                // Collapse multiple spaces/tabs to single space
                while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
                    i += 1;
                }
                result.push(' ');
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    let pattern_normalized = normalize(pattern);
    let content_normalized = normalize(content);

    let matches_in_normalized = _strategy_exact(&content_normalized, &pattern_normalized);

    if matches_in_normalized.is_empty() {
        return vec![];
    }

    _map_normalized_positions(content, &content_normalized, &matches_in_normalized)
}

fn _strategy_indentation_flexible(content: &str, pattern: &str) -> Vec<Match> {
    let content_lines: Vec<&str> = content.split('\n').collect();
    let content_stripped_lines: Vec<String> = content_lines
        .iter()
        .map(|line| line.trim_start().to_string())
        .collect();

    let pattern_lines: Vec<&str> = pattern.split('\n').collect();
    let pattern_stripped: String = pattern_lines
        .iter()
        .map(|line| line.trim_start())
        .collect::<Vec<&str>>()
        .join("\n");

    _find_normalized_matches(
        content,
        &content_lines.iter().map(|s| (*s).to_string()).collect::<Vec<String>>(),
        &content_stripped_lines,
        pattern,
        &pattern_stripped,
    )
}

fn _strategy_escape_normalized(content: &str, pattern: &str) -> Vec<Match> {
    fn unescape(s: &str) -> String {
        s.replace("\\n", "\n").replace("\\t", "\t").replace("\\r", "\r")
    }

    let pattern_unescaped = unescape(pattern);

    if pattern_unescaped == pattern {
        return vec![];
    }

    _strategy_exact(content, &pattern_unescaped)
}

fn _strategy_trimmed_boundary(content: &str, pattern: &str) -> Vec<Match> {
    let pattern_lines: Vec<&str> = pattern.split('\n').collect();
    if pattern_lines.is_empty() {
        return vec![];
    }

    let mut modified_pattern: Vec<String> = pattern_lines.iter().map(|s| (*s).to_string()).collect();
    modified_pattern[0] = modified_pattern[0].trim().to_string();
    if modified_pattern.len() > 1 {
        let last_idx = modified_pattern.len() - 1;
        modified_pattern[last_idx] = modified_pattern[last_idx].trim().to_string();
    }
    let modified_pattern_str = modified_pattern.join("\n");

    let content_lines: Vec<&str> = content.split('\n').collect();
    let content_len = content.len();
    let pattern_line_count = pattern_lines.len();

    let mut matches = Vec::new();

    for i in 0..content_lines.len().saturating_sub(pattern_line_count) + 1 {
        let mut check_lines: Vec<String> = content_lines[i..i + pattern_line_count]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        check_lines[0] = check_lines[0].trim().to_string();
        if check_lines.len() > 1 {
            let last_idx = check_lines.len() - 1;
            check_lines[last_idx] = check_lines[last_idx].trim().to_string();
        }

        if check_lines.join("\n") == modified_pattern_str {
            let (start_pos, end_pos) =
                _calculate_line_positions(&content_lines.iter().map(|s| (*s).to_string()).collect::<Vec<String>>(), i, i + pattern_line_count, content_len);
            matches.push((start_pos, end_pos));
        }
    }

    matches
}

fn _strategy_block_anchor(content: &str, pattern: &str) -> Vec<Match> {
    let norm_pattern = _unicode_normalize(pattern);
    let norm_content = _unicode_normalize(content);

    let pattern_lines: Vec<&str> = norm_pattern.split('\n').collect();
    if pattern_lines.len() < 2 {
        return vec![];
    }

    let first_line = pattern_lines[0].trim();
    let last_line = pattern_lines[pattern_lines.len() - 1].trim();

    let norm_content_lines: Vec<&str> = norm_content.split('\n').collect();
    let orig_content_lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();

    let pattern_line_count = pattern_lines.len();

    let mut potential_matches: Vec<usize> = Vec::new();
    for i in 0..norm_content_lines.len().saturating_sub(pattern_line_count) + 1 {
        if norm_content_lines[i].trim() == first_line
            && norm_content_lines[i + pattern_line_count - 1].trim() == last_line
        {
            potential_matches.push(i);
        }
    }

    let candidate_count = potential_matches.len();
    let threshold = if candidate_count == 1 { 0.10 } else { 0.30 };

    let mut matches = Vec::new();
    let content_len = content.len();

    for &i in &potential_matches {
        let similarity: f64;
        if pattern_line_count <= 2 {
            similarity = 1.0;
        } else {
            let content_middle: String = norm_content_lines[i + 1..i + pattern_line_count - 1]
                .join("\n");
            let pattern_middle: String = pattern_lines[1..pattern_line_count - 1].join("\n");
            similarity = lcs_similarity(&content_middle, &pattern_middle);
        }

        if similarity >= threshold {
            let (start_pos, end_pos) =
                _calculate_line_positions(&orig_content_lines, i, i + pattern_line_count, content_len);
            matches.push((start_pos, end_pos));
        }
    }

    matches
}

fn _strategy_context_aware(content: &str, pattern: &str) -> Vec<Match> {
    let pattern_lines: Vec<&str> = pattern.split('\n').collect();
    let content_lines: Vec<&str> = content.split('\n').collect();

    if pattern_lines.is_empty() {
        return vec![];
    }

    let mut matches = Vec::new();
    let pattern_line_count = pattern_lines.len();
    let content_len = content.len();
    let orig_content_lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();

    for i in 0..content_lines.len().saturating_sub(pattern_line_count) + 1 {
        let block_lines: Vec<&str> = content_lines[i..i + pattern_line_count].to_vec();

        let mut high_similarity_count = 0;
        for (p_line, c_line) in pattern_lines.iter().zip(block_lines.iter()) {
            let sim = lcs_similarity(&p_line.trim(), &c_line.trim());
            if sim >= 0.80 {
                high_similarity_count += 1;
            }
        }

        if high_similarity_count as f64 >= pattern_lines.len() as f64 * 0.5 {
            let (start_pos, end_pos) =
                _calculate_line_positions(&orig_content_lines, i, i + pattern_line_count, content_len);
            matches.push((start_pos, end_pos));
        }
    }

    matches
}

// =============================================================================
// Main API
// =============================================================================

/// fuzzy_find_and_replace(content: str, old_string: str, new_string: str, replace_all: bool)
///     -> Tuple[str, int, Option[str]]
///
/// Find and replace text using a chain of increasingly fuzzy matching strategies.
#[pyfunction]
fn fuzzy_find_and_replace(
    content: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
) -> PyResult<(String, i32, Option<String>)> {
    if old_string.is_empty() {
        return Ok((content, 0, Some("old_string cannot be empty".to_string())));
    }

    if old_string == new_string {
        return Ok((
            content,
            0,
            Some("old_string and new_string are identical".to_string()),
        ));
    }

    let strategies: Vec<(&str, fn(&str, &str) -> Vec<Match>)> = vec![
        ("exact", _strategy_exact),
        ("line_trimmed", _strategy_line_trimmed),
        ("whitespace_normalized", _strategy_whitespace_normalized),
        ("indentation_flexible", _strategy_indentation_flexible),
        ("escape_normalized", _strategy_escape_normalized),
        ("trimmed_boundary", _strategy_trimmed_boundary),
        ("block_anchor", _strategy_block_anchor),
        ("context_aware", _strategy_context_aware),
    ];

    for (_strategy_name, strategy_fn) in strategies {
        let matches = strategy_fn(&content, &old_string);

        if !matches.is_empty() {
            if matches.len() > 1 && !replace_all {
                return Ok((
                    content,
                    0,
                    Some(format!(
                        "Found {} matches for old_string. Provide more context to make it unique, or use replace_all=True.",
                        matches.len()
                    )),
                ));
            }

            let new_content = _apply_replacements(&content, &matches, &new_string);
            return Ok((new_content, matches.len() as i32, None));
        }
    }

    Ok((content, 0, Some("Could not find a match for old_string in the file".to_string())))
}

#[pymodule]
fn fuzzy_match_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(fuzzy_find_and_replace, m)?)?;
    Ok(())
}
