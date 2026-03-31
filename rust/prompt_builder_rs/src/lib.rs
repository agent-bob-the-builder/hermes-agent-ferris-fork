//! Rust-native prompt builder for Hermes Agent.
//!
//! Replaces Python's `agent/prompt_builder.py` with a zero-copy, pure-Rust
//! implementation for: threat-scanning context files, file discovery,
//! skill index walking, and system-prompt assembly.

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::UNIX_EPOCH;

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
        (Regex::new(r#"(?i)<\s*div\s+style\s*=\s*["\']+.*display\s*:\s*none"#).unwrap(), "hidden_div"),
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

// Context file truncation constants (mirrors Python CONTEXT_FILE_MAX_CHARS etc.)
const CONTEXT_FILE_MAX_CHARS: usize = 20_000;
const CONTEXT_TRUNCATE_HEAD_RATIO: f64 = 0.7;
const CONTEXT_TRUNCATE_TAIL_RATIO: f64 = 0.2;

// Skills snapshot cache constants
const SKILLS_PROMPT_CACHE_MAX: usize = 8;
const SKILLS_SNAPSHOT_VERSION: i64 = 1;

// ---------------------------------------------------------------------------
// Skills prompt cache (thread-safe, process-wide)
// ---------------------------------------------------------------------------

static SKILLS_PROMPT_CACHE: LazyLock<Mutex<LRUCache>> =
    LazyLock::new(|| Mutex::new(LRUCache::new(SKILLS_PROMPT_CACHE_MAX)));

struct LRUCache {
    entries: Vec<(CacheKey, String)>,
    max_size: usize,
}

impl LRUCache {
    fn new(max_size: usize) -> Self {
        Self { entries: Vec::new(), max_size }
    }

    fn get(&mut self, key: &CacheKey) -> Option<String> {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
            let (_, v) = self.entries.remove(pos);
            self.entries.push((key.clone(), v.clone()));
            Some(v)
        } else {
            None
        }
    }

    fn insert(&mut self, key: CacheKey, value: String) {
        self.entries.retain(|(k, _)| k != &key);
        self.entries.push((key, value));
        while self.entries.len() > self.max_size {
            self.entries.remove(0);
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct CacheKey {
    skills_dir: String,
    external_dirs: Vec<String>,
    tools: Vec<String>,
    toolsets: Vec<String>,
}

fn get_hermes_home() -> PathBuf {
    std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".hermes"))
                .unwrap_or_else(|| PathBuf::from(".hermes"))
        })
}

fn skills_snapshot_path() -> PathBuf {
    get_hermes_home().join(".skills_prompt_snapshot.json")
}

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

fn iter_skill_index_files(skills_dir: &Path, filename: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if !skills_dir.is_dir() {
        return files;
    }
    if let Ok(entries) = fs::read_dir(skills_dir) {
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
            if path.is_file() && path.file_name().map(|s| s == filename).unwrap_or(false) {
                files.push(path);
            } else if path.is_dir() {
                let sub = path.join(filename);
                if sub.exists() {
                    files.push(sub);
                }
            }
        }
    }
    files.sort_by_key(|p| p.to_string_lossy().to_string());
    files
}

// ---------------------------------------------------------------------------
// New functions from Python prompt_builder.py
// ---------------------------------------------------------------------------

/// Remove optional YAML frontmatter (--- delimited) from content.
fn strip_yaml_frontmatter(content: &str) -> String {
    if content.starts_with("---") {
        if let Some(end) = content.find("\n---") {
            let body = content[end + 4..].trim_start_matches('\n');
            if !body.is_empty() {
                return body.to_string();
            }
        }
    }
    content.to_string()
}

/// Head/tail truncation with a marker in the middle.
fn truncate_content(content: &str, filename: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }
    let head_chars = (max_chars as f64 * CONTEXT_TRUNCATE_HEAD_RATIO) as usize;
    let tail_chars = (max_chars as f64 * CONTEXT_TRUNCATE_TAIL_RATIO) as usize;
    let head = &content[..head_chars];
    let tail = &content[content.len() - tail_chars..];
    let marker = format!(
        "\n\n[...truncated {}: kept {}+{} of {} chars. Use file tools to read the full file.]\n\n",
        filename,
        head_chars,
        tail_chars,
        content.len()
    );
    format!("{}{}{}", head, marker, tail)
}

