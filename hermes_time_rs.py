"""
Timezone-aware clock for Hermes — Rust-native implementation.

This module provides a fast Rust alternative to hermes_time.py.
It is a drop-in replacement: the public API (now, get_timezone,
get_timezone_name, reset_cache) is identical.

The Rust module is loaded via importlib; if unavailable (e.g. not
yet built, or during tests), all functions fall back to the pure-Python
implementation from hermes_time.py.
"""

from __future__ import annotations

import importlib.util
import logging
import os
import sys
from pathlib import Path
from typing import Optional

logger = logging.getLogger(__name__)

# --- Rust loader -------------------------------------------------------------

_RS_MODULE_NAME = "_hermes_time_rs"
_rs_lib_path: Optional[Path] = None

# Compute path to the compiled .so once, at import time.
# This follows the same pattern as _url_safety_rs:
_rust_base = Path(__file__).parent / "rust"
_possible_so = _rust_base / "target" / "release" / f"lib{_RS_MODULE_NAME}.so"
if _possible_so.exists():
    _rs_lib_path = _possible_so
else:
    _debug_so = _rust_base / "target" / "debug" / f"lib{_RS_MODULE_NAME}.so"
    if _debug_so.exists():
        _rs_lib_path = _debug_so

_rs_mod: Optional[object] = None
if _rs_lib_path is not None:
    try:
        spec = importlib.util.spec_from_file_location(_RS_MODULE_NAME, _rs_lib_path)
        if spec and spec.loader:
            _rs_mod = importlib.util.module_from_spec(spec)
            sys.modules[_RS_MODULE_NAME] = _rs_mod
            spec.loader.exec_module(_rs_mod)  # type: ignore[union-attr]
            logger.debug("Loaded Rust time module from %s", _rs_lib_path)
    except Exception as exc:
        logger.warning("Failed to load %s (falling back to Python): %s", _rs_lib_path, exc)
        _rs_mod = None


def _have_rs() -> bool:
    return _rs_mod is not None


# --- Pure-Python fallback (copied from hermes_time.py) -----------------------

try:
    from zoneinfo import ZoneInfo
except ImportError:
    from backports.zoneinfo import ZoneInfo  # type: ignore[no-redef]


def _resolve_timezone_name() -> str:
    """Read the configured IANA timezone string (or empty string)."""
    # 1. HERMES_TIMEZONE env var
    tz_env = os.getenv("HERMES_TIMEZONE", "").strip()
    if tz_env:
        return tz_env

    # 2. ~/.hermes/config.yaml "timezone" key
    try:
        import yaml

        hermes_home = Path(os.getenv("HERMES_HOME", Path.home() / ".hermes"))
        config_path = hermes_home / "config.yaml"
        if config_path.exists():
            with open(config_path) as f:
                cfg = yaml.safe_load(f) or {}
            tz_cfg = cfg.get("timezone", "")
            if isinstance(tz_cfg, str) and tz_cfg.strip():
                return tz_cfg.strip()
    except Exception:
        pass

    return ""


def _get_zoneinfo(name: str) -> Optional[ZoneInfo]:
    """Validate and return a ZoneInfo, or None if invalid."""
    if not name:
        return None
    try:
        return ZoneInfo(name)
    except (KeyError, Exception) as exc:
        logger.warning(
            "Invalid timezone '%s': %s. Falling back to server local time.",
            name,
            exc,
        )
        return None


# Cached state (pure-Python fallback)
_cached_tz: Optional[ZoneInfo] = None
_cached_tz_name: Optional[str] = None
_cache_resolved: bool = False


def _py_get_timezone() -> Optional[ZoneInfo]:
    global _cached_tz, _cached_tz_name, _cache_resolved
    if not _cache_resolved:
        _cached_tz_name = _resolve_timezone_name()
        _cached_tz = _get_zoneinfo(_cached_tz_name)
        _cache_resolved = True
    return _cached_tz


def _py_get_timezone_name() -> str:
    global _cached_tz_name, _cache_resolved
    if not _cache_resolved:
        _py_get_timezone()
    return _cached_tz_name or ""


def _py_now() -> str:
    """Return current ISO8601 string with timezone (pure-Python fallback)."""
    tz = _py_get_timezone()
    if tz is not None:
        from datetime import datetime

        return datetime.now(tz).isoformat()
    from datetime import datetime

    return datetime.now().astimezone().isoformat()


def _py_reset_cache() -> None:
    global _cached_tz, _cached_tz_name, _cache_resolved
    _cached_tz = None
    _cached_tz_name = None
    _cache_resolved = False


# --- Public API --------------------------------------------------------------


def now() -> str:
    """Return the current time as an ISO8601 string with timezone.

    Prefers the Rust implementation for performance; falls back to
    the pure-Python hermes_time.py logic automatically.
    """
    if _have_rs():
        return _rs_mod.now()  # type: ignore[union-attr]
    return _py_now()


def get_timezone() -> Optional[str]:
    """Return the configured IANA timezone name, or None.

    Prefers the Rust implementation; falls back to pure-Python on error.
    """
    if _have_rs():
        return _rs_mod.get_timezone()  # type: ignore[union-attr]
    return _py_get_timezone_name() or None


def get_timezone_name() -> str:
    """Return the IANA name of the configured timezone, or empty string."""
    if _have_rs():
        return _rs_mod.get_timezone_name()  # type: ignore[union-attr]
    return _py_get_timezone_name()


def reset_cache() -> None:
    """Clear the cached timezone (both Rust and Python caches)."""
    if _have_rs():
        _rs_mod.reset_cache()  # type: ignore[union-attr]
    _py_reset_cache()
