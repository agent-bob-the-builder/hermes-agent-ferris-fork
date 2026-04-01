//! PyO3 bindings for the Rust insights computation library.
//!
//! Provides fast computation of session insights including:
//! - Overview statistics (tokens, cost, duration)
//! - Model, platform, and tool breakdowns
//! - Activity patterns (day/hour analysis, streaks)
//! - Notable sessions (longest, most messages, most tokens, most tool calls)
//! - Terminal and gateway formatting

use pyo3::prelude::*;

// ---------------------------------------------------------------------------
// Data structures (mirrors Python dict shapes)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub source: String,
    pub model: String,
    #[serde(default)]
    pub started_at: Option<f64>,
    #[serde(default)]
    pub ended_at: Option<f64>,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub tool_call_count: i64,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub cache_write_tokens: i64,
    #[serde(default)]
    pub billing_provider: Option<String>,
    #[serde(default)]
    pub billing_base_url: Option<String>,
    #[serde(default)]
    pub billing_mode: Option<String>,
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub actual_cost_usd: Option<f64>,
    #[serde(default)]
    pub cost_status: Option<String>,
    #[serde(default)]
    pub cost_source: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolUsageEntry {
    pub tool_name: String,
    pub count: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct MessageStats {
    #[serde(default)]
    pub total_messages: i64,
    #[serde(default)]
    pub user_messages: i64,
    #[serde(default)]
    pub assistant_messages: i64,
    #[serde(default)]
    pub tool_messages: i64,
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn format_duration(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("{:.0}s", seconds)
    } else if seconds < 3600.0 {
        let m = (seconds / 60.0).floor();
        let s = (seconds % 60.0).floor();
        if s > 0.0 {
            format!("{:.0}m {}s", m, s)
        } else {
            format!("{:.0}m", m)
        }
    } else {
        let h = (seconds / 3600.0).floor();
        let m = ((seconds % 3600.0) / 60.0).floor();
        if m > 0.0 {
            format!("{:.0}h {}m", h, m)
        } else {
            format!("{:.0}h", h)
        }
    }
}

fn format_count(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_date(ts: Option<f64>) -> String {
    match ts {
        Some(t) => {
            let days_since_epoch = (t / 86400.0) as i64;
            let (_year, month, day) = civil_from_days(days_since_epoch + 719163);
            let month_names = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            format!("{} {:02}", month_names[(month - 1) as usize], day)
        }
        None => "?".to_string(),
    }
}

fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z_prime = z + 719468;
    let era = z_prime.div_euclid(146097);
    let doe = z_prime - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + (m <= 2) as i64;
    (year, m as u8, d as u8)
}

fn parse_date(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: i64 = parts[1].parse().ok()?;
    let day: i64 = parts[2].parse().ok()?;
    Some(days_to_civil(year, month as u8, day as u8) - 719163)
}

fn days_to_civil(year: i64, month: u8, day: u8) -> i64 {
    let era = if year >= 0 {
        year / 400
    } else {
        (year - 399) / 400
    };
    let yoe = year - era * 400;
    let doy = (153 * ((if month <= 2 { month + 9 } else { month - 3 }) as i64 - 1) + 2) / 5
        + day as i64
        - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// ---------------------------------------------------------------------------
// Computation
// ---------------------------------------------------------------------------

/// Compute high-level overview statistics from sessions and message stats.
pub fn compute_overview(sessions: &[Session], message_stats: &MessageStats) -> serde_json::Value {
    let total_input: i64 = sessions.iter().map(|s| s.input_tokens).sum();
    let total_output: i64 = sessions.iter().map(|s| s.output_tokens).sum();
    let total_cache_read: i64 = sessions.iter().map(|s| s.cache_read_tokens).sum();
    let total_cache_write: i64 = sessions.iter().map(|s| s.cache_write_tokens).sum();
    let total_tokens = total_input + total_output + total_cache_read + total_cache_write;
    let total_tool_calls: i64 = sessions.iter().map(|s| s.tool_call_count).sum();
    let total_messages: i64 = sessions.iter().map(|s| s.message_count).sum();

    let durations: Vec<f64> = sessions
        .iter()
        .filter_map(|s| {
            let start = s.started_at?;
            let end = s.ended_at?;
            if end > start {
                Some(end - start)
            } else {
                None
            }
        })
        .collect();

    let total_hours = if !durations.is_empty() {
        durations.iter().sum::<f64>() / 3600.0
    } else {
        0.0
    };

    let avg_duration = if !durations.is_empty() {
        durations.iter().sum::<f64>() / durations.len() as f64
    } else {
        0.0
    };

    let date_range_start = sessions
        .iter()
        .filter_map(|s| s.started_at)
        .fold(None, |acc, ts| Some(acc.map_or(ts, |a: f64| a.min(ts))));
    let date_range_end = sessions
        .iter()
        .filter_map(|s| s.started_at)
        .fold(None, |acc, ts| Some(acc.map_or(ts, |a: f64| a.max(ts))));

    serde_json::json!({
        "total_sessions": sessions.len() as i64,
        "total_messages": total_messages,
        "total_tool_calls": total_tool_calls,
        "total_input_tokens": total_input,
        "total_output_tokens": total_output,
        "total_cache_read_tokens": total_cache_read,
        "total_cache_write_tokens": total_cache_write,
        "total_tokens": total_tokens,
        "estimated_cost": 0.0f64,
        "actual_cost": 0.0f64,
        "total_hours": total_hours,
        "avg_session_duration": avg_duration,
        "avg_messages_per_session": if !sessions.is_empty() { total_messages as f64 / sessions.len() as f64 } else { 0.0 },
        "avg_tokens_per_session": if !sessions.is_empty() { total_tokens as f64 / sessions.len() as f64 } else { 0.0 },
        "user_messages": message_stats.user_messages,
        "assistant_messages": message_stats.assistant_messages,
        "tool_messages": message_stats.tool_messages,
        "date_range_start": date_range_start,
        "date_range_end": date_range_end,
    })
}

/// Break down usage by model.
pub fn compute_model_breakdown(sessions: &[Session]) -> Vec<serde_json::Value> {
    use std::collections::HashMap;
    #[derive(Default)]
    struct ModelData {
        sessions: i64,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_tokens: i64,
        cache_write_tokens: i64,
        total_tokens: i64,
        tool_calls: i64,
    }
    let mut map: HashMap<String, ModelData> = HashMap::new();
    for s in sessions {
        let display_model = if s.model.contains('/') {
            s.model.split('/').next_back().unwrap_or(&s.model).to_string()
        } else {
            s.model.clone()
        };
        let d = map.entry(display_model).or_default();
        d.sessions += 1;
        d.input_tokens += s.input_tokens;
        d.output_tokens += s.output_tokens;
        d.cache_read_tokens += s.cache_read_tokens;
        d.cache_write_tokens += s.cache_write_tokens;
        d.total_tokens +=
            s.input_tokens + s.output_tokens + s.cache_read_tokens + s.cache_write_tokens;
        d.tool_calls += s.tool_call_count;
    }
    let mut result: Vec<serde_json::Value> = map
        .into_iter()
        .map(|(model, data)| {
            serde_json::json!({
                "model": model,
                "sessions": data.sessions,
                "input_tokens": data.input_tokens,
                "output_tokens": data.output_tokens,
                "cache_read_tokens": data.cache_read_tokens,
                "cache_write_tokens": data.cache_write_tokens,
                "total_tokens": data.total_tokens,
                "tool_calls": data.tool_calls,
                "cost": 0.0f64,
                "has_pricing": false,
                "cost_status": "unknown",
            })
        })
        .collect();
    result.sort_by(|a, b| {
        let tokens_a = a.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        let tokens_b = b.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        let sessions_a = a.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0);
        let sessions_b = b.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0);
        tokens_b
            .cmp(&tokens_a)
            .then_with(|| sessions_b.cmp(&sessions_a))
    });
    result
}