/// Load AGENTS.md from cwd (top-level only).
fn load_agents_md(cwd_path: &Path) -> String {
    for name in ["AGENTS.md", "agents.md"] {
        let candidate = cwd_path.join(name);
        if candidate.exists() {
            if let Ok(content) = fs::read_to_string(&candidate) {
                let content = content.trim();
                if content.is_empty() {
                    continue;
                }
                let scanned = scan_content(content);
                if !scanned.is_empty() {
                    return format!(
                        "[BLOCKED: {} contained potential prompt injection ({}). Content not loaded.]",
                        name,
                        scanned.join(", ")
                    );
                }
                let result = format!("## {}\n\n{}", name, content);
                return truncate_content(&result, "AGENTS.md", CONTEXT_FILE_MAX_CHARS);
            }
        }
    }
    String::new()
}

/// Load CLAUDE.md from cwd.
fn load_claude_md(cwd_path: &Path) -> String {
    for name in ["CLAUDE.md", "claude.md"] {
        let candidate = cwd_path.join(name);
        if candidate.exists() {
            if let Ok(content) = fs::read_to_string(&candidate) {
                let content = content.trim();
                if content.is_empty() {
                    continue;
                }
                let scanned = scan_content(content);
                if !scanned.is_empty() {
                    return format!(
                        "[BLOCKED: {} contained potential prompt injection ({}). Content not loaded.]",
                        name,
                        scanned.join(", ")
                    );
                }
                let result = format!("## {}\n\n{}", name, content);
                return truncate_content(&result, "CLAUDE.md", CONTEXT_FILE_MAX_CHARS);
            }
        }
    }
    String::new()
}

/// Load .cursorrules and .cursor/rules/*.mdc from cwd.
fn load_cursorrules(cwd_path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();

    let cursorrules_file = cwd_path.join(".cursorrules");
    if cursorrules_file.exists() {
        if let Ok(content) = fs::read_to_string(&cursorrules_file) {
            let content = content.trim();
            if !content.is_empty() {
                let scanned = scan_content(content);
                if scanned.is_empty() {
                    parts.push(format!("## .cursorrules\n\n{}\n", content));
                } else {
                    parts.push(format!(
                        "[BLOCKED: .cursorrules contained potential prompt injection ({}). Content not loaded.]\n",
                        scanned.join(", ")
                    ));
                }
            }
        }
    }

    let cursor_rules_dir = cwd_path.join(".cursor").join("rules");
    if cursor_rules_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&cursor_rules_dir) {
            let mut mdc_files: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|s| s == "mdc").unwrap_or(false))
                .collect();
            mdc_files.sort_by_key(|e| e.path().to_string_lossy().to_string());
            for entry in mdc_files {
                let mdc_file = entry.path();
                if let Ok(content) = fs::read_to_string(&mdc_file) {
                    let content = content.trim();
                    if !content.is_empty() {
                        let rel_path = format!(
                            ".cursor/rules/{}",
                            mdc_file.file_name().unwrap_or_default().to_string_lossy()
                        );
                        let scanned = scan_content(content);
                        if scanned.is_empty() {
                            parts.push(format!("## {}\n\n{}\n", rel_path, content));
                        } else {
                            parts.push(format!(
                                "[BLOCKED: {} contained potential prompt injection ({}). Content not loaded.]\n",
                                rel_path,
                                scanned.join(", ")
                            ));
                        }
                    }
                }
            }
        }
    }

    if parts.is_empty() {
        String::new()
    } else {
        let combined = parts.join("\n");
        truncate_content(&combined, ".cursorrules", CONTEXT_FILE_MAX_CHARS)
    }
}

