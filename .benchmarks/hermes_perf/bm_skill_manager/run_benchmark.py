#!/usr/bin/env python3
"""
Benchmark: Skills prompt loading — cold / warm / cached via build_skills_system_prompt.

Three scenarios (via cache-layer manipulation):
- cold:    both in-process LRU and disk snapshot cleared; full parse from disk
- warm:    disk snapshot loaded (matches manifest), but in-process LRU cold
- cached:  in-process LRU cache hot; snapshot already in memory

Three scale points:
- tiny:   5 skills  (~10 KB total)
- normal: 25 skills (~50 KB total)  — representative of a well-populated install
- heavy:  80 skills (~160 KB total) — power-user setup

Measures the full build_skills_system_prompt() call each time so all
parsing, YAML, frontmatter, and snapshot I/O are included appropriately.
"""
import pathlib
import shutil
import sys
import tempfile
import timeit

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from agent.prompt_builder import (
    build_skills_system_prompt,
    clear_skills_system_prompt_cache,
    _SKILLS_PROMPT_CACHE,
    _load_skills_snapshot,
    _write_skills_snapshot,
    _build_skills_manifest,
)
from tools.skill_manager_tool import _find_skill

SKILL_TEMPLATE = """\
---
name: bench-skill-{n}
description: Benchmark skill {n} for loading performance measurement
---

# Skill {n}

This is a benchmark skill for measuring loading performance.
It contains some realistic content to make the file a realistic size.

## Trigger
When the user mentions "skill {n}" or asks about "{n}" specifically.

## Steps

1. First step of the benchmark skill
2. Second step involving some processing
3. Third and final step to complete the task
"""


def make_skills_dir(tmp: pathlib.Path, n: int) -> pathlib.Path:
    """Create a temp skills directory with n skill subdirs, each with SKILL.md."""
    skills_dir = tmp / "skills"
    skills_dir.mkdir(parents=True)
    for i in range(n):
        skill_dir = skills_dir / f"bench-skill-{i}"
        skill_dir.mkdir()
        (skill_dir / "SKILL.md").write_text(SKILL_TEMPLATE.format(n=i), encoding="utf-8")
    return skills_dir


def make_skill_list(skills_dir: pathlib.Path, n: int) -> list[str]:
    """Return the list of skill names that will exist in the temp dir."""
    return [f"bench-skill-{i}" for i in range(n)]


def bench_scale(label: str, skills_dir: pathlib.Path, skill_names: list[str],
                n_cold: int, n_warm: int, n_cached: int, n_warmup: int):
    """
    cold    → clear both caches → call build (full disk parse)
    warm    → clear in-proc LRU only → call build (snapshot hit, still fast)
    cached  → call build again with hot LRU
    """
    # ── Cold: evict everything ───────────────────────────────────────────
    clear_skills_system_prompt_cache(clear_snapshot=True)

    def cold_call():
        clear_skills_system_prompt_cache(clear_snapshot=False)
        return build_skills_system_prompt(skill_names)

    for _ in range(n_warmup):
        cold_call()

    t = timeit.timeit(cold_call, number=n_cold)
    ms_cold = (t / n_cold) * 1000

    # ── Warm: snapshot still valid (manifest unchanged), in-proc LRU cold ─
    # Snapshot was written by the last cold_call via the normal cache path.
    # Just clear the in-process LRU to simulate a fresh process picking up the snapshot.
    def warm_call():
        _SKILLS_PROMPT_CACHE.clear()
        return build_skills_system_prompt(skill_names)

    for _ in range(n_warmup):
        warm_call()

    t = timeit.timeit(warm_call, number=n_warm)
    ms_warm = (t / n_warm) * 1000

    # ── Cached: in-process LRU still warm from warm_call ─────────────────
    def cached_call():
        return build_skills_system_prompt(skill_names)

    for _ in range(n_warmup):
        cached_call()

    t = timeit.timeit(cached_call, number=n_cached)
    ms_cached = (t / n_cached) * 1000

    print(f"  {label:<10}  cold: {ms_cold:>7.3f}ms  "
          f"warm: {ms_warm:>7.3f}ms  cached: {ms_cached:>7.3f}ms")


if __name__ == "__main__":
    print("build_skills_system_prompt — cold / warm / cached")
    print("=" * 65)

    SCALES = [
        ("tiny_5",    5),
        ("norm_25",  25),
        ("heavy_80", 80),
    ]

    N_COLD   = 10
    N_WARM   = 50
    N_CACHED = 100
    N_WARMUP = 3

    tmp = tempfile.TemporaryDirectory()
    tmp_path = pathlib.Path(tmp)

    for label, n in SCALES:
        skills_dir = make_skills_dir(tmp_path, n)
        skill_names = make_skill_list(skills_dir, n)
        bench_scale(label, skills_dir, skill_names,
                    N_COLD, N_WARM, N_CACHED, N_WARMUP)

    tmp.cleanup()
