//! Hermes State — Rust-native SQLite SessionDB with FTS5
//!
//! Replaces hermes_state.py (SessionDB class) with a rusqlite backend.

use lazy_static::lazy_static;
use pyo3::exceptions::{PyException, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::{PyAnyMethods, PyDict, PyList};
use rand::Rng;
use regex::Regex;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Mutex;

const SCHEMA_VERSION: i32 = 6;
const WRITE_MAX_RETRIES: u32 = 15;
const WRITE_RETRY_MIN_MS: f64 = 20.0;
const WRITE_RETRY_MAX_MS: f64 = 150.0;

static STATE: Lazy<Mutex<Option<RustState>>> = Lazy::new(|| Mutex::new(None));

struct RustState {
    conn: Connection,
    write_count: u32,
}

impl RustState {
    fn new(db_path: &str) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )?;
        conn.execute_batch("PRAGMA journal_mode=WAL")?;
        conn.execute_batch("PRAGMA foreign_keys=ON")?;
        Ok(Self { conn, write_count: 0 })
    }
}

// ── JSON helpers ──────────────────────────────────────────────────────────────

fn json_parse(s: &str) -> Result<JsonValue, serde_json::Error> {
    serde_json::from_str(s)
}

fn json_value_to_py(py: Python<'_>, value: JsonValue) -> PyResult<Py<PyAny>> {
    match value {
        JsonValue::Null => Ok(PyNone::new(py).into_any().unbind()),
        JsonValue::Bool(b) => Ok(PyBool::new(py, b).into_any().unbind()),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(PyLong::new(py, i).into_any().unbind())
            } else if let Some(f) = n.as_f64() {
                Ok(PyFloat::new(py, f).into_any().unbind())
            } else {
                Ok(PyLong::new(py, n.as_i64().unwrap_or(0)).into_any().unbind())
            }
        }
        JsonValue::String(s) => Ok(PyString::new(py, &s).into_any().unbind()),
        JsonValue::Array(arr) => {
            let list = PyList::empty(py);
            for v in arr {
                let py_v = json_value_to_py(py, v)?;
                list.append(py_v)?;
            }
            Ok(list.into_any().unbind())
        }
        JsonValue::Object(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                dict.set_item(&k, json_value_to_py(py, v)?)?;
            }
            Ok(dict.into_any().unbind())
        }
    }
}

// ── SQL helpers ─────────────────────────────────────────────────────────────

fn init_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            user_id TEXT,
            model TEXT,
            model_config TEXT,
            system_prompt TEXT,
            parent_session_id TEXT,
            started_at REAL NOT NULL,
            ended_at REAL,
            end_reason TEXT,
            message_count INTEGER DEFAULT 0,
            tool_call_count INTEGER DEFAULT 0,
            input_tokens INTEGER DEFAULT 0,
            output_tokens INTEGER DEFAULT 0,
            cache_read_tokens INTEGER DEFAULT 0,
            cache_write_tokens INTEGER DEFAULT 0,
            reasoning_tokens INTEGER DEFAULT 0,
            billing_provider TEXT,
            billing_base_url TEXT,
            billing_mode TEXT,
            estimated_cost_usd REAL,
            actual_cost_usd REAL,
            cost_status TEXT,
            cost_source TEXT,
            pricing_version TEXT,
            title TEXT,
            FOREIGN KEY (parent_session_id) REFERENCES sessions(id)
        );
        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            role TEXT NOT NULL,
            content TEXT,
            tool_call_id TEXT,
            tool_calls TEXT,
            tool_name TEXT,
            timestamp REAL NOT NULL,
            token_count INTEGER,
            finish_reason TEXT,
            reasoning TEXT,
            reasoning_details TEXT,
            codex_reasoning_items TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_source ON sessions(source);
        CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_id);
        CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, timestamp);
        ",
    )?;

    let has_rows: bool = conn
        .query_row("SELECT COUNT(*) > 0 FROM schema_version", [], |r| r.get(0))
        .unwrap_or(false);

    if !has_rows {
        conn.execute("INSERT INTO schema_version (version) VALUES (?)", params![SCHEMA_VERSION])?;
    }

    let current_version: i32 = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |r| r.get(0))
        .unwrap_or(1);

    run_migrations(conn, current_version)?;

    let fts_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='messages_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(false);

    if !fts_exists {
        conn.execute_batch(
            "
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                content=messages,
                content_rowid=id
            );
            CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS messages_fts_update AFTER UPDATE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content);
                INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
            END;
            ",
        )?;
    }

    Ok(())
}

