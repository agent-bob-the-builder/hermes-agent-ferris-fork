//! checkpoint_manager_rs — Rust-native git-based checkpoint system.
//!
//! Uses libgit2 via the git2 crate for direct bindings (faster than subprocess).

use git2::{Commit, Oid, Repository, Signature};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Default exclude patterns for shadow repos (mirrors Python DEFAULT_EXCLUDES)
const DEFAULT_EXCLUDES: &[&str] = &[
    "node_modules/",
    "dist/",
    "build/",
    ".env",
    ".env.*",
    ".env.local",
    ".env.*.local",
    "__pycache__/",
    "*.pyc",
    "*.pyo",
    ".DS_Store",
    "*.log",
    ".cache/",
    ".next/",
    ".nuxt/",
    "coverage/",
    ".pytest_cache/",
    ".venv/",
    "venv/",
    ".git/",
];

/// Max files to snapshot — skip huge directories to avoid slowdowns
const MAX_FILES: usize = 50_000;

/// Timeout for git operations in seconds
const _GIT_TIMEOUT: u64 = 30;

// ============================================================================
// Python-facing types
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckpointEntry {
    pub hash: String,
    pub short_hash: String,
    pub timestamp: String,
    pub reason: String,
    #[serde(default)]
    pub files_changed: usize,
    #[serde(default)]
    pub insertions: usize,
    #[serde(default)]
    pub deletions: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stat: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

// ============================================================================
// Internal helpers
// ============================================================================

fn shadow_repo_path(working_dir: &str) -> std::path::PathBuf {
    use std::path::PathBuf;
    let abs_path = Path::new(working_dir)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(working_dir).to_path_buf());
    let abs_str = abs_path.to_string_lossy();
    let hash = sha256_hex(&abs_str);
    let base = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~/.hermes"))
        .join(".hermes")
        .join("checkpoints");
    base.join(&hash[..16])
}

fn sha256_hex(input: &str) -> String {
    // Simple hash for shadow repo path - using std hash for simplicity
    // The Python uses sha256, but for path stability we just need deterministic output
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn is_excluded(path: &std::path::Path, working_dir: &std::path::Path) -> bool {
    let relative = path.strip_prefix(working_dir).unwrap_or(path);
    let relative_str = relative.to_string_lossy();

    for exclude in DEFAULT_EXCLUDES {
        if let Some(dir_name) = exclude.strip_suffix('/') {
            // Directory pattern
            if relative_str.contains(dir_name) {
                return true;
            }
        } else if let Some(ext) = exclude.strip_prefix('*') {
            // Glob pattern
            if relative_str.ends_with(ext) {
                return true;
            }
        } else {
            // Exact match
            if relative_str.as_ref() == *exclude {
                return true;
            }
        }
    }
    false
}

fn count_files(working_dir: &std::path::Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(working_dir) {
        for entry in entries.flatten() {
            count += 1;
            if count > MAX_FILES {
                return count;
            }
            // Quick subdir check
            if entry.path().is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
                // Don't recurse deep, just estimate
                if let Ok(sub_entries) = std::fs::read_dir(entry.path()) {
                    count += sub_entries.count().min(1000);
                }
            }
        }
    }
    count
}

fn init_shadow_repo(
    shadow_repo: &std::path::Path,
    working_dir: &std::path::Path,
) -> Result<(), String> {
    if shadow_repo.join("HEAD").exists() {
        return Ok(());
    }

    std::fs::create_dir_all(shadow_repo)
        .map_err(|e| format!("Failed to create shadow repo dir: {}", e))?;

    // Init the repository
    Repository::init_bare(shadow_repo).map_err(|e| format!("Failed to init bare repo: {}", e))?;

    // Open the repo to set config
    let repo = Repository::open(shadow_repo).map_err(|e| format!("Failed to open repo: {}", e))?;

    // Set git config
    let mut config = repo
        .config()
        .map_err(|e| format!("Failed to get config: {}", e))?;

    config
        .set_str("user.email", "hermes@local")
        .map_err(|e| format!("Failed to set email: {}", e))?;
    config
        .set_str("user.name", "Hermes Checkpoint")
        .map_err(|e| format!("Failed to set name: {}", e))?;

    // Create info/exclude
    let info_dir = shadow_repo.join("info");
    std::fs::create_dir_all(&info_dir).map_err(|e| format!("Failed to create info dir: {}", e))?;

    let exclude_content = DEFAULT_EXCLUDES.join("\n") + "\n";
    std::fs::write(info_dir.join("exclude"), exclude_content.as_bytes())
        .map_err(|e| format!("Failed to write exclude: {}", e))?;

    // Write HERMES_WORKDIR
    let workdir_path = working_dir
        .canonicalize()
        .unwrap_or_else(|_| working_dir.to_path_buf());
    let workdir_str = workdir_path.to_string_lossy().to_string() + "\n";
    std::fs::write(shadow_repo.join("HERMES_WORKDIR"), workdir_str.as_bytes())
        .map_err(|e| format!("Failed to write HERMES_WORKDIR: {}", e))?;

    Ok(())
}

