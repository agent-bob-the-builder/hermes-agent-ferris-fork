"""
Python shim for rust_insights module.

Provides PyO3 bindings to the Rust insights computation library while
preserving the same interface. Falls back to pure-Python implementations
if the Rust module is unavailable (e.g., during development).
"""

import json
from typing import Any, Dict, List, Optional

try:
    from rust_insights import (
        rs_compute_overview,
        rs_compute_model_breakdown,
        rs_compute_platform_breakdown,
        rs_compute_tool_breakdown,
        rs_compute_activity_patterns,
        rs_compute_top_sessions,
        rs_format_terminal,
        rs_format_gateway,
    )
    _RUST_AVAILABLE = True
except ImportError:
    _RUST_AVAILABLE = False


def compute_overview(sessions: List[Dict[str, Any]], message_stats: Dict[str, Any]) -> Dict[str, Any]:
    """
    Compute high-level overview statistics from sessions and message stats.
    
    Delegates to Rust for fast computation.
    """
    if _RUST_AVAILABLE:
        result = rs_compute_overview(json.dumps(sessions), json.dumps(message_stats))
        return json.loads(result)
    
    # Pure Python fallback
    from collections import Counter
    total_input = sum(s.get("input_tokens") or 0 for s in sessions)
    total_output = sum(s.get("output_tokens") or 0 for s in sessions)
    total_cache_read = sum(s.get("cache_read_tokens") or 0 for s in sessions)
    total_cache_write = sum(s.get("cache_write_tokens") or 0 for s in sessions)
    total_tokens = total_input + total_output + total_cache_read + total_cache_write
    total_tool_calls = sum(s.get("tool_call_count") or 0 for s in sessions)
    total_messages = sum(s.get("message_count") or 0 for s in sessions)
    
    durations = []
    for s in sessions:
        start = s.get("started_at")
        end = s.get("ended_at")
        if start and end and end > start:
            durations.append(end - start)
    
    total_hours = sum(durations) / 3600 if durations else 0
    avg_duration = sum(durations) / len(durations) if durations else 0
    
    started = [s["started_at"] for s in sessions if s.get("started_at")]
    date_range_start = min(started) if started else None
    date_range_end = max(started) if started else None
    
    return {
        "total_sessions": len(sessions),
        "total_messages": total_messages,
        "total_tool_calls": total_tool_calls,
        "total_input_tokens": total_input,
        "total_output_tokens": total_output,
        "total_cache_read_tokens": total_cache_read,
        "total_cache_write_tokens": total_cache_write,
        "total_tokens": total_tokens,
        "estimated_cost": 0.0,
        "actual_cost": 0.0,
        "total_hours": total_hours,
        "avg_session_duration": avg_duration,
        "avg_messages_per_session": total_messages / len(sessions) if sessions else 0,
        "avg_tokens_per_session": total_tokens / len(sessions) if sessions else 0,
        "user_messages": message_stats.get("user_messages") or 0,
        "assistant_messages": message_stats.get("assistant_messages") or 0,
        "tool_messages": message_stats.get("tool_messages") or 0,
        "date_range_start": date_range_start,
        "date_range_end": date_range_end,
    }


