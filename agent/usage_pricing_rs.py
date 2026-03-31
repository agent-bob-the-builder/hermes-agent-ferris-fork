"""Rust-accelerated usage pricing helpers — wraps usage_pricing_rs.

Pure functions that don't require network I/O are provided by Rust for speed.
All other functions delegate to the pure-Python usage_pricing module.
"""

import json
import logging
import os
from pathlib import Path
from typing import Any, Optional

logger = logging.getLogger(__name__)

_rust = None


def _load_rust():
    global _rust
    if _rust is not None:
        return _rust
    for path in [
        os.getenv("HERMES_RS_LIBS", ""),
        str(Path.home() / ".hermes" / "rs"),
        "/root/.hermes/hermes-agent-ferris-fork/rust/target/release",
    ]:
        if not path:
            continue
        p = Path(path) / "libusage_pricing_rs.so"
        if p.is_file():
            try:
                import importlib.util

                spec = importlib.util.spec_from_file_location("usage_pricing_rs", str(p))
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                _rust = module
                logger.debug("usage_pricing_rs: Rust backend loaded")
                return _rust
            except Exception as e:
                logger.debug("usage_pricing_rs: failed to load Rust: %s", e)
    _rust = False
    return _rust


def resolve_billing_route(
    model_name: str,
    provider: Optional[str] = None,
    base_url: Optional[str] = None,
):
    """Resolve a BillingRoute from model name + provider/base_url (pure)."""
    rust = _load_rust()
    if rust:
        try:
            return rust.py_resolve_billing_route(model_name, provider, base_url)
        except Exception:
            pass
    # Python fallback
    from agent.usage_pricing import resolve_billing_route as pyResolve
    return pyResolve(model_name, provider=provider, base_url=base_url)


def normalize_usage(
    response_usage: Any,
    *,
    provider: Optional[str] = None,
    api_mode: Optional[str] = None,
):
    """Normalize raw API response usage into CanonicalUsage (pure)."""
    rust = _load_rust()
    if rust:
        try:
            # Accept dict/object and serialize to JSON for Rust
            if hasattr(response_usage, "__dict__"):
                usage_dict = vars(response_usage)
            elif hasattr(response_usage, "_asdict"):
                usage_dict = response_usage._asdict()
            elif isinstance(response_usage, dict):
                usage_dict = response_usage
            else:
                usage_dict = {}
            json_str = json.dumps(usage_dict)
            return rust.py_normalize_usage(json_str, provider, api_mode)
        except Exception:
            pass
    # Python fallback
    from agent.usage_pricing import normalize_usage as pyNormalize
    return pyNormalize(response_usage, provider=provider, api_mode=api_mode)


def lookup_official_docs_pricing(provider: str, model: str):
    """Look up official docs pricing for a provider+model (pure)."""
    rust = _load_rust()
    if rust:
        try:
            return rust.py_lookup_official_docs_pricing(provider, model)
        except Exception:
            pass
    # Python fallback
    from agent.usage_pricing import _lookup_official_docs_pricing
    from agent.usage_pricing import BillingRoute
    route = resolve_billing_route(model, provider=provider)
    return _lookup_official_docs_pricing(route)


def has_known_pricing(
    model_name: str,
    provider: Optional[str] = None,
    base_url: Optional[str] = None,
) -> bool:
    """Check if known pricing exists for a model+route (pure)."""
    rust = _load_rust()
    if rust:
        try:
            return rust.py_has_known_pricing(model_name, provider, base_url)
        except Exception:
            pass
    # Python fallback
    from agent.usage_pricing import has_known_pricing as pyHasKnown
    return pyHasKnown(model_name, provider=provider, base_url=base_url)


def compute_cost_from_usage(
    usage: Any,
    pricing_entry: Any,
):
    """Compute cost from canonical usage and pricing entry (pure)."""
    rust = _load_rust()
    if rust:
        try:
            if hasattr(usage, "__dict__"):
                usage_dict = vars(usage)
            elif hasattr(usage, "_asdict"):
                usage_dict = usage._asdict()
            elif isinstance(usage, dict):
                usage_dict = usage
            else:
                usage_dict = {}
            if hasattr(pricing_entry, "__dict__"):
                entry_dict = vars(pricing_entry)
            elif hasattr(pricing_entry, "_asdict"):
                entry_dict = pricing_entry._asdict()
            elif isinstance(pricing_entry, dict):
                entry_dict = pricing_entry
            else:
                entry_dict = {}
            json_usage = json.dumps(usage_dict)
            json_entry = json.dumps(entry_dict)
            result = rust.py_compute_cost_from_usage(json_usage, json_entry)
            return result  # (amount_usd: Option<String>, notes: Vec<String>)
        except Exception:
            pass
    # Python fallback — compute using Python Decimal arithmetic
    from decimal import Decimal
    from agent.usage_pricing import _ONE_MILLION
    amount = Decimal("0")
    notes = []
    for attr in ["input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens"]:
        tokens = getattr(usage, attr, 0) or 0
        if tokens:
            cost_attr = attr.replace("tokens", "cost_per_million")
            cost = getattr(pricing_entry, cost_attr, None)
            if cost is None:
                return (None, [f"{attr} pricing unavailable"])
            amount += Decimal(str(tokens)) * Decimal(str(cost)) / _ONE_MILLION
    return (amount, notes)


def estimate_usage_cost(
    model_name: str,
    usage: Any,
    *,
    provider: Optional[str] = None,
    base_url: Optional[str] = None,
    api_key: Optional[str] = None,
):
    """Estimate usage cost — delegates to Python (uses network for dynamic pricing)."""
    # This function needs get_pricing_entry which uses network APIs, so always use Python
    from agent.usage_pricing import estimate_usage_cost as pyEstimate
    return pyEstimate(
        model_name,
        usage,
        provider=provider,
        base_url=base_url,
        api_key=api_key,
    )


def format_duration_compact(seconds: float) -> str:
    """Format a duration in seconds as a compact string (pure)."""
    rust = _load_rust()
    if rust:
        try:
            return rust.py_format_duration_compact(seconds)
        except Exception:
            pass
    # Python fallback
    if seconds < 60:
        return f"{seconds:.0f}s"
    minutes = seconds / 60
    if minutes < 60:
        return f"{minutes:.0f}m"
    hours = minutes / 60
    if hours < 24:
        remaining_min = int(minutes % 60)
        if remaining_min == 0:
            return f"{int(hours)}h"
        return f"{int(hours)}h {remaining_min}m"
    days = hours / 24
    return f"{days:.1f}d"


def format_token_count_compact(value: int) -> str:
    """Format a token count as a compact string (pure)."""
    rust = _load_rust()
    if rust:
        try:
            return rust.py_format_token_count_compact(value)
        except Exception:
            pass
    # Python fallback
    abs_value = abs(value)
    if abs_value < 1_000:
        return str(value)
    sign = "-" if value < 0 else ""
    for threshold, suffix in [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "K")]:
        if abs_value >= threshold:
            scaled = abs_value / threshold
            if scaled < 10:
                text = f"{scaled:.2f}"
            elif scaled < 100:
                text = f"{scaled:.1f}"
            else:
                text = f"{scaled:.0f}"
            text = text.rstrip("0").rstrip(".")
            return f"{sign}{text}{suffix}"
    return str(value)
