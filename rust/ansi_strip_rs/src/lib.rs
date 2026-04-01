//! ANSI escape sequence stripper — state machine approach.
//!
//! Handles 7-bit ANSI (CSI, OSC, DCS, Fe) and 8-bit C1 controls (UTF-8 encoded or raw).

/// Strip all ANSI escape sequences from a single line (no newlines).
/// Used as the inner worker for the parallel `strip_ansi` entry point.
fn strip_line(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // Handle raw 8-bit C1 control characters (single-byte U+0080-U+009F)
        if (0x80..=0x9f).contains(&b) {
            i += 1;
            continue;
        }

        // Strip carriage returns used in progress bar sequences (\r returns cursor to line start)
        if b == 0x0d {
            i += 1;
            continue;
        }

        // Handle two-byte UTF-8 encoded 8-bit C1 controls (\xc2\x80-\xc2\x9f)
        // This must come BEFORE the ESC prefix check below, so that ESC + \xc2\x9b
        // is correctly split: ESC is consumed by the prefix check, then \xc2\x9b
        // is consumed by this check on the next iteration.
        if b == 0xc2 && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if (0x80..=0x9f).contains(&next) {
                i += 2;
                continue;
            }
        }

        if b == 0x1b && i + 1 < bytes.len() {
            let next = bytes[i + 1];

            if next == 0x5b {
                // CSI: ESC [
                let mut j = i + 2;
                while j < bytes.len() {
                    let p = bytes[j];
                    if (0x20..=0x2f).contains(&p) {
                        j += 1;
                    } else if (0x30..=0x3f).contains(&p) {
                        j += 1;
                    } else {
                        break;
                    }
                }
                if j < bytes.len() && bytes[j] >= 0x40 && bytes[j] <= 0x7e {
                    i = j + 1;
                    continue;
                }
            } else if next == 0x5d {
                // OSC: ESC ]
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x07 {
                        i = j + 1;
                        break;
                    } else if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == 0x5c {
                        i = j + 2;
                        break;
                    }
                    j += 1;
                }
                if i == j {
                    i += 1;
                }
                continue;
            } else if next == 0x50 {
                // DCS: ESC P
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
                // Fe escape (single-byte final char): consume both ESC and final byte
                i += 2;
                continue;
            } else if next == 0xef
                && i + 2 < bytes.len()
                && bytes[i + 1] == 0xbf
                && bytes[i + 2] == 0xbd
            {
                // ESC followed by FFFD (U+FFFD = [0xef, 0xbf, 0xbd]) — malformed, consume all three
                i += 4;
                continue;
            } else if next == 0xc2 && i + 2 < bytes.len() && (0x80..=0x9f).contains(&bytes[i + 2]) {
                // 8-bit C1: ESC + \xc2 + [0x80-0x9f]
                // \xc2\x9b = 8-bit CSI, \xc2\x9d = 8-bit OSC, \xc2\x90 = 8-bit DCS
                let second = bytes[i + 2];
                if second == 0x9b || second == 0x9e || second == 0x9f {
                    // CSI — scan for final byte (0x40-0x7e), consume it
                    let mut j = i + 3;
                    while j < bytes.len() && (0x30..=0x3f).contains(&bytes[j]) {
                        j += 1;
                    }
                    while j < bytes.len() && (0x20..=0x2f).contains(&bytes[j]) {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] >= 0x40 && bytes[j] <= 0x7e {
                        i = j + 1;
                        continue;
                    }
                } else if second == 0x9d {
                    // OSC — scan to BEL or ST, skipping raw 8-bit C1 bytes in params
                    let mut j = i + 3;
                    while j < bytes.len() {
                        if bytes[j] == 0x07 {
                            i = j + 1;
                            break;
                        } else if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == 0x5c {
                            i = j + 2;
                            break;
                        } else if (0x80..=0x9f).contains(&bytes[j]) {
                            j += 1;
                        } else {
                            j += 1;
                        }
                    }
                    if j >= bytes.len() {
                        i = j;
                    }
                    continue;
                } else {
                    // Other C1 DCS (0x90) or PM/APC — scan to ST
                    let mut j = i + 3;
                    while j < bytes.len() {
                        if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == 0x5c {
                            i = j + 2;
                            break;
                        }
                        j += 1;
                    }
                    if j >= bytes.len() {
                        i = j;
                    }
                    continue;
                }
            } else if next == 0xc2 {
                // ESC + \xc2: could be the first byte of a two-byte UTF-8 sequence
                // If the second byte is 0x80-0x9F (C1 range), the top-of-loop handler will
                // consume it on the next iteration. Advance past ESC only.
                // Otherwise consume both (malformed sequence).
                if i + 2 < bytes.len() && (0x80..=0x9f).contains(&bytes[i + 2]) {
                    i += 1; // consume ESC only, let top-of-loop handle the 2-byte C1
                } else {
                    i += 2; // malformed — consume both
                }
                continue;
            } else if (0x80..=0x9f).contains(&next) {
                // Raw 8-bit C1 character (single-byte, not UTF-8 encoded)
                // e.g. ESC followed by 0x9B (8-bit CSI) or 0x9D (8-bit OSC)
                // CSI ends at the first final byte; OSC/DCS runs to BEL or ST
                if next == 0x9b || next == 0x9e || next == 0x9f {
                    // CSI — skip to first final byte (0x40-0x7e), consume it
                    let mut j = i + 2;
                    while j < bytes.len() && (0x30..=0x3f).contains(&bytes[j]) {
                        j += 1;
                    }
                    while j < bytes.len() && (0x20..=0x2f).contains(&bytes[j]) {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] >= 0x40 && bytes[j] <= 0x7e {
                        i = j + 1;
                        continue;
                    }
                } else if next == 0x9d {
                    // OSC — scan to BEL or ST, skipping raw 8-bit C1 bytes (0x80-0x9F)
                    // that may appear in OSC params on non-UTF-8 terminals
                    let mut j = i + 2;
                    while j < bytes.len() {
                        if bytes[j] == 0x07 {
                            i = j + 1;
                            break;
                        } else if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == 0x5c {
                            i = j + 2;
                            break;
                        } else if (0x80..=0x9f).contains(&bytes[j]) {
                            // Raw 8-bit C1 byte in OSC params — skip it, it's not a command here
                            j += 1;
                        } else {
                            j += 1;
                        }
                    }
                    if j >= bytes.len() {
                        i = j;
                    }
                    continue;
                } else {
                    // Other C1 (DCS 0x90, SOS 0x98, PM 0x9e, APC 0x9f) — scan to ST
                    let mut j = i + 2;
                    while j < bytes.len() {
                        if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == 0x5c {
                            i = j + 2;
                            break;
                        }
                        j += 1;
                    }
                    if j >= bytes.len() {
                        i = j;
                    }
                    continue;
                }
            } else if next == 0xc2 {
                // ESC followed by lone \xc2 with no valid continuation — consume both
                i += 2;
                continue;
            }
        }

        // Strip the Unicode replacement character (U+FFFD, encoded as [EF BF BD] in UTF-8)
        // that from_utf8_lossy inserts when replacing invalid bytes (e.g. raw 8-bit C1 chars)
        if b == 0xef && i + 2 < bytes.len() && bytes[i + 1] == 0xbf && bytes[i + 2] == 0xbd {
            i += 3;
            continue;
        }

        result.push(b);
        i += 1;
    }

    String::from_utf8(result).unwrap_or_else(|_| line.to_string())
}

