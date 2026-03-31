//! ANSI escape sequence stripper — state machine approach.
//!
//! Handles 7-bit ANSI (CSI, OSC, DCS, Fe) and 8-bit C1 controls (UTF-8 encoded or raw).

/// Strip all ANSI escape sequences from text.
pub fn strip_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // 7-bit ESC — might start an ANSI sequence
        if b == 0x1b && i + 1 < bytes.len() {
            let next = bytes[i + 1];

            if next == 0x5b {
                // ── CSI: ESC [ (Fe-type) ──────────────────────────────────
                // Format: ESC [ params intermediates final
                // Params: bytes 0x30-0x3f (digits, semicolon, etc.)
                // Intermediates: bytes 0x20-0x2f (space, !"#$%&'()*+,-./)
                // Final: bytes 0x40-0x7e (ASCII printable)
                let mut j = i + 2;
                // Consume params (0x30-0x3f) and intermediates (0x20-0x2f) in any order
                let mut intermediates = 0usize;
                while j < bytes.len() {
                    let p = bytes[j];
                    if (0x20..=0x2f).contains(&p) {
                        intermediates += 1;
                        j += 1;
                    } else if (0x30..=0x3f).contains(&p) {
                        j += 1;
                    } else {
                        break;
                    }
                }
                // Final byte must be 0x40-0x7e
                if j < bytes.len() && bytes[j] >= 0x40 && bytes[j] <= 0x7e {
                    i = j + 1; // skip the whole sequence
                    continue;
                }
                // Not a valid CSI — treat ESC as literal, keep scanning
            } else if next == 0x5d {
                // ── OSC: ESC ] ─────────────────────────────────────────
                // Format: OSC = ESC ] params BEL | ESC ] params ST
                // ST (String Terminator) = ESC \
                let mut j = i + 2;
                // Scan until BEL or ST
                while j < bytes.len() {
                    if bytes[j] == 0x07 {
                        // BEL terminator — include BEL in strip
                        i = j + 1;
                        break;
                    } else if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == 0x5c {
                        // ST = ESC \ — strip through the backslash
                        i = j + 2;
                        break;
                    }
                    j += 1;
                }
                if i == j {
                    // No terminator found, keep scanning
                    i += 1;
                }
                continue;
            } else if next == 0x50 {
                // ── DCS: ESC P ─────────────────────────────────────────
                // Format: DCS = ESC P params ST(ESC \)
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == 0x5c {
                        i = j + 2;
                        break;
                    }
                    j += 1;
                }
                if i == j {
                    i += 1;
                }
                continue;
            } else if (0x40..=0x5f).contains(&next) && next != 0x5b && next != 0x5d {
                // ── Fe-type: ESC c (single-byte Fe) ────────────────────
                // Fe types: ESC D (index), EM (reverse index), EM, FS, GS, RS, US
                i += 2;
                continue;
            }
        }

        // 8-bit C1 controls (UTF-8 encoded: C2 80 through C2 9F)
        if b == 0xc2 && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if (0x80..=0x9f).contains(&next) {
                // 8-bit C1 control — skip it
                i += 2;
                continue;
            }
        }

        // No ANSI sequence at this byte — copy it as-is
        result.push(b);
        i += 1;
    }

    // This is always valid UTF-8 since we only ever removed whole sequences
    String::from_utf8(result).unwrap_or_else(|_| text.to_string())
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
    fn test_8b_csi() {
        // 8-bit C1 CSI: ESC 0x9B params 0x9B final
        let inp = b"\x1b\x9b31mError\x1b\x9b0m";
        assert_eq!(strip_ansi(std::str::from_utf8(inp).unwrap()), "Error");
    }

    #[test]
    fn test_8b_osc() {
        // 8-bit C1 OSC: ESC 0x9D params BEL
        let inp = b"\x1b\x9d0;title\x07";
        assert_eq!(strip_ansi(inp), "");
    }

    #[test]
    fn test_progress() {
        assert_eq!(
            strip_ansi("\x1b[?25l\x1b[2K\r\x1b[32m[####    ]\x1b[0m 40%"),
            "[####    ] 40%"
        );
    }

    #[test]
    fn test_dcs() { assert_eq!(strip_ansi("\x1bP+test\x1b\\"), ""); }
}
