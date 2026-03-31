//! Hermes State — Rust-native SQLite SessionDB with FTS5

use pyo3::exceptions::{PyException, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use rand::Rng;
use regex::Regex;
use rusqlite::{types::FromSql, Connection, OpenFlags};
use serde_json::Value as JsonValue;
use std::sync::Mutex;

const SCHEMA_VERSION: i32 = 6;
const WRITE_MAX_RETRIES: u32 = 15;
const WRITE_RETRY_MIN_MS: f64 = 20.0;
const WRITE_RETRY_MAX_MS: f64 = 150.0;

static STATE: Mutex<Option<RustState>> = Mutex::new(None);

struct RustState {
    conn: rusqlite::Connection,
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

fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

// ── JSON helpers ─────────────────────────────────────────────────────────────

fn val_to_py(py: Python<'_>, v: &JsonValue) -> Py<PyAny> {
    use pyo3::IntoPyObjectExt;
    match v {
        JsonValue::Null => py.None(),
        JsonValue::Bool(b) => (*b).into_py_any(py).unwrap(),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_py_any(py).unwrap()
            } else if let Some(f) = n.as_f64() {
                f.into_py_any(py).unwrap()
            } else {
                0.into_py_any(py).unwrap()
            }
        }
        JsonValue::String(s) => s.clone().into_pyobject(py).unwrap().into_any().into(),
        JsonValue::Array(arr) => arr
            .iter()
            .map(|x| val_to_py(py, x))
            .collect::<Vec<_>>()
            .into_pyobject(py)
            .unwrap()
            .into_any()
            .into(),
        JsonValue::Object(obj) => {
            let dict = PyDict::new(py);
            for (k, val) in obj {
                dict.set_item(k.as_str(), val_to_py(py, val)).unwrap();
            }
            dict.into_any().into()
        }
    }
}

fn json_parse(s: &str) -> Result<JsonValue, serde_json::Error> {
    serde_json::from_str(s)
}

// ── SQL helpers ─────────────────────────────────────────────────────────────

fn init_schema(conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
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
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?)",
            rusqlite::params![SCHEMA_VERSION],
        )?;
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
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(content, content=messages, content_rowid=id);
            CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content); END;
            CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content); END;
            CREATE TRIGGER IF NOT EXISTS messages_fts_update AFTER UPDATE ON messages BEGIN INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content); INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content); END;
            ",
        )?;
    }

    Ok(())
}

