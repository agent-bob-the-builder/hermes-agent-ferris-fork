#[cfg(test)]
mod pattern_tests {
    use regex::Regex;

    fn make_pattern() -> Regex {
        Regex::new(&concat!(
            "\x1b",
            "(?:",
                "\\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]",
                r"|][\s\S]*?(?:[\x07\x1b\\])",
                r"|[PX^_][\s\S]*?(?:\x1b\\)",
                r"|[\x20-\x2f]+[\x30-\x7e]",
                r"|[\x30-\x7e]",
            ")",
            "|\u{009B}[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e}",
            concat!("|\u{009D}", r"[\s\S]*?", r"(?:", "[\x07\u{009C}]", r")"),
            "|[\u{0080}-\u{009F}]"
        )).unwrap()
    }

    #[test]
    fn test_csi_colors() {
        let re = make_pattern();
        assert_eq!(re.replace_all("\x1b[31mError\x1b[0m", ""), "Error");
    }
}
