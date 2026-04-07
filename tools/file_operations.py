#!/usr/bin/env python3
"""
File Operations Module

Provides file manipulation capabilities (read, write, patch, search) that work
across all terminal backends (local, docker, singularity, ssh, modal, daytona).

The key insight is that all file operations can be expressed as shell commands,
so we wrap the terminal backend's execute() interface to provide a unified file API.

Usage:
    from tools.file_operations import ShellFileOperations
    from tools.terminal_tool import _active_environments
    
    # Get file operations for a terminal environment
    file_ops = ShellFileOperations(terminal_env)
    
    # Read a file
    result = file_ops.read_file("/path/to/file.py")
    
    # Write a file
    result = file_ops.write_file("/path/to/new.py", "print('hello')")
    
    # Search for content
    result = file_ops.search("TODO", path=".", file_glob="*.py")
"""

import glob as glob_module
import os
import re
import shutil
import subprocess
import difflib
import threading
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Optional, List, Dict, Any, Tuple
from pathlib import Path
from hermes_constants import get_hermes_home

# Rust accelerator — pure-Rust string utilities for hot-path file ops
try:
    import _file_ops_rs as _rust_fo
except Exception:
    _rust_fo = None


# ---------------------------------------------------------------------------
# Write-path deny list — blocks writes to sensitive system/credential files
# ---------------------------------------------------------------------------

_HOME = str(Path.home())

WRITE_DENIED_PATHS = {
    os.path.realpath(p) for p in [
        os.path.join(_HOME, ".ssh", "authorized_keys"),
        os.path.join(_HOME, ".ssh", "id_rsa"),
        os.path.join(_HOME, ".ssh", "id_ed25519"),
        os.path.join(_HOME, ".ssh", "config"),
        str(get_hermes_home() / ".env"),
        os.path.join(_HOME, ".bashrc"),
        os.path.join(_HOME, ".zshrc"),
        os.path.join(_HOME, ".profile"),
        os.path.join(_HOME, ".bash_profile"),
        os.path.join(_HOME, ".zprofile"),
        os.path.join(_HOME, ".netrc"),
        os.path.join(_HOME, ".pgpass"),
        os.path.join(_HOME, ".npmrc"),
        os.path.join(_HOME, ".pypirc"),
        "/etc/sudoers",
        "/etc/passwd",
        "/etc/shadow",
    ]
}

WRITE_DENIED_PREFIXES = [
    os.path.realpath(p) + os.sep for p in [
        os.path.join(_HOME, ".ssh"),
        os.path.join(_HOME, ".aws"),
        os.path.join(_HOME, ".gnupg"),
        os.path.join(_HOME, ".kube"),
        "/etc/sudoers.d",
        "/etc/systemd",
        os.path.join(_HOME, ".docker"),
        os.path.join(_HOME, ".azure"),
        os.path.join(_HOME, ".config", "gh"),
    ]
]


def _get_safe_write_root() -> Optional[str]:
    """Return the resolved HERMES_WRITE_SAFE_ROOT path, or None if unset.

    When set, all write_file/patch operations are constrained to this
    directory tree.  Writes outside it are denied even if the target is
    not on the static deny list.  Opt-in hardening for gateway/messaging
    deployments that should only touch a workspace checkout.
    """
    root = os.getenv("HERMES_WRITE_SAFE_ROOT", "")
    if not root:
        return None
    try:
        return os.path.realpath(os.path.expanduser(root))
    except Exception:
        return None


def _is_write_denied(path: str) -> bool:
    """Return True if path is on the write deny list."""
    resolved = os.path.realpath(os.path.expanduser(str(path)))

    # 1) Static deny list
    if resolved in WRITE_DENIED_PATHS:
        return True
    for prefix in WRITE_DENIED_PREFIXES:
        if resolved.startswith(prefix):
            return True

    # 2) Optional safe-root sandbox
    safe_root = _get_safe_write_root()
    if safe_root:
        if not (resolved == safe_root or resolved.startswith(safe_root + os.sep)):
            return True

    return False


# =============================================================================
# Result Data Classes
# =============================================================================

@dataclass
class ReadResult:
    """Result from reading a file."""
    content: str = ""
    total_lines: int = 0
    file_size: int = 0
    truncated: bool = False
    hint: Optional[str] = None
    is_binary: bool = False
    is_image: bool = False
    base64_content: Optional[str] = None
    mime_type: Optional[str] = None
    dimensions: Optional[str] = None  # For images: "WIDTHxHEIGHT"
    error: Optional[str] = None
    similar_files: List[str] = field(default_factory=list)
    
    def to_dict(self) -> dict:
        return {k: v for k, v in self.__dict__.items() if v is not None and v != []}


@dataclass
class WriteResult:
    """Result from writing a file."""
    bytes_written: int = 0
    dirs_created: bool = False
    error: Optional[str] = None
    warning: Optional[str] = None
    
    def to_dict(self) -> dict:
        return {k: v for k, v in self.__dict__.items() if v is not None}


@dataclass
class PatchResult:
    """Result from patching a file."""
    success: bool = False
    diff: str = ""
    files_modified: List[str] = field(default_factory=list)
    files_created: List[str] = field(default_factory=list)
    files_deleted: List[str] = field(default_factory=list)
    lint: Optional[Dict[str, Any]] = None
    error: Optional[str] = None
    
    def to_dict(self) -> dict:
        result = {"success": self.success}
        if self.diff:
            result["diff"] = self.diff
        if self.files_modified:
            result["files_modified"] = self.files_modified
        if self.files_created:
            result["files_created"] = self.files_created
        if self.files_deleted:
            result["files_deleted"] = self.files_deleted
        if self.lint:
            result["lint"] = self.lint
        if self.error:
            result["error"] = self.error
        return result


@dataclass
class SearchMatch:
    """A single search match."""
    path: str
    line_number: int
    content: str
    mtime: float = 0.0  # Modification time for sorting