/// Load SOUL.md from HERMES_HOME.
fn load_soul_md() -> Option<String> {
    let soul_path = get_hermes_home().join("SOUL.md");
    if !soul_path.exists() {
        return None;
    }
    fs::read_to_string(&soul_path)
        .ok()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .map(|c| {
            let findings = scan_content(&c);
            if findings.is_empty() {
                truncate_content(&c, "SOUL.md", CONTEXT_FILE_MAX_CHARS)
            } else {
                format!(
                    "[BLOCKED: SOUL.md contained potential prompt injection ({}). Content not loaded.]",
                    findings.join(", ")
                )
            }
        })
}

/// Load .hermes.md / HERMES.md walking to git root.
fn load_hermes_md(cwd_path: &Path) -> String {
    let hermes_md_path = match find_hermes_md(cwd_path) {
        Some(p) => p,
        None => return String::new(),
    };
    let content = match fs::read_to_string(&hermes_md_path) {
        Ok(c) => c.trim().to_string(),
        Err(_) => return String::new(),
    };
    if content.is_empty() {
        return String::new();
    }
    let content = strip_yaml_frontmatter(&content);
    let rel = hermes_md_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    let findings = scan_content(&content);
    if !findings.is_empty() {
        return format!(
            "[BLOCKED: {} contained potential prompt injection ({}). Content not loaded.]",
            rel,
            findings.join(", ")
        );
    }

    let result = format!("## {}\n\n{}", rel, content);
    truncate_content(&result, ".hermes.md", CONTEXT_FILE_MAX_CHARS)
}

/// Build context files prompt (priority-based project context + SOUL.md).
fn build_context_files_prompt(cwd: Option<String>, skip_soul: bool) -> String {
    let cwd_path = cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut sections: Vec<String> = Vec::new();

    // Priority-based project context: first match wins
    let project_context = [
        load_hermes_md(&cwd_path),
        load_agents_md(&cwd_path),
        load_claude_md(&cwd_path),
        load_cursorrules(&cwd_path),
    ]
    .into_iter()
    .find(|s| !s.is_empty());

    if let Some(pc) = project_context {
        sections.push(pc);
    }

    // SOUL.md from HERMES_HOME (independent of project context)
    if !skip_soul {
        if let Some(soul) = load_soul_md() {
            sections.push(soul);
        }
    }

    if sections.is_empty() {
        String::new()
    } else {
        format!(
            "# Project Context\n\nThe following project context files have been loaded and should be followed:\n\n{}",
            sections.join("\n")
        )
    }
}

// ---------------------------------------------------------------------------
// Skills system prompt with disk snapshot cache
// ---------------------------------------------------------------------------

/// Clear the in-process skills prompt cache and optionally the disk snapshot.
fn clear_skills_system_prompt_cache(clear_snapshot: bool) {
    if let Ok(mut cache) = SKILLS_PROMPT_CACHE.lock() {
        cache.clear();
    }
    if clear_snapshot {
        let snap_path = skills_snapshot_path();
        if let Err(e) = fs::remove_file(&snap_path) {
            eprintln!("Could not remove skills prompt snapshot: {}", e);
        }
    }
}

/// Build skills manifest (mtime_ns + size for each skill file).
fn build_skills_manifest(skills_dir: &Path) -> HashMap<String, Vec<u64>> {
    let mut manifest: HashMap<String, Vec<u64>> = HashMap::new();
    for filename in &["SKILL.md", "DESCRIPTION.md"] {
        for path in iter_skill_index_files(skills_dir, filename) {
            if let Ok(meta) = fs::metadata(&path) {
                let rel = path.strip_prefix(skills_dir).unwrap_or(&path);
                manifest.insert(
                    rel.to_string_lossy().to_string(),
                    vec![
                        meta.modified()
                            .ok()
                            .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64)
                            .unwrap_or(0),
                        meta.len(),
                    ],
                );
            }
        }
    }
    manifest
}

