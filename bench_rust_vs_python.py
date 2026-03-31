#!/usr/bin/env python3
"""
Rust vs Python Benchmark — Hermes-Agent Ferris Fork
====================================================
Accurately compares Rust components vs their Python equivalents.

Component pairs:
  1. model_tools.get_tool_definitions    — _model_tools_rs vs pure-Python (subprocess, cold)
  2. compressor_rs operations            — tokenizer-free Rust vs Python approximations
  3. prompt_builder_rs vs agent/prompt_builder — prompt assembly sub-operations
  4. hermes_state SessionDB FTS          — hermes_state_rs (native SQLite) vs Python sqlite3

Run with: source .venv/bin/activate && python bench_rust_vs_python.py
"""

import os
import sys
import json
import time
import subprocess
import statistics
import shlex
import tempfile
from pathlib import Path

HERMES = Path("/root/.hermes/hermes-agent-ferris-fork")
sys.path.insert(0, str(HERMES))
os.environ["HERMES_HOME"] = "/tmp/hermes_bench_home"
PYTHON_BIN = str(HERMES / ".venv/bin/python3")


# ── helpers ──────────────────────────────────────────────────────────────────

def stdev(vals):
    if len(vals) < 2:
        return 0.0
    return statistics.stdev(vals)


def pct(vals, p):
    """nth percentile of a list."""
    s = sorted(vals)
    n = int(len(s) * p / 100)
    return s[min(n, len(s) - 1)]


def run_script(script_content, env=None, timeout=120):
    """
    Run a Python script in a fresh subprocess via a temp file.
    Returns (stdout, stderr, rc, dt_ms).
    """
    full_env = os.environ.copy()
    if env:
        full_env.update(env)
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
        f.write(script_content)
        script_path = f.name
    try:
        t0 = time.perf_counter()
        r = subprocess.run(
            [PYTHON_BIN, script_path],
            capture_output=True,
            text=True,
            cwd=str(HERMES),
            env=full_env,
            timeout=timeout,
        )
        dt = (time.perf_counter() - t0) * 1000
        return r.stdout, r.stderr, r.returncode, dt
    finally:
        try:
            os.unlink(script_path)
        except OSError:
            pass


def bench_subprocess(script_content, runs=10, warmup=2, env=None, timeout=120):
    """
    Time a Python script in a fresh subprocess (cold import each run).
    Returns (mean_ms, median_ms, min_ms, max_ms, p95_ms, stdev_ms, raw_values).
    """
    times = []
    first_error = None
    for i in range(runs + warmup):
        out, err, rc, dt = run_script(script_content, env=env, timeout=timeout)
        if rc == 0:
            times.append(dt)
        elif first_error is None and err:
            first_error = f"rc={rc} err={err[:300]}"

    times = times[warmup:]  # drop warmup
    if not times:
        print(f"    [debug] bench_subprocess first_error: {first_error}")
        return (float("inf"), float("inf"), float("inf"), float("inf"), float("inf"), 0.0, [])
    return (
        statistics.mean(times),
        statistics.median(times),
        min(times),
        max(times),
        pct(times, 95),
        stdev(times),
        times,
    )


# ── 1. get_tool_definitions — Rust vs Pure Python ─────────────────────────────

def bench_get_tool_definitions_rust(runs=20, warmup=5):
    """
    Rust get_tool_definitions — in-process, warm cache.
    Imports _model_tools_rs directly to bypass model_tools' lazy init
    which can fail in cold subprocess envs.
    """
    script = """
import sys, os, json, time
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'

import _model_tools_rs as rust_mod
rust_mod.initialize()

results = []
for _ in range(20):
    t0 = time.perf_counter()
    defs = rust_mod.get_tool_definitions(enabled_toolsets=['file'], quiet_mode=True)
    dt = (time.perf_counter() - t0) * 1000
    results.append(dt)
print(json.dumps({'ok': True, 'count': len(defs), 'times': results}))
"""
    out, err, rc, _ = run_script(script, timeout=60)
    if rc != 0:
        print(f"    [Rust get_tool_definitions] rc={rc} err={err[:300]}")
        return None
    try:
        data = json.loads(out)
    except json.JSONDecodeError:
        print(f"    [Rust get_tool_definitions] bad JSON: {out[:200]}")
        return None
    times = data.get("times", [])
    if not times:
        print(f"    [Rust get_tool_definitions] empty output: {out[:200]}")
        return None
    return (
        statistics.mean(times), statistics.median(times), min(times),
        max(times), pct(times, 95), stdev(times), times,
    )


