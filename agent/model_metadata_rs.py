"""Rust-accelerated model metadata helpers — wraps model_metadata_rs."""

import logging
import os
from pathlib import Path
from typing import Any, Dict, List, Optional

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
        p = Path(path) / "libmodel_metadata_rs.so"
        if p.is_file():
            try:
                import importlib.util
                spec = importlib.util.spec_from_file_location("model_metadata_rs", str(p))
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                _rust = module
                logger.debug("model_metadata_rs: Rust backend loaded")
                return _rust
            except Exception as e:
                logger.debug("model_metadata_rs: failed to load Rust: %s", e)
    _rust = False
    return _rust

def strip_provider_prefix(model: str) -> str:
    rust = _load_rust()
    if rust:
        try:
            return rust.strip_provider_prefix(model)
        except Exception:
            pass
    # Pure Python fallback (from model_metadata.py _strip_provider_prefix)
    import re
    PROVIDER_PREFIXES = frozenset({"openrouter","nous","openai-codex","copilot","copilot-acp","zai","kimi-coding","minimax","minimax-cn","anthropic","deepseek","opencode-zen","opencode-go","ai-gateway","kilocode","alibaba","custom","local","glm","z-ai","z.ai","zhipu","github","github-copilot","github-models","kimi","moonshot","claude","deep-seek","opencode","zen","go","vercel","kilo","dashscope","aliyun","qwen"})
    OLLAMA_TAG_PATTERN = re.compile(r"^(\d+\.?\d*b|latest|stable|q\d|fp?\d|instruct|chat|coder|vision|text)", re.IGNORECASE)
    if ":" not in model or model.startswith("http"):
        return model
    prefix, suffix = model.split(":", 1)
    if prefix.strip().lower() in PROVIDER_PREFIXES:
        if OLLAMA_TAG_PATTERN.match(suffix.strip()):
            return model
        return suffix
    return model

def is_local_endpoint(base_url: str) -> bool:
    rust = _load_rust()
    if rust:
        try:
            return rust.is_local_endpoint(base_url)
        except Exception:
            pass
    # Pure Python fallback
    import ipaddress
    from urllib.parse import urlparse
    normalized = (base_url or "").strip().rstrip("/")
    if not normalized:
        return False
    url = normalized if "://" in normalized else f"http://{normalized}"
    try:
        host = urlparse(url).hostname or ""
    except Exception:
        return False
    LOCAL_HOSTS = ("localhost", "127.0.0.1", "::1", "0.0.0.0")
    if host in LOCAL_HOSTS:
        return True
    try:
        addr = ipaddress.ip_address(host)
        return addr.is_private or addr.is_loopback or addr.is_link_local
    except ValueError:
        pass
    parts = host.split(".")
    if len(parts) == 4:
        try:
            a, b = int(parts[0]), int(parts[1])
            if a == 10: return True
            if a == 172 and 16 <= b <= 31: return True
            if a == 192 and b == 168: return True
        except ValueError:
            pass
    return False

def extract_context_length(json_payload: str) -> Optional[int]:
    rust = _load_rust()
    if rust:
        try:
            return rust.extract_context_length(json_payload)
        except Exception:
            pass
    return None

def extract_max_completion_tokens(json_payload: str) -> Optional[int]:
    rust = _load_rust()
    if rust:
        try:
            return rust.extract_max_completion_tokens(json_payload)
        except Exception:
            pass
    return None

def extract_pricing(json_payload: str) -> Dict[str, Any]:
    rust = _load_rust()
    if rust:
        try:
            import json
            return json.loads(rust.extract_pricing(json_payload))
        except Exception:
            pass
    return {}

def parse_context_limit_from_error(error_msg: str) -> Optional[int]:
    rust = _load_rust()
    if rust:
        try:
            return rust.parse_context_limit_from_error(error_msg)
        except Exception:
            pass
    return None

def model_id_matches(candidate_id: str, lookup_model: str) -> bool:
    rust = _load_rust()
    if rust:
        try:
            return rust.model_id_matches(candidate_id, lookup_model)
        except Exception:
            pass
    return candidate_id == lookup_model or (candidate_id.rsplit("/", 1)[-1] == lookup_model if "/" in candidate_id else False)

def get_next_probe_tier(current_length: int) -> Optional[int]:
    rust = _load_rust()
    if rust:
        try:
            return rust.get_next_probe_tier(current_length)
        except Exception:
            pass
    TIERS = [128_000, 64_000, 32_000, 16_000, 8_000]
    return next((t for t in TIERS if t < current_length), None)

def estimate_tokens_rough(text: str) -> int:
    rust = _load_rust()
    if rust:
        try:
            return rust.estimate_tokens_rough(text)
        except Exception:
            pass
    return len(text) // 4 if text else 0