def compute_model_breakdown(sessions: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    """Break down usage by model. Delegates to Rust."""
    if _RUST_AVAILABLE:
        result = rs_compute_model_breakdown(json.dumps(sessions))
        return json.loads(result)
    
    from collections import defaultdict
    model_data = defaultdict(lambda: {
        "sessions": 0, "input_tokens": 0, "output_tokens": 0,
        "cache_read_tokens": 0, "cache_write_tokens": 0,
        "total_tokens": 0, "tool_calls": 0, "cost": 0.0,
    })
    for s in sessions:
        model = s.get("model") or "unknown"
        display_model = model.split("/")[-1] if "/" in model else model
        d = model_data[display_model]
        d["sessions"] += 1
        inp = s.get("input_tokens") or 0
        out = s.get("output_tokens") or 0
        cr = s.get("cache_read_tokens") or 0
        cw = s.get("cache_write_tokens") or 0
        d["input_tokens"] += inp
        d["output_tokens"] += out
        d["cache_read_tokens"] += cr
        d["cache_write_tokens"] += cw
        d["total_tokens"] += inp + out + cr + cw
        d["tool_calls"] += s.get("tool_call_count") or 0
        d["has_pricing"] = False
        d["cost_status"] = "unknown"
    result = [{"model": model, **data} for model, data in model_data.items()]
    result.sort(key=lambda x: (x["total_tokens"], x["sessions"]), reverse=True)
    return result


def compute_platform_breakdown(sessions: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    """Break down usage by platform/source. Delegates to Rust."""
    if _RUST_AVAILABLE:
        result = rs_compute_platform_breakdown(json.dumps(sessions))
        return json.loads(result)
    
    from collections import defaultdict
    platform_data = defaultdict(lambda: {
        "sessions": 0, "messages": 0, "input_tokens": 0,
        "output_tokens": 0, "cache_read_tokens": 0,
        "cache_write_tokens": 0, "total_tokens": 0, "tool_calls": 0,
    })
    for s in sessions:
        source = s.get("source") or "unknown"
        d = platform_data[source]
        d["sessions"] += 1
        d["messages"] += s.get("message_count") or 0
        inp = s.get("input_tokens") or 0
        out = s.get("output_tokens") or 0
        cr = s.get("cache_read_tokens") or 0
        cw = s.get("cache_write_tokens") or 0
        d["input_tokens"] += inp
        d["output_tokens"] += out
        d["cache_read_tokens"] += cr
        d["cache_write_tokens"] += cw
        d["total_tokens"] += inp + out + cr + cw
        d["tool_calls"] += s.get("tool_call_count") or 0
    result = [{"platform": platform, **data} for platform, data in platform_data.items()]
    result.sort(key=lambda x: x["sessions"], reverse=True)
    return result


def compute_tool_breakdown(tool_usage: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    """Process tool usage data into a ranked list with percentages. Delegates to Rust."""
    if _RUST_AVAILABLE:
        result = rs_compute_tool_breakdown(json.dumps(tool_usage))
        return json.loads(result)
    
    total_calls = sum(t["count"] for t in tool_usage) if tool_usage else 0
    return [
        {"tool": t["tool_name"], "count": t["count"], "percentage": (t["count"] / total_calls * 100) if total_calls else 0}
        for t in tool_usage
    ]


def compute_activity_patterns(sessions: List[Dict[str, Any]]) -> Dict[str, Any]:
    """Analyze activity patterns by day of week and hour. Delegates to Rust."""
    if _RUST_AVAILABLE:
        result = rs_compute_activity_patterns(json.dumps(sessions))
        return json.loads(result)
    
    from collections import Counter
    from datetime import datetime
    day_counts = Counter()
    hour_counts = Counter()
    daily_counts = Counter()
    
    for s in sessions:
        ts = s.get("started_at")
        if not ts:
            continue
        dt = datetime.fromtimestamp(ts)
        day_counts[dt.weekday()] += 1
        hour_counts[dt.hour] += 1
        daily_counts[dt.strftime("%Y-%m-%d")] += 1
    
    day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
    day_breakdown = [{"day": day_names[i], "count": day_counts.get(i, 0)} for i in range(7)]
    hour_breakdown = [{"hour": i, "count": hour_counts.get(i, 0)} for i in range(24)]
    busiest_day = max(day_breakdown, key=lambda x: x["count"]) if day_breakdown else None
    busiest_hour = max(hour_breakdown, key=lambda x: x["count"]) if hour_breakdown else None
    active_days = len(daily_counts)
    
    if daily_counts:
        all_dates = sorted(daily_counts.keys())
        current_streak = 1
        max_streak = 1
        for i in range(1, len(all_dates)):
            d1 = datetime.strptime(all_dates[i - 1], "%Y-%m-%d")
            d2 = datetime.strptime(all_dates[i], "%Y-%m-%d")
            if (d2 - d1).days == 1:
                current_streak += 1
                max_streak = max(max_streak, current_streak)
            else:
                current_streak = 1
    else:
        max_streak = 0
    
    return {
        "by_day": day_breakdown,
        "by_hour": hour_breakdown,
        "busiest_day": busiest_day,
        "busiest_hour": busiest_hour,
        "active_days": active_days,
        "max_streak": max_streak,
    }


def compute_top_sessions(sessions: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    """Find notable sessions (longest, most messages, most tokens, most tool calls). Delegates to Rust."""
    if _RUST_AVAILABLE:
        result = rs_compute_top_sessions(json.dumps(sessions))
        return json.loads(result)
    
    from datetime import datetime
    
    def _format_duration(seconds):
        if seconds < 60:
            return f"{seconds:.0f}s"
        elif seconds < 3600:
            m = seconds / 60
            s = seconds % 60
            return f"{m:.0f}m {s:.0f}s" if s > 0 else f"{m:.0f}m"
        else:
            h = seconds / 3600
            m = (seconds % 3600) / 60
            return f"{h:.0f}h {m:.0f}m" if m > 0 else f"{h:.0f}h"
    
    top = []
    sessions_with_duration = [s for s in sessions if s.get("started_at") and s.get("ended_at")]
    if sessions_with_duration:
        longest = max(sessions_with_duration, key=lambda s: s["ended_at"] - s["started_at"])
        dur = longest["ended_at"] - longest["started_at"]
        top.append({
            "label": "Longest session",
            "session_id": longest["id"][:16],
            "value": _format_duration(dur),
            "date": datetime.fromtimestamp(longest["started_at"]).strftime("%b %d"),
        })
    
    most_msgs = max(sessions, key=lambda s: s.get("message_count") or 0)
    if (most_msgs.get("message_count") or 0) > 0:
        top.append({
            "label": "Most messages",
            "session_id": most_msgs["id"][:16],
            "value": f"{most_msgs['message_count']} msgs",
            "date": datetime.fromtimestamp(most_msgs["started_at"]).strftime("%b %d") if most_msgs.get("started_at") else "?",
        })
    
    most_tokens = max(sessions, key=lambda s: (s.get("input_tokens") or 0) + (s.get("output_tokens") or 0))
    token_total = (most_tokens.get("input_tokens") or 0) + (most_tokens.get("output_tokens") or 0)
    if token_total > 0:
        top.append({
            "label": "Most tokens",
            "session_id": most_tokens["id"][:16],
            "value": f"{token_total:,} tokens",
            "date": datetime.fromtimestamp(most_tokens["started_at"]).strftime("%b %d") if most_tokens.get("started_at") else "?",
        })
    
    most_tools = max(sessions, key=lambda s: s.get("tool_call_count") or 0)
    if (most_tools.get("tool_call_count") or 0) > 0:
        top.append({
            "label": "Most tool calls",
            "session_id": most_tools["id"][:16],
            "value": f"{most_tools['tool_call_count']} calls",
            "date": datetime.fromtimestamp(most_tools["started_at"]).strftime("%b %d") if most_tools.get("started_at") else "?",
        })
    
    return top


def format_terminal(
    overview: Dict[str, Any],
    models: List[Dict[str, Any]],
    platforms: List[Dict[str, Any]],
    tools: List[Dict[str, Any]],
    activity: Dict[str, Any],
    top_sessions: List[Dict[str, Any]],
    days: int,
    source_filter: Optional[str] = None,
) -> str:
    """Format the insights report for terminal display (CLI). Delegates to Rust."""
    if _RUST_AVAILABLE:
        return rs_format_terminal(
            overview=json.dumps(overview),
            models=json.dumps(models),
            platforms=json.dumps(platforms),
            tools=json.dumps(tools),
            activity=json.dumps(activity),
            top_sessions=json.dumps(top_sessions),
            days=days,
            source_filter=source_filter,
        )
    
    # Pure Python fallback (same as original insights.py)
    from agent.insights import _bar_chart, _format_duration
    from datetime import datetime
    
    def bar_chart(values, max_width=20):
        peak = max(values) if values else 1
        if peak == 0:
            return ["" for _ in values]
        return ["█" * max(1, int(v / peak * max_width)) if v > 0 else "" for v in values]
    
    def fmt_dur(seconds):
        if seconds < 60:
            return f"{seconds:.0f}s"
        elif seconds < 3600:
            m = seconds / 60
            s = seconds % 60
            return f"{m:.0f}m {s:.0f}s" if s > 0 else f"{m:.0f}m"
        else:
            h = seconds / 3600
            m = (seconds % 3600) / 60
            return f"{h:.0f}h {m:.0f}m" if m > 0 else f"{h:.0f}h"
    
    lines = []
    o = overview
    
    lines.append("")
    lines.append("  ╔══════════════════════════════════════════════════════════╗")
    lines.append("  ║                    📊 Hermes Insights                    ║")
    period_label = f"Last {days} days"
    if source_filter:
        period_label += f" ({source_filter})"
    padding = 58 - len(period_label) - 2
    left_pad = padding // 2
    right_pad = padding - left_pad
    lines.append(f"  ║{' ' * left_pad} {period_label} {' ' * right_pad}║")
    lines.append("  ╚══════════════════════════════════════════════════════════╝")
    lines.append("")
    
    if o.get("date_range_start") and o.get("date_range_end"):
        start_str = datetime.fromtimestamp(o["date_range_start"]).strftime("%b %d, %Y")
        end_str = datetime.fromtimestamp(o["date_range_end"]).strftime("%b %d, %Y")
        lines.append(f"  Period: {start_str} — {end_str}")
        lines.append("")
    
    lines.append("  📋 Overview")
    lines.append("  " + "─" * 56)
    lines.append(f"  Sessions:          {o['total_sessions']:<12}  Messages:        {o['total_messages']:,}")
    lines.append(f"  Tool calls:        {o['total_tool_calls']:<12,}  User messages:   {o['user_messages']:,}")
    lines.append(f"  Input tokens:      {o['total_input_tokens']:<12,}  Output tokens:   {o['total_output_tokens']:,}")
    cost_str = f"${o['estimated_cost']:.2f}"
    if o.get("models_without_pricing"):
        cost_str += " *"
    lines.append(f"  Total tokens:      {o['total_tokens']:<12,}  Est. cost:       {cost_str}")
    if o["total_hours"] > 0:
        lines.append(f"  Active time:       ~{fmt_dur(o['total_hours'] * 3600):<11}  Avg session:     ~{fmt_dur(o['avg_session_duration'])}")
    lines.append(f"  Avg msgs/session:  {o['avg_messages_per_session']:.1f}")
    lines.append("")
    
    if models:
        lines.append("  🤖 Models Used")
        lines.append("  " + "─" * 56)
        lines.append(f"  {'Model':<30} {'Sessions':>8} {'Tokens':>12} {'Cost':>8}")
        for m in models:
            model_name = m["model"][:28]
            cost_cell = f"${m['cost']:>6.2f}" if m.get("has_pricing") else "     N/A"
            lines.append(f"  {model_name:<30} {m['sessions']:>8} {m['total_tokens']:>12,} {cost_cell}")
        if o.get("models_without_pricing"):
            lines.append("  * Cost N/A for custom/self-hosted models")
        lines.append("")
    
    if len(platforms) > 1 or (platforms and platforms[0]["platform"] != "cli"):
        lines.append("  📱 Platforms")
        lines.append("  " + "─" * 56)
        lines.append(f"  {'Platform':<14} {'Sessions':>8} {'Messages':>10} {'Tokens':>14}")
        for p in platforms:
            lines.append(f"  {p['platform']:<14} {p['sessions']:>8} {p['messages']:>10,} {p['total_tokens']:>14,}")
        lines.append("")
    
    if tools:
        lines.append("  🔧 Top Tools")
        lines.append("  " + "─" * 56)
        lines.append(f"  {'Tool':<28} {'Calls':>8} {'%':>8}")
        for t in tools[:15]:
            lines.append(f"  {t['tool']:<28} {t['count']:>8,} {t['percentage']:>7.1f}%")
        if len(tools) > 15:
            lines.append(f"  ... and {len(tools) - 15} more tools")
        lines.append("")
    
    act = activity
    if act.get("by_day"):
        lines.append("  📅 Activity Patterns")
        lines.append("  " + "─" * 56)
        day_values = [d["count"] for d in act["by_day"]]
        bars = bar_chart(day_values, max_width=15)
        for i, d in enumerate(act["by_day"]):
            lines.append(f"  {d['day']}  {bars[i]:<15} {d['count']}")
        lines.append("")
        
        busy_hours = sorted(act["by_hour"], key=lambda x: x["count"], reverse=True)
        busy_hours = [h for h in busy_hours if h["count"] > 0][:5]
        if busy_hours:
            hour_strs = []
            for h in busy_hours:
                hr = h["hour"]
                ampm = "AM" if hr < 12 else "PM"
                display_hr = hr % 12 or 12
                hour_strs.append(f"{display_hr}{ampm} ({h['count']})")
            lines.append(f"  Peak hours: {', '.join(hour_strs)}")
        
        if act.get("active_days"):
            lines.append(f"  Active days: {act['active_days']}")
        if act.get("max_streak") and act["max_streak"] > 1:
            lines.append(f"  Best streak: {act['max_streak']} consecutive days")
        lines.append("")
    
    if top_sessions:
        lines.append("  🏆 Notable Sessions")
        lines.append("  " + "─" * 56)
        for ts in top_sessions:
            lines.append(f"  {ts['label']:<20} {ts['value']:<18} ({ts['date']}, {ts['session_id']})")
        lines.append("")
    
    return "\n".join(lines)


def format_gateway(
    overview: Dict[str, Any],
    models: List[Dict[str, Any]],
    platforms: List[Dict[str, Any]],
    tools: List[Dict[str, Any]],
    activity: Dict[str, Any],
    days: int,
) -> str:
    """Format the insights report for gateway/messaging (shorter). Delegates to Rust."""
    if _RUST_AVAILABLE:
        return rs_format_gateway(
            overview=json.dumps(overview),
            models=json.dumps(models),
            platforms=json.dumps(platforms),
            tools=json.dumps(tools),
            activity=json.dumps(activity),
            days=days,
        )
    
    # Pure Python fallback (same as original insights.py)
    from agent.insights import _format_duration
    from datetime import datetime
    
    def fmt_dur(seconds):
        if seconds < 60:
            return f"{seconds:.0f}s"
        elif seconds < 3600:
            m = seconds / 60
            s = seconds % 60
            return f"{m:.0f}m {s:.0f}s" if s > 0 else f"{m:.0f}m"
        else:
            h = seconds / 3600
            m = (seconds % 3600) / 60
            return f"{h:.0f}h {m:.0f}m" if m > 0 else f"{h:.0f}h"
    
    lines = []
    o = overview
    
    lines.append(f"📊 **Hermes Insights** — Last {days} days\n")
    lines.append(f"**Sessions:** {o['total_sessions']} | **Messages:** {o['total_messages']:,} | **Tool calls:** {o['total_tool_calls']:,}")
    lines.append(f"**Tokens:** {o['total_tokens']:,} (in: {o['total_input_tokens']:,} / out: {o['total_output_tokens']:,})")
    cost_note = " _(excludes custom/self-hosted models)_" if o.get("models_without_pricing") else ""
    lines.append(f"**Est. cost:** ${o['estimated_cost']:.2f}{cost_note}")
    if o["total_hours"] > 0:
        lines.append(f"**Active time:** ~{fmt_dur(o['total_hours'] * 3600)} | **Avg session:** ~{fmt_dur(o['avg_session_duration'])}")
    lines.append("")
    
    if models:
        lines.append("**🤖 Models:**")
        for m in models[:5]:
            cost_str = f"${m['cost']:.2f}" if m.get("has_pricing") else "N/A"
            lines.append(f"  {m['model'][:25]} — {m['sessions']} sessions, {m['total_tokens']:,} tokens, {cost_str}")
        lines.append("")
    
    if len(platforms) > 1:
        lines.append("**📱 Platforms:**")
        for p in platforms:
            lines.append(f"  {p['platform']} — {p['sessions']} sessions, {p['messages']:,} msgs")
        lines.append("")
    
    if tools:
        lines.append("**🔧 Top Tools:**")
        for t in tools[:8]:
            lines.append(f"  {t['tool']} — {t['count']:,} calls ({t['percentage']:.1f}%)")
        lines.append("")
    
    act = activity
    if act.get("busiest_day") and act.get("busiest_hour"):
        hr = act["busiest_hour"]["hour"]
        ampm = "AM" if hr < 12 else "PM"
        display_hr = hr % 12 or 12
        lines.append(f"**📅 Busiest:** {act['busiest_day']['day']}s ({act['busiest_day']['count']} sessions), {display_hr}{ampm} ({act['busiest_hour']['count']} sessions)")
        if act.get("active_days"):
            lines.append(f"**Active days:** {act['active_days']}")
        if act.get("max_streak", 0) > 1:
            lines.append(f"**Best streak:** {act['max_streak']} consecutive days")
    
    return "\n".join(lines)