/// Break down usage by platform/source.
pub fn compute_platform_breakdown(sessions: &[Session]) -> Vec<serde_json::Value> {
    use std::collections::HashMap;
    #[derive(Default)]
    struct PlatformData {
        sessions: i64,
        messages: i64,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_tokens: i64,
        cache_write_tokens: i64,
        total_tokens: i64,
        tool_calls: i64,
    }
    let mut map: HashMap<String, PlatformData> = HashMap::new();
    for s in sessions {
        let source = if s.source.is_empty() {
            "unknown"
        } else {
            &s.source
        };
        let d = map.entry(source.to_string()).or_default();
        d.sessions += 1;
        d.messages += s.message_count;
        d.input_tokens += s.input_tokens;
        d.output_tokens += s.output_tokens;
        d.cache_read_tokens += s.cache_read_tokens;
        d.cache_write_tokens += s.cache_write_tokens;
        d.total_tokens +=
            s.input_tokens + s.output_tokens + s.cache_read_tokens + s.cache_write_tokens;
        d.tool_calls += s.tool_call_count;
    }
    let mut result: Vec<serde_json::Value> = map
        .into_iter()
        .map(|(platform, data)| {
            serde_json::json!({
                "platform": platform,
                "sessions": data.sessions,
                "messages": data.messages,
                "input_tokens": data.input_tokens,
                "output_tokens": data.output_tokens,
                "cache_read_tokens": data.cache_read_tokens,
                "cache_write_tokens": data.cache_write_tokens,
                "total_tokens": data.total_tokens,
                "tool_calls": data.tool_calls,
            })
        })
        .collect();
    result.sort_by(|a, b| {
        let sessions_a = a.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0);
        let sessions_b = b.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0);
        sessions_b.cmp(&sessions_a)
    });
    result
}