fn run_migrations(conn: &Connection, mut current_version: i32) -> Result<(), rusqlite::Error> {
    if current_version < 2 {
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN finish_reason TEXT", []);
        let _ = conn.execute("UPDATE schema_version SET version = 2", []);
        current_version = 2;
    }
    if current_version < 3 {
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN title TEXT", []);
        let _ = conn.execute("UPDATE schema_version SET version = 3", []);
        current_version = 3;
    }
    if current_version < 4 {
        let _ = conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_title_unique ON sessions(title) WHERE title IS NOT NULL",
            [],
        );
        let _ = conn.execute("UPDATE schema_version SET version = 4", []);
        current_version = 4;
    }
    if current_version < 5 {
        for (col, typ) in &[
            ("cache_read_tokens", "INTEGER DEFAULT 0"),
            ("cache_write_tokens", "INTEGER DEFAULT 0"),
            ("reasoning_tokens", "INTEGER DEFAULT 0"),
            ("billing_provider", "TEXT"),
            ("billing_base_url", "TEXT"),
            ("billing_mode", "TEXT"),
            ("estimated_cost_usd", "REAL"),
            ("actual_cost_usd", "REAL"),
            ("cost_status", "TEXT"),
            ("cost_source", "TEXT"),
            ("pricing_version", "TEXT"),
        ] {
            let sql = format!("ALTER TABLE sessions ADD COLUMN {} {}", col, typ);
            let _ = conn.execute(&sql, []);
        }
        let _ = conn.execute("UPDATE schema_version SET version = 5", []);
        current_version = 5;
    }
    if current_version < 6 {
        for col in &["reasoning", "reasoning_details", "codex_reasoning_items"] {
            let sql = format!("ALTER TABLE messages ADD COLUMN {} TEXT", col);
            let _ = conn.execute(&sql, []);
        }
        let _ = conn.execute("UPDATE schema_version SET version = 6", []);
        current_version = 6;
    }

    let _ = conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_title_unique ON sessions(title) WHERE title IS NOT NULL",
        [],
    );

    Ok(())
}

// ── Write transaction with jitter retry ─────────────────────────────────────

fn execute_write<F, T>(state: &RustState, f: F) -> Result<T, rusqlite::Error>
where
    F: FnOnce(&Connection) -> Result<T, rusqlite::Error>,
{
    let mut rng = rand::thread_rng();
    let mut last_err = None;

    for attempt in 0..WRITE_MAX_RETRIES {
        match state.conn.execute_batch("BEGIN IMMEDIATE") {
            Ok(_) => {}
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        }

        match f(&state.conn) {
            Ok(result) => {
                if let Err(e) = state.conn.execute_batch("COMMIT") {
                    let _ = state.conn.execute_batch("ROLLBACK");
                    last_err = Some(e);
                    continue;
                }
                return Ok(result);
            }
            Err(e) => {
                let _ = state.conn.execute_batch("ROLLBACK");
                let err_str = e.message().to_lowercase();
                if err_str.contains("locked") || err_str.contains("busy") {
                    if attempt < WRITE_MAX_RETRIES - 1 {
                        let jitter = rng.gen_range(WRITE_RETRY_MIN_MS..WRITE_RETRY_MAX_MS);
                        std::thread::sleep(std::time::Duration::from_millis(jitter as u64));
                        last_err = Some(rusqlite::Error::InvalidQuery);
                        continue;
                    }
                }
                return Err(e);
            }
        }
    }

    Err(last_err.unwrap_or(rusqlite::Error::InvalidQuery))
}

// ── Sanitization ────────────────────────────────────────────────────────────

