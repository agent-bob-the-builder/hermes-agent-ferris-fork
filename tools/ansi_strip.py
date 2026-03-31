"""Strip ANSI escape sequences from subprocess output.

Used by terminal_tool, code_execution_tool, and process_registry to clean
command output before returning it to the model.  This prevents ANSI codes
from entering the model's context — which is the root cause of models
copying escape sequences into file writes.

Covers the full ECMA-48 spec: CSI (including private-mode ``?`` prefix,
colon-separated params, intermediate bytes), OSC (BEL and ST terminators),
DCS/SOS/PM/APC string sequences, nF multi-byte escapes, Fp/Fe/Fs
single-byte escapes, and 8-bit C1 control characters.

Uses the Rust `rust_ansi_strip` accelerator when available, falling back
to the pure-Python implementation for compatibility.
"""

import logging
import re

logger = logging.getLogger(__name__)

# -----------------------------------------------------------------------
# Rust accelerator — fast path, loaded at import time
# -----------------------------------------------------------------------
_rust_strip = None
_using_rust = False
try:
    import rust_ansi_strip

    # Verify it actually works (PyInit must run successfully)
    rust_ansi_strip.strip_ansi_text("")
    _rust_strip = rust_ansi_strip
    _using_rust = True
except Exception as _e:
    _rust_strip = None
    _using_rust = False
    logger.debug("rust_ansi_strip unavailable, using pure-Python ansi_strip: %s", _e)

# -----------------------------------------------------------------------
# Pure-Python implementation (fallback)
# -----------------------------------------------------------------------

_ANSI_ESCAPE_RE = re.compile(
    r"\x1b"
    r"(?:"
        r"\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]"     # CSI sequence
        r"|\][\s\S]*?(?:\x07|\x1b\\)"                  # OSC (BEL or ST terminator)
        r"|[PX^_][\s\S]*?(?:\x1b\\)"                   # DCS/SOS/PM/APC strings
        r"|[\x20-\x2f]+[\x30-\x7e]"                    # nF escape sequences
        r"|[\x30-\x7e]"                                 # Fp/Fe/Fs single-byte
    r")"
    r"|\x9b[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]"       # 8-bit CSI
    r"|\x9d[\s\S]*?(?:\x07|\x9c)"                       # 8-bit OSC
    r"|[\x80-\x9f]",                                    # Other 8-bit C1 controls
    re.DOTALL,
)

# Fast-path check — skip full regex when no escape-like bytes are present.
_HAS_ESCAPE = re.compile(r"[\x1b\x80-\x9f]")


def strip_ansi(text: str) -> str:
    """Remove ANSI escape sequences from text.

    Returns the input unchanged (fast path) when no ESC or C1 bytes are
    present.  Safe to call on any string — clean text passes through
    with negligible overhead.

    Uses the Rust `rust_ansi_strip` accelerator when available for ~5x throughput.
    """
    if _rust_strip is not None:
        return _rust_strip.strip_ansi_text(text)
    if not text or not _HAS_ESCAPE.search(text):
        return text
    return _ANSI_ESCAPE_RE.sub("", text)
