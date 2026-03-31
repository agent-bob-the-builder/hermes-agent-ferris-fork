//! Rust port of the pure-Python URL safety fallback in tools/url_safety.py.
//! Exposes is_safe_url_python(url: &str) -> bool for use as a Python fallback
//! when the main _url_safety_rs accelerator is unavailable.

use pyo3::prelude::*;
use std::net::{Ipv4Addr, Ipv6Addr, ToSocketAddrs};

// ============================================================================
// Constants (mirrors of tools/url_safety.py)
// ============================================================================

const BLOCKED_HOSTNAMES: &[&str] = &[
    "metadata.google.internal",
    "metadata.goog",
];

/// 100.64.0.0/10 — CGNAT / Shared Address Space (RFC 6598)
/// Not covered by Ipv4Addr::is_private.
const CGNAT_START: u32 = 0x6440_0000; // 100.64.0.0
const CGNAT_END: u32 = 0x647F_FFFF; // 100.127.255.255

// ============================================================================
// Helpers
// ============================================================================

fn is_cgnat(ip: u32) -> bool {
    ip >= CGNAT_START && ip <= CGNAT_END
}

fn is_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    let addr = u32::from(*ip);
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || is_cgnat(addr)
}

fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    // IPv6 doesn't have is_private in the same way; only loopback, multicast, unspecified
    ip.is_loopback() || ip.is_multicast() || ip.is_unspecified()
}

fn is_blocked_ip_str(ip_str: &str) -> bool {
    if let Ok(ip) = ip_str.parse::<Ipv4Addr>() {
        return is_blocked_ipv4(&ip);
    }
    if let Ok(ip) = ip_str.parse::<Ipv6Addr>() {
        return is_blocked_ipv6(&ip);
    }
    false
}

/// Extract hostname from URL (lowercase), or None if invalid/empty.
fn extract_hostname(url: &str) -> Option<String> {
    let url = url.trim();

    // Strip scheme
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("//"))
        .unwrap_or(url);

    // Strip user:pass@
    let after_user = if let Some(at) = after_scheme.find('@') {
        &after_scheme[at + 1..]
    } else {
        after_scheme
    };

    // Hostname ends at : or / or end of string
    let hostname_end = after_user
        .find(|c| c == ':' || c == '/')
        .unwrap_or(after_user.len());

    let hostname = &after_user[..hostname_end];
    if hostname.is_empty() {
        None
    } else {
        Some(hostname.to_lowercase())
    }
}

// ============================================================================
// Public API
// ============================================================================

/// is_safe_url_python(url: &str) -> bool
///
/// Pure-Rust implementation of the Python fallback logic.
/// Parses URL, resolves hostname via OS DNS, returns false if any resolved
/// IP is private/link-local/loopback/multicast/unspecified/CGNAT.
///
/// Fails closed: DNS resolution failures return false.
#[pyfunction]
pub fn is_safe_url_python(url: &str) -> bool {
    let hostname = match extract_hostname(url) {
        Some(h) if !h.is_empty() => h,
        _ => return false,
    };

    // Block known internal hostnames regardless of DNS resolution
    if BLOCKED_HOSTNAMES.contains(&hostname.as_str()) {
        return false;
    }

    // Resolve via OS DNS (uses /etc/resolv.conf — same as Python's socket.getaddrinfo)
    let addr_str = format!("{}:80", hostname);
    let socket_addrs = match addr_str.to_socket_addrs() {
        Ok(addrs) => addrs,
        Err(_) => return false, // DNS failure → fail closed
    };

    for socket_addr in socket_addrs {
        let ip_str = socket_addr.ip().to_string();
        if is_blocked_ip_str(&ip_str) {
            return false;
        }
    }

    true
}

// ============================================================================
// Python module
// ============================================================================

#[pymodule]
fn url_safety_python_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(is_safe_url_python, m)?)?;
    Ok(())
}