lazy_static! {
    static ref RE_TITLE_ASCII_CTRL: Regex = Regex::new(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]").unwrap();
    static ref RE_TITLE_UNICODE_CTRL: Regex = Regex::new(r"[\u200b-\u200f\u2028-\u202e\u2060-\u2069\ufeff\ufffc\ufff9-\ufffb]").unwrap();
    static ref RE_TITLE_WHITESPACE: Regex = Regex::new(r"\s+").unwrap();
    static ref RE_FTS_QUOTED: Regex = Regex::new(r#""[^"]*""#).unwrap();
    static ref RE_FTS_SPECIAL: Regex = Regex::new(r#"[+{}()"^]"#).unwrap();
    static ref RE_FTS_STARS: Regex = Regex::new(r"\*+").unwrap();
    static ref RE_FTS_LEADING_STAR: Regex = Regex::new(r"(^|\s)\*").unwrap();
    static ref RE_FTS_BOOLEAN_EDGE: Regex = Regex::new(r"(?i)^(AND|OR|NOT)\b\s*| +(?i)(AND|OR|NOT)\s*$").unwrap();
    static ref RE_FTS_HYPHENATED: Regex = Regex::new(r"\b(\w+(?:-\w+)+)\b").unwrap();
    static ref RE_SESSION_PREFIX_ESCAPE: Regex = Regex::new(r"([\\%_])").unwrap();
}

const MAX_TITLE_LENGTH: usize = 100;

fn sanitize_title(title: &str) -> Option<String> {
    if title.is_empty() {
        return None;
    }
    let cleaned = RE_TITLE_WHITESPACE
        .replace_all(
            &RE_TITLE_UNICODE_CTRL.replace_all(
                &RE_TITLE_ASCII_CTRL.replace_all(title, ""),
                "",
            ),
            " ",
        )
        .trim()
        .to_string();

    if cleaned.is_empty() || cleaned.len() > MAX_TITLE_LENGTH {
        return None;
    }
    Some(cleaned)
}

fn sanitize_fts5_query(query: &str) -> String {
    let mut quoted: Vec<&str> = Vec::new();
    let sanitized = RE_FTS_QUOTED.replace_all(query, |caps: &regex::Captures| {
        quoted.push(&caps[0]);
        let placeholder = format!("\x00Q{}\x00", quoted.len() - 1);
        placeholder
    });

    let sanitized = RE_FTS_SPECIAL.replace_all(&sanitized, " ");
    let sanitized = RE_FTS_LEADING_STAR.replace_all(&RE_FTS_STARS.replace_all(&sanitized, "*"), "");
    let sanitized = RE_FTS_BOOLEAN_EDGE.replace_all(&sanitized.trim(), "");
    let sanitized = RE_FTS_HYPHENATED
        .replace_all(&sanitized, |caps: &regex::Captures| format!("\"{}\"", &caps[1]))
        .to_string();

    let mut result = sanitized;
    for (i, q) in quoted.iter().enumerate() {
        result = result.replace(&format!("\x00Q{}\x00", i), q);
    }

    result.trim().to_string()
}

// ── PyO3 module functions ───────────────────────────────────────────────────

#[pyfunction]
fn init(db_path: String) -> PyResult<()> {
    let mut guard = STATE.lock().map_err(|e| PyException::new_err(e.to_string()))?;
    if guard.is_some() {
        return Ok(());
    }
    let mut state = RustState::new(&db_path).map_err(|e| PyException::new_err(e.to_string()))?;
    init_schema(&state.conn).map_err(|e| PyException::new_err(e.to_string()))?;
    *guard = Some(state);
    Ok(())
}

#[pyfunction]
fn is_initialized() -> bool {
    STATE.lock().map(|g| g.is_some()).unwrap_or(false)
}

fn with_state<T, F>(f: F) -> PyResult<T>
where
    F: FnOnce(&RustState) -> PyResult<T>,
{
    let guard = STATE.lock().map_err(|e| PyException::new_err(e.to_string()))?;
    let state = guard.as_ref().ok_or_else(|| {
        PyRuntimeError::new_err("hermes_state_rust: not initialized — call init() first")
    })?;
    f(state)
}

fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

// ── Session lifecycle ────────────────────────────────────────────────────────

#[pyfunction]
fn create_session(
    session_id: String,
    source: String,
    model: Option<String>,
    model_config: Option<String>,
    system_prompt: Option<String>,
    user_id: Option<String>,
    parent_session_id: Option<String>,
) -> PyResult<String> {
    with_state(|state| {
        let model_config_json = model_config
            .as_ref()
            .and_then(|s| json_parse(s).ok())
            .map(|v| serde_json::to_string(&v).unwrap_or_default());

        execute_write(state, |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO sessions (id, source, user_id, model, model_config, system_prompt, parent_session_id, started_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_id,
                    source,
                    user_id,
                    model,
                    model_config_json,
                    system_prompt,
                    parent_session_id,
                    now_f64(),
                ],
            )?;
            Ok(())
        })?;
        Ok(session_id)
    })
}

#[pyfunction]
fn end_session(session_id: String, end_reason: String) -> PyResult<()> {
    with_state(|state| {
        execute_write(state, |conn| {
            conn.execute(
                "UPDATE sessions SET ended_at = ?1, end_reason = ?2 WHERE id = ?3",
                params![now_f64(), end_reason, session_id],
            )?;
            Ok(())
        })?;
        Ok(())
    })
}

#[pyfunction]
fn update_system_prompt(session_id: String, system_prompt: String) -> PyResult<()> {
    with_state(|state| {
        execute_write(state, |conn| {
            conn.execute(
                "UPDATE sessions SET system_prompt = ?1 WHERE id = ?2",
                params![system_prompt, session_id],
            )?;
            Ok(())
        })?;
        Ok(())
    })
}

#[derive(Deserialize)]
struct TokenCounts {
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    reasoning_tokens: i64,
    estimated_cost_usd: Option<f64>,
    actual_cost_usd: Option<f64>,
    cost_status: Option<String>,
    cost_source: Option<String>,
    pricing_version: Option<String>,
    billing_provider: Option<String>,
    billing_base_url: Option<String>,
    billing_mode: Option<String>,
    model: Option<String>,
    absolute: bool,
}

