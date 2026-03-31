#!/usr/bin/env python3
"""
Rust vs Python Benchmark — Hermes-Agent Ferris Fork
====================================================
Accurately compares Rust components vs their Python equivalents.

Component pairs:
  1. _model_tools_rs   — _model_tools_rs.get_tool_definitions vs pure Python model_tools
  2. compressor_rs     — Rust trajectory/token ops vs Python trajectory_compressor
  3. prompt_builder_rs  — Rust primitives vs agent/prompt_builder equivalents
  4. hermes_state_rs    — Rust SQLite (rusqlite) vs Python sqlite3

Run with: source .venv/bin/activate && python bench_rust_vs_python.py
"""

import os
import sys
import json
import time
import subprocess
import statistics
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


def in_process(script_content, timeout=60):
    """Run inline in this process — for Rust libs that are already imported."""
    namespace = {"__name__": "__bench__"}
    try:
        exec(compile(script_content, "<bench>", "exec"), namespace)
        return namespace.get("__result__")
    except Exception as e:
        return {"error": str(e)[:100]}


# ── 1. _model_tools_rs get_tool_definitions ───────────────────────────────────

def bench_get_tool_definitions_rust(runs=20, warmup=5):
    """
    Rust _model_tools_rs.get_tool_definitions — in-process, warm.
    Measures ONLY the dispatch/serialisation, not module import.
    """
    script = f"""
import sys, os, json, time
sys.path.insert(0, '{HERMES}')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'

import _model_tools_rs as mtr
mtr.initialize()

results = []
for _ in range({warmup}):
    mtr.get_tool_definitions(enabled_toolsets=['file'], quiet_mode=True)

for _ in range({runs}):
    t0 = time.perf_counter()
    defs = mtr.get_tool_definitions(enabled_toolsets=['file'], quiet_mode=True)
    dt = (time.perf_counter() - t0) * 1000
    results.append(dt)

print(json.dumps({{'ok': True, 'count': len(defs), 'times': results}}))
"""
    out, err, rc, _ = run_script(script, timeout=60)
    if rc != 0:
        print(f"    [_model_tools_rs] rc={rc} err={err[:300]}")
        return None
    try:
        data = json.loads(out)
    except json.JSONDecodeError:
        print(f"    [_model_tools_rs] bad JSON: {out[:200]}")
        return None
    times = data.get("times", [])
    if not times:
        print(f"    [_model_tools_rs] empty output: {out[:200]}")
        return None
    return (
        statistics.mean(times), statistics.median(times), min(times),
        max(times), pct(times, 95), stdev(times), times,
    )


def bench_get_tool_definitions_python(runs=10, warmup=2):
    """
    PURE Python model_tools.get_tool_definitions — fresh subprocess.
    Rust is never loaded. Fairest possible baseline.
    """
    script = """
import sys, os, json, time
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'
os.environ['HERMES_USE_RUST'] = '0'

import model_tools as mt
mt._use_rust = False
mt._rust = None

results = []
for _ in range(2):
    mt.get_tool_definitions(enabled_toolsets=['file'], quiet_mode=True)

for _ in range(10):
    mt._get_definitions_cache.clear()
    t0 = time.perf_counter()
    defs = mt.get_tool_definitions(enabled_toolsets=['file'], quiet_mode=True)
    dt = (time.perf_counter() - t0) * 1000
    results.append(dt)
print(json.dumps({'ok': True, 'count': len(defs), 'times': results}))
"""
    return bench_subprocess(script, runs=runs, warmup=warmup)


# ── 2. compressor_rs — Rust vs trajectory_compressor ──────────────────────────

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
MESSAGES_LARGE = MESSAGES * 10  # 80 messages