/// Process tool usage data into a ranked list with percentages.
pub fn compute_tool_breakdown(tool_usage: &[ToolUsageEntry]) -> Vec<serde_json::Value> {
    let total_calls: i64 = tool_usage.iter().map(|t| t.count).sum();
    tool_usage
        .iter()
        .map(|t| {
            let pct = if total_calls > 0 {
                (t.count as f64 / total_calls as f64) * 100.0
            } else {
                0.0
            };
            serde_json::json!({
                "tool": t.tool_name,
                "count": t.count,
                "percentage": pct,
            })
        })
        .collect()
}

/// Analyze activity patterns by day of week and hour.
pub fn compute_activity_patterns(sessions: &[Session]) -> serde_json::Value {
    use std::collections::HashMap;
    let mut day_counts: HashMap<u8, i64> = HashMap::new();
    let mut hour_counts: HashMap<u8, i64> = HashMap::new();
    let mut daily_counts: HashMap<String, i64> = HashMap::new();

    for s in sessions {
        if let Some(ts) = s.started_at {
            let days_since_epoch = (ts / 86400.0) as i64;
            let raw_weekday = (3_i64 + (days_since_epoch % 7)) % 7;
            let weekday: u8 = if raw_weekday >= 1 {
                (raw_weekday - 1) as u8
            } else {
                6
            };
            let hour = ((ts % 86400.0) / 3600.0) as u8;
            let days = (ts / 86400.0) as i64;
            let (year, month, day) = civil_from_days(days + 719163);
            let date_str = format!("{:04}-{:02}-{:02}", year, month, day);
            *day_counts.entry(weekday).or_insert(0) += 1;
            *hour_counts.entry(hour).or_insert(0) += 1;
            *daily_counts.entry(date_str).or_insert(0) += 1;
        }
    }

    let day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let day_breakdown: Vec<serde_json::Value> = (0..7)
        .map(|i| {
            serde_json::json!({
                "day": day_names[i as usize],
                "count": day_counts.get(&(i as u8)).copied().unwrap_or(0),
            })
        })
        .collect();
    let hour_breakdown: Vec<serde_json::Value> = (0..24)
        .map(|i| {
            serde_json::json!({
                "hour": i,
                "count": hour_counts.get(&(i as u8)).copied().unwrap_or(0),
            })
        })
        .collect();

    let busiest_day = day_breakdown
        .iter()
        .max_by_key(|d| d.get("count").and_then(|v| v.as_i64()).unwrap_or(0))
        .cloned();
    let busiest_hour = hour_breakdown
        .iter()
        .max_by_key(|h| h.get("count").and_then(|v| v.as_i64()).unwrap_or(0))
        .cloned();
    let active_days = daily_counts.len() as i64;

    let mut all_dates: Vec<&str> = daily_counts.keys().map(|s| s.as_str()).collect();
    all_dates.sort();
    let mut current_streak = 1i64;
    let mut max_streak = 1i64;
    for i in 1..all_dates.len() {
        if let (Some(p), Some(c)) = (parse_date(all_dates[i - 1]), parse_date(all_dates[i])) {
            if c - p == 1 {
                current_streak += 1;
                max_streak = max_streak.max(current_streak);
            } else {
                current_streak = 1;
            }
        }
    }
    if all_dates.is_empty() {
        max_streak = 0;
    }

    serde_json::json!({
        "by_day": day_breakdown,
        "by_hour": hour_breakdown,
        "busiest_day": busiest_day,
        "busiest_hour": busiest_hour,
        "active_days": active_days,
        "max_streak": max_streak,
    })
}