#[pyfunction]
fn update_token_counts(py: Python<'_>, session_id: String, counts_json: String) -> PyResult<()> {
    let counts: TokenCounts = serde_json::from_str(&counts_json)
        .map_err(|e| PyException::new_err(format!("bad counts JSON: {}", e)))?;

    with_state(|state| {
        execute_write(state, |conn| {
            let sql = if counts.absolute {
                "UPDATE sessions SET input_tokens=?1, output_tokens=?2, cache_read_tokens=?3, cache_write_tokens=?4, reasoning_tokens=?5, estimated_cost_usd=COALESCE(?6,0), actual_cost_usd=CASE WHEN ?7 IS NULL THEN actual_cost_usd ELSE ?7 END, cost_status=COALESCE(?8,cost_status), cost_source=COALESCE(?9,cost_source), pricing_version=COALESCE(?10,pricing_version), billing_provider=COALESCE(?11,billing_provider), billing_base_url=COALESCE(?12,billing_base_url), billing_mode=COALESCE(?13,billing_mode), model=COALESCE(?14,model) WHERE id=?15"
            } else {
                "UPDATE sessions SET input_tokens=input_tokens+?1, output_tokens=output_tokens+?2, cache_read_tokens=cache_read_tokens+?3, cache_write_tokens=cache_write_tokens+?4, reasoning_tokens=reasoning_tokens+?5, estimated_cost_usd=COALESCE(estimated_cost_usd,0)+COALESCE(?6,0), actual_cost_usd=CASE WHEN ?7 IS NULL THEN actual_cost_usd ELSE COALESCE(actual_cost_usd,0)+?7 END, cost_status=COALESCE(?8,cost_status), cost_source=COALESCE(?9,cost_source), pricing_version=COALESCE(?10,pricing_version), billing_provider=COALESCE(?11,billing_provider), billing_base_url=COALESCE(?12,billing_base_url), billing_mode=COALESCE(?13,billing_mode), model=COALESCE(?14,model) WHERE id=?15"
            };

            conn.execute(
                sql,
                params![
                    counts.input_tokens,
                    counts.output_tokens,
                    counts.cache_read_tokens,
                    counts.cache_write_tokens,
                    counts.reasoning_tokens,
                    counts.estimated_cost_usd,
                    counts.actual_cost_usd,
                    counts.cost_status,
                    counts.cost_source,
                    counts.pricing_version,
                    counts.billing_provider,
                    counts.billing_base_url,
                    counts.billing_mode,
                    counts.model,
                    session_id,
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    })
}

#[pyfunction]
fn ensure_session(session_id: String, source: String, model: Option<String>) -> PyResult<()> {
    with_state(|state| {
        execute_write(state, |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO sessions (id, source, model, started_at) VALUES (?1, ?2, ?3, ?4)",
                params![session_id, source, model, now_f64()],
            )?;
            Ok(())
        })?;
        Ok(())
    })
}

// ── Message storage ─────────────────────────────────────────────────────────

#[pyfunction]
fn append_message(
    py: Python<'_>,
    session_id: String,
    role: String,
    content: Option<String>,
    tool_call_id: Option<String>,
    tool_calls: Option<String>,
    tool_name: Option<String>,
    token_count: Option<i64>,
    finish_reason: Option<String>,
    reasoning: Option<String>,
    reasoning_details: Option<String>,
    codex_reasoning_items: Option<String>,
) -> PyResult<i64> {
    let tool_calls_json = tool_calls
        .as_ref()
        .and_then(|s| json_parse(s).ok())
        .map(|v| serde_json::to_string(&v).unwrap_or_default());
    let reasoning_details_json = reasoning_details
        .as_ref()
        .and_then(|s| json_parse(s).ok())
        .map(|v| serde_json::to_string(&v).unwrap_or_default());
    let codex_items_json = codex_reasoning_items
        .as_ref()
        .and_then(|s| json_parse(s).ok())
        .map(|v| serde_json::to_string(&v).unwrap_or_default());

    with_state(|state| {
        execute_write(state, |conn| {
            let num_tool_calls: i64 = tool_calls
                .as_ref()
                .and_then(|s| json_parse(s).ok())
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() as i64)
                .unwrap_or(0);

            conn.execute(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, tool_name, timestamp, token_count, finish_reason, reasoning, reasoning_details, codex_reasoning_items) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    session_id,
                    role,
                    content,
                    tool_call_id,
                    tool_calls_json,
                    tool_name,
                    now_f64(),
                    token_count,
                    finish_reason,
                    reasoning,
                    reasoning_details_json,
                    codex_items_json,
                ],
            )?;

            if num_tool_calls > 0 {
                conn.execute(
                    "UPDATE sessions SET message_count=message_count+1, tool_call_count=tool_call_count+?1 WHERE id=?2",
                    params![num_tool_calls, session_id],
                )?;
            } else {
                conn.execute(
                    "UPDATE sessions SET message_count=message_count+1 WHERE id=?1",
                    params![session_id],
                )?;
            }
            Ok(())
        })?;

        let msg_id: i64 = state.conn.query_row(
            "SELECT last_insert_rowid()",
            [],
            |r| r.get(0),
        )?;
        Ok(msg_id)
    })
}

fn row_to_message_dict(py: Python<'_>, row: &rusqlite::Row<'_>) -> PyResult<HashMap<String, JsonValue>> {
    let mut m = HashMap::new();
    let cols = ["id","session_id","role","content","tool_call_id","tool_calls",
                "tool_name","timestamp","token_count","finish_reason",
                "reasoning","reasoning_details","codex_reasoning_items"];
    let getters: Vec<fn(&rusqlite::Row<'_>, usize) -> rusqlite::Result<_, _>> = [
        |r, i| r.get(i), |r, i| r.get(i), |r, i| r.get(i),
        |r, i| r.get(i), |r, i| r.get(i), |r, i| r.get(i),
        |r, i| r.get(i), |r, i| r.get(i), |r, i| r.get(i),
        |r, i| r.get(i), |r, i| r.get(i), |r, i| r.get(i),
        |r, i| r.get(i),
    ];

    for (i, name) in cols.iter().enumerate() {
        let val: rusqlite::types::ValueRef = row.get_ref_unwrap(i);
        let json_val = match val {
            rusqlite::types::ValueRef::Null => JsonValue::Null,
            rusqlite::types::ValueRef::Integer(i) => JsonValue::Number(i.into()),
            rusqlite::types::ValueRef::Real(f) => {
                JsonValue::Number(serde_json::Number::from_f64(f).unwrap_or(serde_json::Number::from(0)))
            }
            rusqlite::types::ValueRef::Text(t) => {
                let s = String::from_utf8_lossy(t).to_string();
                if *name == "tool_calls" || *name == "reasoning_details" || *name == "codex_reasoning_items" {
                    json_parse(&s).unwrap_or(JsonValue::String(s))
                } else {
                    JsonValue::String(s)
                }
            }
            rusqlite::types::ValueRef::Blob(b) => {
                JsonValue::String(format!("<blob {} bytes>", b.len()))
            }
        };
        m.insert((*name).to_string(), json_val);
    }
    Ok(m)
}

