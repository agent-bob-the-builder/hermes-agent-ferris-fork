//! Rust-native URL safety checker — SSRF protection.
//! Pure-Rust alternative to tools/url_safety.py
//! Called on every HTTP request made by web_tools, browser_tool, vision_tools.

use pyo3::prelude::*;
use std::net::{Ipv4Addr, Ipv6Addr, ToSocketAddrs};

// ============================================================================
// Constants (mirrors of tools/url_safety.py)
// ============================================================================

const BLOCKED_HOSTNAMES: &[&str] = &["metadata.google.internal", "metadata.goog"];

/// 100.64.0.0/10 — CGNAT / Shared Address Space (RFC 6598)
/// Not covered by Ipv4Addr::is_private.
const CGNAT_START: u32 = 0x6440_0000; // 100.64.0.0
const CGNAT_END: u32 = 0x647F_FFFF; // 100.127.255.255

// ============================================================================
// Helpers
// ============================================================================

fn is_cgnat(ip: u32) -> bool {
    (CGNAT_START..=CGNAT_END).contains(&ip)
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

/// Returns the hostname from a URL as a lowercase String, or None.
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
        .find([':', '/'])
        .unwrap_or(after_user.len());

    let hostname = &after_user[..hostname_end];
    if hostname.is_empty() {
        None
    } else {
        Some(hostname.to_lowercase())
    }
}

// ============================================================================
// Main exported function
// ============================================================================

/// is_safe_url(url: &str) -> bool
///
/// Pure-Rust SSRF check: parses URL, resolves hostname via OS DNS,
/// returns False if any resolved IP is private/link-local/etc.
///
/// Fails closed: DNS resolution failures return False.
#[pyfunction]
pub fn is_safe_url(url: &str) -> bool {
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
fn url_safety_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(is_safe_url, m)?)?;
    Ok(())
}