/// Find notable sessions (longest, most messages, most tokens, most tool calls).
pub fn compute_top_sessions(sessions: &[Session]) -> Vec<serde_json::Value> {
    let mut top = Vec::new();

    let sessions_with_duration: Vec<_> = sessions
        .iter()
        .filter(|s| s.started_at.is_some() && s.ended_at.is_some())
        .collect();
    if let Some(longest) = sessions_with_duration.iter().max_by(|a, b| {
        let dur_a = a.ended_at.unwrap_or(0.0) - a.started_at.unwrap_or(0.0);
        let dur_b = b.ended_at.unwrap_or(0.0) - b.started_at.unwrap_or(0.0);
        dur_b
            .partial_cmp(&dur_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    }) {
        let dur = longest.ended_at.unwrap_or(0.0) - longest.started_at.unwrap_or(0.0);
        top.push(serde_json::json!({
            "label": "Longest session",
            "session_id": longest.id.chars().take(16).collect::<String>(),
            "value": format_duration(dur),
            "date": format_date(longest.started_at),
        }));
    }

    if let Some(most_msgs) = sessions
        .iter()
        .max_by(|a, b| a.message_count.cmp(&b.message_count))
    {
        if most_msgs.message_count > 0 {
            top.push(serde_json::json!({
                "label": "Most messages",
                "session_id": most_msgs.id.chars().take(16).collect::<String>(),
                "value": format!("{} msgs", most_msgs.message_count),
                "date": format_date(most_msgs.started_at),
            }));
        }
    }

    if let Some(most_tokens) = sessions.iter().max_by(|a, b| {
        let total_a = a.input_tokens + a.output_tokens;
        let total_b = b.input_tokens + b.output_tokens;
        total_b.cmp(&total_a)
    }) {
        let token_total = most_tokens.input_tokens + most_tokens.output_tokens;
        if token_total > 0 {
            top.push(serde_json::json!({
                "label": "Most tokens",
                "session_id": most_tokens.id.chars().take(16).collect::<String>(),
                "value": format!("{} tokens", token_total),
                "date": format_date(most_tokens.started_at),
            }));
        }
    }

    if let Some(most_tools) = sessions
        .iter()
        .max_by(|a, b| a.tool_call_count.cmp(&b.tool_call_count))
    {
        if most_tools.tool_call_count > 0 {
            top.push(serde_json::json!({
                "label": "Most tool calls",
                "session_id": most_tools.id.chars().take(16).collect::<String>(),
                "value": format!("{} calls", most_tools.tool_call_count),
                "date": format_date(most_tools.started_at),
            }));
        }
    }

    top
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

fn bar_chart(values: &[i64], max_width: usize) -> Vec<String> {
    let peak = values.iter().max().copied().unwrap_or(1);
    if peak == 0 {
        return vec!["".to_string(); values.len()];
    }
    values
        .iter()
        .map(|&v| {
            if v > 0 {
                let width = ((v as f64 / peak as f64) * max_width as f64).ceil() as usize;
                "█".repeat(width.max(1))
            } else {
                String::new()
            }
        })
        .collect()
}

/// Format insights report for terminal display (CLI).
pub fn format_terminal(
    overview: &serde_json::Value,
    models: &[serde_json::Value],
    platforms: &[serde_json::Value],
    tools: &[serde_json::Value],
    activity: &serde_json::Value,
    top_sessions: &[serde_json::Value],
    days: i64,
    source_filter: Option<&str>,
) -> String {
    let mut lines = Vec::new();

    lines.push("".to_string());
    lines.push("  ╔══════════════════════════════════════════════════════════╗".to_string());
    lines.push("  ║                    📊 Hermes Insights                    ║".to_string());
    let mut period_label = format!("Last {} days", days);
    if let Some(src) = source_filter {
        period_label += &format!(" ({})", src);
    }
    let padding = 58usize.saturating_sub(period_label.len());
    let left_pad = padding / 2;
    let right_pad = padding - left_pad;
    lines.push(format!(
        "  ║{left}{label} {right}║",
        left = " ".repeat(left_pad),
        label = period_label,
        right = " ".repeat(right_pad)
    ));
    lines.push("  ╚══════════════════════════════════════════════════════════╝".to_string());
    lines.push("".to_string());

    if let (Some(start), Some(end)) = (
        overview.get("date_range_start").and_then(|v| v.as_f64()),
        overview.get("date_range_end").and_then(|v| v.as_f64()),
    ) {
        lines.push(format!(
            "  Period: {} — {}",
            format_date(Some(start)),
            format_date(Some(end))
        ));
        lines.push("".to_string());
    }

    let total_sessions = overview
        .get("total_sessions")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_messages = overview
        .get("total_messages")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_tool_calls = overview
        .get("total_tool_calls")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_input = overview
        .get("total_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_output = overview
        .get("total_output_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_tokens = overview
        .get("total_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let estimated_cost = overview
        .get("estimated_cost")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let total_hours = overview
        .get("total_hours")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let avg_duration = overview
        .get("avg_session_duration")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let avg_msgs = overview
        .get("avg_messages_per_session")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let user_msgs = overview
        .get("user_messages")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let models_without_pricing: Vec<String> = overview
        .get("models_without_pricing")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    lines.push("  📋 Overview".to_string());
    lines.push("  ----------------------------------------------------------------".to_string());
    lines.push(format!(
        "  Sessions:          {:<12}  Messages:        {}",
        total_sessions,
        format_count(total_messages)
    ));
    lines.push(format!(
        "  Tool calls:        {:<12}  User messages:   {}",
        total_tool_calls,
        format_count(user_msgs)
    ));
    lines.push(format!(
        "  Input tokens:      {:<12}  Output tokens:   {}",
        total_input,
        format_count(total_output)
    ));
    let cost_str = if models_without_pricing.is_empty() {
        format!("${:.2}", estimated_cost)
    } else {
        format!("${:.2} *", estimated_cost)
    };
    lines.push(format!(
        "  Total tokens:      {:<12}  Est. cost:       {}",
        total_tokens, cost_str
    ));
    if total_hours > 0.0 {
        lines.push(format!(
            "  Active time:       ~{:<11}  Avg session:     ~{}",
            format_duration(total_hours * 3600.0),
            format_duration(avg_duration)
        ));
    }
    lines.push(format!("  Avg msgs/session:  {:.1}", avg_msgs));
    lines.push("".to_string());

    if !models.is_empty() {
        lines.push("  🤖 Models Used".to_string());
        lines
            .push("  ----------------------------------------------------------------".to_string());
        lines.push(format!(
            "  {:<30} {:>8} {:>12} {:>8}",
            "Model", "Sessions", "Tokens", "Cost"
        ));
        for m in models {
            let model_name = m
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .chars()
                .take(28)
                .collect::<String>();
            let sessions_count = m.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0);
            let tokens = m.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            let cost = m.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let has_pricing = m
                .get("has_pricing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let cost_cell = if has_pricing {
                format!("${:>6.2}", cost)
            } else {
                "     N/A".to_string()
            };
            lines.push(format!(
                "  {:<30} {:>8} {:>12} {}",
                model_name,
                sessions_count,
                format_count(tokens),
                cost_cell
            ));
        }
        if !models_without_pricing.is_empty() {
            lines.push("  * Cost N/A for custom/self-hosted models".to_string());
        }
        lines.push("".to_string());
    }

    let show_platform = if platforms.len() > 1 {
        true
    } else {
        platforms
            .first()
            .and_then(|p| p.get("platform").and_then(|v| v.as_str()))
            != Some("cli")
    };
    if show_platform {
        lines.push("  📱 Platforms".to_string());
        lines
            .push("  ----------------------------------------------------------------".to_string());
        lines.push(format!(
            "  {:<14} {:>8} {:>10} {:>14}",
            "Platform", "Sessions", "Messages", "Tokens"
        ));
        for p in platforms {
            let platform = p.get("platform").and_then(|v| v.as_str()).unwrap_or("");
            let sessions_count = p.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0);
            let messages = p.get("messages").and_then(|v| v.as_i64()).unwrap_or(0);
            let tokens = p.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            lines.push(format!(
                "  {:<14} {:>8} {:>10} {:>14}",
                platform,
                sessions_count,
                format_count(messages),
                format_count(tokens)
            ));
        }
        lines.push("".to_string());
    }

    if !tools.is_empty() {
        lines.push("  🔧 Top Tools".to_string());
        lines
            .push("  ----------------------------------------------------------------".to_string());
        lines.push(format!("  {:<28} {:>8} {:>8}", "Tool", "Calls", "%"));
        for t in tools.iter().take(15) {
            let tool = t.get("tool").and_then(|v| v.as_str()).unwrap_or("");
            let count = t.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            let pct = t.get("percentage").and_then(|v| v.as_f64()).unwrap_or(0.0);
            lines.push(format!("  {:<28} {:>8} {:>7.1}%", tool, count, pct));
        }
        if tools.len() > 15 {
            lines.push(format!("  ... and {} more tools", tools.len() - 15));
        }
        lines.push("".to_string());
    }

    if let Some(by_day) = activity.get("by_day").and_then(|v| v.as_array()) {
        lines.push("  📅 Activity Patterns".to_string());
        lines
            .push("  ----------------------------------------------------------------".to_string());
        let day_values: Vec<i64> = by_day
            .iter()
            .filter_map(|d| d.get("count").and_then(|v| v.as_i64()))
            .collect();
        let bars = bar_chart(&day_values, 15);
        for (i, d) in by_day.iter().enumerate() {
            let day_name = d.get("day").and_then(|v| v.as_str()).unwrap_or("");
            let count = d.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            let bar = bars.get(i).map(|s| s.as_str()).unwrap_or("");
            lines.push(format!("  {}  {:<15} {}", day_name, bar, count));
        }
        lines.push("".to_string());

        if let Some(by_hour) = activity.get("by_hour").and_then(|v| v.as_array()) {
            let mut busy_hours: Vec<_> = by_hour
                .iter()
                .filter_map(|h| {
                    let hour = h.get("hour").and_then(|v| v.as_i64())? as u8;
                    let count = h.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                    if count > 0 {
                        Some((hour, count))
                    } else {
                        None
                    }
                })
                .collect();
            busy_hours.sort_by(|a, b| b.1.cmp(&a.1));
            let top_hours: Vec<_> = busy_hours.into_iter().take(5).collect();
            if !top_hours.is_empty() {
                let hour_strs: Vec<String> = top_hours
                    .iter()
                    .map(|(hr, count)| {
                        let ampm = if *hr < 12 { "AM" } else { "PM" };
                        let display_hr = if *hr % 12 == 0 { 12 } else { *hr % 12 };
                        format!("{}{} ({})", display_hr, ampm, count)
                    })
                    .collect();
                lines.push(format!("  Peak hours: {}", hour_strs.join(", ")));
            }
        }
        if let Some(active_days) = activity.get("active_days").and_then(|v| v.as_i64()) {
            if active_days > 0 {
                lines.push(format!("  Active days: {}", active_days));
            }
        }
        if let Some(max_streak) = activity.get("max_streak").and_then(|v| v.as_i64()) {
            if max_streak > 1 {
                lines.push(format!("  Best streak: {} consecutive days", max_streak));
            }
        }
        lines.push("".to_string());
    }

    if !top_sessions.is_empty() {
        lines.push("  🏆 Notable Sessions".to_string());
        lines
            .push("  ----------------------------------------------------------------".to_string());
        for ts in top_sessions {
            let label = ts.get("label").and_then(|v| v.as_str()).unwrap_or("");
            let value = ts.get("value").and_then(|v| v.as_str()).unwrap_or("");
            let date = ts.get("date").and_then(|v| v.as_str()).unwrap_or("?");
            let session_id = ts.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
            lines.push(format!(
                "  {:<20} {:<18} ({}, {})",
                label, value, date, session_id
            ));
        }
        lines.push("".to_string());
    }

    lines.join("\n")
}

/// Format insights report for gateway/messaging (shorter format).
pub fn format_gateway(
    overview: &serde_json::Value,
    models: &[serde_json::Value],
    platforms: &[serde_json::Value],
    tools: &[serde_json::Value],
    activity: &serde_json::Value,
    days: i64,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("📊 **Hermes Insights** — Last {} days\n", days));

    let total_sessions = overview
        .get("total_sessions")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_messages = overview
        .get("total_messages")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_tool_calls = overview
        .get("total_tool_calls")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_input = overview
        .get("total_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_output = overview
        .get("total_output_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_tokens = overview
        .get("total_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let estimated_cost = overview
        .get("estimated_cost")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let total_hours = overview
        .get("total_hours")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let avg_duration = overview
        .get("avg_session_duration")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let models_without_pricing: Vec<String> = overview
        .get("models_without_pricing")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    lines.push(format!(
        "**Sessions:** {} | **Messages:** {} | **Tool calls:** {}",
        total_sessions,
        format_count(total_messages),
        format_count(total_tool_calls)
    ));
    lines.push(format!(
        "**Tokens:** {} (in: {} / out: {})",
        format_count(total_tokens),
        format_count(total_input),
        format_count(total_output)
    ));
    let cost_note = if models_without_pricing.is_empty() {
        String::new()
    } else {
        " _(excludes custom/self-hosted models)_".to_string()
    };
    lines.push(format!(
        "**Est. cost:** ${:.2}{}\n",
        estimated_cost, cost_note
    ));
    if total_hours > 0.0 {
        lines.push(format!(
            "**Active time:** ~{} | **Avg session:** ~{}\n",
            format_duration(total_hours * 3600.0),
            format_duration(avg_duration)
        ));
    }

    if !models.is_empty() {
        lines.push("**🤖 Models:**".to_string());
        for m in models.iter().take(5) {
            let model_name = m
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .chars()
                .take(25)
                .collect::<String>();
            let sessions_count = m.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0);
            let tokens = m.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            let cost = m.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let has_pricing = m
                .get("has_pricing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let cost_str = if has_pricing {
                format!("${:.2}", cost)
            } else {
                "N/A".to_string()
            };
            lines.push(format!(
                "  {} — {} sessions, {} tokens, {}",
                model_name,
                sessions_count,
                format_count(tokens),
                cost_str
            ));
        }
        lines.push("".to_string());
    }

    if platforms.len() > 1 {
        lines.push("**📱 Platforms:**".to_string());
        for p in platforms {
            let platform = p.get("platform").and_then(|v| v.as_str()).unwrap_or("");
            let sessions_count = p.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0);
            let messages = p.get("messages").and_then(|v| v.as_i64()).unwrap_or(0);
            lines.push(format!(
                "  {} — {} sessions, {} msgs",
                platform,
                sessions_count,
                format_count(messages)
            ));
        }
        lines.push("".to_string());
    }

    if !tools.is_empty() {
        lines.push("**🔧 Top Tools:**".to_string());
        for t in tools.iter().take(8) {
            let tool = t.get("tool").and_then(|v| v.as_str()).unwrap_or("");
            let count = t.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            let pct = t.get("percentage").and_then(|v| v.as_f64()).unwrap_or(0.0);
            lines.push(format!(
                "  {} — {} calls ({:.1}%)",
                tool,
                format_count(count),
                pct
            ));
        }
        lines.push("".to_string());
    }

    if let (Some(busiest_day), Some(busiest_hour)) =
        (activity.get("busiest_day"), activity.get("busiest_hour"))
    {
        let day_name = busiest_day
            .get("day")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let day_count = busiest_day
            .get("count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let hour = busiest_hour
            .get("hour")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u8;
        let hour_count = busiest_hour
            .get("count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let ampm = if hour < 12 { "AM" } else { "PM" };
        let display_hr = if hour.is_multiple_of(12) { 12 } else { hour % 12 };
        lines.push(format!(
            "**📅 Busiest:** {}s ({} sessions), {}{} ({} sessions)",
            day_name, day_count, display_hr, ampm, hour_count
        ));
        if let Some(active_days) = activity.get("active_days").and_then(|v| v.as_i64()) {
            lines.push(format!("**Active days:** {}", active_days));
        }
        if let Some(max_streak) = activity.get("max_streak").and_then(|v| v.as_i64()) {
            if max_streak > 1 {
                lines.push(format!("**Best streak:** {} consecutive days", max_streak));
            }
        }
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// PyO3 bindings
// ---------------------------------------------------------------------------

/// Compute overview statistics (PyO3 wrapper).
/// Takes JSON-serialized sessions list and message stats, returns JSON string.
#[pyfunction]
fn rs_compute_overview(sessions_json: String, message_stats_json: String) -> String {
    let sessions: Vec<Session> = serde_json::from_str(&sessions_json).unwrap_or_default();
    let message_stats: MessageStats = serde_json::from_str(&message_stats_json).unwrap_or_default();
    serde_json::to_string(&compute_overview(&sessions, &message_stats)).unwrap_or_default()
}

/// Compute model breakdown (PyO3 wrapper).
#[pyfunction]
fn rs_compute_model_breakdown(sessions_json: String) -> String {
    let sessions: Vec<Session> = serde_json::from_str(&sessions_json).unwrap_or_default();
    serde_json::to_string(&compute_model_breakdown(&sessions)).unwrap_or_default()
}

/// Compute platform breakdown (PyO3 wrapper).
#[pyfunction]
fn rs_compute_platform_breakdown(sessions_json: String) -> String {
    let sessions: Vec<Session> = serde_json::from_str(&sessions_json).unwrap_or_default();
    serde_json::to_string(&compute_platform_breakdown(&sessions)).unwrap_or_default()
}

/// Compute tool breakdown (PyO3 wrapper).
#[pyfunction]
fn rs_compute_tool_breakdown(tool_usage_json: String) -> String {
    let tool_usage: Vec<ToolUsageEntry> =
        serde_json::from_str(&tool_usage_json).unwrap_or_default();
    serde_json::to_string(&compute_tool_breakdown(&tool_usage)).unwrap_or_default()
}

/// Compute activity patterns (PyO3 wrapper).
#[pyfunction]
fn rs_compute_activity_patterns(sessions_json: String) -> String {
    let sessions: Vec<Session> = serde_json::from_str(&sessions_json).unwrap_or_default();
    serde_json::to_string(&compute_activity_patterns(&sessions)).unwrap_or_default()
}

/// Compute top sessions (PyO3 wrapper).
#[pyfunction]
fn rs_compute_top_sessions(sessions_json: String) -> String {
    let sessions: Vec<Session> = serde_json::from_str(&sessions_json).unwrap_or_default();
    serde_json::to_string(&compute_top_sessions(&sessions)).unwrap_or_default()
}

/// Format insights report for terminal display (PyO3 wrapper).
#[pyfunction]
fn rs_format_terminal(
    overview: String,
    models: String,
    platforms: String,
    tools: String,
    activity: String,
    top_sessions: String,
    days: i64,
    source_filter: Option<String>,
) -> String {
    let overview: serde_json::Value =
        serde_json::from_str(&overview).unwrap_or(serde_json::Value::Null);
    let models: Vec<serde_json::Value> = serde_json::from_str(&models).unwrap_or_default();
    let platforms: Vec<serde_json::Value> = serde_json::from_str(&platforms).unwrap_or_default();
    let tools: Vec<serde_json::Value> = serde_json::from_str(&tools).unwrap_or_default();
    let activity: serde_json::Value =
        serde_json::from_str(&activity).unwrap_or(serde_json::Value::Null);
    let top_sessions: Vec<serde_json::Value> =
        serde_json::from_str(&top_sessions).unwrap_or_default();
    format_terminal(
        &overview,
        &models,
        &platforms,
        &tools,
        &activity,
        &top_sessions,
        days,
        source_filter.as_deref(),
    )
}

/// Format insights report for gateway/messaging (PyO3 wrapper).
#[pyfunction]
fn rs_format_gateway(
    overview: String,
    models: String,
    platforms: String,
    tools: String,
    activity: String,
    days: i64,
) -> String {
    let overview: serde_json::Value =
        serde_json::from_str(&overview).unwrap_or(serde_json::Value::Null);
    let models: Vec<serde_json::Value> = serde_json::from_str(&models).unwrap_or_default();
    let platforms: Vec<serde_json::Value> = serde_json::from_str(&platforms).unwrap_or_default();
    let tools: Vec<serde_json::Value> = serde_json::from_str(&tools).unwrap_or_default();
    let activity: serde_json::Value =
        serde_json::from_str(&activity).unwrap_or(serde_json::Value::Null);
    format_gateway(&overview, &models, &platforms, &tools, &activity, days)
}

#[pymodule]
fn rust_insights(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(rs_compute_overview, m)?)?;
    m.add_function(wrap_pyfunction!(rs_compute_model_breakdown, m)?)?;
    m.add_function(wrap_pyfunction!(rs_compute_platform_breakdown, m)?)?;
    m.add_function(wrap_pyfunction!(rs_compute_tool_breakdown, m)?)?;
    m.add_function(wrap_pyfunction!(rs_compute_activity_patterns, m)?)?;
    m.add_function(wrap_pyfunction!(rs_compute_top_sessions, m)?)?;
    m.add_function(wrap_pyfunction!(rs_format_terminal, m)?)?;
    m.add_function(wrap_pyfunction!(rs_format_gateway, m)?)?;
    Ok(())
}