def bench_get_tool_definitions_python_pure(runs=10, warmup=2):
    """
    PURE Python get_tool_definitions — FRESH subprocess, Rust not loaded.
    Disables Rust via environment variable so model_tools uses only Python.
    """
    script = """
import sys, os, json, time
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'
os.environ['HERMES_USE_RUST'] = '0'

import model_tools as mt
# Force Python-only path
mt._use_rust = False
mt._rust = None

results = []
for _ in range(10):
    mt._get_definitions_cache.clear()
    t0 = time.perf_counter()
    defs = mt.get_tool_definitions(enabled_toolsets=['file'], quiet_mode=True)
    dt = (time.perf_counter() - t0) * 1000
    results.append(dt)
print(json.dumps({'ok': True, 'count': len(defs), 'times': results}))
"""
    return bench_subprocess(script, runs=runs, warmup=warmup)


# ── 2. rust_compressor — individual operations ─────────────────────────────────

MESSAGES = [
    {"role": "system", "content": "You are a helpful AI assistant."},
    {"role": "user", "content": "Hello"},
    {"role": "assistant", "content": "Hi there! How can I help?"},
    {"role": "user", "content": "Help me write code"},
    {"role": "assistant", "content": "What kind of code?"},
    {"role": "tool", "name": "python", "content": "result here", "tool_call_id": "tc1"},
    {"role": "assistant", "content": "Done."},
    {"role": "user", "content": "Thanks"},
]


def bench_rust_compressor_ops(runs=200, warmup=20):
    """Benchmark compressor_rs individual operations."""
    script = """
import sys, os, json, time, statistics
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'
import compressor_rs as rc  # noqa: E402

messages = [
    {'role': 'system', 'content': 'You are a helpful AI assistant.'},
    {'role': 'user', 'content': 'Hello'},
    {'role': 'assistant', 'content': 'Hi there! How can I help?'},
    {'role': 'user', 'content': 'Help me write code'},
    {'role': 'assistant', 'content': 'What kind of code?'},
    {'role': 'tool', 'name': 'python', 'content': 'result here', 'tool_call_id': 'tc1'},
    {'role': 'assistant', 'content': 'Done.'},
    {'role': 'user', 'content': 'Thanks'},
]
messages_large = messages * 10

def bench(fn, n, warmup):
    for _ in range(warmup): fn()
    return [((t0 := time.perf_counter()), fn(), (time.perf_counter() - t0) * 1000)[2] for _ in range(n)]

results = {
    'estimate_8':      bench(lambda: rc.estimate_messages_tokens(messages), 200, 20),
    'estimate_80':     bench(lambda: rc.estimate_messages_tokens(messages_large), 200, 20),
    'find_tail_cut':   bench(lambda: rc.find_tail_cut(messages, 2, 200, 2), 200, 20),
    'prune':           bench(lambda: rc.prune_old_tool_results(messages, 2), 200, 20),
    'serialize':       bench(lambda: rc.serialize_turns(messages), 200, 20),
    'sanitize':        bench(lambda: rc.sanitize_tool_pairs_py(messages), 200, 20),
}
print(json.dumps({'ok': True, 'results': results}))
"""
    out, err, rc_run, _ = run_script(script, timeout=60)
    if rc_run != 0:
        print(f"    [rust_compressor] rc={rc_run} err={err[:200]}")
        return None
    try:
        data = json.loads(out)
        return data["results"]
    except (json.JSONDecodeError, KeyError):
        print(f"    [rust_compressor] bad output: {out[:200]}")
        return None


