"""checkpoint_manager_rs — Python bindings for Rust checkpoint manager.

Loads the cdylib via importlib.resources, falling back to a direct filesystem
path.  This allows the wrapper to work both when Hermes is installed as a
package and when it is run from the repo source tree.

Usage:
    from tools.checkpoint_manager_rs import CheckpointManager

    mgr = CheckpointManager(enabled=True, max_snapshots=50)
    mgr.new_turn()
    mgr.ensure_checkpoint("/path/to/project", "reason")
"""

from __future__ import annotations

import importlib.resources
import os
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional

# Resolve the .so once at import time so the hot path avoids any further filesystem lookups.
_checkpoint_rs: Any = None
_rs_lib_path: str | None = None


def _resolve_rs_lib() -> str:
    """Locate the compiled checkpoint_manager_rs cdylib.

    Search order:
    1. Directory of this file  (repo source tree — hermes-agent/tools/)
    2. ~/.hermes/rs/           (installed location)
    3. HERMES_RS_LIBS env var   (explicit override)
    """
    # Explicit override for CI / non-standard installs
    if env_path := os.getenv("HERMES_RS_LIBS"):
        p = Path(env_path) / "libcheckpoint_manager_rs.so"
        if p.is_file():
            return str(p)

    # Repo source tree:  tools/checkpoint_manager_rs.py → ../rust/target/debug/
    here = Path(__file__).parent.resolve()
    repo_rs = here.parent / "rust" / "target" / "debug" / "libcheckpoint_manager_rs.so"
    if repo_rs.is_file():
        return str(repo_rs)

    # Also check release target
    repo_rs_release = here.parent / "rust" / "target" / "release" / "libcheckpoint_manager_rs.so"
    if repo_rs_release.is_file():
        return str(repo_rs_release)

    # Installed: ~/.hermes/rs/
    home_rs = Path.home() / ".hermes" / "rs" / "libcheckpoint_manager_rs.so"
    if home_rs.is_file():
        return str(home_rs)

    # Last resort: let importlib try the standard package path
    return "checkpoint_manager_rs"