fn run_migrations(conn: &rusqlite::Connection, mut current_version: i32) -> Result<(), rusqlite::Error> {
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

fn execute_write<F, T>(state: &RustState, f: F) -> Result<T, String>
where
    F: FnOnce(&rusqlite::Connection) -> Result<T, String>,
{
    use rand::Rng;
    let mut rng = rand::thread_rng();

    // Try to acquire the write lock with retries
    for attempt in 0..WRITE_MAX_RETRIES {
        match state.conn.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => break,
            Err(e) => {
                if attempt < WRITE_MAX_RETRIES - 1 {
                    std::thread::sleep(std::time::Duration::from_millis(
                        rng.gen_range(WRITE_RETRY_MIN_MS..WRITE_RETRY_MAX_MS) as u64,
                    ));
                    continue;
                }
                return Err(format!("BEGIN IMMEDIATE: {}", e));
            }
        }
    }

    // Execute the write function (only called once)
    match f(&state.conn) {
        Ok(result) => {
            if let Err(e) = state.conn.execute_batch("COMMIT") {
                let _ = state.conn.execute_batch("ROLLBACK");
                return Err(format!("COMMIT: {}", e));
            }
            Ok(result)
        }
        Err(e) => {
            let _ = state.conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ── Sanitization ────────────────────────────────────────────────────────────

lazy_static::lazy_static! {
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
            &RE_TITLE_UNICODE_CTRL
                .replace_all(&RE_TITLE_ASCII_CTRL.replace_all(title, ""), ""),
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
    let mut quoted: Vec<String> = Vec::new();
    let step1 = RE_FTS_QUOTED.replace_all(query, |caps: &regex::Captures| {
        quoted.push(caps[0].to_string());
        format!("\x00Q{}\x00", quoted.len() - 1)
    });
    let step2 = RE_FTS_SPECIAL.replace_all(&step1, " ");
    let step3 = {
        let s = RE_FTS_STARS.replace_all(&step2, "*");
        let s = RE_FTS_LEADING_STAR.replace_all(&s, "");
        RE_FTS_BOOLEAN_EDGE.replace_all(&s, "").to_string()
    };
    let step4 = RE_FTS_HYPHENATED
        .replace_all(&step3, |caps: &regex::Captures| {
            format!("\"{}\"", &caps[1])
        })
        .to_string();
    let mut result = step4;
    for (i, q) in quoted.iter().enumerate() {
        result = result.replace(&format!("\x00Q{}\x00", i), q);
    }
    result.trim().to_string()
}

// ── PyO3 module functions ───────────────────────────────────────────────────

#[pyfunction]
fn init(db_path: String) -> PyResult<()> {
    let mut guard = STATE
        .lock()
        .map_err(|e| PyException::new_err(e.to_string()))?;
    if guard.is_some() {
        return Ok(());
    }
    let mut state = RustState::new(&db_path)
        .map_err(|e| PyException::new_err(format!("open DB: {}", e)))?;
    init_schema(&state.conn)
        .map_err(|e| PyException::new_err(format!("schema: {}", e)))?;
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
    let guard = STATE
        .lock()
        .map_err(|e| PyException::new_err(e.to_string()))?;
    let state = guard.as_ref().ok_or_else(|| {
        PyRuntimeError::new_err("not initialized — call init() first")
    })?;
    f(state)
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
                rusqlite::params![
                    session_id, source, user_id, model, model_config_json,
                    system_prompt, parent_session_id, now_f64(),
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .map_err(|e| PyException::new_err(e))?;
        Ok(session_id)
    })
}

#[pyfunction]
fn end_session(session_id: String, end_reason: String) -> PyResult<()> {
    with_state(|state| {
        execute_write(state, |conn| {
            conn.execute(
                "UPDATE sessions SET ended_at = ?1, end_reason = ?2 WHERE id = ?3",
                rusqlite::params![now_f64(), end_reason, session_id],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .map_err(|e| PyException::new_err(e))
    })
}

#[pyfunction]
fn update_system_prompt(session_id: String, system_prompt: String) -> PyResult<()> {
    with_state(|state| {
        execute_write(state, |conn| {
            conn.execute(
                "UPDATE sessions SET system_prompt = ?1 WHERE id = ?2",
                rusqlite::params![system_prompt, session_id],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .map_err(|e| PyException::new_err(e))
    })
}

#[derive(serde::Deserialize)]
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
fn update_token_counts(_py: Python<'_>, session_id: String, counts_json: String) -> PyResult<()> {
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
                rusqlite::params![
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
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .map_err(|e| PyException::new_err(e))
    })
}

#[pyfunction]
fn ensure_session(session_id: String, source: String, model: Option<String>) -> PyResult<()> {
    with_state(|state| {
        execute_write(state, |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO sessions (id, source, model, started_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![session_id, source, model, now_f64()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .map_err(|e| PyException::new_err(e))
    })
}

// ── Message storage ─────────────────────────────────────────────────────────

#[pyfunction]
fn append_message(
    _py: Python<'_>,
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
    with_state(|state| {
        let num_tool_calls: i64 = tool_calls
            .as_ref()
            .and_then(|s| json_parse(s).ok())
            .and_then(|v| v.as_array().map(|a| a.len() as i64))
            .unwrap_or(0);

        let msg_id = execute_write(state, |conn| {
            conn.execute(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, tool_name, timestamp, token_count, finish_reason, reasoning, reasoning_details, codex_reasoning_items) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    session_id, role, content, tool_call_id, tool_calls,
                    tool_name, now_f64(), token_count, finish_reason,
                    reasoning, reasoning_details, codex_reasoning_items,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(conn.last_insert_rowid())
        })
        .map_err(|e| PyException::new_err(e))?;

        if num_tool_calls > 0 {
            let _ = state.conn.execute(
                "UPDATE sessions SET message_count=message_count+1, tool_call_count=tool_call_count+?1 WHERE id=?2",
                rusqlite::params![num_tool_calls, session_id],
            );
        } else {
            let _ = state.conn.execute(
                "UPDATE sessions SET message_count=message_count+1 WHERE id=?1",
                rusqlite::params![session_id],
            );
        }

        Ok(msg_id)
    })
}

const MSG_COLS: &[&str] = &[
    "id",
    "session_id",
    "role",
    "content",
    "tool_call_id",
    "tool_calls",
    "tool_name",
    "timestamp",
    "token_count",
    "finish_reason",
    "reasoning",
    "reasoning_details",
    "codex_reasoning_items",
];

#[pyfunction]
fn get_messages(py: Python<'_>, session_id: String) -> PyResult<Py<PyAny>> {
    with_state(|state| {
        let mut stmt = state.conn.prepare(
            "SELECT id, session_id, role, content, tool_call_id, tool_calls, tool_name, timestamp, token_count, finish_reason, reasoning, reasoning_details, codex_reasoning_items FROM messages WHERE session_id=?1 ORDER BY timestamp, id",
        ).map_err(|e| PyException::new_err(e.to_string()))?;
        let rows_iter = stmt.query_map(rusqlite::params![session_id], |row| {
            let mut vals: Vec<JsonValue> = Vec::with_capacity(MSG_COLS.len());
            for i in 0..MSG_COLS.len() {
                let rv = row.get_ref_unwrap(i);
                let json_val = match rv {
                    rusqlite::types::ValueRef::Text(t) => {
                        let s = String::from_utf8_lossy(t);
                        if i == 5 || i == 11 || i == 12 {
                            json_parse(&s).unwrap_or(JsonValue::String(s.to_string()))
                        } else {
                            JsonValue::String(s.to_string())
                        }
                    }
                    rusqlite::types::ValueRef::Null => JsonValue::Null,
                    rusqlite::types::ValueRef::Integer(i) => JsonValue::Number(i.into()),
                    rusqlite::types::ValueRef::Real(f) => {
                        serde_json::Number::from_f64(f)
                            .map(JsonValue::Number)
                            .unwrap_or(JsonValue::Null)
                    }
                    rusqlite::types::ValueRef::Blob(b) => {
                        JsonValue::String(format!("<blob {} bytes>", b.len()))
                    }
                };
                vals.push(json_val);
            }
            Ok(vals)
        }).map_err(|e| PyException::new_err(e.to_string()))?;

        let list = PyList::empty(py);
        for row in rows_iter {
            let vals = row.map_err(|e| PyException::new_err(e.to_string()))?;
            let dict = PyDict::new(py);
            for (i, name) in MSG_COLS.iter().enumerate() {
                dict.set_item(name, val_to_py(py, &vals[i]))?;
            }
            list.append(dict)?;
        }
        Ok(list.into_any().unbind())
    })
}

#[pyfunction]
fn get_messages_as_conversation(
    py: Python<'_>,
    session_id: String,
) -> PyResult<Py<PyAny>> {
    with_state(|state| {
        let mut stmt = state.conn.prepare(
            "SELECT role, content, tool_call_id, tool_calls, tool_name, reasoning, reasoning_details, codex_reasoning_items FROM messages WHERE session_id=?1 ORDER BY timestamp, id",
        ).map_err(|e| PyException::new_err(e.to_string()))?;
        let rows_iter = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        }).map_err(|e| PyException::new_err(e.to_string()))?;

        let list = PyList::empty(py);
        for row in rows_iter {
            let (
                role,
                content,
                tool_call_id,
                tool_calls_str,
                tool_name,
                reasoning,
                reasoning_details_str,
                codex_items_str,
            ) = row.map_err(|e| PyException::new_err(e.to_string()))?;

            let dict = PyDict::new(py);
            dict.set_item("role", &role)?;
            dict.set_item("content", content.unwrap_or_default())?;
            if let Some(tcid) = tool_call_id {
                dict.set_item("tool_call_id", &tcid)?;
            }
            if let Some(tn) = tool_name {
                dict.set_item("tool_name", &tn)?;
            }
            if let Some(ref tc_str) = tool_calls_str {
                if let Ok(tc) = json_parse(tc_str) {
                    dict.set_item("tool_calls", val_to_py(py, &tc))?;
                }
            }
            if role == "assistant" {
                if let Some(r) = reasoning {
                    dict.set_item("reasoning", &r)?;
                }
                if let Some(ref rd_str) = reasoning_details_str {
                    if let Ok(rd) = json_parse(rd_str) {
                        dict.set_item("reasoning_details", val_to_py(py, &rd))?;
                    }
                }
                if let Some(ref ci_str) = codex_items_str {
                    if let Ok(ci) = json_parse(ci_str) {
                        dict.set_item("codex_reasoning_items", val_to_py(py, &ci))?;
                    }
                }
            }
            list.append(dict)?;
        }
        Ok(list.into_any().unbind())
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
) -> PyResult<Py<PyAny>> {
    let query = sanitize_fts5_query(&query);
    if query.is_empty() {
        return Ok(PyList::empty(py).into_any().unbind());
    }

    with_state(|state| {
        let escaped = query.replace('\'', "''");
        let mut clauses = vec![format!("messages_fts MATCH '{}'", escaped)];
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref sf) = source_filter {
            let ph: Vec<String> = (0..sf.len()).map(|i| format!("?{}", i + 1)).collect();
            clauses.push(format!("s.source IN ({})", ph.join(",")));
            for s in sf {
                params.push(Box::new(s.clone()));
            }
        }
        if let Some(ref es) = exclude_sources {
            let start = params.len() + 1;
            let ph: Vec<String> = (0..es.len()).map(|i| format!("?{}", start + i)).collect();
            clauses.push(format!("s.source NOT IN ({})", ph.join(", ")));
            for e in es {
                params.push(Box::new(e.clone()));
            }
        }
        if let Some(ref rf) = role_filter {
            let start = params.len() + 1;
            let ph: Vec<String> = (0..rf.len()).map(|i| format!("?{}", start + i)).collect();
            clauses.push(format!("m.role IN ({})", ph.join(",")));
            for r in rf {
                params.push(Box::new(r.clone()));
            }
        }

        let where_sql = clauses.join(" AND ");
        let sql = format!(
            "SELECT m.id, m.session_id, m.role, snippet(messages_fts, 0, '>>>', '<<<', '...', 40) AS snippet, m.timestamp, m.tool_name, s.source, s.model, s.started_at AS session_started FROM messages_fts JOIN messages m ON m.id = messages_fts.rowid JOIN sessions s ON s.id = m.session_id WHERE {} ORDER BY rank LIMIT {} OFFSET {}",
            where_sql, limit, offset
        );

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let mut stmt = state.conn.prepare(&sql).map_err(|e| PyException::new_err(e.to_string()))?;
        let rows_iter = stmt.query_map(params_refs.as_slice(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, f64>(8)?,
            ))
        }).map_err(|e| PyException::new_err(e.to_string()))?;

        let mut results: Vec<JsonValue> = Vec::new();
        for row in rows_iter {
            let (
                id,
                session_id,
                role,
                snippet,
                timestamp,
                tool_name,
                source,
                model,
                session_started,
            ) = row.map_err(|e| PyException::new_err(e.to_string()))?;
            results.push(serde_json::json!({
                "id": id,
                "session_id": session_id,
                "role": role,
                "snippet": snippet,
                "timestamp": timestamp,
                "tool_name": tool_name,
                "source": source,
                "model": model,
                "session_started": session_started,
            }));
        }

        for result in &mut results {
            if let Some(obj) = result.as_object_mut() {
                let sid = obj.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
                let id_val = obj.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                let mut ctx_stmt = state.conn.prepare(
                    "SELECT role, content FROM messages WHERE session_id=?1 AND id >= ?2 AND id <= ?3 ORDER BY id",
                ).map_err(|e| PyException::new_err(e.to_string()))?;
                let ctx_iter =
                    ctx_stmt.query_map(rusqlite::params![sid, id_val - 1, id_val + 1], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        ))
                    }).map_err(|e| PyException::new_err(e.to_string()))?;
                let context: Vec<JsonValue> = ctx_iter
                    .filter_map(|r| r.ok())
                    .map(|(role, content)| serde_json::json!({"role": role, "content": content }))
                    .collect();
                obj.insert("context".into(), JsonValue::Array(context));
            }
        }

        let list = PyList::empty(py);
        for result in results {
            list.append(val_to_py(py, &result))?;
        }
        Ok(list.into_any().unbind())
    })
}

// ── Session queries ──────────────────────────────────────────────────────────

#[pyfunction]
fn get_session(py: Python<'_>, session_id: String) -> PyResult<Py<PyAny>> {
    with_state(|state| {
        let mut stmt = state.conn.prepare("SELECT * FROM sessions WHERE id = ?1")
            .map_err(|e| PyException::new_err(e.to_string()))?;
        let col_count = stmt.column_count();
        let col_names: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap_or("").to_string())
            .collect();
        let mut rows = stmt.query(rusqlite::params![session_id])
            .map_err(|e| PyException::new_err(e.to_string()))?;
        if let Some(row) = rows.next().map_err(|e| PyException::new_err(e.to_string()))? {
            let dict = PyDict::new(py);
            for i in 0..col_count {
                let name = &col_names[i];
                let rv = row.get_ref_unwrap(i);
                let v = match rv {
                    rusqlite::types::ValueRef::Null => JsonValue::Null,
                    rusqlite::types::ValueRef::Integer(i) => JsonValue::Number(i.into()),
                    rusqlite::types::ValueRef::Real(f) => {
                        serde_json::Number::from_f64(f)
                            .map(JsonValue::Number)
                            .unwrap_or(JsonValue::Null)
                    }
                    rusqlite::types::ValueRef::Text(t) => {
                        JsonValue::String(String::from_utf8_lossy(t).to_string())
                    }
                    rusqlite::types::ValueRef::Blob(b) => {
                        JsonValue::String(format!("<blob {} bytes>", b.len()))
                    }
                };
                dict.set_item(name.as_str(), val_to_py(py, &v))?;
            }
            Ok(dict.into_any().unbind())
        } else {
            Ok(py.None())
        }
    })
}

const SESSION_COLS: &[&str] = &[
    "id", "source", "user_id", "model", "model_config", "system_prompt",
    "parent_session_id", "started_at", "ended_at", "end_reason",
    "message_count", "tool_call_count", "input_tokens", "output_tokens",
    "cache_read_tokens", "cache_write_tokens", "reasoning_tokens",
    "billing_provider", "billing_base_url", "billing_mode",
    "estimated_cost_usd", "actual_cost_usd", "cost_status", "cost_source",
    "pricing_version", "title", "last_active",
];

#[pyfunction]
fn list_sessions_rich(
    py: Python<'_>,
    source: Option<String>,
    exclude_sources: Option<Vec<String>>,
    limit: i64,
    offset: i64,
) -> PyResult<Py<PyAny>> {
    with_state(|state| {
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref s) = source {
            clauses.push("s.source = ?".to_string());
            params.push(Box::new(s.clone()));
        }
        if let Some(ref es) = exclude_sources {
            let start = params.len() + 1;
            let ph: Vec<String> =
                (0..es.len()).map(|i| format!("?{}", start + i)).collect();
            clauses.push(format!("s.source NOT IN ({})", ph.join(", ")));
            for e in es {
                params.push(Box::new(e.clone()));
            }
        }

        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };

        let sql = format!(
            "SELECT s.*, COALESCE((SELECT SUBSTR(REPLACE(REPLACE(m.content, X'0A', ' '), X'0D', ' '), 1, 63) FROM messages m WHERE m.session_id=s.id AND m.role='user' AND m.content IS NOT NULL ORDER BY m.timestamp, m.id LIMIT 1), '') AS _preview_raw, COALESCE((SELECT MAX(m2.timestamp) FROM messages m2 WHERE m2.session_id=s.id), s.started_at) AS last_active FROM sessions s {} ORDER BY s.started_at DESC LIMIT ? OFFSET ?",
            where_sql
        );

        params.push(Box::new(limit));
        params.push(Box::new(offset));
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();

        let mut stmt = state.conn.prepare(&sql).map_err(|e| PyException::new_err(e.to_string()))?;
        let col_count = stmt.column_count();
        let rows_iter = stmt.query_map(params_refs.as_slice(), |row| {
            let mut vals: Vec<JsonValue> = Vec::new();
            for i in 0..col_count {
                let rv = row.get_ref_unwrap(i);
                vals.push(match rv {
                    rusqlite::types::ValueRef::Null => JsonValue::Null,
                    rusqlite::types::ValueRef::Integer(i) => JsonValue::Number(i.into()),
                    rusqlite::types::ValueRef::Real(f) => {
                        serde_json::Number::from_f64(f)
                            .map(JsonValue::Number)
                            .unwrap_or(JsonValue::Null)
                    }
                    rusqlite::types::ValueRef::Text(t) => {
                        JsonValue::String(String::from_utf8_lossy(t).to_string())
                    }
                    rusqlite::types::ValueRef::Blob(b) => {
                        JsonValue::String(format!("<blob {} bytes>", b.len()))
                    }
                });
            }
            let preview_raw = vals
                .last()
                .and_then(|v| v.as_str())
                .map(|s| {
                    let t = s.trim();
                    if t.len() > 60 {
                        format!("{}...", &t[..60])
                    } else {
                        t.to_string()
                    }
                })
                .unwrap_or_default();
            Ok((vals, preview_raw))
        }).map_err(|e| PyException::new_err(e.to_string()))?;

        let list = PyList::empty(py);
        for row in rows_iter {
            let (vals, preview) = row.map_err(|e| PyException::new_err(e.to_string()))?;
            let dict = PyDict::new(py);
            for (i, v) in vals.iter().enumerate() {
                if i < SESSION_COLS.len() {
                    dict.set_item(SESSION_COLS[i], val_to_py(py, v))?;
                }
            }
            dict.set_item("preview", &preview)?;
            list.append(dict)?;
        }
        Ok(list.into_any().unbind())
    })
}

#[pyfunction]
fn resolve_session_id(py: Python<'_>, session_id_or_prefix: String) -> PyResult<Py<PyAny>> {
    with_state(|state| {
        let exists: bool = state
            .conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sessions WHERE id = ?1",
                rusqlite::params![session_id_or_prefix],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if exists {
            return Ok(PyString::new(py, &session_id_or_prefix).into_any().unbind());
        }
        let escaped =
            RE_SESSION_PREFIX_ESCAPE.replace_all(&session_id_or_prefix, "\\$1");
        let mut stmt = state.conn.prepare(
            "SELECT id FROM sessions WHERE id LIKE ?1 ORDER BY started_at DESC LIMIT 2",
        ).map_err(|e| PyException::new_err(e.to_string()))?;
        let mut rows = stmt
            .query(rusqlite::params![format!("{}%", escaped)])
            .map_err(|e| PyException::new_err(e.to_string()))?;
        if let Some(row) = rows.next().map_err(|e| PyException::new_err(e.to_string()))? {
            let id: String = row.get(0)
                .map_err(|e| PyException::new_err(e.to_string()))?;
            Ok(PyString::new(py, &id).into_any().unbind())
        } else {
            Ok(py.None())
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
            if let Some(ref t) = Some(&title) {
                let conflict: Option<String> = conn
                    .query_row(
                        "SELECT id FROM sessions WHERE title = ?1 AND id != ?2",
                        rusqlite::params![t, session_id],
                        |r| r.get(0),
                    )
                    .ok();
                if conflict.is_some() {
                    return Err(format!("Title '{}' already in use", t));
                }
            }
            let rows = conn
                .execute(
                    "UPDATE sessions SET title = ?1 WHERE id = ?2",
                    rusqlite::params![title, session_id],
                )
                .map_err(|e| e.to_string())?;
            Ok(rows > 0)
        })
        .map_err(|e| PyException::new_err(e))
    })
}