@dataclass
class SearchResult:
    """Result from searching."""
    matches: List[SearchMatch] = field(default_factory=list)
    files: List[str] = field(default_factory=list)
    counts: Dict[str, int] = field(default_factory=dict)
    total_count: int = 0
    truncated: bool = False
    error: Optional[str] = None
    
    def to_dict(self) -> dict:
        result = {"total_count": self.total_count}
        if self.matches:
            result["matches"] = [
                {"path": m.path, "line": m.line_number, "content": m.content}
                for m in self.matches
            ]
        if self.files:
            result["files"] = self.files
        if self.counts:
            result["counts"] = self.counts
        if self.truncated:
            result["truncated"] = True
        if self.error:
            result["error"] = self.error
        return result


@dataclass
class LintResult:
    """Result from linting a file."""
    success: bool = True
    skipped: bool = False
    output: str = ""
    message: str = ""
    
    def to_dict(self) -> dict:
        if self.skipped:
            return {"status": "skipped", "message": self.message}
        return {
            "status": "ok" if self.success else "error",
            "output": self.output
        }


@dataclass
class ExecuteResult:
    """Result from executing a shell command."""
    stdout: str = ""
    exit_code: int = 0


# =============================================================================
# Abstract Interface
# =============================================================================

class FileOperations(ABC):
    """Abstract interface for file operations across terminal backends."""
    
    @abstractmethod
    def read_file(self, path: str, offset: int = 1, limit: int = 500) -> ReadResult:
        """Read a file with pagination support."""
        ...
    
    @abstractmethod
    def write_file(self, path: str, content: str) -> WriteResult:
        """Write content to a file, creating directories as needed."""
        ...
    
    @abstractmethod
    def patch_replace(self, path: str, old_string: str, new_string: str, 
                      replace_all: bool = False) -> PatchResult:
        """Replace text in a file using fuzzy matching."""
        ...
    
    @abstractmethod
    def patch_v4a(self, patch_content: str) -> PatchResult:
        """Apply a V4A format patch."""
        ...
    
    @abstractmethod
    def search(self, pattern: str, path: str = ".", target: str = "content",
               file_glob: Optional[str] = None, limit: int = 50, offset: int = 0,
               output_mode: str = "content", context: int = 0) -> SearchResult:
        """Search for content or files."""
        ...


# =============================================================================
# Shell-based Implementation
# =============================================================================

# Binary file extensions (fast path check)
# -------------------------------------------------------------------------
# Native search — no shell, no bash -lic subprocess overhead
# -------------------------------------------------------------------------
_RG_BINARY: Optional[str] = None
_RG_BINARY_LOCK = threading.Lock()

BINARY_EXTENSIONS = {
    # Images
    '.png', '.jpg', '.jpeg', '.gif', '.webp', '.bmp', '.ico', '.tiff', '.tif',
    '.svg',  # SVG is text but often treated as binary
    # Audio/Video
    '.mp3', '.mp4', '.wav', '.avi', '.mov', '.mkv', '.flac', '.ogg', '.webm',
    # Archives
    '.zip', '.tar', '.gz', '.bz2', '.xz', '.7z', '.rar',
    # Documents
    '.pdf', '.doc', '.docx', '.xls', '.xlsx', '.ppt', '.pptx',
    # Compiled/Binary
    '.exe', '.dll', '.so', '.dylib', '.o', '.a', '.pyc', '.pyo', '.class',
    '.wasm', '.bin',
    # Fonts
    '.ttf', '.otf', '.woff', '.woff2', '.eot',
    # Other
    '.db', '.sqlite', '.sqlite3',
}

# Image extensions (subset of binary that we can return as base64)
IMAGE_EXTENSIONS = {'.png', '.jpg', '.jpeg', '.gif', '.webp', '.bmp', '.ico'}

# Linters by file extension
LINTERS = {
    '.py': 'python -m py_compile {file} 2>&1',
    '.js': 'node --check {file} 2>&1',
    '.ts': 'npx tsc --noEmit {file} 2>&1',
    '.go': 'go vet {file} 2>&1',
    '.rs': 'rustfmt --check {file} 2>&1',
}

# Max limits for read operations
MAX_LINES = 2000
MAX_LINE_LENGTH = 2000
MAX_FILE_SIZE = 50 * 1024  # 50KB