#[pyfunction]
fn get_messages(py: Python<'_>, session_id: String) -> PyResult<Py<PyList>> {
    with_state(|state| {
        let mut stmt = state.conn.prepare(
            "SELECT id, session_id, role, content, tool_call_id, tool_calls, tool_name, timestamp, token_count, finish_reason, reasoning, reasoning_details, codex_reasoning_items FROM messages WHERE session_id=?1 ORDER BY timestamp, id",
        )?;
        let rows_iter = stmt.query_map(params![session_id], |r| row_to_message_dict(py, r))?;
        let list = PyList::empty(py);
        for row in rows_iter {
            let dict = PyDict::new(py);
            for (k, v) in row? {
                dict.set_item(&k, json_value_to_py(py, v)?)?;
            }
            list.append(dict)?;
        }
        Ok(list.into())
    })
}

#[pyfunction]
fn get_messages_as_conversation(py: Python<'_>, session_id: String) -> PyResult<Py<PyList>> {
    with_state(|state| {
        let mut stmt = state.conn.prepare(
            "SELECT role, content, tool_call_id, tool_calls, tool_name, reasoning, reasoning_details, codex_reasoning_items FROM messages WHERE session_id=?1 ORDER BY timestamp, id",
        )?;
        let rows_iter = stmt.query_map(params![session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?;

        let list = PyList::empty(py);
        for row in rows_iter {
            let (role, content, tool_call_id, tool_calls_str, tool_name,
                 reasoning, reasoning_details_str, codex_items_str) = row.map_err(|e| rusqlite::Error::ToSqlConversionError(e.to_string()))?;

            let mut msg = serde_json::json!({
                "role": role,
                "content": content.unwrap_or_default(),
            });
            if let Some(tcid) = tool_call_id {
                if let Some(obj) = msg.as_object_mut() {
                    obj.insert("tool_call_id".into(), JsonValue::String(tcid));
                }
            }
            if let Some(tn) = tool_name {
                if let Some(obj) = msg.as_object_mut() {
                    obj.insert("tool_name".into(), JsonValue::String(tn));
                }
            }
            if let Some(tc_str) = tool_calls_str {
                if let Ok(tc) = json_parse(&tc_str) {
                    if let Some(obj) = msg.as_object_mut() {
                        obj.insert("tool_calls".into(), tc);
                    }
                }
            }
            if role == "assistant" {
                if let Some(r) = reasoning {
                    if let Some(obj) = msg.as_object_mut() {
                        obj.insert("reasoning".into(), JsonValue::String(r));
                    }
                }
                if let Some(rd_str) = reasoning_details_str {
                    if let Ok(rd) = json_parse(&rd_str) {
                        if let Some(obj) = msg.as_object_mut() {
                            obj.insert("reasoning_details".into(), rd);
                        }
                    }
                }
                if let Some(ci_str) = codex_items_str {
                    if let Ok(ci) = json_parse(&ci_str) {
                        if let Some(obj) = msg.as_object_mut() {
                            obj.insert("codex_reasoning_items".into(), ci);
                        }
                    }
                }
            }
            list.append(json_value_to_py(py, msg)?)?;
        }
        Ok(list.into())
    })
}

// ── Search ──────────────────────────────────────────────────────────────────

#[pyfunction]
fn search_messages(
    py: Python<'_>,
    query: String,
    source_filter: Option<Vec<String>>,
    exclude_sources: Option<Vec<String>>,
    role_filter: Option<Vec<String>>,
    limit: i64,
    offset: i64,
) -> PyResult<Py<PyList>> {
    let query = sanitize_fts5_query(&query);
    if query.is_empty() {
        return Ok(PyList::empty(py).into());
    }

    with_state(|state| {
        let mut where_clauses = vec![format!("messages_fts MATCH '{}'", query.replace('\'', "''"))];
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref sf) = source_filter {
            let placeholders: Vec<String> = (0..sf.len()).map(|i| format!("?{}", i + 1)).collect();
            where_clauses.push(format!("s.source IN ({})", placeholders.join(",")));
            for s in sf { params_vec.push(Box::new(s.clone())); }
        }
        if let Some(ref es) = exclude_sources {
            let start = params_vec.len() + 1;
            let placeholders: Vec<String> = (0..es.len()).map(|i| format!("?{}", start + i)).collect();
            where_clauses.push(format!("s.source NOT IN ({})", placeholders.join(",")));
            for e in es { params_vec.push(Box::new(e.clone())); }
        }
        if let Some(ref rf) = role_filter {
            let start = params_vec.len() + 1;
            let placeholders: Vec<String> = (0..rf.len()).map(|i| format!("?{}", start + i)).collect();
            where_clauses.push(format!("m.role IN ({})", placeholders.join(",")));
            for r in rf { params_vec.push(Box::new(r.clone())); }
        }

        let where_sql = where_clauses.join(" AND ");
        let sql = format!(
            "SELECT m.id, m.session_id, m.role, snippet(messages_fts, 0, '>>>', '<<<', '...', 40) AS snippet, m.content, m.timestamp, m.tool_name, s.source, s.model, s.started_at AS session_started \
             FROM messages_fts JOIN messages m ON m.id = messages_fts.rowid JOIN sessions s ON s.id = m.session_id \
             WHERE {} ORDER BY rank LIMIT {} OFFSET {}",
            where_sql, limit, offset
        );

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        let mut stmt = state.conn.prepare(&sql)?;
        let rows_iter = stmt.query_map(params_refs.as_slice(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, f64>(9)?,
            ))
        })?;

        let mut results: Vec<HashMap<String, JsonValue>> = Vec::new();
        for row in rows_iter {
            let (id, session_id, role, snippet, content, timestamp, tool_name, source, model, session_started) = row.map_err(|e| rusqlite::Error::ToSqlConversionError(e.to_string()))?;
            let mut m = HashMap::new();
            m.insert("id".into(), JsonValue::Number(id.into()));
            m.insert("session_id".into(), JsonValue::String(session_id));
            m.insert("role".into(), JsonValue::String(role));
            m.insert("snippet".into(), JsonValue::String(snippet));
            m.insert("timestamp".into(), JsonValue::Number(serde_json::Number::from_f64(timestamp).unwrap_or(serde_json::Number::from(0))));
            m.insert("tool_name".into(), tool_name.map(JsonValue::String).unwrap_or(JsonValue::Null));
            m.insert("source".into(), JsonValue::String(source));
            m.insert("model".into(), model.map(JsonValue::String).unwrap_or(JsonValue::Null));
            m.insert("session_started".into(), JsonValue::Number(serde_json::Number::from_f64(session_started).unwrap_or(serde_json::Number::from(0))));
            results.push(m);
        }

        for result in &mut results {
            let sid = result.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
            let id_val = result.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            let ctx_sql = "SELECT role, content FROM messages WHERE session_id=?1 AND id >= ?2 AND id <= ?3 ORDER BY id";
            let mut ctx_stmt = state.conn.prepare(ctx_sql)?;
            let ctx_iter = ctx_stmt.query_map(params![sid, id_val - 1, id_val + 1], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))
            })?;
            let context: Vec<JsonValue> = ctx_iter.filter_map(|r| r.ok())
                .map(|(role, content)| serde_json::json!({"role": role, "content": content}))
                .collect();
            result.insert("context".into(), JsonValue::Array(context));
        }

        let list = PyList::empty(py);
        for result in results {
            let dict = PyDict::new(py);
            for (k, v) in result {
                dict.set_item(&k, json_value_to_py(py, v)?)?;
            }
            list.append(dict)?;
        }
        Ok(list.into())
    })
}