#[pyfunction]
fn get_session_title(py: Python<'_>, session_id: String) -> PyResult<Py<PyAny>> {
    with_state(|state| {
        let title: Option<String> = state
            .conn
            .query_row(
                "SELECT title FROM sessions WHERE id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .ok();
        match title {
            Some(t) => Ok(PyString::new(py, &t).into_any().unbind()),
            None => Ok(py.None()),
        }
    })
}

#[pyfunction]
fn get_session_by_title(py: Python<'_>, title: String) -> PyResult<Py<PyAny>> {
    with_state(|state| {
        let mut stmt = state.conn.prepare("SELECT * FROM sessions WHERE title = ?1")
            .map_err(|e| PyException::new_err(e.to_string()))?;
        let col_count = stmt.column_count();
        let col_names: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap_or("").to_string())
            .collect();
        let mut rows = stmt.query(rusqlite::params![title])
            .map_err(|e| PyException::new_err(e.to_string()))?;
        if let Some(row) = rows.next().map_err(|e| PyException::new_err(e.to_string()))? {
            let dict = PyDict::new(py);
            for i in 0..col_count {
                let name = &col_names[i];
                let rv = row.get_ref_unwrap(i);
                let v = match rv {
                    rusqlite::types::ValueRef::Null => JsonValue::Null,
                    rusqlite::types::ValueRef::Integer(i) => JsonValue::Number(i.into()),
                    rusqlite::types::ValueRef::Real(f) => {
                        serde_json::Number::from_f64(f)
                            .map(JsonValue::Number)
                            .unwrap_or(JsonValue::Null)
                    }
                    rusqlite::types::ValueRef::Text(t) => {
                        JsonValue::String(String::from_utf8_lossy(t).to_string())
                    }
                    rusqlite::types::ValueRef::Blob(b) => {
                        JsonValue::String(format!("<blob {} bytes>", b.len()))
                    }
                };
                dict.set_item(name.as_str(), val_to_py(py, &v))?;
            }
            Ok(dict.into_any().unbind())
        } else {
            Ok(py.None())
        }
    })
}

