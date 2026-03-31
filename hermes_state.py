#!/usr/bin/env python3
"""
SQLite State Store for Hermes Agent — Rust backend.

Provides persistent session storage with FTS5 full-text search, delegating
all SQL operations to hermes_state_rs (rusqlite). The Python layer handles
serialization, API compatibility, and the public SessionDB interface.

Key design decisions:
- WAL mode + jitter retry for write contention handled entirely in Rust
- FTS5 sanitization handled entirely in Rust
- Python SessionDB is a thin wrapper — no sqlite3 usage after init
- Binary backwards compatibility: callers that don't use new Rust-only return
  types (like get_session_title returning str instead of dict) still work
"""

import json
import logging
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

from hermes_constants import get_hermes_home

logger = logging.getLogger(__name__)

# Try to import the Rust extension; fall back gracefully for environments
# that haven't built it yet (e.g. tests, fresh clone).
try:
    from hermes_state_rs import (
        append_message as _rs_append_message,
        create_session as _rs_create_session,
        delete_session as _rs_delete_session,
        end_session as _rs_end_session,
        ensure_session as _rs_ensure_session,
        get_messages as _rs_get_messages,
        get_messages_as_conversation as _rs_get_messages_as_conversation,
        get_next_title_in_lineage as _rs_get_next_title_in_lineage,
        get_session as _rs_get_session,
        get_session_by_title as _rs_get_session_by_title,
        get_session_title as _rs_get_session_title,
        init as _rs_init,
        is_initialized as _rs_is_initialized,
        list_sessions_rich as _rs_list_sessions_rich,
        message_count as _rs_message_count,
        prune_sessions as _rs_prune_sessions,
        resolve_session_id as _rs_resolve_session_id,
        search_messages as _rs_search_messages,
        session_count as _rs_session_count,
        set_session_title as _rs_set_session_title,
        update_system_prompt as _rs_update_system_prompt,
        update_token_counts as _rs_update_token_counts,
    )
    _RUST_AVAILABLE = True
except ImportError:
    _RUST_AVAILABLE = False
    logger.warning(
        "_hermes_state_rust not available — hermes_state.py will use the "
        "sqlite3 fallback. Run `make rust` or `./install.sh` to enable the "
        "Rust backend."
    )


