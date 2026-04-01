#[cfg(test)]
mod debug {
    #[test]
    fn test_simple_json_match() {
        use regex::Regex;
        
        // Simple pattern - no (?i)
        let re1 = Regex::new(r#"(api_?[Kk]ey)\s*:\s*"([^"]+)""#).unwrap();
        let text = r#""apiKey": "sk-abcdefghijklmnop""#;
        println!("Pattern1: (api_?[Kk]ey)\\s*:\\s*\"([^\"]+)\"");
        println!("Text: {}", text);
        if let Some(caps) = re1.captures(text) {
            println!("MATCH! group(1)={:?}, group(2)={:?}", caps.get(1).map(|m| m.as_str()), caps.get(2).map(|m| m.as_str()));
        } else {
            println!("NO MATCH");
        }
        
        // With (?i) at START
        let re2 = Regex::new(&format!(r"(?i)({})\\s*:\\s*\"([^\"]+)\"", 
            r"api_?[Kk]ey|token|secret|password")).unwrap();
        println!("\nPattern2 with (?i) at start");
        if let Some(caps) = re2.captures(text) {
            println!("MATCH! group(1)={:?}, group(2)={:?}", caps.get(1).map(|m| m.as_str()), caps.get(2).map(|m| m.as_str()));
        } else {
            println!("NO MATCH");
        }
        
        // With (?i) inside capturing group (like current code)
        let pattern3 = format!(r"((?i){})\s*:\s*\"([^\"]+)\"", 
            r"api_?[Kk]ey|token|secret|password");
        println!("\nPattern3 (current): {}", &pattern3[..80]);
        let re3 = Regex::new(&pattern3).unwrap();
        if let Some(caps) = re3.captures(text) {
            println!("MATCH! group(1)={:?}, group(2)={:?}", caps.get(1).map(|m| m.as_str()), caps.get(2).map(|m| m.as_str()));
        } else {
            println!("NO MATCH");
        }
    }
}