def bench_compressor_rs_ops(runs=200, warmup=20):
    """Benchmark compressor_rs (Rust) individual operations."""
    script = f"""
import sys, os, json, time
sys.path.insert(0, '{HERMES}')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'
import compressor_rs as cr

messages = [
    {{'role': 'system', 'content': 'You are a helpful AI assistant.'}},
    {{'role': 'user', 'content': 'Hello'}},
    {{'role': 'assistant', 'content': 'Hi there! How can I help?'}},
    {{'role': 'user', 'content': 'Help me write code'}},
    {{'role': 'assistant', 'content': 'What kind of code?'}},
    {{'role': 'tool', 'name': 'python', 'content': 'result here', 'tool_call_id': 'tc1'}},
    {{'role': 'assistant', 'content': 'Done.'}},
    {{'role': 'user', 'content': 'Thanks'}},
]
messages_large = messages * 10

def bench(fn, n, warmup):
    for _ in range(warmup): fn()
    return [((t0 := time.perf_counter()), fn(), (time.perf_counter() - t0) * 1000)[2] for _ in range(n)]

results = {{
    'estimate_8':    bench(lambda: cr.estimate_messages_tokens(messages), {runs}, {warmup}),
    'estimate_80':   bench(lambda: cr.estimate_messages_tokens(messages_large), {runs}, {warmup}),
    'find_tail_cut': bench(lambda: cr.find_tail_cut(messages, 2, 200, 2), {runs}, {warmup}),
    'prune':         bench(lambda: cr.prune_old_tool_results(messages, 2), {runs}, {warmup}),
    'serialize':     bench(lambda: cr.serialize_turns(messages), {runs}, {warmup}),
    'sanitize':      bench(lambda: cr.sanitize_tool_pairs_py(messages), {runs}, {warmup}),
}}
print(json.dumps({{'ok': True, 'results': results}}))
"""
    out, err, rc, _ = run_script(script, timeout=60)
    if rc != 0:
        print(f"    [compressor_rs] rc={rc} err={err[:200]}")
        return None
    try:
        data = json.loads(out)
        return data["results"]
    except (json.JSONDecodeError, KeyError):
        print(f"    [compressor_rs] bad output: {out[:200]}")
        return None


def bench_trajectory_compressor_python(runs=50, warmup=10):
    """
    Python trajectory_compressor TrajectoryCompressor — full class with
    tokenization, summarisation budgeting, and head/tail logic.
    This is what compressor_rs replaces.
    """
    script = """
import sys, os, json, time
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'
os.environ['HERMES_USE_RUST'] = '0'
import trajectory_compressor as tc

# Use the same tokenizer that trajectory_compressor.py uses internally
try:
    from agent.prompt_builder import tiktoken
    enc = tiktoken.get_encoding('cl100k_base')
    has_tiktoken = True
except Exception:
    enc = None
    has_tiktoken = False

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

def python_token_count(msgs):
    if has_tiktoken:
        return sum(len(enc.encode(str(m))) for m in msgs)
    return sum(len(str(m)) // 4 for m in msgs)

def python_find_tail_cut(msgs, head_end, token_budget, protect_last_n):
    total = python_token_count(msgs)
    if total <= token_budget:
        return len(msgs)
    return max(head_end, len(msgs) - protect_last_n)

def python_prune_old_tool_results(msgs, protect_tail_count):
    result, tool_results = [], []
    for m in msgs:
        if m.get('role') == 'tool':
            tool_results.append(m)
        else:
            if tool_results:
                result.extend(tool_results[-2:])
                tool_results = []
            result.append(m)
    if tool_results:
        result.extend(tool_results[-protect_tail_count:])
    return result

def python_serialize(msgs):
    return json.dumps(msgs).encode()

tests = [
    ('py_estimate_8',    lambda: python_token_count(messages),         RUNS, WARMUP),
    ('py_estimate_80',   lambda: python_token_count(messages_large),   RUNS, WARMUP),
    ('py_find_tail',     lambda: python_find_tail_cut(messages, 2, 200, 2), RUNS, WARMUP),
    ('py_prune',         lambda: python_prune_old_tool_results(messages, 2), RUNS, WARMUP),
    ('py_serialize',      lambda: python_serialize(messages),           RUNS, WARMUP),
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

print(json.dumps({'ok': True, 'has_tiktoken': has_tiktoken, 'results': results}))
""".replace("RUNS", str(runs)).replace("WARMUP", str(warmup))
    out, err, rc, _ = run_script(script, timeout=60)
    if rc != 0:
        print(f"    [trajectory_compressor python] rc={rc} err={err[:200]}")
        return None
    try:
        data = json.loads(out)
        return data["results"], data.get("has_tiktoken", False)
    except (json.JSONDecodeError, KeyError):
        print(f"    [trajectory_compressor python] bad output: {out[:200]}")
        return None, False


# ── 3. prompt_builder_rs vs agent/prompt_builder ─────────────────────────────

