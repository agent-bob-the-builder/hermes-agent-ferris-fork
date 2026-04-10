"""
Session Insights Engine for Hermes Agent.

Analyzes historical session data from the SQLite state database to produce
comprehensive usage insights — token consumption, cost estimates, tool usage
patterns, activity trends, model/platform breakdowns, and session metrics.

Inspired by Claude Code's /insights command, adapted for Hermes Agent's
multi-platform architecture with additional cost estimation and platform
breakdown capabilities.

Usage:
    from agent.insights import InsightsEngine
    engine = InsightsEngine(db)
    report = engine.generate(days=30)
    print(engine.format_terminal(report))
"""

import json
import time
from collections import Counter, defaultdict
from datetime import datetime
from typing import Any, Dict, List

from agent.usage_pricing import (
    CanonicalUsage,
    DEFAULT_PRICING,
    estimate_usage_cost,
    format_duration_compact,
    get_pricing,
    has_known_pricing,
)

from agent import insights_rs

_DEFAULT_PRICING = DEFAULT_PRICING


def _has_known_pricing(model_name: str, provider: str = None, base_url: str = None) -> bool:
    """Check if a model has known pricing (vs unknown/custom endpoint)."""
    return has_known_pricing(model_name, provider=provider, base_url=base_url)


def _estimate_cost(
    session_or_model: Dict[str, Any] | str,
    input_tokens: int = 0,
    output_tokens: int = 0,
    *,
    cache_read_tokens: int = 0,
    cache_write_tokens: int = 0,
    provider: str = None,
    base_url: str = None,
) -> tuple[float, str]:
    """Estimate the USD cost for a session row or a model/token tuple."""
    if isinstance(session_or_model, dict):
        session = session_or_model
        model = session.get("model") or ""
        usage = CanonicalUsage(
            input_tokens=session.get("input_tokens") or 0,
            output_tokens=session.get("output_tokens") or 0,
            cache_read_tokens=session.get("cache_read_tokens") or 0,
            cache_write_tokens=session.get("cache_write_tokens") or 0,
        )
        provider = session.get("billing_provider")
        base_url = session.get("billing_base_url")
    else:
        model = session_or_model or ""
        usage = CanonicalUsage(
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            cache_read_tokens=cache_read_tokens,
            cache_write_tokens=cache_write_tokens,
        )
    result = estimate_usage_cost(
        model,
        usage,
        provider=provider,
        base_url=base_url,
    )
    return float(result.amount_usd or 0.0), result.status


def _format_duration(seconds: float) -> str:
    """Format seconds into a human-readable duration string."""
    return format_duration_compact(seconds)


def _bar_chart(values: List[int], max_width: int = 20) -> List[str]:
    """Create simple horizontal bar chart strings from values."""
    peak = max(values) if values else 1
    if peak == 0:
        return ["" for _ in values]
    return ["█" * max(1, int(v / peak * max_width)) if v > 0 else "" for v in values]