// ── Session queries ──────────────────────────────────────────────────────────

fn row_to_session_dict(py: Python<'_>, row: &rusqlite::Row<'_>) -> PyResult<HashMap<String, JsonValue>> {
    let mut m = HashMap::new();
    for i in 0..row.column_count() {
        let name = row.column_name(i).unwrap_or("");
        let val: rusqlite::types::ValueRef = row.get_ref_unwrap(i);
        let json_val = match val {
            rusqlite::types::ValueRef::Null => JsonValue::Null,
            rusqlite::types::ValueRef::Integer(i) => JsonValue::Number(i.into()),
            rusqlite::types::ValueRef::Real(f) => {
                JsonValue::Number(serde_json::Number::from_f64(f).unwrap_or(serde_json::Number::from(0)))
            }
            rusqlite::types::ValueRef::Text(t) => {
                JsonValue::String(String::from_utf8_lossy(t).to_string())
            }
            rusqlite::types::ValueRef::Blob(b) => {
                JsonValue::String(format!("<blob {} bytes>", b.len()))
            }
        };
        m.insert(name.to_string(), json_val);
    }
    Ok(m)
}

#[pyfunction]
fn get_session(py: Python<'_>, session_id: String) -> PyResult<Option<Py<PyDict>>> {
    with_state(|state| {
        let mut stmt = state.conn.prepare("SELECT * FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(params![session_id])?;
        if let Some(row) = rows.next()? {
            let dict = PyDict::new(py);
            for (k, v) in row_to_session_dict(py, row)? {
                dict.set_item(&k, json_value_to_py(py, v)?)?;
            }
            Ok(Some(dict.into()))
        } else {
            Ok(None)
        }
    })
}

#[pyfunction]
fn list_sessions_rich(
    py: Python<'_>,
    source: Option<String>,
    exclude_sources: Option<Vec<String>>,
    limit: i64,
    offset: i64,
) -> PyResult<Py<PyList>> {
    with_state(|state| {
        let mut where_clauses = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref s) = source {
            where_clauses.push("s.source = ?");
            params_vec.push(Box::new(s.clone()));
        }
        if let Some(ref es) = exclude_sources {
            let start = params_vec.len() + 1;
            let placeholders: Vec<String> = (0..es.len()).map(|i| format!("?{}", start + i)).collect();
            where_clauses.push(format!("s.source NOT IN ({})", placeholders.join(",")));
            for e in es { params_vec.push(Box::new(e.clone())); }
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        let sql = format!(
            "SELECT s.*, COALESCE((SELECT SUBSTR(REPLACE(REPLACE(m.content, X'0A', ' '), X'0D', ' '), 1, 63) FROM messages m WHERE m.session_id=s.id AND m.role='user' AND m.content IS NOT NULL ORDER BY m.timestamp, m.id LIMIT 1), '') AS _preview_raw, COALESCE((SELECT MAX(m2.timestamp) FROM messages m2 WHERE m2.session_id=s.id), s.started_at) AS last_active FROM sessions s {} ORDER BY s.started_at DESC LIMIT ? OFFSET ?",
            where_sql
        );

        params_vec.push(Box::new(limit));
        params_vec.push(Box::new(offset));
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();

        let mut stmt = state.conn.prepare(&sql)?;
        let rows_iter = stmt.query_map(params_refs.as_slice(), |r| {
            let mut cols = row_to_session_dict(py, r)?;
            let preview_raw = cols.remove("_preview_raw")
                .and_then(|v| v.as_str())
                .map(|s| {
                    let trimmed = s.trim();
                    if trimmed.len() > 60 {
                        format!("{}...", &trimmed[..60])
                    } else {
                        trimmed.to_string()
                    }
                })
                .unwrap_or_default();
            Ok((cols, preview_raw))
        })?;

        let list = PyList::empty(py);
        for row in rows_iter {
            let (mut cols, preview) = row.map_err(|e| rusqlite::Error::ToSqlConversionError(e.to_string()))?;
            let dict = PyDict::new(py);
            for (k, v) in cols {
                dict.set_item(&k, json_value_to_py(py, v)?)?;
            }
            dict.set_item("preview", json_value_to_py(py, JsonValue::String(preview))?)?;
            list.append(dict)?;
        }
        Ok(list.into())
    })
}

#[pyfunction]
fn resolve_session_id(session_id_or_prefix: String) -> PyResult<Option<String>> {
    with_state(|state| {
        let exists: bool = state.conn.query_row(
            "SELECT COUNT(*) > 0 FROM sessions WHERE id = ?1",
            params![session_id_or_prefix],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(Some(session_id_or_prefix));
        }
        let escaped = RE_SESSION_PREFIX_ESCAPE.replace_all(&session_id_or_prefix, "\\$1");
        let mut stmt = state.conn.prepare(
            "SELECT id FROM sessions WHERE id LIKE ?1 ORDER BY started_at DESC LIMIT 2",
        )?;
        let mut rows = stmt.query(params![format!("{}%", escaped)])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    })
}

#[pyfunction]
fn set_session_title(session_id: String, title: String) -> PyResult<bool> {
    let title = sanitize_title(&title).ok_or_else(|| {
        PyException::new_err(format!(
            "Title too long ({} chars, max {})",
            title.len(),
            MAX_TITLE_LENGTH
        ))
    })?;

    with_state(|state| {
        execute_write(state, |conn| {
            if let Some(ref t) = title {
                let conflict: Option<String> = conn.query_row(
                    "SELECT id FROM sessions WHERE title = ?1 AND id != ?2",
                    params![t, session_id],
                    |r| r.get(0),
                ).ok();
                if conflict.is_some() {
                    return Err(rusqlite::Error::ToSqlConversionError(
                        format!("Title '{}' already in use", t)
                    ));
                }
            }
            let rows = conn.execute(
                "UPDATE sessions SET title = ?1 WHERE id = ?2",
                params![title, session_id],
            )?;
            Ok(rows > 0)
        })
        .map_err(|e| PyException::new_err(format!("set_session_title error: {}", e)))
    })
}

#[pyfunction]
fn get_session_title(session_id: String) -> PyResult<Option<String>> {
    with_state(|state| {
        let title: Option<String> = state.conn.query_row(
            "SELECT title FROM sessions WHERE id = ?1",
            params![session_id],
            |r| r.get(0),
        ).ok();
        Ok(title)
    })
}

#[pyfunction]
fn get_session_by_title(py: Python<'_>, title: String) -> PyResult<Option<Py<PyDict>>> {
    with_state(|state| {
        let mut stmt = state.conn.prepare("SELECT * FROM sessions WHERE title = ?1")?;
        let mut rows = stmt.query(params![title])?;
        if let Some(row) = rows.next()? {
            let dict = PyDict::new(py);
            for (k, v) in row_to_session_dict(py, row)? {
                dict.set_item(&k, json_value_to_py(py, v)?)?;
            }
            Ok(Some(dict.into()))
        } else {
            Ok(None)
        }
    })
}

#[pyfunction]
fn get_next_title_in_lineage(base_title: String) -> PyResult<String> {
    with_state(|state| {
        let base = Regex::new(r"^(.*?) #(\d+)$")
            .unwrap()
            .captures(&base_title)
            .map(|c| c.get(1).unwrap().as_str().to_string())
            .unwrap_or_else(|| base_title.clone());

        let escaped = RE_SESSION_PREFIX_ESCAPE.replace_all(&base, "\\$1");
        let mut stmt = state.conn.prepare(
            "SELECT title FROM sessions WHERE title = ?1 OR title LIKE ?2 ORDER BY title",
        )?;
        let rows_iter = stmt.query_map(params![base, format!("{} #%%", escaped)], |r| {
            r.get::<_, String>(0)
        })?;

        let existing: Vec<String> = rows_iter.filter_map(|r| r.ok()).collect();
        let mut max_num = 1;
        for t in &existing {
            if let Some(caps) = Regex::new(r"^.* #(\d+)$").unwrap().captures(t) {
                if let Ok(n) = caps.get(1).unwrap().as_str().parse::<i32>() {
                    max_num = max_num.max(n);
                }
            }
        }
        if existing.is_empty() {
            Ok(base)
        } else {
            Ok(format!("{} #{}", base, max_num + 1))
        }
    })
}

// ── Counts / utility ────────────────────────────────────────────────────────

#[pyfunction]
fn session_count(source: Option<String>) -> PyResult<i64> {
    with_state(|state| {
        let count: i64 = if let Some(s) = source {
            state.conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE source = ?1",
                params![s],
                |r| r.get(0),
            )?
        } else {
            state.conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?
        };
        Ok(count)
    })
}