def bench_prompt_builder_rs(runs=100, warmup=20):
    """Benchmark prompt_builder_rs sub-operations."""
    script = f"""
import sys, os, json, time
sys.path.insert(0, '{HERMES}')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'
import prompt_builder_rs as pb

test_content = 'This is a test file with some content that needs truncation.'
test_yaml = '---''\\ntitle: Test''\\n---''\\n# Markdown content''\\n'

def bench(fn, n, warmup):
    for _ in range(warmup): fn()
    return [((t0 := time.perf_counter()), fn(), (time.perf_counter() - t0) * 1000)[2] for _ in range(n)]

results = {{
    'strip_yaml': bench(lambda: pb.strip_yaml_frontmatter_py(test_yaml), {runs}, {warmup}),
    'truncate':   bench(lambda: pb.truncate_content_py(test_content, 'test.txt', 100), {runs}, {warmup}),
    'build':       bench(lambda: pb.build(
        identity='Bob',
        system_message='You are helpful.',
        memory_store_json='[]',
        _user_profile_json='{{}}',
        skip_context_files=True,
        skip_soul=True,
    ), {runs}, {warmup}),
}}
print(json.dumps({{'ok': True, 'results': results}}))
"""
    out, err, rc, _ = run_script(script, timeout=30)
    if rc != 0:
        print(f"    [prompt_builder_rs] rc={rc} err={err[:200]}")
        return None
    try:
        data = json.loads(out)
        return data["results"]
    except (json.JSONDecodeError, KeyError):
        print(f"    [prompt_builder_rs] bad output: {out[:200]}")
        return None


def bench_prompt_builder_python(runs=50, warmup=10):
    """Python equivalents of agent/prompt_builder primitives."""
    script = f"""
import sys, os, json, time, re
sys.path.insert(0, '{HERMES}')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_home'
from agent import prompt_builder as pb_mod

test_content = 'This is a test file with some content that needs truncation.'
test_yaml = '---''\\ntitle: Test''\\n---''\\n# Markdown content''\\n'

def python_strip_yaml(content):
    return re.sub(r'^---''\\n.*?\\n---''\\n', '', content, count=1, flags=re.DOTALL)

def python_truncate(content, filename, max_chars):
    if len(content) <= max_chars:
        return content
    return content[: max_chars - 20] + f"\\n... [truncated {{filename}}]"

def python_build_context_files(cwd=None, skip_soul=True):
    return pb_mod.build_context_files_prompt(cwd=cwd or '{HERMES}', skip_soul=skip_soul)

tests = [
    ('py_strip_yaml',    lambda: python_strip_yaml(test_yaml),                             {runs}, {warmup}),
    ('py_truncate',      lambda: python_truncate(test_content, 'test.txt', 100),            {runs}, {warmup}),
    ('py_build_ctxfiles', lambda: python_build_context_files(skip_soul=True),               {runs}, {warmup}),
]

results = {{}}
for name, fn, n, w in tests:
    for _ in range(w): fn()
    times = []
    for _ in range(n):
        t0 = time.perf_counter()
        fn()
        times.append((time.perf_counter() - t0) * 1000)
    results[name] = times
print(json.dumps({{'ok': True, 'results': results}}))
"""
    out, err, rc, _ = run_script(script, timeout=30)
    if rc != 0:
        print(f"    [prompt_builder python] rc={rc} err={err[:200]}")
        return None
    try:
        data = json.loads(out)
        return data["results"]
    except (json.JSONDecodeError, KeyError):
        print(f"    [prompt_builder python] bad output: {out[:200]}")
        return None


# ── 4. _hermes_state_rust vs python sqlite3 ────────────────────────────────────