class InsightsEngine:
    """
    Analyzes session history and produces usage insights.

    Works directly with a SessionDB instance (or raw sqlite3 connection)
    to query session and message data.
    """

    def __init__(self, db):
        """
        Initialize with a SessionDB instance.

        Args:
            db: A SessionDB instance (from hermes_state.py)
        """
        self.db = db
        self._conn = db._conn

    def generate(self, days: int = 30, source: str = None) -> Dict[str, Any]:
        """
        Generate a complete insights report.

        Args:
            days: Number of days to look back (default: 30)
            source: Optional filter by source platform

        Returns:
            Dict with all computed insights
        """
        cutoff = time.time() - (days * 86400)

        # Gather raw data
        sessions = self._get_sessions(cutoff, source)
        tool_usage = self._get_tool_usage(cutoff, source)
        message_stats = self._get_message_stats(cutoff, source)

        if not sessions:
            return {
                "days": days,
                "source_filter": source,
                "empty": True,
                "overview": {},
                "models": [],
                "platforms": [],
                "tools": [],
                "activity": {},
                "top_sessions": [],
            }

        # Compute insights
        overview = self._compute_overview(sessions, message_stats)
        models = self._compute_model_breakdown(sessions)
        platforms = self._compute_platform_breakdown(sessions)
        tools = self._compute_tool_breakdown(tool_usage)
        activity = self._compute_activity_patterns(sessions)
        top_sessions = self._compute_top_sessions(sessions)

        return {
            "days": days,
            "source_filter": source,
            "empty": False,
            "generated_at": time.time(),
            "overview": overview,
            "models": models,
            "platforms": platforms,
            "tools": tools,
            "activity": activity,
            "top_sessions": top_sessions,
        }

    # =========================================================================
    # Data gathering (SQL queries)
    # =========================================================================

    # Columns we actually need (skip system_prompt, model_config blobs)
    _SESSION_COLS = ("id, source, model, started_at, ended_at, "
                     "message_count, tool_call_count, input_tokens, output_tokens, "
                     "cache_read_tokens, cache_write_tokens, billing_provider, "
                     "billing_base_url, billing_mode, estimated_cost_usd, "
                     "actual_cost_usd, cost_status, cost_source")

    # Pre-computed query strings — f-string evaluated once at class definition,
    # not at runtime, so no user-controlled value can alter the query structure.
    _GET_SESSIONS_WITH_SOURCE = (
        f"SELECT {_SESSION_COLS} FROM sessions"
        " WHERE started_at >= ? AND source = ?"
        " ORDER BY started_at DESC"
    )
    _GET_SESSIONS_ALL = (
        f"SELECT {_SESSION_COLS} FROM sessions"
        " WHERE started_at >= ?"
        " ORDER BY started_at DESC"
    )

    def _get_sessions(self, cutoff: float, source: str = None) -> List[Dict]:
        """Fetch sessions within the time window."""
        if source:
            cursor = self._conn.execute(self._GET_SESSIONS_WITH_SOURCE, (cutoff, source))
        else:
            cursor = self._conn.execute(self._GET_SESSIONS_ALL, (cutoff,))
        return [dict(row) for row in cursor.fetchall()]

    def _get_tool_usage(self, cutoff: float, source: str = None) -> List[Dict]:
        """Get tool call counts from messages.

        Uses two sources:
        1. tool_name column on 'tool' role messages (set by gateway)
        2. tool_calls JSON on 'assistant' role messages (covers CLI where
           tool_name is not populated on tool responses)
        """
        tool_counts = Counter()

        # Source 1: explicit tool_name on tool response messages
        if source:
            cursor = self._conn.execute(
                """SELECT m.tool_name, COUNT(*) as count
                   FROM messages m
                   JOIN sessions s ON s.id = m.session_id
                   WHERE s.started_at >= ? AND s.source = ?
                     AND m.role = 'tool' AND m.tool_name IS NOT NULL
                   GROUP BY m.tool_name
                   ORDER BY count DESC""",
                (cutoff, source),
            )
        else:
            cursor = self._conn.execute(
                """SELECT m.tool_name, COUNT(*) as count
                   FROM messages m
                   JOIN sessions s ON s.id = m.session_id
                   WHERE s.started_at >= ?
                     AND m.role = 'tool' AND m.tool_name IS NOT NULL
                   GROUP BY m.tool_name
                   ORDER BY count DESC""",
                (cutoff,),
            )
        for row in cursor.fetchall():
            tool_counts[row["tool_name"]] += row["count"]

        # Source 2: extract from tool_calls JSON on assistant messages
        # (covers CLI sessions where tool_name is NULL on tool responses)
        if source:
            cursor2 = self._conn.execute(
                """SELECT m.tool_calls
                   FROM messages m
                   JOIN sessions s ON s.id = m.session_id
                   WHERE s.started_at >= ? AND s.source = ?
                     AND m.role = 'assistant' AND m.tool_calls IS NOT NULL""",
                (cutoff, source),
            )
        else:
            cursor2 = self._conn.execute(
                """SELECT m.tool_calls
                   FROM messages m
                   JOIN sessions s ON s.id = m.session_id
                   WHERE s.started_at >= ?
                     AND m.role = 'assistant' AND m.tool_calls IS NOT NULL""",
                (cutoff,),
            )

        tool_calls_counts = Counter()
        for row in cursor2.fetchall():
            try:
                calls = row["tool_calls"]
                if isinstance(calls, str):
                    calls = json.loads(calls)
                if isinstance(calls, list):
                    for call in calls:
                        func = call.get("function", {}) if isinstance(call, dict) else {}
                        name = func.get("name")
                        if name:
                            tool_calls_counts[name] += 1
            except (json.JSONDecodeError, TypeError, AttributeError):
                continue

        # Merge: prefer tool_name source, supplement with tool_calls source
        # for tools not already counted
        if not tool_counts and tool_calls_counts:
            # No tool_name data at all — use tool_calls exclusively
            tool_counts = tool_calls_counts
        elif tool_counts and tool_calls_counts:
            # Both sources have data — use whichever has the higher count per tool
            # (they may overlap, so take the max to avoid double-counting)
            all_tools = set(tool_counts) | set(tool_calls_counts)
            merged = Counter()
            for tool in all_tools:
                merged[tool] = max(tool_counts.get(tool, 0), tool_calls_counts.get(tool, 0))
            tool_counts = merged

        # Convert to the expected format
        return [
            {"tool_name": name, "count": count}
            for name, count in tool_counts.most_common()
        ]

    def _get_message_stats(self, cutoff: float, source: str = None) -> Dict:
        """Get aggregate message statistics."""
        if source:
            cursor = self._conn.execute(
                """SELECT
                     COUNT(*) as total_messages,
                     SUM(CASE WHEN m.role = 'user' THEN 1 ELSE 0 END) as user_messages,
                     SUM(CASE WHEN m.role = 'assistant' THEN 1 ELSE 0 END) as assistant_messages,
                     SUM(CASE WHEN m.role = 'tool' THEN 1 ELSE 0 END) as tool_messages
                   FROM messages m
                   JOIN sessions s ON s.id = m.session_id
                   WHERE s.started_at >= ? AND s.source = ?""",
                (cutoff, source),
            )
        else:
            cursor = self._conn.execute(
                """SELECT
                     COUNT(*) as total_messages,
                     SUM(CASE WHEN m.role = 'user' THEN 1 ELSE 0 END) as user_messages,
                     SUM(CASE WHEN m.role = 'assistant' THEN 1 ELSE 0 END) as assistant_messages,
                     SUM(CASE WHEN m.role = 'tool' THEN 1 ELSE 0 END) as tool_messages
                   FROM messages m
                   JOIN sessions s ON s.id = m.session_id
                   WHERE s.started_at >= ?""",
                (cutoff,),
            )
        row = cursor.fetchone()
        return dict(row) if row else {
            "total_messages": 0, "user_messages": 0,
            "assistant_messages": 0, "tool_messages": 0,
        }

    # =========================================================================
    # Computation
    # =========================================================================

    def _compute_overview(self, sessions: List[Dict], message_stats: Dict) -> Dict:
        """Compute high-level overview statistics.

        Delegates to Rust for fast computation. Cost fields requiring Python
        pricing lookup are applied afterwards.
        """
        result = insights_rs.compute_overview(sessions, message_stats)

        # Cost estimation requires Python pricing lookup — apply here
        total_cost = 0.0
        actual_cost = 0.0
        models_with_pricing = set()
        models_without_pricing = set()
        unknown_cost_sessions = 0
        included_cost_sessions = 0
        for s in sessions:
            model = s.get("model") or ""
            estimated, status = _estimate_cost(s)
            total_cost += estimated
            actual_cost += s.get("actual_cost_usd") or 0.0
            display = model.split("/")[-1] if "/" in model else (model or "unknown")
            if status == "included":
                included_cost_sessions += 1
            elif status == "unknown":
                unknown_cost_sessions += 1
            if _has_known_pricing(model, s.get("billing_provider"), s.get("billing_base_url")):
                models_with_pricing.add(display)
            else:
                models_without_pricing.add(display)

        result["estimated_cost"] = total_cost
        result["actual_cost"] = actual_cost
        result["models_with_pricing"] = sorted(models_with_pricing)
        result["models_without_pricing"] = sorted(models_without_pricing)
        result["unknown_cost_sessions"] = unknown_cost_sessions
        result["included_cost_sessions"] = included_cost_sessions
        return result

    def _compute_model_breakdown(self, sessions: List[Dict]) -> List[Dict]:
        """Break down usage by model.

        Delegates to Rust for fast computation. Cost fields requiring Python
        pricing lookup are applied afterwards.
        """
        result = insights_rs.compute_model_breakdown(sessions)
        # Apply cost estimation and pricing flags
        for item in result:
            model = item.get("model") or ""
            # Find original session(s) for this model to estimate cost
            model_sessions = [s for s in sessions if (s.get("model") or "").endswith(model)]
            total_cost = 0.0
            has_pricing = False
            cost_status = "unknown"
            for s in model_sessions:
                est, status = _estimate_cost(s)
                total_cost += est
                if _has_known_pricing(s.get("model") or "", s.get("billing_provider"), s.get("billing_base_url")):
                    has_pricing = True
                cost_status = status
            item["cost"] = total_cost
            item["has_pricing"] = has_pricing
            item["cost_status"] = cost_status
        return result

    def _compute_platform_breakdown(self, sessions: List[Dict]) -> List[Dict]:
        """Break down usage by platform/source.

        Delegates to Rust for fast computation.
        """
        return insights_rs.compute_platform_breakdown(sessions)

    def _compute_tool_breakdown(self, tool_usage: List[Dict]) -> List[Dict]:
        """Process tool usage data into a ranked list with percentages.

        Delegates to Rust for fast computation.
        """
        return insights_rs.compute_tool_breakdown(tool_usage)

    def _compute_activity_patterns(self, sessions: List[Dict]) -> Dict:
        """Analyze activity patterns by day of week and hour.

        Delegates to Rust for fast computation.
        """
        return insights_rs.compute_activity_patterns(sessions)

    def _compute_top_sessions(self, sessions: List[Dict]) -> List[Dict]:
        """Find notable sessions (longest, most messages, most tokens, most tool calls).

        Delegates to Rust for fast computation.
        """
        return insights_rs.compute_top_sessions(sessions)

    def format_terminal(self, report: Dict) -> str:
        """Format the insights report for terminal display (CLI).

        Delegates to Rust for fast formatting.
        """
        if report.get("empty"):
            days = report.get("days", 30)
            src = f" (source: {report['source_filter']})" if report.get("source_filter") else ""
            return f"  No sessions found in the last {days} days{src}."
        return insights_rs.format_terminal(
            overview=report["overview"],
            models=report["models"],
            platforms=report["platforms"],
            tools=report["tools"],
            activity=report["activity"],
            top_sessions=report["top_sessions"],
            days=report["days"],
            source_filter=report.get("source_filter"),
        )

    def format_gateway(self, report: Dict) -> str:
        """Format the insights report for gateway/messaging (shorter).

        Delegates to Rust for fast formatting.
        """
        if report.get("empty"):
            days = report.get("days", 30)
            return f"No sessions found in the last {days} days."
        return insights_rs.format_gateway(
            overview=report["overview"],
            models=report["models"],
            platforms=report["platforms"],
            tools=report["tools"],
            activity=report["activity"],
            days=report["days"],
        )
