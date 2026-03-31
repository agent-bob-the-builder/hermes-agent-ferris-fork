"""subprocess_rs — Python bindings for the Rust subprocess engine.

Loads the cdylib via importlib.resources, falling back to a direct filesystem
path.  This allows the wrapper to work both when Hermes is installed as a
package and when it is run from the repo source tree.

Usage:
    from tools.subprocess_rs import spawn, SubprocessHandle

    handle = spawn(["echo", "hello"], cwd="", timeout_ms=5000,
                   stdin_data="", env={})
    result = handle.wait()
    print(result.output, result.returncode)
"""

from __future__ import annotations

import importlib.resources
import os
import sys
from pathlib import Path
from typing import Any

# Resolve the .so once at import time so the hot path (every terminal call)
# avoids any further filesystem lookups.
_subprocess_rs: Any = None
_rs_lib_path: str | None = None


def _resolve_rs_lib() -> str:
    """Locate the compiled subprocess_rs cdylib.

    Search order:
    1. Directory of this file  (repo source tree — hermes-agent/tools/)
    2. ~/.hermes/rs/           (installed location)
    3. HERMES_RS_LIBS env var   (explicit override)
    """
    # Explicit override for CI / non-standard installs
    if env_path := os.getenv("HERMES_RS_LIBS"):
        p = Path(env_path) / "libsubprocess_rs.so"
        if p.is_file():
            return str(p)

    # Repo source tree:  tools/subprocess_rs.py → ../rust/target/debug/
    here = Path(__file__).parent.resolve()
    repo_rs = here.parent / "rust" / "target" / "debug" / "libsubprocess_rs.so"
    if repo_rs.is_file():
        return str(repo_rs)

    # Installed: ~/.hermes/rs/
    home_rs = Path.home() / ".hermes" / "rs" / "libsubprocess_rs.so"
    if home_rs.is_file():
        return str(home_rs)

    # Last resort: let importlib try the standard package path
    return "subprocess_rs"


def _load_rs_module():
    global _subprocess_rs, _rs_lib_path
    if _subprocess_rs is not None:
        return

    lib_path = _resolve_rs_lib()
    _rs_lib_path = lib_path

    try:
        # PyO3 cdylib exposes the module as "subprocess_rs" regardless of
        # the crate's lib name, because #[pymodule] is fn subprocess_rs(…).
        _subprocess_rs = __import__("subprocess_rs")
    except ImportError:
        # Fallback: load via importlib.util directly from the .so path.
        # This works when the cdylib isn't installed as a Python package.
        import importlib.util

        spec = importlib.util.spec_from_file_location("subprocess_rs", lib_path)
        if spec is None or spec.loader is None:
            raise ImportError(f"Could not create module spec for {lib_path}")
        module = importlib.util.module_from_spec(spec)
        sys.modules["subprocess_rs"] = module
        spec.loader.exec_module(module)
        _subprocess_rs = module


def spawn(
    cmd: list[str],
    cwd: str = "",
    timeout_ms: int = 0,
    stdin_data: str = "",
    env: dict[str, str] | None = None,
) -> "SubprocessHandle":
    """Spawn a subprocess and return a handle.

    Args:
        cmd:        Command + arguments as a list of strings.
        cwd:        Working directory (empty = inherit).
        timeout_ms: Deadline in milliseconds (0 = no timeout).
        stdin_data: String to pipe to stdin then close (sudo passwords, etc.).
        env:        Extra environment variables merged with inherited env.

    Returns:
        A SubprocessHandle.  Call .wait() to block for completion.
    """
    _load_rs_module()
    handle = _subprocess_rs.spawn(
        cmd=cmd,
        cwd=cwd,
        timeout_ms=timeout_ms,
        stdin_data=stdin_data,
        env=env or {},
    )
    return SubprocessHandle(handle)


def interrupt(session_id: str) -> bool:
    """Send SIGKILL to a process session by ID (returns True if found)."""
    _load_rs_module()
    return _subprocess_rs.interrupt(session_id)


def cleanup_session(session_id: str) -> None:
    """Remove a session from the Rust process registry."""
    _load_rs_module()
    _subprocess_rs.cleanup_session(session_id)


# ─────────────────────────────────────────────────────────────────────────────
# SubprocessHandle — thin wrapper around the Py<SubprocessHandle> returned by
# Rust.  The Rust wait() method returns an ExecuteResult object that has
# .output, .returncode, .interrupted, .timed_out attributes.
# ─────────────────────────────────────────────────────────────────────────────

class SubprocessHandle:
    """Python handle to a Rust-spawned subprocess.

    Attributes:
        session_id: Unique session ID (used for interrupt/cleanup).
    """

    __slots__ = ("_handle", "_session_id")

    def __init__(self, handle: Any):
        # 'handle' is the Py<SubprocessHandle> returned by Rust spawn()
        self._handle = handle
        self._session_id: str | None = None

    @property
    def session_id(self) -> str:
        if self._session_id is None:
            self._session_id = self._handle.session_id
        return self._session_id

    def wait(self) -> "ExecuteResult":
        """Block until the subprocess exits, is interrupted, or times out.

        Returns:
            ExecuteResult with .output, .returncode, .interrupted, .timed_out.
        """
        _load_rs_module()
        return self._handle.wait()

    def drain_partial(self) -> str:
        """Read accumulated output so far (non-blocking)."""
        return self._handle.drain_partial()

    def is_done(self) -> bool:
        """True if the subprocess has exited."""
        return self._handle.is_done()

    def kill(self) -> None:
        """Kill the subprocess and all its children."""
        self._handle.kill()

    def interrupt(self) -> None:
        """Interrupt the subprocess (sets interrupted flag + kills)."""
        self._handle.interrupt()


class ExecuteResult:
    """Result of a completed subprocess execution.

    Attributes:
        output:      Captured stdout/stderr text.
        returncode:  Process exit code (0 = success, 124 = timeout, 130 = interrupt).
        interrupted: True if killed by user interrupt.
        timed_out:   True if killed by timeout.
    """

    __slots__ = ("output", "returncode", "interrupted", "timed_out")

    def __init__(self, handle: Any):
        self.output: str = handle.output
        self.returncode: int = handle.returncode
        self.interrupted: bool = handle.interrupted
        self.timed_out: bool = handle.timed_out

    def __repr__(self) -> str:
        return (
            f"ExecuteResult(returncode={self.returncode}, "
            f"interrupted={self.interrupted}, timed_out={self.timed_out}, "
            f"output_len={len(self.output)})"
        )