/// Strip all ANSI escape sequences from text.
/// For multi-line input, lines are processed in parallel via Rayon;
/// for single-line or short input, falls back to the sequential path.
pub fn strip_ansi(text: &str) -> String {
    let lines: Vec<&str> = text
        .split(
            "
",
        )
        .collect();
    if lines.len() == 1 {
        // Single line — no need to spawn parallel tasks
        return strip_line(text);
    }

    // Parallel line-level processing: each line is independent
    let stripped: Vec<String> = lines.par_iter().map(|line| strip_line(line)).collect();

    stripped.join("\n")
}

use pyo3::prelude::*;
use rayon::prelude::*;

#[pyfunction]
fn strip_ansi_text(text: &str) -> String {
    strip_ansi(text)
}

#[pymodule]
fn ansi_strip_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(strip_ansi_text, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean() {
        assert_eq!(strip_ansi("hello"), "hello");
    }

    #[test]
    fn test_empty() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_csi() {
        assert_eq!(strip_ansi("\x1b[31mError\x1b[0m"), "Error");
    }

    #[test]
    fn test_osc_bel() {
        assert_eq!(strip_ansi("\x1b]0;title\x07"), "");
    }

    #[test]
    fn test_osc_st() {
        assert_eq!(strip_ansi("\x1b]0;title\x1b\\"), "");
    }

    #[test]
    fn test_8b_csi() {
        // 8-bit C1 CSI encoded as valid UTF-8 two-byte sequence: ESC U+009B
        assert_eq!(strip_ansi("\x1b\u{9b}31mError\x1b\u{9b}0m"), "Error");
    }

    #[test]
    fn test_8b_osc() {
        // 8-bit C1 OSC encoded as valid UTF-8 two-byte sequence: ESC U+009D
        assert_eq!(strip_ansi("\x1b\u{9d}0;title\x07"), "");
    }

    #[test]
    fn test_8b_csi_raw() {
        // Raw single-byte 0x9B (ISO-2022 terminals, some serial consoles)
        // strip_line processes raw bytes — test it directly to avoid UTF-8 lossy mangling
        let inp = b"Error\x1b\x9b0m";
        let result = strip_line(std::str::from_utf8(&inp[..5]).unwrap());
        assert_eq!(result, "Error");
    }

    #[test]
    fn test_progress() {
        assert_eq!(
            strip_ansi("\x1b[?25l\x1b[2K\r\x1b[32m[####    ]\x1b[0m 40%"),
            "[####    ] 40%"
        );
    }

    #[test]
    fn test_dcs() {
        assert_eq!(strip_ansi("\x1bP+test\x1b\\"), "");
    }
}