#[pyfunction]
fn message_count(session_id: Option<String>) -> PyResult<i64> {
    with_state(|state| {
        let count: i64 = if let Some(sid) = session_id {
            state.conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![sid],
                |r| r.get(0),
            )?
        } else {
            state.conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?
        };
        Ok(count)
    })
}

// ── Delete / prune ───────────────────────────────────────────────────────────

#[pyfunction]
fn delete_session(session_id: String) -> PyResult<bool> {
    with_state(|state| {
        execute_write(state, |conn| {
            let n: i64 = conn.execute(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![session_id],
            )?;
            if n == 0 {
                return Ok(false);
            }
            conn.execute("DELETE FROM messages WHERE session_id = ?1", params![session_id])?;
            conn.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
            Ok(true)
        })
        .map_err(|e| PyException::new_err(e.to_string()))
    })
}

#[pyfunction]
fn prune_sessions(older_than_days: i64, source: Option<String>) -> PyResult<i64> {
    with_state(|state| {
        execute_write(state, |conn| {
            let cutoff = now_f64() - (older_than_days as f64 * 86400.0);

            let sids: Vec<String> = if let Some(ref s) = source {
                let mut stmt = conn.prepare(
                    "SELECT id FROM sessions WHERE started_at < ?1 AND ended_at IS NOT NULL AND source = ?2",
                )?;
                stmt.query_map(params![cutoff, s], |r| r.get(0))?
                    .filter_map(|r| r.ok())
                    .collect()
            } else {
                let mut stmt = conn.prepare(
                    "SELECT id FROM sessions WHERE started_at < ?1 AND ended_at IS NOT NULL",
                )?;
                stmt.query_map(params![cutoff], |r| r.get(0))?
                    .filter_map(|r| r.ok())
                    .collect()
            };

            for sid in &sids {
                conn.execute("DELETE FROM messages WHERE session_id = ?1", params![sid])?;
                conn.execute("DELETE FROM sessions WHERE id = ?1", params![sid])?;
            }
            Ok(sids.len() as i64)
        })
        .map_err(|e| PyException::new_err(e.to_string()))
    })
}

