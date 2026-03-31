"""Rust-accelerated title generation helpers — wraps title_generator_rs."""

import logging
import os
from pathlib import Path
from typing import Optional

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
        p = Path(path) / "libtitle_generator_rs.so"
        if p.is_file():
            try:
                import importlib.util
                spec = importlib.util.spec_from_file_location("title_generator_rs", str(p))
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                _rust = module
                logger.debug("title_generator_rs: Rust backend loaded")
                return _rust
            except Exception as e:
                logger.debug("title_generator_rs: failed to load Rust: %s", e)
    _rust = False
    return _rust

def format_title_prompt(user_message: str, assistant_response: str) -> str:
    """Build the messages JSON for title generation LLM call."""
    rust = _load_rust()
    if rust:
        try:
            return rust.format_title_prompt(user_message, assistant_response)
        except Exception:
            pass
    # Pure Python fallback
    import json
    user_snippet = user_message[:500] if user_message else ""
    assistant_snippet = assistant_response[:500] if assistant_response else ""
    prompt = "Generate a short, descriptive title (3-7 words)..."
    return json.dumps([
        {"role": "system", "content": prompt},
        {"role": "user", "content": f"User: {user_snippet}\n\nAssistant: {assistant_snippet}"},
    ])

def parse_title_response(response: str) -> Optional[str]:
    """Parse LLM response into cleaned title."""
    rust = _load_rust()
    if rust:
        try:
            return rust.parse_title_response(response)
        except Exception:
            pass
    # Pure Python fallback
    cleaned = response.strip().strip('"\'').strip()
    if cleaned.lower().startswith("title:"):
        cleaned = cleaned[6:].strip()
    cleaned = cleaned.strip().rstrip('.').rstrip('!').rstrip('?')
    if not cleaned or len(cleaned) > 80:
        return None
    if len(cleaned) > 77:
        cleaned = cleaned[:77] + "..."
    return cleaned

def should_auto_title(conversation_history: list) -> bool:
    """Check if we should auto-generate a title (≤2 user messages seen)."""
    rust = _load_rust()
    if rust:
        try:
            import json
            return rust.should_auto_title(json.dumps(conversation_history))
        except Exception:
            pass
    # Pure Python fallback
    user_count = sum(1 for m in (conversation_history or []) if m.get("role") == "user")
    return user_count <= 2