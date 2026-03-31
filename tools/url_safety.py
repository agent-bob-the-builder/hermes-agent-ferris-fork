"""URL safety checks — blocks requests to private/internal network addresses.

Prevents SSRF (Server-Side Request Forgery) where a malicious prompt or
skill could trick the agent into fetching internal resources like cloud
metadata endpoints (169.254.169.254), localhost services, or private
network hosts.

Limitations (documented, not fixable at pre-flight level):
  - DNS rebinding (TOCTOU): an attacker-controlled DNS server with TTL=0
    can return a public IP for the check, then a private IP for the actual
    connection. Fixing this requires connection-level validation (e.g.
    Python's Champion library or an egress proxy like Stripe's Smokescreen).
  - Redirect-based bypass in vision_tools is mitigated by an httpx event
    hook that re-validates each redirect target. Web tools use third-party
    SDKs (Firecrawl/Tavily) where redirect handling is on their servers.

Uses the Rust `_url_safety_rs` accelerator when available (~5-10x faster
than the pure-Python implementation), falling back to Python for
compatibility in environments where the Rust extension is not built.
"""

import logging
import os
from pathlib import Path

logger = logging.getLogger(__name__)

# -----------------------------------------------------------------------
# Rust accelerator — loaded at import time from ~/.hermes/rs/
# -----------------------------------------------------------------------
_rust_url_safety = None
_using_rust = False

# Secondary fallback: url_safety_python_rs (Rust port of Python fallback)
_rust_python_fallback = None

_rs_lib_path: str | None = None


def _resolve_rs_lib() -> str:
    """Locate liburl_safety_rs.so.

    Search order:
    1. HERMES_RS_LIBS env var (explicit override)
    2. ~/.hermes/rs/ (installed location)
    3. Rust target directory (repo source tree — hermes-agent/rust/target/release/)
    """
    if env_path := os.getenv("HERMES_RS_LIBS"):
        p = Path(env_path) / "liburl_safety_rs.so"
        if p.is_file():
            return str(p)

    home_rs = Path.home() / ".hermes" / "rs" / "liburl_safety_rs.so"
    if home_rs.is_file():
        return str(home_rs)

    # Repo source tree
    here = Path(__file__).parent.resolve()
    repo_rs = here.parent / "rust" / "target" / "release" / "liburl_safety_rs.so"
    if repo_rs.is_file():
        return str(repo_rs)

    return "url_safety_rs"  # let importlib try the usual path


def _load_rs_module():
    global _rust_url_safety, _using_rust, _rs_lib_path
    if _rust_url_safety is not None:
        return

    lib_path = _resolve_rs_lib()
    _rs_lib_path = lib_path

    try:
        import importlib.util
        spec = importlib.util.spec_from_file_location("_url_safety_rs", lib_path)
        if spec is None or spec.loader is None:
            raise ImportError(f"Could not create module spec for {lib_path}")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        # Verify it works
        module.is_safe_url("")
        _rust_url_safety = module
        _using_rust = True
        logger.debug("url_safety: Rust backend initialized OK (lib: %s)", lib_path)
    except Exception as _e:
        _rust_url_safety = False
        _using_rust = False
        logger.debug("url_safety: Rust backend unavailable, using pure Python (%s)", _e)


def _resolve_rs_python_lib() -> str | None:
    """Locate liburl_safety_python_rs.so for the Python fallback path."""
    if env_path := os.getenv("HERMES_RS_LIBS"):
        p = Path(env_path) / "liburl_safety_python_rs.so"
        if p.is_file():
            return str(p)

    home_rs = Path.home() / ".hermes" / "rs" / "liburl_safety_python_rs.so"
    if home_rs.is_file():
        return str(home_rs)

    # Repo source tree
    here = Path(__file__).parent.resolve()
    repo_rs = here.parent / "rust" / "target" / "release" / "liburl_safety_python_rs.so"
    if repo_rs.is_file():
        return str(repo_rs)

    return None