// ── Module definition ────────────────────────────────────────────────────────

#[pymodule]
fn _hermes_state_rust(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(init, module)?)?;
    module.add_function(wrap_pyfunction!(is_initialized, module)?)?;
    module.add_function(wrap_pyfunction!(create_session, module)?)?;
    module.add_function(wrap_pyfunction!(end_session, module)?)?;
    module.add_function(wrap_pyfunction!(update_system_prompt, module)?)?;
    module.add_function(wrap_pyfunction!(update_token_counts, module)?)?;
    module.add_function(wrap_pyfunction!(ensure_session, module)?)?;
    module.add_function(wrap_pyfunction!(append_message, module)?)?;
    module.add_function(wrap_pyfunction!(get_messages, module)?)?;
    module.add_function(wrap_pyfunction!(get_messages_as_conversation, module)?)?;
    module.add_function(wrap_pyfunction!(search_messages, module)?)?;
    module.add_function(wrap_pyfunction!(get_session, module)?)?;
    module.add_function(wrap_pyfunction!(list_sessions_rich, module)?)?;
    module.add_function(wrap_pyfunction!(resolve_session_id, module)?)?;
    module.add_function(wrap_pyfunction!(set_session_title, module)?)?;
    module.add_function(wrap_pyfunction!(get_session_title, module)?)?;
    module.add_function(wrap_pyfunction!(get_session_by_title, module)?)?;
    module.add_function(wrap_pyfunction!(get_next_title_in_lineage, module)?)?;
    module.add_function(wrap_pyfunction!(session_count, module)?)?;
    module.add_function(wrap_pyfunction!(message_count, module)?)?;
    module.add_function(wrap_pyfunction!(delete_session, module)?)?;
    module.add_function(wrap_pyfunction!(prune_sessions, module)?)?;
    module.add("__doc__", "Rust-native SessionDB for Hermes — rusqlite backend with FTS5.")?;
    Ok(())
}
