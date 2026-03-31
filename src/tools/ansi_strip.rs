//! Strip ANSI escape sequences from subprocess output.
//!
//! Used by terminal_tool, code_execution_tool, and process_registry to clean
//! command output before returning it to the model.  This prevents ANSI codes
//! from entering the model's context — which is the root cause of models
//! copying escape sequences into file writes.
//!
//! Covers the full ECMA-48 spec: CSI (including private-mode `?` prefix,
//! colon-separated params, intermediate bytes), OSC (BEL and ST terminators),
//! DCS/SOS/PM/APC string sequences, nF multi-byte escapes, Fp/Fe/Fs
//! single-byte escapes, and 8-bit C1 control characters.

use once_cell::sync::Lazy;
use regex::Regex;

/// Core ANSI escape regex — matches all ECMA-48 escape sequences.
///
/// Components:
/// - `\x1b` — ESC / CSI introducer
/// - CSI: `\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]` — 7-bit CSI (params + intermediates + final byte)
/// - OSC: `][\s\S]*?(?:\x07|\x1b\\)` — Operating System Command, BEL or ST terminator
/// - DCS/SOS/PM/APC: `[PX^_][\s\S]*?(?:\x1b\\)` — string sequences
/// - nF: `[\x20-\x2f]+[\x30-\x7e]` — Fe-type escapes with parameter bytes
/// - Fp/Fe/Fs: `[\x30-\x7e]` — standalone final bytes
/// - 8-bit CSI: `\x9b[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]`
/// - 8-bit OSC: `\x9d[\s\S]*?(?:\x07|\x9c)`
/// - Other 8-bit C1: `[\x80-\x9f]`
static ANSI_ESCAPE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\x1b"
        r"(?:"
            r"\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]"       // CSI sequence
            r"|][\s\S]*?(?:\x07|\x1b\\)"                  // OSC (BEL or ST terminator)
            r"|[PX^_][\s\S]*?(?:\x1b\\)"                   // DCS/SOS/PM/APC strings
            r"|[\x20-\x2f]+[\x30-\x7e]"                    // nF escape sequences
            r"|[\x30-\x7e]"                                // Fp/Fe/Fs single-byte
        r")"
        r"|\x9b[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]"       // 8-bit CSI
        r"|\x9d[\s\S]*?(?:\x07|\x9c)"                      // 8-bit OSC
        r"|[\x80-\x9f]"                                    // Other 8-bit C1 controls
    )
    .expect("invalid ANSI_ESCAPE_RE regex")
});

/// Fast-path check — skip full regex when no escape-like bytes are present.
static HAS_ESCAPE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[\x1b\x80-\x9f]").expect("invalid HAS_ESCAPE regex")
});

/// Remove ANSI escape sequences from text.
///
/// Returns the input unchanged (fast path) when no ESC or C1 bytes are
/// present.  Safe to call on any string — clean text passes through
/// with negligible overhead.
#[inline]
pub fn strip_ansi(text: &str) -> String {
    if text.is_empty() || !HAS_ESCAPE.is_match(text) {
        return text.to_string();
    }
    ANSI_ESCAPE_RE.replace_all(text, "").to_string()
}

/// In-place variant that avoids allocation when no ANSI sequences are found.
#[inline]
pub fn strip_ansi_in_place(text: &mut String) {
    if text.is_empty() || !HAS_ESCAPE.is_match(text) {
        return;
    }
    *text = ANSI_ESCAPE_RE.replace_all(text, "").to_string();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_text_unchanged() {
        let input = "Hello, world! No ANSI here.";
        assert_eq!(strip_ansi(input), input);
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_csi_color_codes() {
        // \x1b[31m = red, \x1b[0m = reset
        let input = "\x1b[31mError:\x1b[0m Something went wrong";
        assert_eq!(strip_ansi(input), "Error: Something went wrong");
    }

    #[test]
    fn test_csi_cursor_movement() {
        // \x1b[10A = cursor up 10, \x1b[5B = cursor down 5
        let input = "\x1b[10A\x1b[5Bsome text";
        assert_eq!(strip_ansi(input), "some text");
    }

    #[test]
    fn test_osc_window_title() {
        // \x1b]0;title\x07 = set window title
        let input = "\x1b]0;My Terminal\x07Normal text";
        assert_eq!(strip_ansi(input), "Normal text");
    }

    #[test]
    fn test_osc_bel_terminator() {
        let input = "\x1b]0;title\x07";
        assert_eq!(strip_ansi(input), "");
    }

    #[test]
    fn test_osc_st_terminator() {
        let input = "\x1b]0;title\x1b\\";
        assert_eq!(strip_ansi(input), "");
    }

    #[test]
    fn test_private_mode_csi() {
        // \x1b[?1049h = alternate buffer (private-mode ? prefix)
        let input = "\x1b[?1049h\x1b[?1049l";
        assert_eq!(strip_ansi(input), "");
    }

    #[test]
    fn test_dcs_sequence() {
        // DCS + ST terminator
        let input = "\x1bP+test\x1b\\normal";
        assert_eq!(strip_ansi(input), "normal");
    }

    #[test]
    fn test_sgr_parameters() {
        // \x1b[38;2;255;0;0m = set RGB foreground
        let input = "\x1b[38;2;255;0;0mRed text\x1b[0m";
        assert_eq!(strip_ansi(input), "Red text");
    }

    #[test]
    fn test_8bit_c1_controls() {
        // 8-bit C1 set — some terminals use these directly
        let input = "\x9b31mError\x9b0m";
        assert_eq!(strip_ansi(input), "Error");
    }

    #[test]
    fn test_8bit_osc() {
        let input = "\x9d0;title\x07";
        assert_eq!(strip_ansi(input), "");
    }

    #[test]
    fn test_mixed_realistic_output() {
        let input = "\x1b[32m✓\x1b[0m \x1b[1mBold text\x1b[0m \x1b[3mItalic\x1b[0m";
        assert_eq!(strip_ansi(input), "✓ Bold text Italic");
    }

    #[test]
    fn test_progress_bar_ansi() {
        // Simulates a terminal progress bar with color and cursor movement
        let input = "\x1b[?25l\x1b[2K\r\x1b[32m[####    ]\x1b[0m 40%";
        assert_eq!(strip_ansi(input), "[####    ] 40%");
    }

    #[test]
    fn test_ansi_in_place() {
        let mut text = "\x1b[31mRed\x1b[0m normal".to_string();
        strip_ansi_in_place(&mut text);
        assert_eq!(text, "Red normal");
    }

    #[test]
    fn test_ansi_in_place_no_change() {
        let mut text = "clean text".to_string();
        strip_ansi_in_place(&mut text);
        assert_eq!(text, "clean text");
    }
}