def _load_rs_python_fallback():
    """Load the Rust port of the Python fallback (url_safety_python_rs)."""
    global _rust_python_fallback
    if _rust_python_fallback is not None:
        return

    lib_path = _resolve_rs_python_lib()
    if lib_path is None:
        _rust_python_fallback = False
        return

    try:
        import importlib.util
        spec = importlib.util.spec_from_file_location("url_safety_python_rs", lib_path)
        if spec is None or spec.loader is None:
            raise ImportError(f"Could not create module spec for {lib_path}")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        # Verify it works
        module.is_safe_url_python("")
        _rust_python_fallback = module
        logger.debug("url_safety: Rust Python fallback initialized OK (lib: %s)", lib_path)
    except Exception as _e:
        _rust_python_fallback = False
        logger.debug("url_safety: Rust Python fallback unavailable (%s)", _e)


# Load Rust at import time
_load_rs_module()
_load_rs_python_fallback()

# -----------------------------------------------------------------------
# Pure-Python implementation (fallback) — identical logic to the Rust version
# -----------------------------------------------------------------------
import ipaddress
import socket
from urllib.parse import urlparse

_BLOCKED_HOSTNAMES = frozenset({
    "metadata.google.internal",
    "metadata.goog",
})

# 100.64.0.0/10 (CGNAT / Shared Address Space, RFC 6598) is NOT covered by
# ipaddress.is_private — it returns False for both is_private and is_global.
# Must be blocked explicitly. Used by carrier-grade NAT, Tailscale/WireGuard
# VPNs, and some cloud internal networks.
_CGNAT_NETWORK = ipaddress.ip_network("100.64.0.0/10")


def _is_blocked_ip(ip: ipaddress.IPv4Address | ipaddress.IPv6Address) -> bool:
    """Return True if the IP should be blocked for SSRF protection."""
    if ip.is_private or ip.is_loopback or ip.is_link_local or ip.is_reserved:
        return True
    if ip.is_multicast or ip.is_unspecified:
        return True
    # CGNAT range not covered by is_private
    if ip in _CGNAT_NETWORK:
        return True
    return False


# -----------------------------------------------------------------------
# Public API
# -----------------------------------------------------------------------

def is_safe_url(url: str) -> bool:
    """Return True if the URL target is not a private/internal address.

    Resolves the hostname to an IP and checks against private ranges.
    Fails closed: DNS errors and unexpected exceptions block the request.

    Uses the Rust `_url_safety_rs` accelerator when available for ~5-10x
    throughput on repeated calls. The pure-Python path is identical in
    behaviour and only used when the Rust .so is unavailable.
    """
    # Fast path: Rust accelerator
    if _rust_url_safety is not False:
        # _rust_url_safety is either the module or False; False means we
        # already tried and failed, so skip re-trying every call
        if _rust_url_safety:
            try:
                return bool(_rust_url_safety.is_safe_url(url))
            except Exception:
                # Fall through to Python on any error from the Rust side
                pass

    # Python fallback — try Rust port first, then pure Python
    try:
        # Try Rust Python fallback first
        if _rust_python_fallback is not False:
            if _rust_python_fallback:
                try:
                    return bool(_rust_python_fallback.is_safe_url_python(url))
                except Exception:
                    pass

        # Pure Python fallback (original implementation)
        parsed = urlparse(url)
        hostname = (parsed.hostname or "").strip().lower()
        if not hostname:
            return False

        # Block known internal hostnames
        if hostname in _BLOCKED_HOSTNAMES:
            logger.warning("Blocked request to internal hostname: %s", hostname)
            return False

        # Try to resolve and check IP
        try:
            addr_info = socket.getaddrinfo(hostname, None, socket.AF_UNSPEC, socket.SOCK_STREAM)
        except socket.gaierror:
            # DNS resolution failed — fail closed
            logger.warning("Blocked request — DNS resolution failed for: %s", hostname)
            return False

        for family, _, _, _, sockaddr in addr_info:
            ip_str = sockaddr[0]
            try:
                ip = ipaddress.ip_address(ip_str)
            except ValueError:
                continue

            if _is_blocked_ip(ip):
                logger.warning(
                    "Blocked request to private/internal address: %s -> %s",
                    hostname, ip_str,
                )
                return False

        return True

    except Exception as exc:
        logger.warning("Blocked request — URL safety check error for %s: %s", url, exc)
        return False
