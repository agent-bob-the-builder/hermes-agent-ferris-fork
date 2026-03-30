#!/usr/bin/env python3
"""
Benchmark: SessionDB FTS search with varying data sizes and query complexity.

Three scenarios:
- tiny:  10 sessions × 20 msgs each  — small-memory dev machine profile
- small: 50 sessions × 50 msgs each  — typical daily usage
- large: 200 sessions × 200 msgs each — heavy user / long history

Two query modes per scenario:
- narrow: specific term (low result set)
- broad:  common term (high result set, stresses ranking)
"""
import json
import pathlib
import sys
import tempfile
import timeit
from datetime import datetime

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from hermes_state import SessionDB


def build_session(db: SessionDB, session_id: str, n_messages: int, wide: bool = False):
    """Create a session with n_messages alternating user/assistant pairs."""
    db.create_session(session_id=session_id, working_dir="/tmp")
    base = [
        "fix", "build", "error", "test", "run", "debug", "deploy",
        "optimize", "refactor", "benchmark", "search", "write",
    ]
    # All messages contain the "narrow" query words so FTS always returns results.
    # "wide" messages additionally repeat common English words to inflate result sets.
    content_base = "working on the implementation and pipeline" if wide else "fixing the build"
    for i in range(n_messages):
        role = "user" if i % 2 == 0 else "assistant"
        content = f"{content_base} in iteration {i} — {' '.join(base * 3)}"
        db.append_message(
            session_id=session_id,
            role=role,
            content=content,
            model="test",
            provider="test",
            audio_url=None,
            image_urls=None,
        )


def bench_search(n_sessions: int, n_msgs_per_session: int, broad: bool):
    """Return (setup_time_ms, search_time_ms) for one configuration."""
    tmp = tempfile.TemporaryDirectory()
    db_path = pathlib.Path(tmp.name) / "state.db"
    db = SessionDB(db_path)

    # Seed data
    for i in range(n_sessions):
        sid = f"session_{i:04d}"
        build_session(db, sid, n_msgs_per_session, wide=broad)

    # broad: very common English words — high result count, stresses BM25 ranking
    # narrow: less common terms — small result set
    query = "the and to is" if broad else "build pipeline"
    n_results = 10 if broad else 3

    # Time the search itself (hot path — data is in memory)
    t_search = timeit.timeit(
        lambda: db.search_messages(
            query=query,
            limit=n_results,
        ),
        number=20,
    )
    search_ms = (t_search / 20) * 1000

    db.close()
    tmp.cleanup()
    return search_ms


def run_configs():
    configs = [
        # (n_sessions, n_msgs, broad, label)
        (10,  20,  False, "tiny_narrow"),
        (10,  20,  True,  "tiny_broad"),
        (50,  50,  False, "small_narrow"),
        (50,  50,  True,  "small_broad"),
        (200, 200, False, "large_narrow"),
        (200, 200, True,  "large_broad"),
    ]
    for n_sess, n_msgs, broad, label in configs:
        ms = bench_search(n_sess, n_msgs, broad)
        print(f"  {label:<20}  {ms:>8.3f} ms/search")


if __name__ == "__main__":
    print("SessionDB FTS search benchmarks")
    print("=" * 55)
    run_configs()