def bench_python_compressor_equivalents(runs=50, warmup=10):
    """Python equivalents of rust_compressor operations."""

    def python_estimate_tokens(messages):
        return sum(len(str(m)) // 4 for m in messages)

    def python_find_tail_cut(messages, head_end, token_budget, protect_last_n):
        total = python_estimate_tokens(messages)
        if total <= token_budget:
            return len(messages)
        return max(head_end, len(messages) - protect_last_n)

    def python_prune_old_tool_results(messages, protect_tail_count):
        result, tool_results = [], []
        for m in messages:
            if m.get("role") == "tool":
                tool_results.append(m)
            else:
                if tool_results:
                    result.extend(tool_results[-2:])
                    tool_results = []
                result.append(m)
        if tool_results:
            result.extend(tool_results[-protect_tail_count:])
        return result

    def python_serialize(messages):
        return json.dumps(messages).encode()

    def python_sanitize(messages):
        result, prev_tool = [], None
        for m in messages:
            if m.get("role") == "tool":
                if prev_tool and prev_tool.get("tool_call_id") == m.get("tool_call_id"):
                    continue
                prev_tool = m
            else:
                prev_tool = None
            result.append(m)
        return result

    tests = [
        ("py_estimate_8",   (lambda: python_estimate_tokens(MESSAGES)),         runs, warmup),
        ("py_estimate_80",  (lambda: python_estimate_tokens(MESSAGES * 10)),     runs, warmup),
        ("py_find_tail",    (lambda: python_find_tail_cut(MESSAGES, 2, 200, 2)), runs, warmup),
        ("py_prune",       (lambda: python_prune_old_tool_results(MESSAGES, 2)), runs, warmup),
        ("py_serialize",    (lambda: python_serialize(MESSAGES)),                 runs, warmup),
        ("py_sanitize",    (lambda: python_sanitize(MESSAGES)),                   runs, warmup),
    ]

    results = {}
    for name, fn, n, w in tests:
        for _ in range(w): fn()
        times = []
        for _ in range(n):
            t0 = time.perf_counter()
            fn()
            times.append((time.perf_counter() - t0) * 1000)
        results[name] = times
    return results


# ── 3. prompt_builder_rs vs agent/prompt_builder ────────────────────────────

def bench_prompt_builder_rust(runs=100, warmup=20):
    """Benchmark prompt_builder_rs sub-operations."""
    script = """
import sys, os, json, time
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'
import prompt_builder_rs as pb

test_content = 'This is a test file with some content that needs truncation.'
test_yaml = '---\\ntitle: Test\\n---\\n# Markdown content\\n'

def bench(fn, n, warmup):
    for _ in range(warmup): fn()
    return [((t0 := time.perf_counter()), fn(), (time.perf_counter() - t0) * 1000)[2] for _ in range(n)]

results = {
    'strip_yaml': bench(lambda: pb.strip_yaml_frontmatter_py(test_yaml), 100, 20),
    'truncate':   bench(lambda: pb.truncate_content_py(test_content, 'test.txt', 100), 100, 20),
    'build':       bench(lambda: pb.build(identity='Bob', system_message='You are helpful.', memory_store_json='[]', _user_profile_json='{}', skip_context_files=True, skip_soul=True), 100, 20),
}
print(json.dumps({'ok': True, 'results': results}))
"""
    out, err, rc_run, _ = run_script(script, timeout=30)
    if rc_run != 0:
        print(f"    [_prompt_builder_rust] rc={rc_run} err={err[:200]}")
        return None
    try:
        data = json.loads(out)
        return data["results"]
    except (json.JSONDecodeError, KeyError):
        print(f"    [_prompt_builder_rust] bad output: {out[:200]}")
        return None


def bench_prompt_builder_python(runs=50, warmup=10):
    """Python equivalents of agent/prompt_builder operations."""
    import re

    def python_strip_yaml(content):
        return re.sub(r"^---\n.*?\n---\n", "", content, count=1, flags=re.DOTALL)

    def python_truncate(content, filename, max_chars):
        if len(content) <= max_chars:
            return content
        return content[: max_chars - 20] + f"\n... [truncated {filename}]"

    test_content = "This is a test file with some content that needs truncation."
    test_yaml = "---\ntitle: Test\n---\n# Markdown content\n"

    tests = [
        ("py_strip_yaml", (lambda: python_strip_yaml(test_yaml)),    runs, warmup),
        ("py_truncate",    (lambda: python_truncate(test_content, "test.txt", 100)), runs, warmup),
    ]

    results = {}
    for name, fn, n, w in tests:
        for _ in range(w): fn()
        times = []
        for _ in range(n):
            t0 = time.perf_counter()
            fn()
            times.append((time.perf_counter() - t0) * 1000)
        results[name] = times
    return results


# ── 4. SessionDB FTS — hermes_state (Rust SQLite) vs python sqlite3 ─────────

def bench_sessiondb_fts_rust(runs=50, warmup=10):
    """hermes_state SessionDB FTS — uses _hermes_state_rs (rusqlite)."""
    script = """
import sys, os, json, time, pathlib
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_hermes'
import hermes_state

db_path = pathlib.Path('/tmp/hermes_bench_hermes/bench_session.db')
db_path.parent.mkdir(parents=True, exist_ok=True)
db = hermes_state.SessionDB(str(db_path))

for sid in ['s1','s2','s3','s4','s5']:
    db.create_session(session_id=sid, source='bench')
    for i in range(40):
        db.append_message(sid, 'user', f'test message {i} with keywords', 'bench')
        db.append_message(sid, 'assistant', f'response {i} content', 'bench')

times = []
for _ in range(warmup):
    db.search_messages('test keywords', session_id='s1', limit=5)
for _ in range(runs):
    t0 = time.perf_counter()
    rows = db.search_messages('test keywords', session_id='s1', limit=5)
    times.append((time.perf_counter() - t0) * 1000)
print(json.dumps({'ok': True, 'rows': len(rows), 'times': times}))
"""
    out, err, rc_run, _ = run_script(script, timeout=30)
    if rc_run != 0:
        print(f"    [SessionDB rust] rc={rc_run} err={err[:200]}")
        return None
    try:
        data = json.loads(out)
        times = data.get("times", [])
        if not times:
            print(f"    [SessionDB rust] no times: {out[:200]}")
            return None
        return (
            statistics.mean(times), statistics.median(times), min(times),
            max(times), pct(times, 95), stdev(times), times,
        )
    except (json.JSONDecodeError, KeyError):
        print(f"    [SessionDB rust] bad output: {out[:200]}")
        return None


def bench_sessiondb_fts_python_sqlite(runs=50, warmup=10):
    """Pure Python sqlite3 + FTS5 — equivalent of what Rust backend accelerates."""
    import sqlite3

    db_path = Path("/tmp/hermes_bench_hermes/bench_python_fts.db")
    db_path.parent.mkdir(parents=True, exist_ok=True)
    if db_path.exists():
        db_path.unlink()

    conn = sqlite3.connect(str(db_path))
    c = conn.cursor()
    c.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS messages USING fts5("
        "session_id, role, content, source, tokenize='porter unicode61')"
    )
    for sid in ["s1", "s2", "s3", "s4", "s5"]:
        for i in range(40):
            c.execute(
                "INSERT INTO messages(session_id, role, content, source) VALUES (?, ?, ?, ?)",
                (sid, "user", f"test message {i} with keywords", "bench"),
            )
            c.execute(
                "INSERT INTO messages(session_id, role, content, source) VALUES (?, ?, ?, ?)",
                (sid, "assistant", f"response {i} content", "bench"),
            )
    conn.commit()

    times = []
    for _ in range(warmup):
        c.execute(
            "SELECT session_id, role, content, source FROM messages "
            "WHERE session_id = 's1' AND content MATCH 'test keywords' LIMIT 5"
        )
        c.fetchall()

    for _ in range(runs):
        t0 = time.perf_counter()
        c.execute(
            "SELECT session_id, role, content, source FROM messages "
            "WHERE session_id = 's1' AND content MATCH 'test keywords' LIMIT 5"
        )
        c.fetchall()
        times.append((time.perf_counter() - t0) * 1000)
    conn.close()

    return (
        statistics.mean(times), statistics.median(times), min(times),
        max(times), pct(times, 95), stdev(times), times,
    )


# ── 5. Cold import ────────────────────────────────────────────────────────────

def bench_cold_import(runs=5, warmup=1):
    """Cold import of model_tools with Rust backend."""
    script = """
import sys, os, json, time
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'
t0 = time.perf_counter()
from model_tools import _use_rust, _rust
dt = (time.perf_counter() - t0) * 1000
print(json.dumps({'ok': True, 'rust_active': _use_rust, 'dt': dt}))
"""
    return bench_subprocess(script, runs=runs, warmup=warmup)


# ── report helpers ───────────────────────────────────────────────────────────

def fmt_ms(mean, p95=None, median=None, min_=None, sigma=None):
    if mean is None:
        return "N/A"
    parts = f"{mean:.3f}ms mean"
    if median is not None:
        parts += f" (med: {median:.3f}ms)"
    if min_ is not None:
        parts += f", min: {min_:.3f}ms"
    if sigma is not None and sigma > 0:
        parts += f", σ: {sigma:.1f}ms"
    if p95 is not None:
        parts += f", p95: {p95:.3f}ms"
    return parts


def section(title):
    print()
    print(f"{'=' * 60}")
    print(f"  {title}")
    print(f"{'=' * 60}")


def result_row(name, rust_ms, py_ms, note=""):
    rust_s = f"{rust_ms:.3f}ms" if rust_ms is not None else "N/A"
    py_s   = f"{py_ms:.3f}ms"  if py_ms   is not None else "N/A"
    ratio = ""
    if rust_ms and py_ms and rust_ms > 0 and py_ms > 0:
        r = py_ms / rust_ms
        faster = "🐇 Rust" if r > 1 else "🐍 Python"
        ratio = f"  → {r:.1f}x {faster}"
    print(f"  {name:<38} Rust: {rust_s:>9}  Py: {py_s:>9} {ratio}")
    if note:
        print(f"    {note}")


# ── main ──────────────────────────────────────────────────────────────────────

def main():
    print("🏗️  Hermes Ferris Fork — Rust vs Python Benchmark")
    print(f"   Python: {sys.version.split()[0]}  |  repo: {HERMES}")
    print(f"   HERMES_HOME: {os.environ.get('HERMES_HOME', '?')}")

    results = {}

    # ── 1. Cold import ───────────────────────────────────────────────────────
    section("1. Cold Import (model_tools — Rust backend)")
    ci = bench_cold_import(runs=5, warmup=1)
    if ci and ci[0] != float("inf"):
        n, m, mn, mx, p95, sd, vals = ci
        print(f"  Cold import (Rust active): {n:.1f}ms mean  (p95: {p95:.1f}ms, min: {mn:.1f}ms, σ: {sd:.1f}ms)")
        print(f"  Runs: {[f'{v:.1f}' for v in vals]}")
        results["cold_import"] = {"mean_ms": n, "p95_ms": p95}
    else:
        print("  ERR — cold import benchmark failed")

    # ── 2. get_tool_definitions ──────────────────────────────────────────────
    section("2. get_tool_definitions — Rust fast-path vs Pure Python")
    print("  [Rust path: direct _model_tools_rust import, 20 runs]")
    rust_defs = bench_get_tool_definitions_rust(runs=20, warmup=5)
    if rust_defs:
        n, m, mn, mx, p95, sd, vals = rust_defs
        print(f"  Rust:  {fmt_ms(n, p95, m, mn, sd)}")
        results["get_tool_definitions_rust"] = {"mean_ms": n, "p95_ms": p95}
    else:
        print("  ERR — Rust get_tool_definitions not available")

    print("  [Python path: fresh subprocess, HERMES_USE_RUST=0, 10 runs]")
    py_defs = bench_get_tool_definitions_python_pure(runs=10, warmup=2)
    if py_defs and py_defs[0] != float("inf"):
        n, m, mn, mx, p95, sd, vals = py_defs
        print(f"  Python: {fmt_ms(n, p95, m, mn, sd)}")
        results["get_tool_definitions_python"] = {"mean_ms": n, "p95_ms": p95}
        if rust_defs:
            ratio = n / rust_defs[0]
            print(f"  → Rust is {ratio:.0f}x faster for get_tool_definitions")
    else:
        print("  ERR — Python get_tool_definitions failed")

    # ── 3. rust_compressor operations ────────────────────────────────────────
    section("3. rust_compressor.so — Individual Operations")
    print("  Rust tokenizer-free ops vs Python equivalents (200 runs each)")
    rust_ops = bench_rust_compressor_ops(runs=200, warmup=20)
    py_ops = bench_python_compressor_equivalents(runs=50, warmup=10)

    if rust_ops and py_ops:
        pairings = [
            ("estimate_tokens (8 msg)",   "estimate_8",     "py_estimate_8"),
            ("estimate_tokens (80 msg)",  "estimate_80",    "py_estimate_80"),
            ("find_tail_cut",             "find_tail_cut",   "py_find_tail"),
            ("prune_old_tool_results",    "prune",           "py_prune"),
            ("serialize_turns",           "serialize",        "py_serialize"),
            ("sanitize_tool_pairs",       "sanitize",         "py_sanitize"),
        ]
        for name, rkey, pkey in pairings:
            if rkey in rust_ops and pkey in py_ops:
                result_row(name, statistics.mean(rust_ops[rkey]), statistics.mean(py_ops[pkey]),
                           note="(Python = naive char/4 estimator)")
    else:
        print("  ERR — compressor benchmarks failed")

    # ── 4. _prompt_builder_rust ─────────────────────────────────────────────
    section("4. prompt_builder_rs — Prompt Building Sub-operations")
    rust_pb = bench_prompt_builder_rust(runs=100, warmup=20)
    py_pb = bench_prompt_builder_python(runs=50, warmup=10)

    if rust_pb and py_pb:
        pairings = [
            ("strip_yaml_frontmatter", "strip_yaml", "py_strip_yaml"),
            ("truncate_content",       "truncate",    "py_truncate"),
        ]
        for name, rkey, pkey in pairings:
            if rkey in rust_pb and pkey in py_pb:
                result_row(name, statistics.mean(rust_pb[rkey]), statistics.mean(py_pb[pkey]))
        if "build" in rust_pb:
            print(f"  {'build (full prompt)':<38} Rust: {statistics.mean(rust_pb['build']):.3f}ms  (N/A for Python)")
    else:
        print("  ERR — prompt builder benchmark failed")

    # ── 5. SessionDB FTS ─────────────────────────────────────────────────────
    section("5. SessionDB FTS — hermes_state_rs (rusqlite) vs python sqlite3")
    sdb_rust = bench_sessiondb_fts_rust(runs=50, warmup=10)
    sdb_py = bench_sessiondb_fts_python_sqlite(runs=50, warmup=10)

    if sdb_rust:
        n, m, mn, mx, p95, sd, vals = sdb_rust
        print(f"  hermes_state (rusqlite/SQLite FTS5): {fmt_ms(n, p95, m, mn, sd)}")
        results["sessiondb_rust"] = {"mean_ms": n, "p95_ms": p95}
    else:
        print("  ERR — hermes_state SessionDB benchmark failed")

    if sdb_py:
        n, m, mn, mx, p95, sd, vals = sdb_py
        print(f"  python sqlite3 (pure FTS5):           {fmt_ms(n, p95, m, mn, sd)}")
        results["sessiondb_python"] = {"mean_ms": n, "p95_ms": p95}
        if sdb_rust:
            ratio = n / sdb_rust[0]
            faster = "Rust" if ratio > 1 else "Python"
            print(f"  → {ratio:.1f}x faster ({faster})")
    else:
        print("  ERR — python sqlite3 FTS benchmark failed")

    # ── Summary ──────────────────────────────────────────────────────────────
    section("SUMMARY — What Rust Accelerates")
    print("""
  Component pairs compared:
    get_tool_definitions  → _model_tools_rs fast-path vs pure Python (no Rust loaded)
    compressor_rs ops      → tokenizer-free Rust vs Python approximations
    prompt_builder_rs      → prompt assembly primitives
    hermes_state SessionDB → rusqlite/SQLite FTS5 vs python sqlite3

  Note: The Python comparison for get_tool_definitions uses a FRESH subprocess
  with HERMES_USE_RUST=0 — the fairest possible baseline (no Rust involvement).
""")

    # Discord embed summary
    section("DISCORD EMBED FIELDS")
    fields = []
    if rust_defs and py_defs and rust_defs[0] and py_defs[0]:
        fields.append((f"get_tool_definitions Rust", f"{rust_defs[0]:.3f}ms", True))
        fields.append((f"get_tool_definitions Python (pure)", f"{py_defs[0]:.1f}ms", True))
        fields.append((f"Speedup (Rust/Python)", f"{py_defs[0]/rust_defs[0]:.0f}x 🐇", False))
    if rust_ops and py_ops and "estimate_8" in rust_ops and "py_estimate_8" in py_ops:
        rm, pm = statistics.mean(rust_ops["estimate_8"]), statistics.mean(py_ops["py_estimate_8"])
        fields.append(("estimate_tokens Rust (8 msg)", f"{rm:.3f}ms", False))
        fields.append(("estimate_tokens Python (naive)", f"{pm:.3f}ms", False))
        fields.append(("Token count speedup", f"{pm/rm:.0f}x 🐇", False))
    if sdb_rust:
        fields.append(("SessionDB FTS (rusqlite)", f"{sdb_rust[0]:.2f}ms", True))
    if sdb_py and sdb_rust:
        fields.append(("SessionDB FTS (python sqlite3)", f"{sdb_py[0]:.2f}ms", False))
        fields.append(("FTS speedup", f"{sdb_py[0]/sdb_rust[0]:.1f}x", False))

    for name, val, important in fields:
        flag = "📌" if important else "  "
        print(f"  {flag} {name:<38} {val}")

    # Save results
    out_path = HERMES / "bench_results.json"
    with open(out_path, "a") as f:
        f.write(json.dumps({"ts": time.time(), "results": results}) + "\n")
    print(f"\n  📊 Results appended to {out_path}")


if __name__ == "__main__":
    main()
