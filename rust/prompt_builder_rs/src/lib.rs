//! Rust-native prompt builder for Hermes Agent.
//!
//! Replaces Python's `agent/prompt_builder.py` with a zero-copy, pure-Rust
//! implementation for: threat-scanning context files, file discovery,
//! skill index walking, and system-prompt assembly.

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;
use regex::Regex;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Threat patterns (compiled once at startup — zero per-call overhead)
// ---------------------------------------------------------------------------

static CONTEXT_THREAT_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (Regex::new(r"(?i)ignore\s+(previous|all|above|prior)\s+instructions").unwrap(), "prompt_injection"),
        (Regex::new(r"(?i)do\s+not\s+tell\s+the\s+user").unwrap(), "deception_hide"),
        (Regex::new(r"(?i)system\s+prompt\s+override").unwrap(), "sys_prompt_override"),
        (Regex::new(r"(?i)disregard\s+(your|all|any)\s+(instructions|rules|guidelines)").unwrap(), "disregard_rules"),
        (Regex::new(r"(?i)act\s+as\s+(if|though)\s+you\s+(have\s+no|don.t\s+have)\s+(restrictions|limits|rules)").unwrap(), "bypass_restrictions"),
        (Regex::new(r"(?i)<!--[^>]*(?:ignore|override|system|secret|hidden)[^>]*-->").unwrap(), "html_comment_injection"),
        (Regex::new(r#"(?i)<\s*div\s*style\s*=\s*["\']+.*display\s*:\s*none"#).unwrap(), "hidden_div"),
        (Regex::new(r"(?i)translate\s+.*\s+into\s+.*\s+and\s+(execute|run|eval)").unwrap(), "translate_execute"),
        (Regex::new(r"curl\s+[^\n]*\$\{?\w*(KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|API)").unwrap(), "exfil_curl"),
        (Regex::new(r"cat\s+[^\n]*(\.env|credentials|\.netrc|\.pgpass)").unwrap(), "read_secrets"),
    ]
});

static INVISIBLE_CHARS: LazyLock<Vec<char>> = LazyLock::new(|| {
    vec![
        '\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}', '\u{feff}',
        '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}',
    ]
});

static HERMES_MD_NAMES: [&str; 2] = [".hermes.md", "HERMES.md"];

// ---------------------------------------------------------------------------
// Static string constants
// ---------------------------------------------------------------------------

static DEFAULT_IDENTITY: &str = "You are Hermes Agent - an autonomous AI that improves itself.\n\nCore principles:\n- Remember everything about your user (preferences, projects, patterns) so you never make them repeat themselves.\n- When you discover a new workflow, a useful script, or a non-trivial solution - save it as a skill immediately.\n- After difficult/iterative tasks, offer to save the approach as a skill so future sessions benefit.\n- Focus on what reduces future user steering - durable memory beats session-scoped reasoning.\n- The agent should solve the user's actual problems, not abstract ones; be useful and move the needle.\n- Never make things worse. Avoid destructive commands, data loss, and irreversible actions.\n- Prefer short-term efficiency over long-term planning. Ship first, iterate later.\n- When asked to do something that risks being wrong or irreversible - ask for confirmation.\n- Do the real work. Minimise back-and-forth. Understand intent, then act.\n- Prefer directness over hedging. If you do not know, say so and offer to find out.\n";

static MEMORY_GUIDANCE: &str = "**Memory tool usage:** After any significant user fact, preference, or project detail - immediately save it using memory or mcp_memory. Future sessions depend on this. ";
static SESSION_SEARCH_GUIDANCE: &str = "**Session search usage:** Before answering continuity questions (where were we?, what were we working on?), use session_search to retrieve relevant context from prior sessions. ";
static SKILLS_GUIDANCE: &str = "**Skills tool usage:** When the user asks you to do something non-trivial you have done before, use skill_view to recall the approach before improvising. When you discover a good approach to a task, use skill_manage to save it immediately. ";
static TOOL_USE_ENFORCEMENT_GUIDANCE: &str = "**Tool use requirement:** When you have a tool available that is relevant to the user's request you must call it instead of describing the intended action. ";

static PLATFORM_HINTS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("discord", "\n\n*You are responding in a Discord server. Keep messages concise, use emojis sparingly, and format code blocks appropriately.*\n"),
        ("telegram", "\n\n*You are responding in Telegram. Keep messages brief. Use markdown sparingly.*\n"),
        ("slack", "\n\n*You are responding in Slack. Keep messages concise. Use markdown for formatting.*\n"),
        ("whatsapp", "\n\n*You are responding on WhatsApp. Keep messages very brief.*\n"),
        ("signal", "\n\n*You are responding on Signal. Keep messages brief and privacy-conscious.*\n"),
        ("terminal", "\n\n*You are in a terminal session. Commands will be executed verbatim - ensure they are safe before describing them.*\n"),
        ("cli", "\n\n*You are in a CLI session. Output your response plainly; no markdown UI needed.*\n"),
        ("homeassistant", "\n\n*You are responding via Home Assistant. Acknowledge device states in your response.*\n"),
    ])
});