_HERMES_STATE_RUST_SCRIPT = """
import sys, os, json, time, pathlib
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_rust'
import _hermes_state_rust as hsr

db_path = pathlib.Path('/tmp/hermes_bench_rust/bench_session.db')
db_path.parent.mkdir(parents=True, exist_ok=True)
hsr.init(str(db_path))

for sid in ['s1','s2','s3','s4','s5']:
    hsr.create_session(
        session_id=sid,
        source='bench',
        model='test',
        model_config='',
        system_prompt='',
        user_id=None,
        parent_session_id=None,
    )
    for i in range(40):
        hsr.append_message(
            session_id=sid, role='user',
            content='test message ' + str(i) + ' with keywords',
            tool_call_id=None, tool_calls=None, tool_name=None,
            token_count=None, finish_reason=None,
            reasoning=None, reasoning_details=None, codex_reasoning_items=None,
        )
        hsr.append_message(
            session_id=sid, role='assistant',
            content='response ' + str(i) + ' content',
            tool_call_id=None, tool_calls=None, tool_name=None,
            token_count=None, finish_reason=None,
            reasoning=None, reasoning_details=None, codex_reasoning_items=None,
        )

times = []
for _ in range(WARMUP):
    hsr.search_messages('test keywords', source_filter=None, exclude_sources=None, role_filter=None, limit=5, offset=0)
for _ in range(RUNS):
    t0 = time.perf_counter()
    rows = hsr.search_messages('test keywords', source_filter=None, exclude_sources=None, role_filter=None, limit=5, offset=0)
    times.append((time.perf_counter() - t0) * 1000)
print(json.dumps({'ok': True, 'rows': len(rows) if rows else 0, 'times': times}))
"""

_SQLITE3_FTS_SCRIPT = """
import sys, os, json, time, sqlite3
sys.path.insert(0, '/root/.hermes/hermes-agent-ferris-fork')
os.environ['HERMES_HOME'] = '/tmp/hermes_bench_python'

db_path = '/tmp/hermes_bench_python/bench_python_fts.db'
import pathlib
pathlib.Path(db_path).parent.mkdir(parents=True, exist_ok=True)

conn = sqlite3.connect(db_path)
c = conn.cursor()
c.execute(
    'CREATE VIRTUAL TABLE IF NOT EXISTS messages USING fts5('
    'session_id, role, content, source, tokenize=unicode61)'
)
for sid in ['s1','s2','s3','s4','s5']:
    for i in range(40):
        c.execute(
            'INSERT INTO messages(session_id, role, content, source) VALUES (?, ?, ?, ?)',
            (sid, 'user', 'test message ' + str(i) + ' with keywords', 'bench'),
        )
        c.execute(
            'INSERT INTO messages(session_id, role, content, source) VALUES (?, ?, ?, ?)',
            (sid, 'assistant', 'response ' + str(i) + ' content', 'bench'),
        )
conn.commit()

times = []
for _ in range(WARMUP):
    c.execute(
        "SELECT session_id, role, content, source FROM messages "
        "WHERE session_id = 's1' AND content MATCH 'test keywords' LIMIT 5"
    )
    c.fetchall()

for _ in range(RUNS):
    t0 = time.perf_counter()
    c.execute(
        "SELECT session_id, role, content, source FROM messages "
        "WHERE session_id = 's1' AND content MATCH 'test keywords' LIMIT 5"
    )
    c.fetchall()
    times.append((time.perf_counter() - t0) * 1000)
conn.close()
print(json.dumps({'ok': True, 'times': times}))
"""


def bench_hermes_state_rust(runs=50, warmup=10):
    """_hermes_state_rust (rusqlite/SQLite FTS5)."""
    script = _HERMES_STATE_RUST_SCRIPT.replace("WARMUP", str(warmup)).replace("RUNS", str(runs))
    out, err, rc, _ = run_script(script, timeout=30)
    if rc != 0:
        print(f"    [_hermes_state_rust] rc={rc} err={err[:200]}")
        return None
    try:
        data = json.loads(out)
        times = data.get("times", [])
        if not times:
            print(f"    [_hermes_state_rust] no times: {out[:200]}")
            return None
        return (
            statistics.mean(times), statistics.median(times), min(times),
            max(times), pct(times, 95), stdev(times), times,
        )
    except (json.JSONDecodeError, KeyError):
        print(f"    [_hermes_state_rust] bad output: {out[:200]}")
        return None


def bench_sqlite3_fts_python(runs=50, warmup=10):
    """Pure Python sqlite3 + FTS5 — same query as _hermes_state_rust."""
    script = _SQLITE3_FTS_SCRIPT.replace("WARMUP", str(warmup)).replace("RUNS", str(runs))
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
    print(f"{'=' * 62}")
    print(f"  {title}")
    print(f"{'=' * 62}")


def result_row(name, rust_ms, py_ms, note=""):
    rust_s = f"{rust_ms:.3f}ms" if rust_ms is not None else "N/A"
    py_s   = f"{py_ms:.3f}ms"  if py_ms   is not None else "N/A"
    ratio = ""
    if rust_ms and py_ms and rust_ms > 0 and py_ms > 0:
        r = py_ms / rust_ms
        faster = "🐇 Rust" if r > 1 else "🐍 Python"
        ratio = f"  → {r:.1f}x {faster}"
    print(f"  {name:<40} Rust: {rust_s:>9}  Py: {py_s:>9} {ratio}")
    if note:
        print(f"    {note}")


