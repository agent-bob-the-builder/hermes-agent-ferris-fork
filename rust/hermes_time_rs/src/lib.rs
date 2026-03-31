//! Rust-native timezone-aware clock for Hermes.
//!
//! Resolution order:
//!   1. `HERMES_TIMEZONE` environment variable
//!   2. `timezone` key in `~/.hermes/config.yaml`
//!   3. Falls back to the server's local time
//!
//! Caches the resolved timezone in a static.

use chrono::Utc;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use pyo3::prelude::*;
use std::path::PathBuf;
use std::sync::Mutex;

/// Path to the Hermes home directory (mirrors hermes_constants.get_hermes_home)
fn hermes_home() -> PathBuf {
    std::env::var("HERMES_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".hermes")
        })
}

/// Cached timezone name (the raw string, e.g. "Asia/Kolkata")
static CACHED_TZ_NAME: Lazy<RwLock<Option<String>>> = Lazy::new(|| RwLock::new(None));

/// Cached resolved chrono-tz timezone
static CACHED_TZ: Lazy<RwLock<Option<chrono_tz::Tz>>> = Lazy::new(|| RwLock::new(None));

/// Whether the cache has been resolved
static CACHE_RESOLVED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

/// Read the configured IANA timezone string (or empty string).
/// This does file I/O when falling through to config.yaml.
fn resolve_timezone_name() -> String {
    // 1. Environment variable (highest priority)
    if let Ok(tz_env) = std::env::var("HERMES_TIMEZONE") {
        let tz = tz_env.trim();
        if !tz.is_empty() {
            return tz.to_string();
        }
    }

    // 2. config.yaml "timezone" key
    let hermes_path = hermes_home();
    let config_path = hermes_path.join("config.yaml");
    if config_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(cfg) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                if let Some(tz) = cfg.get("timezone") {
                    if let Some(tz_str) = tz.as_str() {
                        let trimmed = tz_str.trim();
                        if !trimmed.is_empty() {
                            return trimmed.to_string();
                        }
                    }
                }
            }
        }
    }

    String::new()
}

/// Validate and return a chrono-tz Tz, or None if invalid.
fn get_zoneinfo(name: &str) -> Option<chrono_tz::Tz> {
    if name.is_empty() {
        return None;
    }
    name.parse::<chrono_tz::Tz>().ok()
}

/// Ensure the cache is resolved (idempotent)
fn ensure_cache_resolved() {
    let resolved = CACHE_RESOLVED.lock().unwrap();
    if *resolved {
        return;
    }
    drop(resolved);

    let tz_name = resolve_timezone_name();
    let tz = get_zoneinfo(&tz_name);

    *CACHED_TZ_NAME.write() = Some(tz_name);
    *CACHED_TZ.write() = tz;
    *CACHE_RESOLVED.lock().unwrap() = true;
}

/// now() -> String
///
/// Return the current time as an ISO8601 string with timezone.
#[pyfunction]
pub fn now() -> String {
    ensure_cache_resolved();

    let tz_guard = CACHED_TZ.read();
    if let Some(tz) = *tz_guard {
        Utc::now().with_timezone(&tz).format("%Y-%m-%dT%H:%M:%S%.f%:z").to_string()
    } else {
        // No configured timezone — use local time
        let local = chrono::Local::now();
        local.format("%Y-%m-%dT%H:%M:%S%.f%:z").to_string()
    }
}

/// get_timezone() -> Option<String>
///
/// Return the configured timezone name, or None.
#[pyfunction]
pub fn get_timezone() -> Option<String> {
    ensure_cache_resolved();
    CACHED_TZ_NAME.read().clone()
}

/// get_timezone_name() -> String
///
/// Return the IANA name of the configured timezone, or empty string.
#[pyfunction]
pub fn get_timezone_name() -> String {
    ensure_cache_resolved();
    CACHED_TZ_NAME.read().clone().unwrap_or_default()
}

/// reset_cache() -> None
///
/// Clear the cached timezone. Used by tests and after config changes.
#[pyfunction]
pub fn reset_cache() {
    *CACHED_TZ_NAME.write() = None;
    *CACHED_TZ.write() = None;
    *CACHE_RESOLVED.lock().unwrap() = false;
}

/// hermes_time_now_ts() -> f64
///
/// Return the current unix timestamp for performance measurements.
#[pyfunction]
pub fn hermes_time_now_ts() -> f64 {
    ensure_cache_resolved();
    let now = Utc::now();
    now.timestamp() as f64 + now.timestamp_subsec_nanos() as f64 * 1e-9
}

#[pymodule]
fn _hermes_time_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(now, m)?)?;
    m.add_function(wrap_pyfunction!(get_timezone, m)?)?;
    m.add_function(wrap_pyfunction!(get_timezone_name, m)?)?;
    m.add_function(wrap_pyfunction!(reset_cache, m)?)?;
    m.add_function(wrap_pyfunction!(hermes_time_now_ts, m)?)?;
    Ok(())
}