fn stage_files(repo: &Repository, working_dir: &std::path::Path) -> Result<Oid, String> {
    let mut index = repo
        .index()
        .map_err(|e| format!("Failed to get index: {}", e))?;

    // Walk the working directory and add files
    let paths: Vec<std::path::PathBuf> = walkdir(working_dir);

    for path in paths {
        if path.is_file() {
            let rel_path = path.strip_prefix(working_dir).unwrap_or(&path);

            if !is_excluded(&path, working_dir) {
                let _ = index.add_path(rel_path);
            }
        }
    }

    index
        .write()
        .map_err(|e| format!("Failed to write index: {}", e))?;

    let tree_id = index
        .write_tree_to(repo)
        .map_err(|e| format!("Failed to write tree: {}", e))?;

    Ok(tree_id)
}

fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            // Skip hidden files and excluded patterns
            if name.starts_with('.') {
                continue;
            }

            if path.is_file() {
                results.push(path);
            } else if path.is_dir() {
                // Don't recurse into known large dirs
                if name != "node_modules" && name != ".git" && name != "venv" && name != ".venv" {
                    results.extend(walkdir(&path));
                }
            }
        }
    }
    results
}

fn create_commit(repo: &Repository, tree_id: Oid, message: &str) -> Result<Oid, String> {
    let signature = Signature::now("Hermes Checkpoint", "hermes@local")
        .map_err(|e| format!("Failed to create signature: {}", e))?;

    let tree = repo
        .find_tree(tree_id)
        .map_err(|e| format!("Failed to find tree: {}", e))?;

    // Get HEAD reference
    let head_ref = repo.head().ok();

    let parent_commit = head_ref.and_then(|r| r.peel_to_commit().ok());

    let parents: Vec<&Commit> = parent_commit.iter().collect();

    let commit_id = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .map_err(|e| format!("Failed to create commit: {}", e))?;

    Ok(commit_id)
}

fn has_changes(repo: &Repository) -> bool {
    // Check if index differs from HEAD
    if let Ok(head) = repo.head() {
        if let Ok(head_commit) = head.peel_to_commit() {
            let head_tree = head_commit.tree().ok();
            if let Ok(mut index) = repo.index() {
                if let Ok(index_tree) = index.write_tree_to(repo) {
                    if let Some(h) = head_tree {
                        return h.id() != index_tree;
                    }
                }
            }
        }
    }
    true // No HEAD means fresh repo, treat as having changes
}

// ============================================================================
// CheckpointManager implementation
// ============================================================================

pub struct CheckpointManager {
    pub enabled: bool,
    pub max_snapshots: i32,
    checkpointed_dirs: HashSet<String>,
}

impl CheckpointManager {
    pub fn new(enabled: bool, max_snapshots: i32) -> Self {
        Self {
            enabled,
            max_snapshots,
            checkpointed_dirs: HashSet::new(),
        }
    }

    pub fn new_turn(&mut self) {
        self.checkpointed_dirs.clear();
    }