class ShellFileOperations(FileOperations):
    """
    File operations implemented via shell commands.
    
    Works with ANY terminal backend that has execute(command, cwd) method.
    This includes local, docker, singularity, ssh, modal, and daytona environments.
    """
    
    def __init__(self, terminal_env, cwd: str = None):
        """
        Initialize file operations with a terminal environment.
        
        Args:
            terminal_env: Any object with execute(command, cwd) method.
                         Returns {"output": str, "returncode": int}
            cwd: Working directory (defaults to env's cwd or current directory)
        """
        self.env = terminal_env
        # Determine cwd from various possible sources.
        # IMPORTANT: do NOT fall back to os.getcwd() -- that's the HOST's local
        # path which doesn't exist inside container/cloud backends (modal, docker).
        # If nothing provides a cwd, use "/" as a safe universal default.
        self.cwd = cwd or getattr(terminal_env, 'cwd', None) or \
                   getattr(getattr(terminal_env, 'config', None), 'cwd', None) or "/"
        
        # Cache for command availability checks
        self._command_cache: Dict[str, bool] = {}
    
    def _exec(self, command: str, cwd: str = None, timeout: int = None,
              stdin_data: str = None) -> ExecuteResult:
        """Execute command via terminal backend.
        
        Args:
            stdin_data: If provided, piped to the process's stdin instead of
                        embedding in the command string. Bypasses ARG_MAX.
        """
        kwargs = {}
        if timeout:
            kwargs['timeout'] = timeout
        if stdin_data is not None:
            kwargs['stdin_data'] = stdin_data
        
        result = self.env.execute(command, cwd=cwd or self.cwd, **kwargs)
        return ExecuteResult(
            stdout=result.get("output", ""),
            exit_code=result.get("returncode", 0)
        )
    
    def _has_command(self, cmd: str) -> bool:
        """Check if a command exists in the environment (cached)."""
        if cmd not in self._command_cache:
            result = self._exec(f"command -v {cmd} >/dev/null 2>&1 && echo 'yes'")
            self._command_cache[cmd] = result.stdout.strip() == 'yes'
        return self._command_cache[cmd]
    
    def _is_likely_binary(self, path: str, content_sample: str = None) -> bool:
        """
        Check if a file is likely binary.

        Uses extension check (fast) + content analysis (fallback).
        """
        if _rust_fo is not None:
            try:
                return _rust_fo.is_likely_binary_py(path, content_sample or "")
            except Exception:
                pass

        ext = os.path.splitext(path)[1].lower()
        if ext in BINARY_EXTENSIONS:
            return True

        # Content analysis: >30% non-printable chars = binary
        if content_sample:
            if not content_sample:
                return False
            non_printable = sum(1 for c in content_sample[:1000]
                               if ord(c) < 32 and c not in '\n\r\t')
            return non_printable / min(len(content_sample), 1000) > 0.30

        return False
    
    def _is_image(self, path: str) -> bool:
        """Check if file is an image we can return as base64."""
        ext = os.path.splitext(path)[1].lower()
        return ext in IMAGE_EXTENSIONS
    
    def _add_line_numbers(self, content: str, start_line: int = 1) -> str:
        """Add line numbers to content in LINE_NUM|CONTENT format."""
        if _rust_fo is not None:
            try:
                return _rust_fo.add_line_numbers_py(content, start_line)
            except Exception:
                pass

        lines = content.split('\n')
        numbered = []
        for i, line in enumerate(lines, start=start_line):
            # Truncate long lines
            if len(line) > MAX_LINE_LENGTH:
                line = line[:MAX_LINE_LENGTH] + "... [truncated]"
            numbered.append(f"{i:6d}|{line}")
        return '\n'.join(numbered)
    
    def _native_expand_path(self, path: str) -> str:
        """
        Expand shell-style paths like ~ and ~user to absolute paths.
        Uses native Python — no shell commands.
        """
        if not path:
            return path

        if _rust_fo is not None:
            try:
                return _rust_fo.native_expand_path_py(path)
            except Exception:
                pass

        if path.startswith('~'):
            # Validate before using expanduser (security: no command injection)
            if path == '~':
                return os.path.expanduser('~')
            if path.startswith('~/'):
                return os.path.expanduser('~') + path[1:]
            # ~username format — validate username is safe before expanding
            rest = path[1:]  # strip leading ~
            slash_idx = rest.find('/')
            username = rest[:slash_idx] if slash_idx >= 0 else rest
            if username and re.fullmatch(r'[a-zA-Z0-9._-]+', username):
                return os.path.expanduser(f'~{username}') + path[1 + len(username):]

        return path
    
    def _escape_shell_arg(self, arg: str) -> str:
        """Escape a string for safe use in shell commands."""
        if _rust_fo is not None:
            try:
                return _rust_fo.escape_shell_arg_py(arg)
            except Exception:
                pass
        # Use single quotes and escape any single quotes in the string
        return "'" + arg.replace("'", "'\"'\"'") + "'"
    
    def _unified_diff(self, old_content: str, new_content: str, filename: str) -> str:
        """Generate unified diff between old and new content."""
        if _rust_fo is not None:
            try:
                return _rust_fo.unified_diff_py(old_content, new_content, filename)
            except Exception:
                pass

        old_lines = old_content.splitlines(keepends=True)
        new_lines = new_content.splitlines(keepends=True)
        diff = difflib.unified_diff(
            old_lines, new_lines,
            fromfile=f"a/{filename}",
            tofile=f"b/{filename}"
        )
        return ''.join(diff)
    
    # =========================================================================
    # READ Implementation
    # =========================================================================
    
    def read_file(self, path: str, offset: int = 1, limit: int = 500) -> ReadResult:
        """
        Read a file with pagination, binary detection, and line numbers.
        Uses native Python I/O — no shell commands.

        Args:
            path: File path (absolute or relative to cwd)
            offset: Line number to start from (1-indexed, default 1)
            limit: Maximum lines to return (default 500, max 2000)

        Returns:
            ReadResult with content, metadata, or error info
        """
        # Expand ~ paths using native Python
        path = self._native_expand_path(path)
        limit = min(limit, MAX_LINES)

        # Get file size and check existence with os.stat()
        try:
            stat_result = os.stat(path)
            file_size = stat_result.st_size
        except OSError:
            return self._suggest_similar_files(path)

        # Images are never inlined — redirect to the vision tool
        if self._is_image(path):
            return ReadResult(
                is_image=True,
                is_binary=True,
                file_size=file_size,
                hint=(
                    "Image file detected. Automatically redirected to vision_analyze tool. "
                    "Use vision_analyze with this file path to inspect the image contents."
                ),
            )

        # Binary detection: read first 1000 bytes and check for non-printable chars
        try:
            with open(path, 'rb') as f:
                sample = f.read(1000)
            sample_text = sample.decode('utf-8', errors='replace')
        except OSError:
            sample_text = ''

        if self._is_likely_binary(path, sample_text):
            return ReadResult(
                is_binary=True,
                file_size=file_size,
                error="Binary file - cannot display as text. Use appropriate tools to handle this file type."
            )

        # Read the file content with pagination
        try:
            with open(path, 'r', encoding='utf-8', errors='replace') as f:
                all_lines = f.readlines()
        except OSError as e:
            return ReadResult(error=f"Failed to read file: {e}")

        total_lines = len(all_lines)
        end_line = offset + limit - 1  # inclusive

        # Clamp to available lines
        if offset > total_lines:
            return ReadResult(
                content='',
                total_lines=total_lines,
                file_size=file_size,
                truncated=False,
                hint=f"Offset {offset} exceeds file length ({total_lines} lines). Use offset=1."
            )

        slice_lines = all_lines[offset - 1:end_line]
        read_content = ''.join(slice_lines)
        truncated = total_lines > end_line

        hint = None
        if truncated:
            hint = f"Use offset={end_line + 1} to continue reading (showing {offset}-{end_line} of {total_lines} lines)"

        return ReadResult(
            content=self._add_line_numbers(read_content, offset),
            total_lines=total_lines,
            file_size=file_size,
            truncated=truncated,
            hint=hint
        )
    
    # Images larger than this are too expensive to inline as base64 in the
    # conversation context. Return metadata only and suggest vision_analyze.
    MAX_IMAGE_BYTES = 512 * 1024  # 512 KB

    def _read_image(self, path: str) -> ReadResult:
        """Read an image file, returning base64 content."""
        # Get file size (wc -c is POSIX, works on Linux + macOS)
        stat_cmd = f"wc -c < {self._escape_shell_arg(path)} 2>/dev/null"
        stat_result = self._exec(stat_cmd)
        try:
            file_size = int(stat_result.stdout.strip())
        except ValueError:
            file_size = 0
        
        if file_size > self.MAX_IMAGE_BYTES:
            return ReadResult(
                is_image=True,
                is_binary=True,
                file_size=file_size,
                hint=(
                    f"Image is too large to inline ({file_size:,} bytes). "
                    "Use vision_analyze to inspect the image, or reference it by path."
                ),
            )
        
        # Get base64 content — pipe through tr to strip newlines portably.
        # GNU base64 supports -w 0 but macOS base64 does not; both wrap by
        # default, so stripping with tr is portable across all backends.
        b64_cmd = f"base64 {self._escape_shell_arg(path)} 2>/dev/null | tr -d '\\n'"
        b64_result = self._exec(b64_cmd, timeout=30)
        
        if b64_result.exit_code != 0:
            return ReadResult(
                is_image=True,
                is_binary=True,
                file_size=file_size,
                error=f"Failed to read image: {b64_result.stdout}"
            )
        
        # Try to get dimensions (requires ImageMagick)
        dimensions = None
        if self._has_command('identify'):
            dim_cmd = f"identify -format '%wx%h' {self._escape_shell_arg(path)} 2>/dev/null"
            dim_result = self._exec(dim_cmd)
            if dim_result.exit_code == 0:
                dimensions = dim_result.stdout.strip()
        
        # Determine MIME type from extension
        ext = os.path.splitext(path)[1].lower()
        mime_types = {
            '.png': 'image/png',
            '.jpg': 'image/jpeg',
            '.jpeg': 'image/jpeg',
            '.gif': 'image/gif',
            '.webp': 'image/webp',
            '.bmp': 'image/bmp',
            '.ico': 'image/x-icon',
        }
        mime_type = mime_types.get(ext, 'application/octet-stream')
        
        return ReadResult(
            is_image=True,
            is_binary=True,
            file_size=file_size,
            base64_content=b64_result.stdout,
            mime_type=mime_type,
            dimensions=dimensions
        )
    
    def _suggest_similar_files(self, path: str) -> ReadResult:
        """Suggest similar files when the requested file is not found.
        Uses native Python os.listdir — no shell commands.
        """
        # Get directory and filename
        dir_path = os.path.dirname(path) or "."
        filename = os.path.basename(path)

        # Try Rust first (Rust walkdir is faster than os.listdir + character overlap)
        if _rust_fo is not None:
            try:
                similar_paths = _rust_fo.suggest_similar_files_py(path)
                return ReadResult(
                    error=f"File not found: {path}",
                    similar_files=similar_paths[:5]
                )
            except Exception:
                pass

        similar = []
        try:
            entries = os.listdir(dir_path)
        except OSError:
            return ReadResult(error=f"File not found: {path}", similar_files=[])

        for f in entries:
            # Simple similarity: files that share some characters with the target
            common = set(filename.lower()) & set(f.lower())
            if len(common) >= len(filename) * 0.5:  # 50% character overlap
                similar.append(os.path.join(dir_path, f))

        return ReadResult(
            error=f"File not found: {path}",
            similar_files=similar[:5]
        )
    
    # =========================================================================
    # WRITE Implementation
    # =========================================================================
    
    def write_file(self, path: str, content: str) -> WriteResult:
        """
        Write content to a file, creating parent directories as needed.

        Pipes content through stdin to avoid OS ARG_MAX limits on large
        files. The content never appears in the shell command string —
        only the file path does.

        Args:
            path: File path to write
            content: Content to write

        Returns:
            WriteResult with bytes written or error
        """
        # Expand ~ and other shell paths
        path = self._native_expand_path(path)

        # Block writes to sensitive paths
        if _is_write_denied(path):
            return WriteResult(error=f"Write denied: '{path}' is a protected system/credential file.")

        # Create parent directories
        parent = os.path.dirname(path)
        dirs_created = False
        
        if parent:
            mkdir_cmd = f"mkdir -p {self._escape_shell_arg(parent)}"
            mkdir_result = self._exec(mkdir_cmd)
            if mkdir_result.exit_code == 0:
                dirs_created = True
        
        # Write via stdin pipe — content bypasses shell arg parsing entirely,
        # so there's no ARG_MAX limit regardless of file size.
        write_cmd = f"cat > {self._escape_shell_arg(path)}"
        write_result = self._exec(write_cmd, stdin_data=content)
        
        if write_result.exit_code != 0:
            return WriteResult(error=f"Failed to write file: {write_result.stdout}")
        
        # Get bytes written (wc -c is POSIX, works on Linux + macOS)
        stat_cmd = f"wc -c < {self._escape_shell_arg(path)} 2>/dev/null"
        stat_result = self._exec(stat_cmd)
        
        try:
            bytes_written = int(stat_result.stdout.strip())
        except ValueError:
            bytes_written = len(content.encode('utf-8'))
        
        return WriteResult(
            bytes_written=bytes_written,
            dirs_created=dirs_created
        )
    
    # =========================================================================
    # PATCH Implementation (Replace Mode)
    # =========================================================================
    
    def patch_replace(self, path: str, old_string: str, new_string: str,
                      replace_all: bool = False) -> PatchResult:
        """
        Replace text in a file using fuzzy matching.

        Args:
            path: File path to modify
            old_string: Text to find (must be unique unless replace_all=True)
            new_string: Replacement text
            replace_all: If True, replace all occurrences

        Returns:
            PatchResult with diff and lint results
        """
        # Expand ~ and other shell paths (native Python, no shell)
        path = self._native_expand_path(path)

        # Block writes to sensitive paths
        if _is_write_denied(path):
            return PatchResult(error=f"Write denied: '{path}' is a protected system/credential file.")

        # Read current content (native Python I/O)
        try:
            with open(path, 'r', encoding='utf-8', errors='replace') as f:
                content = f.read()
        except OSError as e:
            return PatchResult(error=f"Failed to read file: {e}")
        
        # Import and use fuzzy matching
        from tools.fuzzy_match import fuzzy_find_and_replace
        
        new_content, match_count, error = fuzzy_find_and_replace(
            content, old_string, new_string, replace_all
        )
        
        if error:
            return PatchResult(error=error)
        
        if match_count == 0:
            return PatchResult(error=f"Could not find match for old_string in {path}")
        
        # Write back
        write_result = self.write_file(path, new_content)
        if write_result.error:
            return PatchResult(error=f"Failed to write changes: {write_result.error}")
        
        # Generate diff
        diff = self._unified_diff(content, new_content, path)
        
        # Auto-lint
        lint_result = self._check_lint(path)
        
        return PatchResult(
            success=True,
            diff=diff,
            files_modified=[path],
            lint=lint_result.to_dict() if lint_result else None
        )
    
    def patch_v4a(self, patch_content: str) -> PatchResult:
        """
        Apply a V4A format patch.
        
        V4A format:
            *** Begin Patch
            *** Update File: path/to/file.py
            @@ context hint @@
             context line
            -removed line
            +added line
            *** End Patch
        
        Args:
            patch_content: V4A format patch string
        
        Returns:
            PatchResult with changes made
        """
        # Import patch parser
        from tools.patch_parser import parse_v4a_patch, apply_v4a_operations
        
        operations, parse_error = parse_v4a_patch(patch_content)
        if parse_error:
            return PatchResult(error=f"Failed to parse patch: {parse_error}")
        
        # Apply operations
        result = apply_v4a_operations(operations, self)
        return result
    
    def _check_lint(self, path: str) -> LintResult:
        """
        Run syntax check on a file after editing.
        
        Args:
            path: File path to lint
        
        Returns:
            LintResult with status and any errors
        """
        ext = os.path.splitext(path)[1].lower()
        
        if ext not in LINTERS:
            return LintResult(skipped=True, message=f"No linter for {ext} files")
        
        # Check if linter command is available
        linter_cmd = LINTERS[ext]
        # Extract the base command (first word)
        base_cmd = linter_cmd.split()[0]
        
        if not self._has_command(base_cmd):
            return LintResult(skipped=True, message=f"{base_cmd} not available")
        
        # Run linter
        cmd = linter_cmd.format(file=self._escape_shell_arg(path))
        result = self._exec(cmd, timeout=30)
        
        return LintResult(
            success=result.exit_code == 0,
            output=result.stdout.strip() if result.stdout.strip() else ""
        )
    
    # =========================================================================
    # SEARCH Implementation
    # =========================================================================
    
    def search(self, pattern: str, path: str = ".", target: str = "content",
               file_glob: Optional[str] = None, limit: int = 50, offset: int = 0,
               output_mode: str = "content", context: int = 0) -> SearchResult:
        """
        Search for content or files.
        
        Args:
            pattern: Regex (for content) or glob pattern (for files)
            path: Directory/file to search (default: cwd)
            target: "content" (grep) or "files" (glob)
            file_glob: File pattern filter for content search (e.g., "*.py")
            limit: Max results (default 50)
            offset: Skip first N results
            output_mode: "content", "files_only", or "count"
            context: Lines of context around matches
        
        Returns:
            SearchResult with matches or file list
        """
        # Expand ~ and other shell paths (native Python, no shell)
        path = self._native_expand_path(path)

        # Validate that the path exists before searching
        if not os.path.exists(path):
            return SearchResult(
                error=f"Path not found: {path}. Verify the path exists (use 'terminal' to check).",
                total_count=0
            )
        
        if target == "files":
            return self._search_files(pattern, path, limit, offset)
        else:
            return self._search_content(pattern, path, file_glob, limit, offset, 
                                        output_mode, context)
    
    def _search_files(self, pattern: str, path: str, limit: int, offset: int) -> SearchResult:
        """Search for files by name pattern (glob-like).
        
        Tries (in order):
        1. Native Python glob — bypasses shell entirely
        2. Direct ripgrep --files subprocess — bypasses bash -lic overhead
        3. Shell find fallback
        """
        # 1. Native Python glob — fast, works everywhere, no shell overhead
        native_result = self._search_files_native(pattern, path, limit, offset)
        if native_result is not None:
            return native_result

        # 2. Direct ripgrep --files subprocess
        rg_result = self._search_files_rg_subprocess(pattern, path, limit, offset)
        if rg_result is not None:
            return rg_result

        # 3. Shell fallback
        # Auto-prepend **/ for recursive search if not already present
        if not pattern.startswith('**/') and '/' not in pattern:
            search_pattern = pattern
        else:
            search_pattern = pattern.split('/')[-1]

        # Prefer ripgrep: respects .gitignore, excludes hidden dirs by
        # default, and has parallel directory traversal (~200x faster than
        # find on wide trees).  Mirrors _search_content which already uses rg.
        if self._has_command('rg'):
            return self._search_files_rg(search_pattern, path, limit, offset)

        # Fallback: find (slower, no .gitignore awareness)
        if not self._has_command('find'):
            return SearchResult(
                error="File search requires 'rg' (ripgrep) or 'find'. "
                      "Install ripgrep for best results: "
                      "https://github.com/BurntSushi/ripgrep#installation"
            )

        # Exclude hidden directories (matching ripgrep's default behavior).
        hidden_exclude = "-not -path '*/.*'"

        cmd = f"find {self._escape_shell_arg(path)} {hidden_exclude} -type f -name {self._escape_shell_arg(search_pattern)} " \
              f"-printf '%T@ %p\\n' 2>/dev/null | sort -rn | tail -n +{offset + 1} | head -n {limit}"

        result = self._exec(cmd, timeout=60)

        if not result.stdout.strip():
            # Try without -printf (BSD find compatibility -- macOS)
            cmd_simple = f"find {self._escape_shell_arg(path)} {hidden_exclude} -type f -name {self._escape_shell_arg(search_pattern)} " \
                        f"2>/dev/null | head -n {limit + offset} | tail -n +{offset + 1}"
            result = self._exec(cmd_simple, timeout=60)

        files = []
        for line in result.stdout.strip().split('\n'):
            if not line:
                continue
            parts = line.split(' ', 1)
            if len(parts) == 2 and parts[0].replace('.', '').isdigit():
                files.append(parts[1])
            else:
                files.append(line)

        return SearchResult(
            files=files,
            total_count=len(files)
        )

    def _search_files_rg(self, pattern: str, path: str, limit: int, offset: int) -> SearchResult:
        """Search for files by name using ripgrep's --files mode.

        rg --files respects .gitignore and excludes hidden directories by
        default, and uses parallel directory traversal for ~200x speedup
        over find on wide trees.
        """
        # rg --files -g uses glob patterns; wrap bare names so they match
        # at any depth (equivalent to find -name).
        if '/' not in pattern and not pattern.startswith('*'):
            glob_pattern = f"*{pattern}"
        else:
            glob_pattern = pattern

        fetch_limit = limit + offset
        cmd = (
            f"rg --files -g {self._escape_shell_arg(glob_pattern)} "
            f"{self._escape_shell_arg(path)} 2>/dev/null "
            f"| head -n {fetch_limit}"
        )
        result = self._exec(cmd, timeout=60)

        all_files = [f for f in result.stdout.strip().split('\n') if f]
        page = all_files[offset:offset + limit]

        return SearchResult(
            files=page,
            total_count=len(all_files),
            truncated=len(all_files) >= fetch_limit,
        )
    
    def _search_content(self, pattern: str, path: str, file_glob: Optional[str],
                        limit: int, offset: int, output_mode: str, context: int) -> SearchResult:
        """Search for content inside files (grep-like).
        
        Tries (in order):
        1. Native Python os.walk + re.search  — bypasses shell entirely
        2. Direct subprocess.run of ripgrep     — bypasses bash -lic login shell
        3. Shell _exec fallback                 — original path for docker/ssh backends
        """
        # 1. Native Python search — always try first (fastest for local; works everywhere)
        native_result = self._search_native(pattern, path, file_glob, limit, offset,
                                            output_mode, context)
        if native_result is not None:
            return native_result

        # 2. Direct ripgrep subprocess — bypasses bash -lic login shell overhead
        rg_result = self._search_with_rg_subprocess(
            pattern, path, file_glob, limit, offset, output_mode, context
        )
        if rg_result is not None:
            return rg_result

        # 3. Shell fallback (docker/ssh remote environments)
        if self._has_command('rg'):
            return self._search_with_rg(pattern, path, file_glob, limit, offset,
                                        output_mode, context)
        elif self._has_command('grep'):
            return self._search_with_grep(pattern, path, file_glob, limit, offset,
                                          output_mode, context)
        else:
            return SearchResult(
                error="Content search requires ripgrep (rg) or grep. "
                      "Install ripgrep: https://github.com/BurntSushi/ripgrep#installation"
            )
    

    # -------------------------------------------------------------------------
    # Native Python + direct-subprocess search (no bash -lic shell overhead)
    # -------------------------------------------------------------------------

    def _get_rg_binary(self) -> Optional[str]:
        """Find ripgrep binary once, cache it. Returns None if not found."""
        global _RG_BINARY
        if _RG_BINARY is not None:
            return _RG_BINARY if _RG_BINARY is not False else None
        with _RG_BINARY_LOCK:
            if _RG_BINARY is None:
                path = shutil.which("rg")
                _RG_BINARY = path or False
            return _RG_BINARY if _RG_BINARY is not False else None

    def _search_files_native(self, pattern: str, path: str,
                             limit: int, offset: int) -> Optional[SearchResult]:
        """Search for files by name using native Python glob -- no shell.
        
        Returns None on any I/O error so caller falls back gracefully.
        """
        try:
            path = os.path.abspath(os.path.expanduser(path))
            if not os.path.isdir(path):
                return None

            # Convert our pattern to a Python glob pattern
            if pattern.startswith('**/'):
                glob_pattern = pattern[3:]
                recursive = True
            elif '/' in pattern:
                parts = pattern.split('/')
                glob_pattern = os.path.join(*parts)
                recursive = '*' in pattern or '?' in pattern
            else:
                glob_pattern = "*" + pattern + "*"
                recursive = True

            matches = []
            hidden = {'.git', 'node_modules', '__pycache__', '.hub', '.venv', 'venv'}
            for root, dirs, files in os.walk(path):
                dirs[:] = [d for d in dirs if d not in hidden and not d.startswith('.')]
                for filename in files:
                    if filename.startswith('.'):
                        continue
                    if glob_module.fnmatch.fnmatch(filename, glob_pattern):
                        matches.append(os.path.join(root, filename))
                if len(matches) > (limit + offset) * 2 and recursive:
                    break

            total = len(matches)
            return SearchResult(
                files=matches[offset:offset + limit],
                total_count=total,
                truncated=total > limit + offset
            )
        except (OSError, PermissionError):
            return None

    def _search_native(self, pattern: str, path: str, file_glob: Optional[str],
                        limit: int, offset: int, output_mode: str,
                        context: int) -> Optional[SearchResult]:
        """Search for content using os.walk + re.search -- no shell, no subprocess.
        
        Returns None on any exception so caller falls back to ripgrep subprocess.
        """
        # Try Rust first (Rust os.walk is ~5-10x faster than Python's)
        if _rust_fo is not None:
            try:
                rust_result = _rust_fo.search_native_py(
                    pattern, path, file_glob,
                    limit, offset, output_mode, context
                )
                if rust_result is not None:
                    import json
                    data = json.loads(rust_result)
                    matches = [
                        SearchMatch(
                            path=m["path"],
                            line_number=m["line"],
                            content=m["content"],
                        )
                        for m in data.get("matches", [])
                    ]
                    return SearchResult(
                        matches=matches,
                        total_count=data.get("total_count", len(matches)),
                        truncated=data.get("truncated", False),
                    )
            except Exception:
                pass

        try:
            path = os.path.abspath(os.path.expanduser(path))
            if not os.path.isdir(path):
                return None
            try:
                compiled = re.compile(pattern)
            except re.error:
                return None
            matches: List[SearchMatch] = []
            hidden = {'.git', 'node_modules', '__pycache__', '.hub', '.venv', 'venv'}
            for root, dirs, files in os.walk(path):
                dirs[:] = [d for d in dirs if d not in hidden and not d.startswith('.')]
                for filename in files:
                    if filename.startswith('.'):
                        continue
                    if file_glob and not glob_module.fnmatch.fnmatch(filename, file_glob):
                        continue
                    ext = os.path.splitext(filename)[1].lower()
                    if ext in BINARY_EXTENSIONS:
                        continue
                    filepath = os.path.join(root, filename)
                    try:
                        with open(filepath, 'r', encoding='utf-8', errors='replace') as f:
                            for lineno, line in enumerate(f, 1):
                                if compiled.search(line):
                                    matches.append(SearchMatch(
                                        path=filepath,
                                        line_number=lineno,
                                        content=line.rstrip('\n\r')[:500]
                                    ))
                                    if len(matches) >= limit + offset:
                                        break
                    except (OSError, UnicodeDecodeError):
                        continue
                    if len(matches) >= limit + offset:
                        break
                if len(matches) >= limit + offset:
                    break
            total = len(matches)
            return SearchResult(
                matches=matches[offset:offset + limit],
                total_count=total,
                truncated=total > limit + offset
            )
        except Exception:
            return None

    def _rg_subprocess(self, args: List[str], timeout: int = 60) -> tuple:
        """Run ripgrep as a direct subprocess, bypassing bash -lic."""
        import sys, time as _time, os as _os
        rg_path = self._get_rg_binary()
        if rg_path is None:
            sys.stderr.write(f"[_rg_subprocess] rg_path=None, returning (-1)\n")
            sys.stderr.flush()
            return ("", -1)
        sys.stderr.write(f"[_rg_subprocess] cmd=[rg_path] + {args}, cwd={self.cwd!r}\n")
        sys.stderr.flush()
        t0 = _time.time()
        try:
            result = subprocess.run(
                [rg_path] + args,
                capture_output=True,
                text=True,
                timeout=timeout,
                cwd=self.cwd or None,
            )
            sys.stderr.write(f"[_rg_subprocess] done in {_time.time()-t0:.3f}s, rc={result.returncode}\n")
            sys.stderr.flush()
            return (result.stdout, result.returncode)
        except subprocess.TimeoutExpired:
            sys.stderr.write(f"[_rg_subprocess] TIMEOUT after {timeout}s\n")
            sys.stderr.flush()
            return ("", -1)
        except (FileNotFoundError, OSError) as e:
            sys.stderr.write(f"[_rg_subprocess] OSError: {e}\n")
            sys.stderr.flush()
            return ("", -1)

    def _search_with_rg_subprocess(self, pattern: str, path: str,
                                     file_glob: Optional[str], limit: int,
                                     offset: int, output_mode: str,
                                     context: int) -> Optional[SearchResult]:
        """Search using ripgrep via direct subprocess -- bypasses bash -lic."""
        cmd = ["--line-number", "--no-heading", "--with-filename"]
        if context > 0:
            cmd.extend(["-C", str(context)])
        if file_glob:
            cmd.extend(["--glob", file_glob])
        if output_mode == "files_only":
            cmd.append("-l")
        elif output_mode == "count":
            cmd.append("-c")
        cmd.append(pattern)
        cmd.append(path)
        stdout, code = self._rg_subprocess(cmd)
        if code == -1:
            return None
        if code == 2 and not stdout.strip():
            return SearchResult(error="Search failed", total_count=0)
        if output_mode == "files_only":
            all_files = [f for f in stdout.strip().split('\n') if f]
            total = len(all_files)
            return SearchResult(files=all_files[offset:offset + limit], total_count=total)
        elif output_mode == "count":
            counts = {}
            for line in stdout.strip().split('\n'):
                if ':' in line:
                    parts = line.rsplit(':', 1)
                    try:
                        counts[parts[0]] = int(parts[1])
                    except ValueError:
                        pass
            return SearchResult(counts=counts, total_count=sum(counts.values()))
        else:
            _match_re = re.compile(r'^([A-Za-z]:)?(.*?):(\d+):(.*)$')
            matches = []
            for line in stdout.strip().split('\n'):
                if not line or line == "--":
                    continue
                m = _match_re.match(line)
                if m:
                    matches.append(SearchMatch(
                        path=(m.group(1) or '') + m.group(2),
                        line_number=int(m.group(3)),
                        content=m.group(4)[:500]
                    ))
            total = len(matches)
            return SearchResult(
                matches=matches[offset:offset + limit],
                total_count=total,
                truncated=total > limit + offset
            )

    def _search_files_rg_subprocess(self, pattern: str, path: str,
                                     limit: int, offset: int) -> Optional[SearchResult]:
        """Search for files using ripgrep --files via direct subprocess."""
        if '/' not in pattern and not pattern.startswith('*'):
            glob_pattern = "*" + pattern + "*"
        else:
            glob_pattern = pattern
        stdout, code = self._rg_subprocess(["--files", "-g", glob_pattern, path])
        if code == -1:
            return None
        all_files = [f for f in stdout.strip().split('\n') if f]
        total = len(all_files)
        return SearchResult(
            files=all_files[offset:offset + limit],
            total_count=total,
            truncated=total >= limit + offset
        )

    def _search_with_rg(self, pattern: str, path: str, file_glob: Optional[str],
                        limit: int, offset: int, output_mode: str, context: int) -> SearchResult:
        """Search using ripgrep."""
        cmd_parts = ["rg", "--line-number", "--no-heading", "--with-filename"]
        
        # Add context if requested
        if context > 0:
            cmd_parts.extend(["-C", str(context)])
        
        # Add file glob filter (must be quoted to prevent shell expansion)
        if file_glob:
            cmd_parts.extend(["--glob", self._escape_shell_arg(file_glob)])
        
        # Output mode handling
        if output_mode == "files_only":
            cmd_parts.append("-l")  # Files only
        elif output_mode == "count":
            cmd_parts.append("-c")  # Count per file
        
        # Add pattern and path
        cmd_parts.append(self._escape_shell_arg(pattern))
        cmd_parts.append(self._escape_shell_arg(path))
        
        # Fetch extra rows so we can report the true total before slicing.
        # For context mode, rg emits separator lines ("--") between groups,
        # so we grab generously and filter in Python.
        fetch_limit = limit + offset + 200 if context > 0 else limit + offset
        cmd_parts.extend(["|", "head", "-n", str(fetch_limit)])
        
        cmd = " ".join(cmd_parts)
        result = self._exec(cmd, timeout=60)
        
        # rg exit codes: 0=matches found, 1=no matches, 2=error
        if result.exit_code == 2 and not result.stdout.strip():
            error_msg = result.stderr.strip() if hasattr(result, 'stderr') and result.stderr else "Search error"
            return SearchResult(error=f"Search failed: {error_msg}", total_count=0)
        
        # Parse results based on output mode
        if output_mode == "files_only":
            all_files = [f for f in result.stdout.strip().split('\n') if f]
            total = len(all_files)
            page = all_files[offset:offset + limit]
            return SearchResult(files=page, total_count=total)
        
        elif output_mode == "count":
            counts = {}
            for line in result.stdout.strip().split('\n'):
                if ':' in line:
                    parts = line.rsplit(':', 1)
                    if len(parts) == 2:
                        try:
                            counts[parts[0]] = int(parts[1])
                        except ValueError:
                            pass
            return SearchResult(counts=counts, total_count=sum(counts.values()))
        
        else:
            # Parse content matches and context lines.
            # rg match lines:   "file:lineno:content"  (colon separator)
            # rg context lines: "file-lineno-content"   (dash separator)
            # rg group seps:    "--"
            # Note: on Windows, paths contain drive letters (e.g. C:\path),
            # so naive split(":") breaks. Use regex to handle both platforms.
            _match_re = re.compile(r'^([A-Za-z]:)?(.*?):(\d+):(.*)$')
            _ctx_re = re.compile(r'^([A-Za-z]:)?(.*?)-(\d+)-(.*)$')
            matches = []
            for line in result.stdout.strip().split('\n'):
                if not line or line == "--":
                    continue
                
                # Try match line first (colon-separated: file:line:content)
                m = _match_re.match(line)
                if m:
                    matches.append(SearchMatch(
                        path=(m.group(1) or '') + m.group(2),
                        line_number=int(m.group(3)),
                        content=m.group(4)[:500]
                    ))
                    continue
                
                # Try context line (dash-separated: file-line-content)
                # Only attempt if context was requested to avoid false positives
                if context > 0:
                    m = _ctx_re.match(line)
                    if m:
                        matches.append(SearchMatch(
                            path=(m.group(1) or '') + m.group(2),
                            line_number=int(m.group(3)),
                            content=m.group(4)[:500]
                        ))
            
            total = len(matches)
            page = matches[offset:offset + limit]
            return SearchResult(
                matches=page,
                total_count=total,
                truncated=total > offset + limit
            )
    
    def _search_with_grep(self, pattern: str, path: str, file_glob: Optional[str],
                          limit: int, offset: int, output_mode: str, context: int) -> SearchResult:
        """Fallback search using grep."""
        cmd_parts = ["grep", "-rnH"]  # -H forces filename even for single-file searches
        
        # Exclude hidden directories (matching ripgrep's default behavior).
        # This prevents searching inside .hub/index-cache/, .git/, etc.
        cmd_parts.append("--exclude-dir='.*'")
        
        # Add context if requested
        if context > 0:
            cmd_parts.extend(["-C", str(context)])
        
        # Add file pattern filter (must be quoted to prevent shell expansion)
        if file_glob:
            cmd_parts.extend(["--include", self._escape_shell_arg(file_glob)])
        
        # Output mode handling
        if output_mode == "files_only":
            cmd_parts.append("-l")
        elif output_mode == "count":
            cmd_parts.append("-c")
        
        # Add pattern and path
        cmd_parts.append(self._escape_shell_arg(pattern))
        cmd_parts.append(self._escape_shell_arg(path))
        
        # Fetch generously so we can compute total before slicing
        fetch_limit = limit + offset + (200 if context > 0 else 0)
        cmd_parts.extend(["|", "head", "-n", str(fetch_limit)])
        
        cmd = " ".join(cmd_parts)
        result = self._exec(cmd, timeout=60)
        
        # grep exit codes: 0=matches found, 1=no matches, 2=error
        if result.exit_code == 2 and not result.stdout.strip():
            error_msg = result.stderr.strip() if hasattr(result, 'stderr') and result.stderr else "Search error"
            return SearchResult(error=f"Search failed: {error_msg}", total_count=0)
        
        if output_mode == "files_only":
            all_files = [f for f in result.stdout.strip().split('\n') if f]
            total = len(all_files)
            page = all_files[offset:offset + limit]
            return SearchResult(files=page, total_count=total)
        
        elif output_mode == "count":
            counts = {}
            for line in result.stdout.strip().split('\n'):
                if ':' in line:
                    parts = line.rsplit(':', 1)
                    if len(parts) == 2:
                        try:
                            counts[parts[0]] = int(parts[1])
                        except ValueError:
                            pass
            return SearchResult(counts=counts, total_count=sum(counts.values()))
        
        else:
            # grep match lines:   "file:lineno:content" (colon)
            # grep context lines: "file-lineno-content"  (dash)
            # grep group seps:    "--"
            # Note: on Windows, paths contain drive letters (e.g. C:\path),
            # so naive split(":") breaks. Use regex to handle both platforms.
            _match_re = re.compile(r'^([A-Za-z]:)?(.*?):(\d+):(.*)$')
            _ctx_re = re.compile(r'^([A-Za-z]:)?(.*?)-(\d+)-(.*)$')
            matches = []
            for line in result.stdout.strip().split('\n'):
                if not line or line == "--":
                    continue
                
                m = _match_re.match(line)
                if m:
                    matches.append(SearchMatch(
                        path=(m.group(1) or '') + m.group(2),
                        line_number=int(m.group(3)),
                        content=m.group(4)[:500]
                    ))
                    continue
                
                if context > 0:
                    m = _ctx_re.match(line)
                    if m:
                        matches.append(SearchMatch(
                            path=(m.group(1) or '') + m.group(2),
                            line_number=int(m.group(3)),
                            content=m.group(4)[:500]
                        ))

            
            total = len(matches)
            page = matches[offset:offset + limit]
            return SearchResult(
                matches=page,
                total_count=total,
                truncated=total > offset + limit
            )