#[pyfunction]
fn get_next_title_in_lineage(
    py: Python<'_>,
    session_id: String,
    base_title: String,
) -> PyResult<Py<PyAny>> {
    with_state(|state| {
        let base = Regex::new(r"^(.*?) #(\d+)$")
            .unwrap()
            .captures(&base_title)
            .map(|c| c.get(1).unwrap().as_str().to_string())
            .unwrap_or_else(|| base_title.clone());

        let escaped = RE_SESSION_PREFIX_ESCAPE.replace_all(&base, "\\$1");
        let mut stmt = state.conn.prepare(
            "SELECT title FROM sessions WHERE (title = ?1 OR title LIKE ?2) AND id != ?3 ORDER BY title",
        ).map_err(|e| PyException::new_err(e.to_string()))?;
        let rows_iter = stmt.query_map(
            rusqlite::params![base, format!("{} #%%", escaped), session_id],
            |r| r.get::<_, String>(0),
        ).map_err(|e| PyException::new_err(e.to_string()))?;

        let existing: Vec<String> = rows_iter.filter_map(|r| r.ok()).collect();
        let mut max_num = 1;
        for t in &existing {
            if let Some(caps) = Regex::new(r"^.* #(\d+)$").unwrap().captures(t) {
                if let Ok(n) = caps.get(1).unwrap().as_str().parse::<i32>() {
                    max_num = max_num.max(n);
                }
            }
        }
        let result = if existing.is_empty() {
            base.clone()
        } else {
            format!("{} #{}", base, max_num + 1)
        };
        Ok(PyString::new(py, &result).into_any().unbind())
    })
}