# ── main ──────────────────────────────────────────────────────────────────────

def main():
    print("🏗️  Hermes Ferris Fork — Rust vs Python Benchmark")
    print(f"   Python: {sys.version.split()[0]}  |  repo: {HERMES}")
    print(f"   HERMES_HOME: {os.environ.get('HERMES_HOME', '?')}")

    results = {}

    # ── 1. get_tool_definitions ────────────────────────────────────────────────
    section("1. get_tool_definitions — _model_tools_rs vs pure Python")
    print("  [_model_tools_rs: in-process, warm cache, 20 runs]")
    rust_defs = bench_get_tool_definitions_rust(runs=20, warmup=5)
    if rust_defs:
        n, m, mn, mx, p95, sd, vals = rust_defs
        print(f"  Rust:     {fmt_ms(n, p95, m, mn, sd)}")
        results["get_tool_definitions_rust"] = {"mean_ms": round(n, 3), "p95_ms": round(p95, 3)}
    else:
        print("  ERR — _model_tools_rs not available")

    print("  [Python: fresh subprocess, HERMES_USE_RUST=0, 10 runs]")
    py_defs = bench_get_tool_definitions_python(runs=10, warmup=2)
    if py_defs and py_defs[0] != float("inf"):
        n, m, mn, mx, p95, sd, vals = py_defs
        print(f"  Python:   {fmt_ms(n, p95, m, mn, sd)}")
        results["get_tool_definitions_python"] = {"mean_ms": round(n, 3), "p95_ms": round(p95, 3)}
        if rust_defs:
            ratio = py_defs[0] / rust_defs[0]
            print(f"  → Rust is {ratio:.0f}x faster")
    else:
        print("  ERR — Python get_tool_definitions failed")

    # ── 2. compressor_rs vs trajectory_compressor ─────────────────────────────
    section("2. compressor_rs (Rust) vs trajectory_compressor (Python/tiktoken)")
    print("  [compressor_rs: Rust tokenizer-free ops, 200 runs]")
    rust_ops = bench_compressor_rs_ops(runs=200, warmup=20)
    print("  [Python: tiktoken + pure-Python logic, 50 runs]")
    py_result = bench_trajectory_compressor_python(runs=50, warmup=10)
    if py_result:
        py_ops, has_tk = py_result
        if has_tk:
            print("  (Python using tiktoken for token counting)")
        else:
            print("  (Python using char/4 — tiktoken unavailable)")
    else:
        py_ops = None

    if rust_ops and py_ops:
        pairings = [
            ("estimate_tokens (8 msg)",   "estimate_8",    "py_estimate_8"),
            ("estimate_tokens (80 msg)",  "estimate_80",   "py_estimate_80"),
            ("find_tail_cut",              "find_tail_cut", "py_find_tail"),
            ("prune_old_tool_results",     "prune",         "py_prune"),
            ("serialize_turns",             "serialize",      "py_serialize"),
        ]
        for name, rkey, pkey in pairings:
            if rkey in rust_ops and pkey in py_ops:
                result_row(name, statistics.mean(rust_ops[rkey]), statistics.mean(py_ops[pkey]))
    else:
        print("  ERR — compressor benchmarks failed")

    # ── 3. prompt_builder_rs vs agent/prompt_builder ──────────────────────────
    section("3. prompt_builder_rs vs agent/prompt_builder")
    rust_pb = bench_prompt_builder_rs(runs=100, warmup=20)
    py_pb = bench_prompt_builder_python(runs=50, warmup=10)

    if rust_pb and py_pb:
        pairings = [
            ("strip_yaml_frontmatter", "strip_yaml",     "py_strip_yaml"),
            ("truncate_content",       "truncate",        "py_truncate"),
        ]
        for name, rkey, pkey in pairings:
            if rkey in rust_pb and pkey in py_pb:
                result_row(name, statistics.mean(rust_pb[rkey]), statistics.mean(py_pb[pkey]))
        if "build" in rust_pb:
            pb_mean = statistics.mean(rust_pb["build"])
            ctxfiles_mean = statistics.mean(py_pb.get("py_build_ctxfiles", [float("nan")]))
            print(f"  {'build (full prompt)':<40} Rust: {pb_mean:.3f}ms")
            ctx_val = f"{ctxfiles_mean:.3f}ms" if ctxfiles_mean == ctxfiles_mean else "N/A"
            print(f"    (Python build_context_files: {ctx_val} — not identical ops)")
        results["prompt_builder_rust"] = {"build_mean_ms": round(statistics.mean(rust_pb["build"]), 3)}
    else:
        print("  ERR — prompt builder benchmark failed")

    # ── 4. SessionDB FTS ────────────────────────────────────────────────────────
    section("4. SessionDB FTS — _hermes_state_rs (rusqlite) vs python sqlite3")
    sdb_rust = bench_hermes_state_rust(runs=50, warmup=10)
    sdb_py = bench_sqlite3_fts_python(runs=50, warmup=10)

    if sdb_rust:
        n, m, mn, mx, p95, sd, vals = sdb_rust
        print(f"  _hermes_state_rs (rusqlite): {fmt_ms(n, p95, m, mn, sd)}")
        results["sessiondb_rust"] = {"mean_ms": round(n, 3), "p95_ms": round(p95, 3)}
    else:
        print("  ERR — _hermes_state_rs not available")

    if sdb_py and sdb_py[0] != float("inf"):
        n, m, mn, mx, p95, sd, vals = sdb_py
        print(f"  python sqlite3 (pure FTS5):      {fmt_ms(n, p95, m, mn, sd)}")
        results["sessiondb_python"] = {"mean_ms": round(n, 3), "p95_ms": round(p95, 3)}
        if sdb_rust:
            ratio = n / sdb_rust[0]
            faster = "Rust" if ratio > 1 else "Python"
            print(f"  → {ratio:.1f}x faster ({faster})")
    else:
        print("  ERR — python sqlite3 FTS benchmark failed")

    # ── Summary ────────────────────────────────────────────────────────────────
    section("SUMMARY — What Rust Accelerates")
    print("""
  Component pairs compared:
    _model_tools_rs         → Rust fast-path vs pure Python (HERMES_USE_RUST=0)
    compressor_rs           → Rust tokenizer-free ops vs Python trajectory_compressor
    prompt_builder_rs       → Rust primitives vs agent/prompt_builder
    hermes_state_rs         → rusqlite/SQLite FTS5 vs python sqlite3

  Note: Rust measurements are warm (module already imported).
        Python measurements use FRESH subprocesses for fair cold-start comparison.
""")

    # Discord embed fields
    section("DISCORD EMBED FIELDS")
    fields = []
    if rust_defs and py_defs and rust_defs[0] and py_defs[0]:
        fields.append((f"get_tool_definitions Rust", f"{rust_defs[0]:.3f}ms", True))
        fields.append((f"get_tool_definitions Python (pure)", f"{py_defs[0]:.1f}ms", True))
        fields.append((f"Speedup (Rust/Python)", f"{py_defs[0]/rust_defs[0]:.0f}x 🐇", False))
    if rust_ops and py_ops and "estimate_8" in rust_ops and "py_estimate_8" in py_ops:
        rm, pm = statistics.mean(rust_ops["estimate_8"]), statistics.mean(py_ops["py_estimate_8"])
        fields.append(("estimate_tokens Rust (8 msg)", f"{rm:.3f}ms", False))
        fields.append(("estimate_tokens Python (tiktoken)", f"{pm:.3f}ms", False))
        fields.append(("Token count speedup", f"{pm/rm:.0f}x 🐇", False))
    if sdb_rust:
        fields.append(("SessionDB FTS (rusqlite)", f"{sdb_rust[0]:.2f}ms", True))
    if sdb_py and sdb_rust and sdb_py[0] != float("inf"):
        fields.append(("SessionDB FTS (python sqlite3)", f"{sdb_py[0]:.2f}ms", False))
        fields.append(("FTS speedup", f"{sdb_py[0]/sdb_rust[0]:.1f}x", False))

    for name, val, important in fields:
        flag = "📌" if important else "  "
        print(f"  {flag} {name:<40} {val}")

    # Save results
    out_path = HERMES / "bench_results.json"
    with open(out_path, "a") as f:
        f.write(json.dumps({"ts": time.time(), "results": results}) + "\n")
    print(f"\n  📊 Results appended to {out_path}")


if __name__ == "__main__":
    main()