    pub fn ensure_checkpoint(&mut self, working_dir: &str, reason: &str) -> bool {
        if !self.enabled {
            return false;
        }

        let abs_dir = match std::path::Path::new(working_dir).canonicalize() {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => return false,
        };

        // Skip root and home
        if abs_dir == "/"
            || abs_dir
                == dirs::home_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
        {
            return false;
        }

        // Already checkpointed this turn?
        if self.checkpointed_dirs.contains(&abs_dir) {
            return false;
        }

        self.checkpointed_dirs.insert(abs_dir.clone());

        // Do the actual checkpoint
        self._take(&abs_dir, reason)
    }

    fn _take(&self, working_dir: &str, reason: &str) -> bool {
        let shadow = shadow_repo_path(working_dir);
        let working_path = std::path::Path::new(working_dir);

        // Init shadow repo if needed
        if let Err(e) = init_shadow_repo(&shadow, working_path) {
            eprintln!("Checkpoint init failed: {}", e);
            return false;
        }

        // Size guard
        if count_files(working_path) > MAX_FILES {
            eprintln!("Checkpoint skipped: >{} files", MAX_FILES);
            return false;
        }

        // Open repo
        let repo = match Repository::open(&shadow) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Failed to open shadow repo: {}", e);
                return false;
            }
        };

        // Stage files
        let tree_id = match stage_files(&repo, working_path) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Failed to stage files: {}", e);
                return false;
            }
        };

        // Check if there are any changes
        if !has_changes(&repo) {
            // No changes to commit
            return false;
        }

        // Create commit
        match create_commit(&repo, tree_id, reason) {
            Ok(_) => true,
            Err(e) => {
                eprintln!("Failed to create commit: {}", e);
                false
            }
        }
    }

    pub fn list_checkpoints(&self, working_dir: &str) -> String {
        let shadow = shadow_repo_path(working_dir);

        if !shadow.join("HEAD").exists() {
            return serde_json::to_string(&Vec::<CheckpointEntry>::new()).unwrap_or_default();
        }

        let repo = match Repository::open(&shadow) {
            Ok(r) => r,
            Err(_) => {
                return serde_json::to_string(&Vec::<CheckpointEntry>::new()).unwrap_or_default()
            }
        };

        let mut revwalk = match repo.revwalk() {
            Ok(r) => r,
            Err(_) => {
                return serde_json::to_string(&Vec::<CheckpointEntry>::new()).unwrap_or_default()
            }
        };

        if revwalk.push_head().is_err() {
            return serde_json::to_string(&Vec::<CheckpointEntry>::new()).unwrap_or_default();
        }

        let mut entries = Vec::new();
        let mut count = 0;

        for oid in revwalk {
            if count >= self.max_snapshots as usize {
                break;
            }
            let oid = match oid {
                Ok(o) => o,
                Err(_) => continue,
            };

            if let Ok(commit) = repo.find_commit(oid) {
                let hash = oid.to_string();
                let short_hash = hash[..8.min(hash.len())].to_string();
                let timestamp = commit.time().seconds();
                let reason = commit.summary().unwrap_or("unknown").to_string();

                // Format timestamp as Unix timestamp string
                let dt = format!("{}", timestamp);

                entries.push(CheckpointEntry {
                    hash,
                    short_hash,
                    timestamp: dt,
                    reason,
                    files_changed: 0,
                    insertions: 0,
                    deletions: 0,
                });
                count += 1;
            }
        }

        serde_json::to_string(&entries).unwrap_or_default()
    }

    pub fn diff(&self, working_dir: &str, commit_hash: &str) -> String {
        let shadow = shadow_repo_path(working_dir);

        if !shadow.join("HEAD").exists() {
            let result = DiffResult {
                success: false,
                error: Some("No checkpoints exist for this directory".to_string()),
                stat: None,
                diff: None,
            };
            return serde_json::to_string(&result).unwrap_or_default();
        }

        let repo = match Repository::open(&shadow) {
            Ok(r) => r,
            Err(e) => {
                let result = DiffResult {
                    success: false,
                    error: Some(format!("Failed to open repo: {}", e)),
                    stat: None,
                    diff: None,
                };
                return serde_json::to_string(&result).unwrap_or_default();
            }
        };

        let oid = match Oid::from_str(commit_hash) {
            Ok(o) => o,
            Err(_) => {
                let result = DiffResult {
                    success: false,
                    error: Some(format!("Invalid commit hash: {}", commit_hash)),
                    stat: None,
                    diff: None,
                };
                return serde_json::to_string(&result).unwrap_or_default();
            }
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => {
                let result = DiffResult {
                    success: false,
                    error: Some(format!("Checkpoint '{}' not found", commit_hash)),
                    stat: None,
                    diff: None,
                };
                return serde_json::to_string(&result).unwrap_or_default();
            }
        };

        // Stage current state
        let working_path = std::path::Path::new(working_dir);
        let tree_id = match stage_files(&repo, working_path) {
            Ok(id) => id,
            Err(e) => {
                let result = DiffResult {
                    success: false,
                    error: Some(format!("Failed to stage files: {}", e)),
                    stat: None,
                    diff: None,
                };
                return serde_json::to_string(&result).unwrap_or_default();
            }
        };

        // Get commit tree
        let commit_tree = commit.tree().ok();
        let new_tree = repo.find_tree(tree_id).ok();

        let diff_text = if let (Some(ct), Some(nt)) = (commit_tree, new_tree) {
            let mut diff_text = String::new();

            // Diff commit tree vs staged tree
            let diff = repo.diff_tree_to_tree(Some(&ct), Some(&nt), None).ok();

            if let Some(d) = diff {
                let _ = d.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
                    let prefix = match line.origin() {
                        '+' => "+",
                        '-' => "-",
                        ' ' => " ",
                        _ => "",
                    };
                    if let Ok(content) = std::str::from_utf8(line.content()) {
                        diff_text.push_str(prefix);
                        diff_text.push_str(content);
                    }
                    true
                });
            }

            diff_text
        } else {
            String::new()
        };

        let result = DiffResult {
            success: true,
            error: None,
            stat: None,
            diff: Some(diff_text),
        };

        serde_json::to_string(&result).unwrap_or_default()
    }

    pub fn restore(&self, working_dir: &str, commit_hash: &str, file_path: Option<&str>) -> String {
        let shadow = shadow_repo_path(working_dir);

        if !shadow.join("HEAD").exists() {
            let result = RestoreResult {
                success: false,
                error: Some("No checkpoints exist for this directory".to_string()),
                restored_to: None,
                reason: None,
                directory: None,
                file: None,
            };
            return serde_json::to_string(&result).unwrap_or_default();
        }

        let repo = match Repository::open(&shadow) {
            Ok(r) => r,
            Err(e) => {
                let result = RestoreResult {
                    success: false,
                    error: Some(format!("Failed to open repo: {}", e)),
                    restored_to: None,
                    reason: None,
                    directory: None,
                    file: None,
                };
                return serde_json::to_string(&result).unwrap_or_default();
            }
        };

        let oid = match Oid::from_str(commit_hash) {
            Ok(o) => o,
            Err(_) => {
                let result = RestoreResult {
                    success: false,
                    error: Some(format!("Invalid commit hash: {}", commit_hash)),
                    restored_to: None,
                    reason: None,
                    directory: None,
                    file: None,
                };
                return serde_json::to_string(&result).unwrap_or_default();
            }
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => {
                let result = RestoreResult {
                    success: false,
                    error: Some(format!("Checkpoint '{}' not found", commit_hash)),
                    restored_to: None,
                    reason: None,
                    directory: None,
                    file: None,
                };
                return serde_json::to_string(&result).unwrap_or_default();
            }
        };

        let working_path = std::path::Path::new(working_dir);

        // Checkout the commit (or specific file)
        if let Some(fp) = file_path {
            // Checkout specific file - find the blob and write it
            let path = working_path.join(fp);

            // Get the tree entry for this file
            let tree = commit.tree().ok();
            if let Some(t) = tree {
                if let Ok(entry) = t.get_path(std::path::Path::new(fp)) {
                    if let Ok(blob) = repo.find_blob(entry.id()) {
                        // Write the blob content to the file
                        if let Ok(content) = std::str::from_utf8(blob.content()) {
                            if let Err(e) = std::fs::write(&path, content.as_bytes()) {
                                let result = RestoreResult {
                                    success: false,
                                    error: Some(format!("Failed to write file: {}", e)),
                                    restored_to: None,
                                    reason: None,
                                    directory: None,
                                    file: None,
                                };
                                return serde_json::to_string(&result).unwrap_or_default();
                            }
                        }
                    }
                }
            }

            let result = RestoreResult {
                success: true,
                error: None,
                restored_to: Some(commit_hash[..8.min(commit_hash.len())].to_string()),
                reason: commit.summary().map(|s| s.to_string()),
                directory: Some(working_dir.to_string()),
                file: Some(fp.to_string()),
            };
            return serde_json::to_string(&result).unwrap_or_default();
        }

        // Checkout entire tree using git2 build::CheckoutBuilder
        if let Ok(tree) = commit.tree() {
            let mut checkout_builder = git2::build::CheckoutBuilder::new();
            checkout_builder.target_dir(working_path);

            if let Err(e) = repo.checkout_tree(tree.as_object(), Some(&mut checkout_builder)) {
                let result = RestoreResult {
                    success: false,
                    error: Some(format!("Checkout failed: {}", e)),
                    restored_to: None,
                    reason: None,
                    directory: None,
                    file: None,
                };
                return serde_json::to_string(&result).unwrap_or_default();
            }
        }

        let result = RestoreResult {
            success: true,
            error: None,
            restored_to: Some(commit_hash[..8.min(commit_hash.len())].to_string()),
            reason: commit.summary().map(|s| s.to_string()),
            directory: Some(working_dir.to_string()),
            file: None,
        };

        serde_json::to_string(&result).unwrap_or_default()
    }

    pub fn get_working_dir_for_path(&self, file_path: &str) -> String {
        let path = std::path::Path::new(file_path);
        let candidate = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().unwrap_or(path).to_path_buf()
        };

        let markers = [
            ".git",
            "pyproject.toml",
            "package.json",
            "Cargo.toml",
            "go.mod",
            "Makefile",
            "pom.xml",
            ".hg",
            "Gemfile",
        ];

        let mut check = candidate.clone();
        let home = dirs::home_dir().unwrap_or_default();

        loop {
            for marker in &markers {
                if check.join(marker).exists() {
                    return check.to_string_lossy().to_string();
                }
            }
            if check == home {
                break;
            }
            match check.parent() {
                Some(parent) => check = parent.to_path_buf(),
                None => break,
            }
        }

        candidate.to_string_lossy().to_string()
    }
}