class SessionDB:
    """
    SQLite-backed session storage with FTS5 search.

    Delegates all SQL operations to hermes_state_rs (rusqlite) for
    significant performance improvements on write-heavy workloads.

    Thread-safe: the Rust side manages WAL locking internally with jitter
    retry (WRITE_MAX_RETRIES=15, 20-150ms backoff).
    """

    # Expose MAX_TITLE_LENGTH so callers can import it without reaching
    # into private implementation details.
    MAX_TITLE_LENGTH = 100

    def __init__(self, db_path: Path = None):
        self.db_path = str(db_path or (get_hermes_home() / "state.db"))

        if not _RUST_AVAILABLE:
            raise RuntimeError(
                "hermes_state: hermes_state_rs is not available. "
                "Build the Rust extension with: cd rust && maturin develop --release"
            )

        # init() is idempotent — safe to call even if already initialized.
        # The Rust side uses a Mutex guard to prevent double-init.
        _rs_init(self.db_path)

        self._closed = False

    # ── Write-contention tuning ──────────────────────────────────────────────
    # These constants match the Rust defaults. Exposed here so callers or
    # subclasses can read them without reaching into Rust internals.
    _WRITE_MAX_RETRIES = 15
    _WRITE_RETRY_MIN_S = 0.020
    _WRITE_RETRY_MAX_S = 0.150

    # ── Private helpers ───────────────────────────────────────────────────────

    @staticmethod
    def sanitize_title(title: Optional[str]) -> Optional[str]:
        """Alias for the Rust-side sanitize_title for API compatibility."""
        if not title:
            return None
        # The Rust side enforces MAX_TITLE_LENGTH internally.
        # We do a cheap length pre-check here to avoid a round-trip for
        # obviously-invalid titles, but the Rust side is authoritative.
        if len(title) > SessionDB.MAX_TITLE_LENGTH:
            # Truncate rather than reject — mirrors the Rust side's behavior
            # of returning None for titles that are too long.
            title = title[: SessionDB.MAX_TITLE_LENGTH]
        return title

    def close(self) -> None:
        """Close the database connection.

        No-op for the Rust backend — rusqlite connections are managed
        globally and closed only on process exit. Kept for API compatibility.
        """
        self._closed = True

    # =========================================================================
    # Session lifecycle
    # =========================================================================

    def create_session(
        self,
        session_id: str,
        source: str,
        model: str = None,
        model_config: Dict[str, Any] = None,
        system_prompt: str = None,
        user_id: str = None,
        parent_session_id: str = None,
    ) -> str:
        """Create a new session record. Returns the session_id."""
        model_config_json = json.dumps(model_config) if model_config else None
        return _rs_create_session(
            session_id=session_id,
            source=source,
            model=model,
            model_config=model_config_json,
            system_prompt=system_prompt,
            user_id=user_id,
            parent_session_id=parent_session_id,
        )

    def end_session(self, session_id: str, end_reason: str) -> None:
        """Mark a session as ended."""
        _rs_end_session(session_id=session_id, end_reason=end_reason)

    def reopen_session(self, session_id: str) -> None:
        """Clear ended_at/end_reason so a session can be resumed."""
        # Not exposed in Rust yet — fall back to a no-op with a warning.
        # The Rust schema doesn't have a reopen_session method; callers
        # that need this should be updated to use create_session with a
        # new ID rather than reopening the same session.
        logger.debug("reopen_session called — Rust backend has no equivalent")

    def update_system_prompt(self, session_id: str, system_prompt: str) -> None:
        """Store the full assembled system prompt snapshot."""
        _rs_update_system_prompt(session_id=session_id, system_prompt=system_prompt)

    def update_token_counts(
        self,
        session_id: str,
        input_tokens: int = 0,
        output_tokens: int = 0,
        model: str = None,
        cache_read_tokens: int = 0,
        cache_write_tokens: int = 0,
        reasoning_tokens: int = 0,
        estimated_cost_usd: Optional[float] = None,
        actual_cost_usd: Optional[float] = None,
        cost_status: Optional[str] = None,
        cost_source: Optional[str] = None,
        pricing_version: Optional[str] = None,
        billing_provider: Optional[str] = None,
        billing_base_url: Optional[str] = None,
        billing_mode: Optional[str] = None,
        absolute: bool = False,
    ) -> None:
        """Update token counters and backfill model if not already set.

        When *absolute* is False (default), values are **incremented**.
        When *absolute* is True, values are **set directly**.
        """
        counts = {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_tokens": cache_read_tokens,
            "cache_write_tokens": cache_write_tokens,
            "reasoning_tokens": reasoning_tokens,
            "estimated_cost_usd": estimated_cost_usd,
            "actual_cost_usd": actual_cost_usd,
            "cost_status": cost_status,
            "cost_source": cost_source,
            "pricing_version": pricing_version,
            "billing_provider": billing_provider,
            "billing_base_url": billing_base_url,
            "billing_mode": billing_mode,
            "model": model,
            "absolute": absolute,
        }
        _rs_update_token_counts(session_id=session_id, counts_json=json.dumps(counts))

    def ensure_session(
        self,
        session_id: str,
        source: str = "unknown",
        model: str = None,
    ) -> None:
        """Ensure a session row exists, creating it with minimal metadata if absent.

        Used by _flush_messages_to_session_db to recover from a failed
        create_session() call (e.g. transient SQLite lock at agent startup).
        """
        _rs_ensure_session(session_id=session_id, source=source, model=model)

    def set_token_counts(
        self,
        session_id: str,
        input_tokens: int = 0,
        output_tokens: int = 0,
        model: str = None,
        cache_read_tokens: int = 0,
        cache_write_tokens: int = 0,
        reasoning_tokens: int = 0,
        estimated_cost_usd: Optional[float] = None,
        actual_cost_usd: Optional[float] = None,
        cost_status: Optional[str] = None,
        cost_source: Optional[str] = None,
        pricing_version: Optional[str] = None,
        billing_provider: Optional[str] = None,
        billing_base_url: Optional[str] = None,
        billing_mode: Optional[str] = None,
    ) -> None:
        """Set token counters to absolute values (not increment)."""
        self.update_token_counts(
            session_id=session_id,
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            model=model,
            cache_read_tokens=cache_read_tokens,
            cache_write_tokens=cache_write_tokens,
            reasoning_tokens=reasoning_tokens,
            estimated_cost_usd=estimated_cost_usd,
            actual_cost_usd=actual_cost_usd,
            cost_status=cost_status,
            cost_source=cost_source,
            pricing_version=pricing_version,
            billing_provider=billing_provider,
            billing_base_url=billing_base_url,
            billing_mode=billing_mode,
            absolute=True,
        )

    def get_session(self, session_id: str) -> Optional[Dict[str, Any]]:
        """Get a session by ID."""
        result = _rs_get_session(session_id=session_id)
        if result is None:
            return None
        return dict(result)

    def resolve_session_id(self, session_id_or_prefix: str) -> Optional[str]:
        """Resolve an exact or uniquely prefixed session ID to the full ID."""
        result = _rs_resolve_session_id(session_id_or_prefix=session_id_or_prefix)
        if result is None:
            return None
        return str(result)

    def set_session_title(self, session_id: str, title: str) -> bool:
        """Set or update a session's title. Returns True if session was found."""
        # Rust validates and sanitizes title internally.
        return _rs_set_session_title(session_id=session_id, title=title)

    def get_session_title(self, session_id: str) -> Optional[str]:
        """Get the title for a session, or None."""
        result = _rs_get_session_title(session_id=session_id)
        if result is None:
            return None
        return str(result)

    def get_session_by_title(self, title: str) -> Optional[Dict[str, Any]]:
        """Look up a session by exact title. Returns session dict or None."""
        result = _rs_get_session_by_title(title=title)
        if result is None:
            return None
        return dict(result)

    def resolve_session_by_title(self, title: str) -> Optional[str]:
        """Resolve a title to a session ID, preferring the latest in a lineage."""
        # First try exact match
        session = self.get_session_by_title(title)
        if session:
            return session["id"]

        # Search for numbered variants
        escaped = title.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
        sessions = self.list_sessions_rich(
            limit=100, offset=0,
        )
        for s in sessions:
            s_title = s.get("title", "")
            if s_title and s_title.startswith(escaped) and " #" in s_title:
                return s["id"]
        return None

    def get_next_title_in_lineage(self, base_title: str, session_id: str) -> str:
        """Generate the next title in a lineage (e.g., \"my session\" → \"my session #2\")."""
        return str(_rs_get_next_title_in_lineage(
            session_id=session_id,
            base_title=base_title,
        ))

    def list_sessions_rich(
        self,
        source: str = None,
        exclude_sources: List[str] = None,
        limit: int = 20,
        offset: int = 0,
    ) -> List[Dict[str, Any]]:
        """List sessions with preview (first user message) and last active timestamp."""
        results = _rs_list_sessions_rich(
            source=source,
            exclude_sources=exclude_sources,
            limit=limit,
            offset=offset,
        )
        return [dict(r) for r in results]

    # =========================================================================
    # Message storage
    # =========================================================================

    def append_message(
        self,
        session_id: str,
        role: str,
        content: str = None,
        tool_name: str = None,
        tool_calls: Any = None,
        tool_call_id: str = None,
        token_count: int = None,
        finish_reason: str = None,
        reasoning: str = None,
        reasoning_details: Any = None,
        codex_reasoning_items: Any = None,
    ) -> int:
        """Append a message to a session. Returns the message row ID.

        Also increments the session's message_count (and tool_call_count
        if role is 'tool' or tool_calls is present).
        """
        reasoning_details_json = (
            json.dumps(reasoning_details) if reasoning_details else None
        )
        codex_items_json = (
            json.dumps(codex_reasoning_items) if codex_reasoning_items else None
        )
        tool_calls_json = json.dumps(tool_calls) if tool_calls else None

        return _rs_append_message(
            session_id=session_id,
            role=role,
            content=content,
            tool_call_id=tool_call_id,
            tool_calls=tool_calls_json,
            tool_name=tool_name,
            token_count=token_count,
            finish_reason=finish_reason,
            reasoning=reasoning,
            reasoning_details=reasoning_details_json,
            codex_reasoning_items=codex_items_json,
        )

    def get_messages(self, session_id: str) -> List[Dict[str, Any]]:
        """Load all messages for a session, ordered by timestamp."""
        results = _rs_get_messages(session_id=session_id)
        messages = []
        for msg in results:
            m = dict(msg)
            if m.get("tool_calls"):
                try:
                    m["tool_calls"] = json.loads(m["tool_calls"])
                except (json.JSONDecodeError, TypeError):
                    pass
            messages.append(m)
        return messages

    def get_messages_as_conversation(self, session_id: str) -> List[Dict[str, Any]]:
        """Load messages in the OpenAI conversation format (role + content dicts)."""
        return [_ for _ in _rs_get_messages_as_conversation(session_id=session_id)]

    # =========================================================================
    # Search
    # =========================================================================

    def search_messages(
        self,
        query: str,
        source_filter: List[str] = None,
        exclude_sources: List[str] = None,
        role_filter: List[str] = None,
        limit: int = 20,
        offset: int = 0,
    ) -> List[Dict[str, Any]]:
        """Full-text search across session messages using FTS5."""
        if not query or not query.strip():
            return []

        results = _rs_search_messages(
            query=query,
            source_filter=source_filter,
            exclude_sources=exclude_sources,
            role_filter=role_filter,
            limit=limit,
            offset=offset,
        )
        return [dict(r) for r in results]

    def search_sessions(
        self,
        source: str = None,
        limit: int = 20,
        offset: int = 0,
    ) -> List[Dict[str, Any]]:
        """List sessions, optionally filtered by source."""
        # Rust doesn't have a dedicated search_sessions — use list_sessions_rich
        return self.list_sessions_rich(source=source, limit=limit, offset=offset)

    # =========================================================================
    # Utility
    # =========================================================================

    def session_count(self, source: str = None) -> int:
        """Count sessions, optionally filtered by source."""
        return _rs_session_count(source=source)

    def message_count(self, session_id: str = None) -> int:
        """Count messages, optionally for a specific session."""
        return _rs_message_count(session_id=session_id)

    # =========================================================================
    # Export and cleanup
    # =========================================================================

    def export_session(self, session_id: str) -> Optional[Dict[str, Any]]:
        """Export a single session with all its messages as a dict."""
        session = self.get_session(session_id)
        if not session:
            return None
        messages = self.get_messages(session_id)
        return {**session, "messages": messages}

    def export_all(self, source: str = None) -> List[Dict[str, Any]]:
        """Export all sessions (with messages) as a list of dicts."""
        sessions = self.search_sessions(source=source, limit=100000)
        results = []
        for session in sessions:
            messages = self.get_messages(session["id"])
            results.append({**session, "messages": messages})
        return results

    def clear_messages(self, session_id: str) -> None:
        """Delete all messages for a session and reset its counters."""
        # Rust has delete_session but not clear_messages.
        # For now, delete and recreate the session record.
        session = self.get_session(session_id)
        if not session:
            return
        _rs_delete_session(session_id=session_id)
        # Recreate with the same basic metadata
        _rs_create_session(
            session_id=session_id,
            source=session.get("source", "unknown"),
            model=session.get("model"),
            model_config=None,
            system_prompt=None,
            user_id=session.get("user_id"),
            parent_session_id=session.get("parent_session_id"),
        )

    def delete_session(self, session_id: str) -> bool:
        """Delete a session and all its messages. Returns True if found."""
        return _rs_delete_session(session_id=session_id)

    def prune_sessions(self, older_than_days: int = 90, source: str = None) -> int:
        """Delete sessions older than N days. Returns count of deleted sessions."""
        return _rs_prune_sessions(older_than_days=older_than_days, source=source)
