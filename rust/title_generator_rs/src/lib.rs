use pyo3::prelude::*;
use serde_json::json;

const TITLE_PROMPT: &str = "Generate a short, descriptive title (3-7 words) for a conversation that starts with the following exchange. The title should capture the main topic or intent. Return ONLY the title text, nothing else. No quotes, no punctuation at the end, no prefixes.";

#[pyfunction]
pub fn format_title_prompt(user_message: &str, assistant_response: &str) -> String {
    let user_snippet = if user_message.len() > 500 { &user_message[..500] } else { user_message };
    let assistant_snippet = if assistant_response.len() > 500 { &assistant_response[..500] } else { assistant_response };
    let messages = json!([
        {"role": "system", "content": TITLE_PROMPT},
        {"role": "user", "content": format!("User: {}\n\nAssistant: {}", user_snippet, assistant_snippet)}
    ]);
    serde_json::to_string(&messages).unwrap()
}

#[pyfunction]
pub fn parse_title_response(response: &str) -> Option<String> {
    let trimmed = response.trim();
    let cleaned = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(trimmed);
    let cleaned = cleaned.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(cleaned);
    let title = if cleaned.to_lowercase().starts_with("title:") { &cleaned[6..] } else { cleaned };
    let title = title.trim().trim_end_matches('.').trim_end_matches('!').trim_end_matches('?');
    if title.is_empty() { return None; }
    let final_title = if title.len() > 77 { format!("{}...", &title[..77]) } else { title.to_string() };
    Some(final_title)
}

#[pyfunction]
pub fn should_auto_title(conversation_history_json: &str) -> bool {
    let Ok(messages) = serde_json::from_str::<serde_json::Value>(conversation_history_json) else { return false; };
    let user_count = messages.as_array()
        .map(|arr| arr.iter().filter(|m| m.get("role").and_then(|v| v.as_str()) == Some("user")).count())
        .unwrap_or(0);
    user_count <= 2
}

#[pymodule]
pub fn title_generator_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(format_title_prompt, m)?)?;
    m.add_function(wrap_pyfunction!(parse_title_response, m)?)?;
    m.add_function(wrap_pyfunction!(should_auto_title, m)?)?;
    Ok(())
}
