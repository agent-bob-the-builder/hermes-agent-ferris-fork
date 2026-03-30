#!/usr/bin/env python3
"""
Benchmark: Tool dispatch overhead — registry lookup + handler invocation (no I/O).

Two scenarios:
- sync:    synchronous tool handler (e.g. read_file, terminal)
- async:   async tool handler (e.g. delegate_task)

Three data sizes per scenario:
- tiny args:   ~100 bytes
- small args:  ~2 KB
- medium args: ~20 KB  (realistic long prompt / large file list)
"""
import pathlib
import sys
import timeit

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from tools.registry import registry

# ── Fake handlers (never actually do I/O) ──────────────────────────────────

def fake_sync_handler(args, task_id=None):
    _ = args.get("data")
    return '{"ok": true}'

async def fake_async_handler(args, task_id=None):
    _ = args.get("data")
    return '{"ok": true}'


# ── Pre-warm the registry so this is a pure dispatch benchmark ─────────────
# Register temp tools under synthetic names to avoid collisions.
TEMP_TOOLS = []
for i in range(10):
    registry.register(
        name=f"__bench_sync_{i}__",
        toolset="__benchmark__",
        schema={
            "name": f"__bench_sync_{i}__",
            "description": "benchmark",
            "parameters": {"type": "object", "properties": {"data": {"type": "string"}}},
        },
        handler=fake_sync_handler,
        check_fn=lambda: True,
    )
    registry.register(
        name=f"__bench_async_{i}__",
        toolset="__benchmark__",
        schema={
            "name": f"__bench_async_{i}__",
            "description": "benchmark",
            "parameters": {"type": "object", "properties": {"data": {"type": "string"}}},
        },
        handler=fake_async_handler,
        check_fn=lambda: True,
        is_async=True,
    )
    TEMP_TOOLS.append(f"__bench_sync_{i}__")
    TEMP_TOOLS.append(f"__bench_async_{i}__")


# ── Benchmark cases ────────────────────────────────────────────────────────

TINY  = {"data": "x" * 100}
SMALL = {"data": "x" * 2_000}
MED   = {"data": "x" * 20_000}

CASES = [
    # (tool_prefix, args, label)
    ("__bench_sync_", TINY,  "sync_tiny"),
    ("__bench_sync_", SMALL, "sync_small"),
    ("__bench_sync_", MED,   "sync_medium"),
    ("__bench_async_", TINY, "async_tiny"),
    ("__bench_async_", SMALL,"async_small"),
    ("__bench_async_", MED,  "async_medium"),
]


if __name__ == "__main__":
    print("Tool dispatch overhead (registry lookup + handler)")
    print("=" * 55)

    N_RUN = 500
    N_WARMUP = 50

    for prefix, args, label in CASES:
        tool_name = f"{prefix}0"

        for _ in range(N_WARMUP):
            registry.dispatch(tool_name, args)

        t = timeit.timeit(
            lambda tn=tool_name, a=args.copy(): registry.dispatch(tn, a),
            number=N_RUN,
        )
        ms = (t / N_RUN) * 1000
        print(f"  {label:<18}  {ms:>8.4f} ms/call")