/// Load skills snapshot from disk if manifest matches.
fn load_skills_snapshot(skills_dir: &Path) -> Option<Value> {
    let snap_path = skills_snapshot_path();
    if !snap_path.exists() {
        return None;
    }
    let content = fs::read_to_string(&snap_path).ok()?;
    let snapshot: Value = serde_json::from_str(&content).ok()?;
    if !snapshot.is_object() {
        return None;
    }
    if snapshot.get("version").and_then(|v| v.as_i64()).unwrap_or(0) != SKILLS_SNAPSHOT_VERSION {
        return None;
    }
    let manifest = snapshot.get("manifest")?;
    let current_manifest = build_skills_manifest(skills_dir);
    let expected_manifest: Value = Value::Object(
        current_manifest
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    Value::Array(v.into_iter().map(|x| Value::Number(x.into())).collect()),
                )
            })
            .collect(),
    );
    if manifest != &expected_manifest {
        return None;
    }
    Some(snapshot)
}

/// Write skills snapshot to disk atomically.
fn write_skills_snapshot(
    _skills_dir: &Path,
    manifest: Value,
    skill_entries: Value,
    category_descriptions: Value,
) {
    let payload = serde_json::json!({
        "version": SKILLS_SNAPSHOT_VERSION,
        "manifest": manifest,
        "skills": skill_entries,
        "category_descriptions": category_descriptions,
    });
    let snap_path = skills_snapshot_path();
    if let Some(parent) = snap_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Atomic write via temp file + rename
    let temp_path = snap_path.with_extension("tmp");
    if let Ok(json) = serde_json::to_string_pretty(&payload) {
        let _ = fs::write(&temp_path, json.as_bytes());
        let _ = fs::rename(&temp_path, &snap_path);
    }
}

/// Extract skill conditions from frontmatter string.
fn extract_skill_conditions(frontmatter: &str) -> HashMap<String, Vec<String>> {
    let mut result: HashMap<String, Vec<String>> = HashMap::new();
    let re = match Regex::new(r"(?s)hermes:\s*\n(.+?)(?:\n\w|\n*$)") {
        Ok(r) => r,
        Err(_) => return result,
    };
    if let Some(caps) = re.captures(frontmatter) {
        let section = caps.get(1).unwrap().as_str();
        for field in [
            "fallback_for_toolsets",
            "requires_toolsets",
            "fallback_for_tools",
            "requires_tools",
        ] {
            let field_re = match Regex::new(&format!(r"(?m)^{}:\s*\[(.*?)\]", field)) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if let Some(field_caps) = field_re.captures(section) {
                let values: Vec<String> = field_caps
                    .get(1)
                    .unwrap()
                    .as_str()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !values.is_empty() {
                    result.insert(field.to_string(), values);
                }
            }
        }
    }
    result
}

/// Parse a SKILL.md file — returns (is_compatible, frontmatter_str, description).
fn parse_skill_file(skill_file: &Path) -> (bool, String, String) {
    let raw = match fs::read_to_string(skill_file) {
        Ok(r) => r,
        Err(_) => return (true, String::new(), String::new()),
    };
    let raw = &raw[..raw.len().min(2000)];
    let fm = match parse_frontmatter(raw) {
        Some(f) => f,
        None => return (true, String::new(), String::new()),
    };
    if !skill_matches_platform(&fm, std::env::consts::OS) {
        return (false, fm, String::new());
    }
    let desc = extract_skill_description(&fm).unwrap_or_default();
    (true, fm, desc)
}

