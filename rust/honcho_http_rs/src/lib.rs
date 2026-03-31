//! honcho_http_rs — Async reqwest-based HTTP client for Honcho API integration.
//!
//! Replaces Python `httpx` sync calls with async `reqwest` for Honcho's
//! short-lived HTTP requests (prefetch, sync, user observations).
//!
//! ## Design
//!
//! - `async fn prefetch(...)`  → Honcho recall lookup
//! - `async fn sync(...)`       → Write conversation turn
//! - `async fn save_observation(...)` → User observation
//!
//! Python calls are synchronous, so we wrap async reqwest with a
//! tokio runtime via `Python::attach` for GIL-safe GIL acquisition.

use pyo3::prelude::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

// ─── HTTP client (single reqwest instance) ────────────────────────────────────

static CLIENT: once_cell::sync::Lazy<Client> = once_cell::sync::Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest Client must be built")
});

// ─── Serialized types ────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SyncRequest<'a> {
    session_key: &'a str,
    peer: &'a str,
    user_message: &'a str,
    assistant_response: &'a str,
}

#[derive(Debug, Serialize)]
struct ObservationRequest<'a> {
    peer: &'a str,
    observation: &'a str,
}

#[derive(Debug, Deserialize)]
struct PrefetchResponse {
    #[serde(flatten)]
    extra: Value,
    context: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SyncResponse {
    ok: bool,
    #[serde(flatten)]
    extra: Value,
}

// ─── Core async functions ────────────────────────────────────────────────────

/// GET {base_url}/api/recall — Honcho recall lookup.
async fn prefetch_async(
    base_url: &str,
    api_key: &str,
    peer_name: &str,
    session_key: &str,
    query: &str,
) -> Result<Option<String>, String> {
    let url = format!("{}/api/recall", base_url.trim_end_matches('/'));
    let resp = CLIENT
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("X-Peer", peer_name)
        .query(&[("session_key", session_key), ("query", query)])
        .send()
        .await
        .map_err(|e| format!("prefetch request failed: {}", e))?;

    let status_code = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("prefetch HTTP {}: {}", status_code, body));
    }

    let body: PrefetchResponse = resp
        .json()
        .await
        .map_err(|e| format!("prefetch parse failed: {}", e))?;

    Ok(body.context)
}

/// POST {base_url}/api/sync — Write conversation turn.
async fn sync_async(
    base_url: &str,
    api_key: &str,
    peer_name: &str,
    session_key: &str,
    user_message: &str,
    assistant_response: &str,
) -> Result<bool, String> {
    let url = format!("{}/api/sync", base_url.trim_end_matches('/'));
    let resp = CLIENT
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&SyncRequest {
            session_key,
            peer: peer_name,
            user_message,
            assistant_response,
        })
        .send()
        .await
        .map_err(|e| format!("sync request failed: {}", e))?;

    let status_code = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("sync HTTP {}: {}", status_code, body));
    }

    let body: SyncResponse = resp
        .json()
        .await
        .map_err(|e| format!("sync parse failed: {}", e))?;

    Ok(body.ok)
}

/// POST {base_url}/api/observation — Save a user observation.
async fn save_observation_async(
    base_url: &str,
    api_key: &str,
    peer_name: &str,
    observation: &str,
) -> Result<bool, String> {
    let url = format!("{}/api/observation", base_url.trim_end_matches('/'));
    let resp = CLIENT
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&ObservationRequest {
            peer: peer_name,
            observation,
        })
        .send()
        .await
        .map_err(|e| format!("observation request failed: {}", e))?;

    let status_code = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("observation HTTP {}: {}", status_code, body));
    }

    Ok(true)
}

// ─── PyO3 sync wrappers (block_on per call) ───────────────────────────────────

fn to_pyerr<E: std::fmt::Display>(e: E) -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(e.to_string())
}

#[pyfunction]
fn rs_prefetch(
    base_url: &str,
    api_key: &str,
    peer_name: &str,
    session_key: &str,
    query: &str,
) -> PyResult<Option<String>> {
    // Each call gets its own runtime — Honcho calls are infrequent.
    let rt = tokio::runtime::Runtime::new().map_err(to_pyerr)?;
    rt.block_on(prefetch_async(base_url, api_key, peer_name, session_key, query))
        .map_err(to_pyerr)
}

#[pyfunction]
fn rs_sync(
    base_url: &str,
    api_key: &str,
    peer_name: &str,
    session_key: &str,
    user_message: &str,
    assistant_response: &str,
) -> PyResult<bool> {
    let rt = tokio::runtime::Runtime::new().map_err(to_pyerr)?;
    rt.block_on(sync_async(
        base_url,
        api_key,
        peer_name,
        session_key,
        user_message,
        assistant_response,
    ))
    .map_err(to_pyerr)
}

#[pyfunction]
fn rs_save_observation(
    base_url: &str,
    api_key: &str,
    peer_name: &str,
    observation: &str,
) -> PyResult<bool> {
    let rt = tokio::runtime::Runtime::new().map_err(to_pyerr)?;
    rt.block_on(save_observation_async(base_url, api_key, peer_name, observation))
        .map_err(to_pyerr)
}

#[pymodule]
fn _honcho_http_rust(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(rs_prefetch, module)?)?;
    module.add_function(wrap_pyfunction!(rs_sync, module)?)?;
    module.add_function(wrap_pyfunction!(rs_save_observation, module)?)?;
    module.add(
        "__doc__",
        "Async reqwest HTTP client for Honcho API — Rust backend.",
    )?;
    Ok(())
}