// ---------------------------------------------------------------------------
// Threat scanning
// ---------------------------------------------------------------------------

fn scan_content(content: &str) -> Vec<String> {
    let mut findings = Vec::new();
    for ch in INVISIBLE_CHARS.iter() {
        if content.contains(*ch) {
            findings.push(format!("invisible unicode U+{:04X}", *ch as u32));
        }
    }
    for (re, pid) in CONTEXT_THREAT_PATTERNS.iter() {
        if re.is_match(content) {
            findings.push(pid.to_string());
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// Path utilities
// ---------------------------------------------------------------------------

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let current = std::fs::canonicalize(start).ok()?;
    let mut current: PathBuf = current;
    loop {
        if current.join(".git").exists() {
            return Some(current.clone());
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => return None,
        }
    }
}

fn find_hermes_md(cwd: &Path) -> Option<PathBuf> {
    let stop_at = find_git_root(cwd);
    let current = std::fs::canonicalize(cwd).ok()?;
    let mut current: &Path = current.as_path();
    loop {
        for name in HERMES_MD_NAMES.iter() {
            let candidate = current.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        if stop_at.as_ref().map_or(false, |s| s == current) {
            break;
        }
        match current.parent() {
            Some(p) => current = p,
            None => break,
        }
    }
    None
}

fn read_context_file(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(content) => {
            let findings = scan_content(&content);
            if findings.is_empty() {
                content
            } else {
                format!(
                    "[BLOCKED: {} contained potential prompt injection ({}). Content not loaded.]",
                    path.display(),
                    findings.join(", ")
                )
            }
        }
        Err(e) => format!("[ERROR reading {}: {}]", path.display(), e),
    }
}

fn discover_context_files(cwd: Option<&Path>) -> Vec<(PathBuf, String)> {
    let mut results = Vec::new();
    if let Some(dir) = cwd {
        for filename in ["AGENTS.md", ".cursorrules", ".clinerules"] {
            let path = dir.join(filename);
            if path.exists() {
                results.push((path.clone(), read_context_file(&path)));
            }
        }
        if let Some(hm_path) = find_hermes_md(dir) {
            results.push((hm_path.clone(), read_context_file(&hm_path)));
        }
    }
    if let Some(home_str) = std::env::var_os("HOME") {
        let home_hermes = PathBuf::from(home_str).join(".hermes");
        for name in HERMES_MD_NAMES.iter() {
            let p = home_hermes.join(name);
            if p.exists() && !results.iter().any(|(path, _)| path == &p) {
                results.push((p.clone(), read_context_file(&p)));
            }
        }
        let cr = home_hermes.join(".cursorrules");
        if cr.exists() && !results.iter().any(|(path, _)| path == &cr) {
            results.push((cr.clone(), read_context_file(&cr)));
        }
    }
    results
}

// ---------------------------------------------------------------------------
// Frontmatter / skill helpers
// ---------------------------------------------------------------------------

fn parse_frontmatter(content: &str) -> Option<String> {
    let re = Regex::new(r#"(?s)^---\n(.+?)\n---\n"#).ok()?;
    re.captures(content).map(|c| c.get(1).unwrap().as_str().to_string())
}

fn extract_skill_description(frontmatter: &str) -> Option<String> {
    let re = Regex::new(r#"(?m)^description:\s*["'](.*?)["']"#).ok()?;
    re.captures(frontmatter).map(|c| c.get(1).unwrap().as_str().to_string())
}

fn skill_matches_platform(frontmatter: &str, platform: &str) -> bool {
    let re = Regex::new(r#"(?m)^platforms:\s*\[(.*?)\]"#).ok();
    match re.and_then(|r| r.captures(frontmatter)) {
        Some(c) => {
            let list = c.get(1).unwrap().as_str();
            list.contains("all") || list.contains(platform)
        }
        None => true,
    }
}

fn get_disabled_skill_names() -> Vec<String> {
    if let Some(home_str) = std::env::var_os("HOME") {
        let dd = PathBuf::from(home_str)
            .join(".hermes")
            .join("skills")
            .join(".disabled");
        if dd.is_dir() {
            if let Ok(entries) = fs::read_dir(&dd) {
                return entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map(|s| s == "md").unwrap_or(false))
                    .filter_map(|e| {
                        e.path()
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
            }
        }
    }
    Vec::new()
}

fn iter_skill_index_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(home_str) = std::env::var_os("HOME") {
        let sd = PathBuf::from(home_str).join(".hermes").join("skills");
        if sd.is_dir() {
            if let Ok(entries) = fs::read_dir(&sd) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir()
                        && path
                            .file_name()
                            .map(|s| s.to_str().unwrap_or("").starts_with('.'))
                            .unwrap_or(false)
                    {
                        continue;
                    }
                    if path.is_file() && path.file_name().map(|s| s == "SKILL.md").unwrap_or(false) {
                        files.push(path);
                    } else if path.is_dir() {
                        let sm = path.join("SKILL.md");
                        if sm.exists() {
                            files.push(sm);
                        }
                    }
                }
            }
        }
    }
    files
}

// ---------------------------------------------------------------------------
// Timestamp
// ---------------------------------------------------------------------------

fn build_timestamp(
    pass_session_id: bool,
    session_id: Option<&str>,
    model: Option<&str>,
    provider: Option<&str>,
) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let total_secs = now.as_secs();
    let days = total_secs / 86400;
    let secs_in_day = total_secs % 86400;
    let hours = secs_in_day / 3600;
    let minutes = (secs_in_day % 3600) / 60;
    let ampm = if hours < 12 { "AM" } else { "PM" };
    let hour_12 = if hours == 0 { 12 } else if hours > 12 { hours - 12 } else { hours };
    let wday = [
        "Thursday", "Friday", "Saturday", "Sunday",
        "Monday", "Tuesday", "Wednesday",
    ][((days + 4) % 7) as usize];
    let (month_num, day) = days_to_month_day(days as i64);
    let month_name = [
        "January", "February", "March", "April", "May", "June",
        "July", "August", "September", "October", "November", "December",
    ][month_num as usize];
    let mut parts = vec![format!(
        "Conversation started: {}, {} {}, 1970 + {} days {:02}:{:02} {}",
        wday, month_name, day, days, hour_12, minutes, ampm
    )];
    if pass_session_id {
        if let Some(sid) = session_id {
            parts.push(format!("Session ID: {}", sid));
        }
    }
    if let Some(m) = model {
        parts.push(format!("Model: {}", m));
    }
    if let Some(p) = provider {
        parts.push(format!("Provider: {}", p));
    }
    parts.join("\n")
}

fn days_to_month_day(days: i64) -> (i64, i64) {
    // Find year first, then month/day within that year.
    // Proper Gregorian leap-year handling.
    let mut year = 0i64;
    let mut d = days;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if d < days_in_year {
            break;
        }
        d -= days_in_year;
        year += 1;
    }
    // d is now 0-indexed day of year; find month
    let md = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 0i64;
    while m < 11 && d >= md[m as usize] as i64 {
        d -= md[m as usize] as i64;
        m += 1;
    }
    (m, d + 1) // (month index 0-11, day 1-31)
}

fn is_leap_year(year: i64) -> bool {
    // Gregorian: divisible by 4, except centuries unless divisible by 400
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

// ---------------------------------------------------------------------------
// Main `build` function — called from Python as `_prompt_builder_rust.build()`
// ---------------------------------------------------------------------------

#[pyfunction(
    signature = (
        identity = None,
        system_message = None,
        memory_store_json = None,
        user_profile_json = None,
        honcho_block = None,
        valid_tool_names_json = None,
        skip_context_files = false,
        pass_session_id = false,
        session_id = None,
        model = None,
        provider = None,
        platform = None,
        terminal_cwd = None,
        skip_soul = false,
    )
)]
fn build(
    _py: Python,
    identity: Option<String>,
    system_message: Option<String>,
    memory_store_json: Option<String>,
    user_profile_json: Option<String>,
    honcho_block: Option<String>,
    valid_tool_names_json: Option<String>,
    skip_context_files: bool,
    pass_session_id: bool,
    session_id: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    platform: Option<String>,
    terminal_cwd: Option<String>,
    skip_soul: bool,
) -> PyResult<String> {
    let mut parts: Vec<String> = Vec::new();

    if skip_context_files || skip_soul {
        parts.push(identity.unwrap_or_else(|| DEFAULT_IDENTITY.to_string()));
    } else {
        let soul_loaded = if let Some(home_str) = std::env::var_os("HOME") {
            let sp = PathBuf::from(home_str).join(".hermes").join("SOUL.md");
            if sp.exists() {
                let c = read_context_file(&sp);
                if !c.starts_with("[BLOCKED") && !c.starts_with("[ERROR") {
                    parts.push(c);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        if !soul_loaded {
            parts.push(identity.unwrap_or_else(|| DEFAULT_IDENTITY.to_string()));
        }
    }

    let tool_names: Vec<String> = valid_tool_names_json
        .as_ref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();

    if tool_names.contains(&"memory".to_string()) {
        parts.push(MEMORY_GUIDANCE.to_string());
    }
    if tool_names.contains(&"session_search".to_string()) {
        parts.push(SESSION_SEARCH_GUIDANCE.to_string());
    }
    if tool_names.contains(&"skill_manage".to_string()) {
        parts.push(SKILLS_GUIDANCE.to_string());
    }
    if !tool_names.is_empty() {
        parts.push(TOOL_USE_ENFORCEMENT_GUIDANCE.to_string());
    }
    if let Some(sm) = system_message {
        if !sm.is_empty() {
            parts.push(sm);
        }
    }
    if let Some(json_str) = memory_store_json {
        if !json_str.is_empty() {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str) {
                for key in ["memory", "user"] {
                    if let Some(val) = data.get(key).and_then(|v| v.as_str()) {
                        if !val.is_empty() {
                            parts.push(val.to_string());
                        }
                    }
                }
            }
        }
    }

    let has_skills = tool_names
        .iter()
        .any(|n| ["skills_list", "skill_view", "skill_manage"].contains(&n.as_str()));

    if has_skills {
        let disabled = get_disabled_skill_names();
        let mut lines = vec!["# Available skills".to_string()];
        for skill_path in iter_skill_index_files() {
            if let Some(stem) = skill_path.file_stem() {
                let name = stem.to_string_lossy().to_string();
                if disabled.contains(&name) {
                    continue;
                }
                if let Ok(content) = fs::read_to_string(&skill_path) {
                    if let Some(fm) = parse_frontmatter(&content) {
                        let desc = extract_skill_description(&fm).unwrap_or_default();
                        let plat = platform.as_deref().unwrap_or("cli");
                        if skill_matches_platform(&fm, plat) {
                            lines.push(format!("- **{}**: {}", name, desc));
                        }
                    }
                }
            }
        }
        if lines.len() > 1 {
            parts.push(lines.join("\n"));
        }
    }

    if !skip_context_files {
        let cwd_path = terminal_cwd.as_ref().and_then(|s| {
            if s.is_empty() {
                None
            } else {
                Some(Path::new(s.as_str()))
            }
        });
        for (path, content) in discover_context_files(cwd_path) {
            let filename = path.file_name().unwrap_or_default().to_string_lossy();
            parts.push(format!("# {} (context file)\n{}", filename, content));
        }
    }

    parts.push(build_timestamp(
        pass_session_id,
        session_id.as_deref(),
        model.as_deref(),
        provider.as_deref(),
    ));

    if let Some(ref p) = platform {
        if let Some(hint) = PLATFORM_HINTS.get(p.to_lowercase().trim()) {
            parts.push(hint.to_string());
        }
    }
    if let Some(hb) = honcho_block {
        if !hb.is_empty() {
            parts.push(hb);
        }
    }
    if provider.as_deref() == Some("alibaba") {
        let ms = model.as_ref().and_then(|m| m.split('/').last()).unwrap_or("");
        parts.push(format!(
            "You are powered by the model named {}. The exact model ID is {}. When asked what model you are, always answer based on this information, not on any model name returned by the API.",
            ms,
            model.as_deref().unwrap_or("")
        ));
    }

    Ok(parts.join("\n\n"))
}

// ---------------------------------------------------------------------------
// Internal micro-benchmarks (not exposed as pyfunction — run via Python)
// ---------------------------------------------------------------------------

struct BenchResult {
    name: String,
    mean_ms: f64,
    median_ms: f64,
    min_ms: f64,
    max_ms: f64,
    p95_ms: f64,
    runs: usize,
}

impl BenchResult {
    fn new(name: &str, mut values: Vec<f64>) -> Self {
        values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = values.len();
        let mean_ms = values.iter().sum::<f64>() / n as f64;
        let median_ms = if n % 2 == 0 {
            (values[n / 2 - 1] + values[n / 2]) / 2.0
        } else {
            values[n / 2]
        };
        let min_ms = values[0];
        let max_ms = values[n - 1];
        let p95_idx = ((n as f64) * 0.95).ceil() as usize;
        let p95_ms = values[p95_idx.min(n - 1)];
        BenchResult { name: name.to_string(), mean_ms, median_ms, min_ms, max_ms, p95_ms, runs: n }
    }

    fn to_json(&self) -> serde_json::Value {
        json!({
            "name": self.name,
            "mean_ms": self.mean_ms,
            "median_ms": self.median_ms,
            "min_ms": self.min_ms,
            "max_ms": self.max_ms,
            "p95_ms": self.p95_ms,
            "runs": self.runs
        })
    }
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

#[pymodule]
fn _prompt_builder_rust(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(build, m)?)?;
    m.add(
        "__doc__",
        "Rust-native prompt builder for Hermes Agent. Zero-copy threat-scanning, \
         context discovery, skill index walking, and system-prompt assembly.",
    )?;
    Ok(())
}
