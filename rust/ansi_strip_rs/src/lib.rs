//! PyO3 bindings for the Rust ANSI escape sequence stripper.
//!
//! Used by terminal_tool, code_execution_tool, and process_registry.

use once_cell::sync::Lazy;
use regex::Regex;

static ANSI_ESCAPE_RE: Lazy<Regex> = Lazy::new(|| {
    // ANSI escape sequences (ECMA-48):
    // CSI: ESC [ params intermediates final
    // OSC: ESC ] params BEL|ST(ESC \)  
    // DCS: ESC P/X/^/_ params ST(ESC \)
    // Fe:  ESC ^/_/] params Fe terminator
    // 8-bit C1 controls (UTF-8 encoded as UTF-8 C1, or single U+009X chars):
    //   CSI: U+009B + params + final
    //   OSC: U+009D + params + BEL/U+009C
    //   OSC ST: U+009C (8-bit string terminator)
    let pat = r"\x1b(?:\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]|\x1b\u{009D}[\s\S]*?(?:[\x07\u{009C}])|\x1b[PX^_][\s\S]*?(?:\x1b\\)|[\x20-\x2f]+[\x30-\x7e]|[\x30-\x7e])|\u{009B}[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]|\u{009D}[\s\S]*?(?:[\x07\u{009C}])|[\u{80}-\u{9F}]";
    Regex::new(pat).expect("invalid ANSI_ESCAPE_RE")
});

static HAS_ESCAPE: Lazy<Regex> = Lazy::new(|| {
    // Quick check: does the text contain any ESC or 8-bit C1 char?
    // Matches: 7-bit ESC (\x1b), or UTF-8 C1 lead bytes (0xc2) followed by trail
    Regex::new(r"[\x1b\xc2]").expect("invalid HAS_ESCAPE")
});

#[inline]
pub fn strip_ansi(text: &str) -> String {
    if text.is_empty() || !HAS_ESCAPE.is_match(text) {
        return text.to_string();
    }
    ANSI_ESCAPE_RE.replace_all(text, "").to_string()
}

use pyo3::prelude::*;

#[pyfunction]
fn strip_ansi_text(text: &str) -> String { strip_ansi(text) }

#[pymodule]
fn ansi_strip_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(strip_ansi_text, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean() { assert_eq!(strip_ansi("hello"), "hello"); }

    #[test]
    fn test_empty() { assert_eq!(strip_ansi(""), ""); }

    #[test]
    fn test_csi() { assert_eq!(strip_ansi("\x1b[31mError\x1b[0m"), "Error"); }

    #[test]
    fn test_osc_bel() { assert_eq!(strip_ansi("\x1b]0;title\x07"), ""); }

    #[test]
    fn test_osc_st() { assert_eq!(strip_ansi("\x1b]0;title\x1b\\"), ""); }

    #[test]
    fn test_8b_csi() { assert_eq!(strip_ansi("\u{009B}31mError\u{009B}0m"), "Error"); }

    #[test]
    fn test_8b_osc() { assert_eq!(strip_ansi("\u{009D}0;title\x07"), ""); }

    #[test]
    fn test_progress() { assert_eq!(strip_ansi("\x1b[?25l\x1b[2K\r\x1b[32m[####    ]\x1b[0m 40%"), "[####    ] 40%"); }

    #[test]
    fn test_dcs() { assert_eq!(strip_ansi("\x1bP+test\x1b\\"), ""); }
}