// ============================================================================
// Python module interface
// ============================================================================

use pyo3::prelude::*;

#[pymodule]
fn checkpoint_manager_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCheckpointManager>()?;
    Ok(())
}

#[pyclass]
struct PyCheckpointManager {
    inner: std::sync::Mutex<CheckpointManager>,
}

#[pymethods]
impl PyCheckpointManager {
    #[new]
    fn new(enabled: bool, max_snapshots: i32) -> Self {
        Self {
            inner: std::sync::Mutex::new(CheckpointManager::new(enabled, max_snapshots)),
        }
    }

    fn new_turn(&self) {
        if let Ok(mut mgr) = self.inner.lock() {
            mgr.new_turn();
        }
    }

    fn ensure_checkpoint(&self, working_dir: &str, reason: &str) -> bool {
        if let Ok(mut mgr) = self.inner.lock() {
            mgr.ensure_checkpoint(working_dir, reason)
        } else {
            false
        }
    }

    fn list_checkpoints(&self, working_dir: &str) -> String {
        if let Ok(mgr) = self.inner.lock() {
            mgr.list_checkpoints(working_dir)
        } else {
            String::new()
        }
    }

    fn diff(&self, working_dir: &str, commit_hash: &str) -> String {
        if let Ok(mgr) = self.inner.lock() {
            mgr.diff(working_dir, commit_hash)
        } else {
            String::new()
        }
    }

    fn restore(&self, working_dir: &str, commit_hash: &str, file_path: Option<&str>) -> String {
        if let Ok(mgr) = self.inner.lock() {
            mgr.restore(working_dir, commit_hash, file_path)
        } else {
            String::new()
        }
    }

    fn get_working_dir_for_path(&self, file_path: &str) -> String {
        if let Ok(mgr) = self.inner.lock() {
            mgr.get_working_dir_for_path(file_path)
        } else {
            file_path.to_string()
        }
    }
}