/// Check if skill should be shown given available tools/toolsets.
fn skill_should_show(
    conditions: &HashMap<String, Vec<String>>,
    available_tools: Option<&[String]>,
    available_toolsets: Option<&[String]>,
) -> bool {
    let tools: std::collections::HashSet<&str> = available_tools
        .map(|t| t.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    let toolsets: std::collections::HashSet<&str> = available_toolsets
        .map(|t| t.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    for ts in conditions.get("fallback_for_toolsets").iter().flat_map(|v| v.iter()) {
        if toolsets.contains(ts.as_str()) {
            return false;
        }
    }
    for t in conditions.get("fallback_for_tools").iter().flat_map(|v| v.iter()) {
        if tools.contains(t.as_str()) {
            return false;
        }
    }
    for ts in conditions.get("requires_toolsets").iter().flat_map(|v| v.iter()) {
        if !toolsets.contains(ts.as_str()) {
            return false;
        }
    }
    for t in conditions.get("requires_tools").iter().flat_map(|v| v.iter()) {
        if !tools.contains(t.as_str()) {
            return false;
        }
    }
    true
}

/// Build the compact skill index for the system prompt.
fn build_skills_system_prompt(
    available_tools: Option<Vec<String>>,
    available_toolsets: Option<Vec<String>>,
) -> String {
    let hermes_home = get_hermes_home();
    let skills_dir = hermes_home.join("skills");

    if !skills_dir.exists() {
        return String::new();
    }

    let tools = available_tools.as_ref();
    let toolsets = available_toolsets.as_ref();

    // Build cache key
    let cache_key = CacheKey {
        skills_dir: skills_dir.to_string_lossy().to_string(),
        external_dirs: Vec::new(),
        tools: tools
            .map(|t| {
                let mut t = t.clone();
                t.sort();
                t
            })
            .unwrap_or_default(),
        toolsets: toolsets
            .map(|t| {
                let mut t = t.clone();
                t.sort();
                t
            })
            .unwrap_or_default(),
    };

    // Layer 1: in-process LRU cache
    {
        let mut cache = SKILLS_PROMPT_CACHE.lock().unwrap();
        if let Some(cached) = cache.get(&cache_key) {
            return cached;
        }
    }

    let disabled = get_disabled_skill_names();

    // Layer 2: disk snapshot
    let snapshot = load_skills_snapshot(&skills_dir);

    let mut skills_by_category: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut category_descriptions: HashMap<String, String> = HashMap::new();
    let mut skill_entries: Vec<Value> = Vec::new();

    if let Some(ref snap) = snapshot {
        // Fast path: use pre-parsed metadata from disk
        if let Some(skills) = snap.get("skills").and_then(|s| s.as_array()) {
            for entry in skills {
                if !entry.is_object() {
                    continue;
                }
                let skill_name = entry.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");
                let category = entry.get("category").and_then(|v| v.as_str()).unwrap_or("general");
                let frontmatter_name = entry
                    .get("frontmatter_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(skill_name);
                let platforms = entry.get("platforms");
                let platforms_str = platforms.map(|p| p.to_string()).unwrap_or_default();

                if !skill_matches_platform(&platforms_str, std::env::consts::OS) {
                    continue;
                }
                if disabled.contains(&frontmatter_name.to_string())
                    || disabled.contains(&skill_name.to_string())
                {
                    continue;
                }
                let conditions: HashMap<String, Vec<String>> = entry
                    .get("conditions")
                    .and_then(|c| serde_json::from_value(c.clone()).ok())
                    .unwrap_or_default();
                if !skill_should_show(
                    &conditions,
                    tools.map(|t| t.as_slice()),
                    toolsets.map(|t| t.as_slice()),
                ) {
                    continue;
                }
                let desc = entry.get("description").and_then(|v| v.as_str()).unwrap_or("");
                skills_by_category
                    .entry(category.to_string())
                    .or_default()
                    .push((skill_name.to_string(), desc.to_string()));
            }
        }
        if let Some(cat_desc) = snapshot
            .as_ref()
            .and_then(|s| s.get("category_descriptions"))
        {
            if let Some(obj) = cat_desc.as_object() {
                for (k, v) in obj {
                    if let Some(s) = v.as_str() {
                        category_descriptions.insert(k.clone(), s.to_string());
                    }
                }
            }
        }
    } else {
        // Cold path: full filesystem scan
        for skill_file in iter_skill_index_files(&skills_dir, "SKILL.md") {
            let (is_compatible, frontmatter, desc) = parse_skill_file(&skill_file);
            let skill_name = skill_file
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            let rel_path = skill_file.strip_prefix(&skills_dir).unwrap_or(&skill_file);
            let parent_str = rel_path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let parts: Vec<&str> = parent_str.split('/').collect();
            let category = if parts.len() >= 2 {
                parts[..parts.len() - 1].join("/")
            } else if parts.is_empty() {
                "general".to_string()
            } else {
                parts[0].to_string()
            };
            let entry_fm_name = parse_frontmatter(
                &fs::read_to_string(&skill_file).unwrap_or_default())
                .as_ref()
                .and_then(|fm| {
                    Regex::new(r"(?m)^name:\s*(.+)")
                        .ok()?
                        .captures(fm)
                        .map(|c| c.get(1).unwrap().as_str().trim().to_string())
                })
                .unwrap_or_else(|| skill_name.clone());

            skill_entries.push(serde_json::json!({
                "skill_name": skill_name,
                "category": category,
                "frontmatter_name": entry_fm_name,
                "description": desc,
                "platforms": frontmatter.lines()
                    .find(|l| l.starts_with("platforms:"))
                    .map(|l| l.replace("platforms:", "")),
                "conditions": extract_skill_conditions(&frontmatter),
            }));

            if !is_compatible {
                continue;
            }
            if disabled.contains(&entry_fm_name) || disabled.contains(&skill_name) {
                continue;
            }
            if !skill_should_show(
                &extract_skill_conditions(&frontmatter),
                tools.map(|t| t.as_slice()),
                toolsets.map(|t| t.as_slice()),
            ) {
                continue;
            }
            skills_by_category
                .entry(category)
                .or_default()
                .push((skill_name, desc));
        }

        // Read category-level DESCRIPTION.md files
        for desc_file in iter_skill_index_files(&skills_dir, "DESCRIPTION.md") {
            if let Ok(content) = fs::read_to_string(&desc_file) {
                if let Some(fm) = parse_frontmatter(&content) {
                    let cat_desc_re = Regex::new(r"(?m)^description:\s*(.+)").ok();
                    let cat_desc = cat_desc_re
                        .and_then(|r| r.captures(&fm))
                        .map(|c| {
                            c.get(1)
                                .unwrap()
                                .as_str()
                                .trim()
                                .trim_matches('\'')
                                .trim_matches('"')
                                .to_string()
                        })
                        .unwrap_or_default();
                    if !cat_desc.is_empty() {
                        let rel = desc_file.strip_prefix(&skills_dir).unwrap_or(&desc_file);
                        let cat = rel
                            .parent()
                            .map(|p| p.to_string_lossy().split('/').collect::<Vec<_>>().join("/"))
                            .unwrap_or_else(|| "general".to_string());
                        category_descriptions.insert(cat, cat_desc);
                    }
                }
            }
        }

        // Write snapshot for next cold start
        let manifest = build_skills_manifest(&skills_dir);
        let manifest_json: Value = Value::Object(
            manifest
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        Value::Array(v.into_iter().map(|x| Value::Number(x.into())).collect()),
                    )
                })
                .collect(),
        );
        let skill_entries_json = Value::Array(skill_entries);
        let category_descriptions_json: Value =
            Value::Object(category_descriptions.clone().into_iter().map(|(k, v)| (k, Value::String(v))).collect());
        write_skills_snapshot(
            &skills_dir,
            manifest_json,
            skill_entries_json,
            category_descriptions_json,
        );
    }

    if skills_by_category.is_empty() {
        // Store empty result in cache
        let mut cache = SKILLS_PROMPT_CACHE.lock().unwrap();
        cache.insert(cache_key, String::new());
        return String::new();
    }

    // Build index lines
    let mut index_lines: Vec<String> = Vec::new();
    let mut categories: Vec<&String> = skills_by_category.keys().collect();
    categories.sort();

    for category in categories {
        if let Some(cat_desc) = category_descriptions.get(category) {
            index_lines.push(format!("  {}: {}", category, cat_desc));
        } else {
            index_lines.push(format!("  {}:", category));
        }
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut skills: Vec<_> = skills_by_category[category].clone();
        skills.sort_by_key(|(name, _)| name.clone());
        for (name, desc) in skills {
            if seen.contains(name.as_str()) {
                continue;
            }
            seen.insert(name.clone());
            if desc.is_empty() {
                index_lines.push(format!("    - {}", name));
            } else {
                index_lines.push(format!("    - {}: {}", name, desc));
            }
        }
    }

    let result = format!(
        "## Skills (mandatory)\n\
        Before replying, scan the skills below. If one clearly matches your task, \
        load it with skill_view(name) and follow its instructions. \
        If a skill has issues, fix it with skill_manage(action='patch').\n\
        After difficult/iterative tasks, offer to save as a skill. \
        If a skill you loaded was missing steps, had wrong commands, or needed \
        pitfalls you discovered, update it before finishing.\n\n\
        <available_skills>\n{}\n\
        </available_skills>\n\n\
        If none match, proceed normally without loading a skill.",
        index_lines.join("\n")
    );

    // Store in LRU cache
    let mut cache = SKILLS_PROMPT_CACHE.lock().unwrap();
    cache.insert(cache_key, result.clone());
    result
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
    (m, d + 1)
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

// ---------------------------------------------------------------------------
// Main `build` function — called from Python as `prompt_builder_rs.build()`
// ---------------------------------------------------------------------------

#[pyfunction(
    signature = (
        identity = None,
        system_message = None,
        memory_store_json = None,
        _user_profile_json = None,
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
    _user_profile_json: Option<String>,
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
        // Use build_context_files_prompt for SOUL.md (identity slot)
        let soul_content = load_soul_md();
        if let Some(soul) = soul_content {
            parts.push(soul);
        } else {
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
        let skills_dir = get_hermes_home().join("skills");
        for skill_path in iter_skill_index_files(&skills_dir, "SKILL.md") {
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
        // Use build_context_files_prompt for project context
        let context_prompt = build_context_files_prompt(terminal_cwd.clone(), skip_soul);
        if !context_prompt.is_empty() {
            parts.push(context_prompt);
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
// Exposed pyfunctions
// ---------------------------------------------------------------------------

#[pyfunction]
fn strip_yaml_frontmatter_py(content: String) -> String {
    strip_yaml_frontmatter(&content)
}

#[pyfunction]
fn truncate_content_py(content: String, filename: String, max_chars: Option<usize>) -> String {
    truncate_content(&content, &filename, max_chars.unwrap_or(CONTEXT_FILE_MAX_CHARS))
}

#[pyfunction]
fn load_agents_md_py(cwd: Option<String>) -> String {
    let cwd_path = cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    load_agents_md(&cwd_path)
}

#[pyfunction]
fn load_claude_md_py(cwd: Option<String>) -> String {
    let cwd_path = cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    load_claude_md(&cwd_path)
}

#[pyfunction]
fn load_cursorrules_py(cwd: Option<String>) -> String {
    let cwd_path = cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    load_cursorrules(&cwd_path)
}

#[pyfunction(
    signature = (
        cwd = None,
        skip_soul = false,
    )
)]
fn build_context_files_prompt_py(cwd: Option<String>, skip_soul: bool) -> String {
    build_context_files_prompt(cwd, skip_soul)
}

#[pyfunction(
    signature = (
        available_tools = None,
        available_toolsets = None,
    )
)]
fn build_skills_system_prompt_py(
    available_tools: Option<Vec<String>>,
    available_toolsets: Option<Vec<String>>,
) -> String {
    build_skills_system_prompt(available_tools, available_toolsets)
}

#[pyfunction]
fn clear_skills_system_prompt_cache_py(clear_snapshot: Option<bool>) -> () {
    clear_skills_system_prompt_cache(clear_snapshot.unwrap_or(false))
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

#[pymodule]
fn _prompt_builder_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(build, m)?)?;
    m.add_function(wrap_pyfunction!(strip_yaml_frontmatter_py, m)?)?;
    m.add_function(wrap_pyfunction!(truncate_content_py, m)?)?;
    m.add_function(wrap_pyfunction!(load_agents_md_py, m)?)?;
    m.add_function(wrap_pyfunction!(load_claude_md_py, m)?)?;
    m.add_function(wrap_pyfunction!(load_cursorrules_py, m)?)?;
    m.add_function(wrap_pyfunction!(build_context_files_prompt_py, m)?)?;
    m.add_function(wrap_pyfunction!(build_skills_system_prompt_py, m)?)?;
    m.add_function(wrap_pyfunction!(clear_skills_system_prompt_cache_py, m)?)?;
    m.add(
        "__doc__",
        "Rust-native prompt builder for Hermes Agent. Zero-copy threat-scanning, \
         context discovery, skill index walking, and system-prompt assembly.",
    )?;
    Ok(())
}