def _load_rs_module():
    global _checkpoint_rs, _rs_lib_path
    if _checkpoint_rs is not None:
        return

    lib_path = _resolve_rs_lib()
    _rs_lib_path = lib_path

    # Use importlib.util to load the .so directly
    import importlib.util

    spec = importlib.util.spec_from_file_location("checkpoint_manager_rs", lib_path)
    if spec is None or spec.loader is None:
        raise ImportError(f"Could not create module spec for {lib_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules["checkpoint_manager_rs"] = module
    spec.loader.exec_module(module)
    _checkpoint_rs = module


# =============================================================================
# Fallback pure-Python CheckpointManager (if Rust fails)
# =============================================================================

# Import the original Python CheckpointManager as fallback
try:
    from tools.checkpoint_manager import (
        CheckpointManager as _PyCheckpointManager,
        format_checkpoint_list as _format_checkpoint_list,
        _shadow_repo_path as _py_shadow_repo_path,
    )
    _HAS_PY_FALLBACK = True
except ImportError:
    _HAS_PY_FALLBACK = False


# =============================================================================
# Rust-backed CheckpointManager
# =============================================================================

class CheckpointManager:
    """Python wrapper around the Rust checkpoint_manager_rs implementation.
    
    This class wraps the PyO3 bindings to the Rust CheckpointManager.
    Falls back to pure Python if Rust loading fails.
    
    Parameters
    ----------
    enabled : bool
        Master switch (from config / CLI flag).
    max_snapshots : int
        Keep at most this many checkpoints per directory.
    """

    __slots__ = ("_rust", "_python", "_use_rust")

    def __init__(self, enabled: bool = False, max_snapshots: int = 50):
        self._rust: Any = None
        self._python: Any = None
        self._use_rust: bool = False

        if enabled:
            try:
                _load_rs_module()
                self._rust = _checkpoint_rs.PyCheckpointManager(enabled, max_snapshots)
                self._use_rust = True
            except Exception as e:
                import logging
                logger = logging.getLogger(__name__)
                logger.debug("Failed to load Rust checkpoint_manager_rs, falling back to Python: %s", e)
                if _HAS_PY_FALLBACK:
                    self._python = _PyCheckpointManager(enabled, max_snapshots)
                else:
                    raise RuntimeError(f"Neither Rust nor Python checkpoint manager available: {e}")
        else:
            # When disabled, use Python fallback if available
            if _HAS_PY_FALLBACK:
                self._python = _PyCheckpointManager(enabled, max_snapshots)

    def new_turn(self) -> None:
        """Reset per-turn dedup. Call at the start of each agent iteration."""
        if self._use_rust and self._rust:
            self._rust.new_turn()
        elif self._python:
            self._python.new_turn()

    def ensure_checkpoint(self, working_dir: str, reason: str = "auto") -> bool:
        """Take a checkpoint if enabled and not already done this turn.

        Returns True if a checkpoint was taken, False otherwise.
        Never raises — all errors are silently logged.
        """
        if self._use_rust and self._rust:
            return self._rust.ensure_checkpoint(working_dir, reason)
        elif self._python:
            return self._python.ensure_checkpoint(working_dir, reason)
        return False

    def list_checkpoints(self, working_dir: str) -> List[Dict]:
        """List available checkpoints for a directory.

        Returns a list of dicts with keys: hash, short_hash, timestamp, reason,
        files_changed, insertions, deletions. Most recent first.
        """
        import json
        if self._use_rust and self._rust:
            result = self._rust.list_checkpoints(working_dir)
            if result:
                try:
                    return json.loads(result)
                except json.JSONDecodeError:
                    return []
            return []
        elif self._python:
            return self._python.list_checkpoints(working_dir)
        return []

    def diff(self, working_dir: str, commit_hash: str) -> Dict:
        """Show diff between a checkpoint and the current working tree.

        Returns dict with success, diff text, and stat summary.
        """
        import json
        if self._use_rust and self._rust:
            result = self._rust.diff(working_dir, commit_hash)
            if result:
                try:
                    return json.loads(result)
                except json.JSONDecodeError:
                    return {"success": False, "error": "Failed to parse diff result"}
            return {"success": False, "error": "No result from Rust"}
        elif self._python:
            return self._python.diff(working_dir, commit_hash)
        return {"success": False, "error": "No checkpoint manager available"}

    def restore(self, working_dir: str, commit_hash: str, file_path: str = None) -> Dict:
        """Restore files to a checkpoint state.

        Parameters
        ----------
        file_path : str, optional
            If provided, restore only this file instead of the entire directory.

        Returns dict with success/error info.
        """
        import json
        if self._use_rust and self._rust:
            result = self._rust.restore(working_dir, commit_hash, file_path)
            if result:
                try:
                    return json.loads(result)
                except json.JSONDecodeError:
                    return {"success": False, "error": "Failed to parse restore result"}
            return {"success": False, "error": "No result from Rust"}
        elif self._python:
            return self._python.restore(working_dir, commit_hash, file_path)
        return {"success": False, "error": "No checkpoint manager available"}

    def get_working_dir_for_path(self, file_path: str) -> str:
        """Resolve a file path to its working directory for checkpointing.

        Walks up from the file's parent to find a reasonable project root
        (directory containing .git, pyproject.toml, package.json, etc.).
        Falls back to the file's parent directory.
        """
        if self._use_rust and self._rust:
            return self._rust.get_working_dir_for_path(file_path)
        elif self._python:
            return self._python.get_working_dir_for_path(file_path)
        # Fallback: just return parent
        p = Path(file_path)
        if p.is_dir():
            return str(p)
        return str(p.parent)


def format_checkpoint_list(checkpoints: List[Dict], directory: str) -> str:
    """Format checkpoint list for display to user.
    
    This uses the Python implementation directly since it's just formatting.
    """
    if _HAS_PY_FALLBACK:
        return _format_checkpoint_list(checkpoints, directory)
    
    if not checkpoints:
        return f"No checkpoints found for {directory}"

    lines = [f"📸 Checkpoints for {directory}:\n"]
    for i, cp in enumerate(checkpoints, 1):
        ts = cp.get("timestamp", "")
        if ts:
            # Handle Unix timestamp or ISO format
            try:
                if "T" in ts:
                    ts = ts.split("T")[1].split("+")[0].split("-")[0][:5]
                    date = cp["timestamp"].split("T")[0]
                    ts = f"{date} {ts}"
            except (IndexError, ValueError):
                pass

        files = cp.get("files_changed", 0)
        ins = cp.get("insertions", 0)
        dele = cp.get("deletions", 0)
        if files:
            stat = f"  ({files} file{'s' if files != 1 else ''}, +{ins}/-{dele})"
        else:
            stat = ""

        lines.append(f"  {i}. {cp.get('short_hash', '?')}  {ts}  {cp.get('reason', '')}{stat}")

    lines.append("\n  /rollback <N>             restore to checkpoint N")
    lines.append("  /rollback diff <N>        preview changes since checkpoint N")
    lines.append("  /rollback <N> <file>      restore a single file from checkpoint N")
    return "\n".join(lines)
