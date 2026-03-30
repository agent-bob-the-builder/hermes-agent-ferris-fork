#!/usr/bin/env python3
"""
Benchmark: V4A patch parsing across three realistic edit sizes.

- tiny:   1 hunk, 10 context lines, 1 removal + 1 addition  (~200 bytes)
- small:  3 hunks, 30 context lines each, 5±2 changes each   (~3 KB)
- medium: 8 hunks, 80 context lines each, 20±5 changes each (~40 KB)
  — represents a mid-sized refactor or multi-file feature change
"""
import pathlib
import sys
import timeit

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "tools"))
from patch_parser import parse_v4a_patch

# ── Tiny: single small edit ────────────────────────────────────────────────
PATCH_TINY = """\
*** Begin Patch
*** Update File: src/foo.py
@@ def foo() @@
    def foo():
-        return 1
+        return 42
*** End Patch
"""

# ── Small: 3 hunks typical of a focused change ─────────────────────────────
LINE = "    # This is a comment line explaining the logic below.\n"
CHANGES = "".join(f"-        val[{i}] = {i} * 2\n+        val[{i}] = {i} ** 2\n" for i in range(5))

PATCH_SMALL = f"""\
*** Begin Patch
*** Update File: src/bar.py
@@ init @@
{ LINE * 8}\
-        val[0] = 0 * 2
-        val[1] = 1 * 2
-        val[2] = 2 * 2
-        val[3] = 3 * 2
-        val[4] = 4 * 2
+        val[0] = 0 ** 2
+        val[1] = 1 ** 2
+        val[2] = 2 ** 2
+        val[3] = 3 ** 2
+        val[4] = 4 ** 2
@@ process @@
{ LINE * 10}\
-        result = sum(values) / len(values)
+        result = sum(values) / max(len(values), 1)
@@ finalize @@
{ LINE * 12}\
-        return None
+        return result
*** End Patch
"""

# ── Medium: 8 hunks, ~80 context lines each ─────────────────────────────────
def make_medium_patch() -> str:
    lines = ["*** Begin Patch\n"]
    files = ["src/a.py", "src/b.py", "src/c.py", "src/d.py",
             "src/e.py", "src/f.py", "src/g.py", "src/h.py"]
    for fname in files:
        lines.append(f"*** Update File: {fname}\n")
        lines.append(f"@@ class {fname.split('.')[0]} @@\n")
        lines.append(LINE * 80)
        for i in range(20):
            lines.append(f"-        old_value_{i} = {i} * 3 + {i}\n")
            lines.append(f"+        new_value_{i} = {i} ** 3 - {i}\n")
    lines.append("*** End Patch\n")
    return "".join(lines)

PATCH_MEDIUM = make_medium_patch()


CASES = [
    (PATCH_TINY,  "tiny"),
    (PATCH_SMALL, "small"),
    (PATCH_MEDIUM,"medium"),
]


if __name__ == "__main__":
    print("V4A patch parsing — parse only (no file I/O)")
    print("=" * 55)

    N_RUN   = 100
    N_WARMUP = 5

    for patch, label in CASES:
        for _ in range(N_WARMUP):
            parse_v4a_patch(patch)

        t = timeit.timeit(
            lambda p=patch: parse_v4a_patch(p),
            number=N_RUN,
        )
        ms = (t / N_RUN) * 1000
        size_kb = len(patch.encode()) / 1024
        print(f"  {label:<8} ({size_kb:5.1f} KB patch)  {ms:>7.3f} ms/parse")