// ── Counts / utility ────────────────────────────────────────────────────────

#[pyfunction]
fn session_count(source: Option<String>) -> PyResult<i64> {
    with_state(|state| {
        let count: i64 = if let Some(s) = source {
            state.conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE source = ?1",
                rusqlite::params![s],
                |r| r.get::<_, i64>(0),
            ).map_err(|e| PyException::new_err(e.to_string()))?
        } else {
            state
                .conn
                .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
                .map_err(|e| PyException::new_err(e.to_string()))?
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
                rusqlite::params![sid],
                |r| r.get::<_, i64>(0),
            ).map_err(|e| PyException::new_err(e.to_string()))?
        } else {
            state
                .conn
                .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .map_err(|e| PyException::new_err(e.to_string()))?
        };
        Ok(count)
    })
}

// ── Delete / prune ───────────────────────────────────────────────────────────

#[pyfunction]
fn delete_session(session_id: String) -> PyResult<bool> {
    with_state(|state| {
        execute_write(state, |conn| {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                    rusqlite::params![session_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok(false);
            }
            conn.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                rusqlite::params![session_id],
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "DELETE FROM sessions WHERE id = ?1",
                rusqlite::params![session_id],
            )
            .map_err(|e| e.to_string())?;
            Ok(true)
        })
        .map_err(|e| PyException::new_err(e))
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
                ).map_err(|e| e.to_string())?;
                let iter = stmt.query_map(rusqlite::params![cutoff, s], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?;
                iter.filter_map(|r| r.ok()).collect()
            } else {
                let mut stmt = conn.prepare(
                    "SELECT id FROM sessions WHERE started_at < ?1 AND ended_at IS NOT NULL",
                ).map_err(|e| e.to_string())?;
                let iter = stmt.query_map(rusqlite::params![cutoff], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?;
                iter.filter_map(|r| r.ok()).collect()
            };

            for sid in &sids {
                conn.execute(
                    "DELETE FROM messages WHERE session_id = ?1",
                    rusqlite::params![sid],
                )
                .map_err(|e| e.to_string())?;
                conn.execute(
                    "DELETE FROM sessions WHERE id = ?1",
                    rusqlite::params![sid],
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(sids.len() as i64)
        })
        .map_err(|e| PyException::new_err(e))
    })
}

// ── Module definition ────────────────────────────────────────────────────────

#[pymodule]
fn _hermes_state_rs(_py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
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
    module.add(
        "__doc__",
        "Rust-native SessionDB for Hermes — rusqlite backend with FTS5.",
    )?;
    Ok(())
}
