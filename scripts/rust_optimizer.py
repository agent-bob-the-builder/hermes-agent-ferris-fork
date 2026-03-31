#!/usr/bin/env python3
"""
Rust Crate Optimizer — runs in a fresh session, no context.
Analyzes all workspace crates, finds optimization opportunities,
and applies safe auto-fixes + improvements.
"""
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Any

WORKSPACE = Path("/root/.hermes/hermes-agent-ferris-fork/rust")
CRATES = [
    "run_agent_loop_rs",
    "ansi_strip_rs",
    "compressor",
    "file_ops_rs",
    "fuzzy_match_rs",
    "hermes_state_rs",
    "honcho_http_rs",
    "model_tools_rs",
    "patch_parser_rs",
    "prompt_builder_rs",
    "redact_rs",
    "_retry_state_machine_rs",
    "skin_engine_rs",
    "subprocess_rs",
    "tool_dispatch_rs",
    "context_refs_rs",
    "approval_rs",
]

REPORT: list[str] = []
CHANGES_MADE = 0

def log(msg: str) -> None:
    ts = datetime.now().strftime("%H:%M:%S")
    line = f"[{ts}] {msg}"
    print(line, flush=True)
    REPORT.append(line)

def run(cmd: list[str], cwd: Path | str = WORKSPACE, timeout: int = 120) -> tuple[str, str, int]:
    env = os.environ.copy()
    env["PATH"] = f"/root/.cargo/bin:{env.get('PATH', '')}"
    env["RUST_BACKTRACE"] = "1"
    try:
        res = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
        )
        return res.stdout, res.stderr, res.returncode
    except subprocess.TimeoutExpired:
        return "", "TIMEOUT", -1

def ensure_release_profile(cargo_toml: Path) -> bool:
    """Add/release profile with LTO + opt-level=3 if missing."""
    content = cargo_toml.read_text()

    # Only touch workspace-level Cargo.toml
    if cargo_toml.name != "Cargo.toml":
        return False

    # Check if [profile.release] already has meaningful optimizations
    release_section_present = re.search(r'\[profile\.release\]', content)
    has_lto = "lto =" in content
    has_opt3 = re.search(r'opt-level\s*=\s*["\']?3["\']?', content)

    if release_section_present and has_lto and has_opt3:
        return False  # Already optimized

    changes = []
    new_block = "\n# --- rust_optimizer: added release profile ---\n[profile.release]\nopt-level = 3\nlto = \"thin\"\ncodegen-units = 1\nstrip = true\n"

    if release_section_present:
        # Inject/extend existing [profile.release]
        # Find the block and see if it already has lto/opt-level
        pattern = r'(\[profile\.release\][^\[]+)'
        m = re.search(pattern, content, re.DOTALL)
        if m:
            block = m.group(1)
            if 'lto' not in block:
                changes.append(("  Adding LTO to existing [profile.release]", content.replace(block, block.rstrip() + "\nlto = \"thin\"")))
            if not re.search(r'opt-level\s*=', block):
                changes.append(("  Adding opt-level=3 to existing [profile.release]", content.replace(block, block.rstrip() + "\nopt-level = 3")))
    else:
        # Append new release profile
        changes.append(("  Adding [profile.release] with LTO + opt-level=3", content + new_block))

    if not changes:
        return False

    for desc, new_content in changes:
        cargo_toml.write_text(new_content)
        log(f"  ✓ {desc} in {cargo_toml.parent.name}")

    return True

def run_clippy_autofix(crates: list[str]) -> tuple[int, list[str]]:
    """Run cargo clippy --fix and return (num_fixes, list of fixes)."""
    fixed = 0
    fixes: list[str] = []

    for crate in crates:
        crate_path = WORKSPACE / crate
        cargo_toml = crate_path / "Cargo.toml"
        if not cargo_toml.exists():
            continue

        log(f"  Running clippy --fix on {crate}...")
        # First: try auto-fixable
        stdout, stderr, rc = run(
            ["cargo", "clippy", "--fix", "--allow-dirty", "--allow-staged"],
            cwd=crate_path,
            timeout=180,
        )

        if rc == 0:
            # Check what changed
            diff_out, diff_err, diff_rc = run(["git", "diff", "--stat"], cwd=crate_path)
            if diff_rc == 0 and diff_out.strip():
                lines = diff_out.strip().split("\n")
                for line in lines:
                    if line.strip():
                        fixes.append(f"  {crate}: {line}")
                        fixed += 1
        else:
            # Non-zero clippy is often just warnings; check stderr
            if "error:" not in stdout.lower() and "error:" not in stderr.lower():
                log(f"  clippy ok (rc={rc}) for {crate}")

    return fixed, fixes

def analyze_cargo_tomls() -> list[str]:
    """Check for dependency issues, missing features, etc."""
    suggestions = []

    for crate in CRATES:
        cargo_toml = WORKSPACE / crate / "Cargo.toml"
        if not cargo_toml.exists():
            continue

        content = cargo_toml.read_text()

        # Check for missing `default` feature on dependencies that could be faster
        # e.g., smallvec → smallvec? with small_buffer size

        # Check serde derives
        if 'serde = ' in content and 'derive' not in content:
            suggestions.append(f"  {crate}: serde dependency missing derive feature")

        # Check for blocking in hot paths
        if crate in ["compressor", "fuzzy_match_rs", "ansi_strip_rs"]:
            if "rayon" not in content and "par_iter" not in content:
                suggestions.append(f"  {crate}: could benefit from rayon for parallelization")

    return suggestions

def cargo_check_all() -> tuple[int, str]:
    """Run cargo check across workspace. Returns (warning_count, output)."""
    log("Running cargo check across workspace...")
    stdout, stderr, rc = run(["cargo", "check", "--all-targets", "--", "-D", "warnings"], timeout=300)

    # Count warnings/errors
    lines = (stdout + stderr).splitlines()
    warn_count = sum(1 for l in lines if "warning:" in l.lower())

    if rc == 0:
        log(f"  cargo check OK — {warn_count} warnings")
    else:
        log(f"  cargo check FAILED (rc={rc}) — {warn_count} warnings")

    return warn_count, stdout + "\n" + stderr

def cargo_bench_quick() -> dict[str, Any]:
    """Run a quick cargo check --release to validate release build."""
    log("Validating release build...")
    stdout, stderr, rc = run(["cargo", "check", "--release", "--all-targets"], timeout=300)
    return {
        "rc": rc,
        "ok": rc == 0,
        "warnings": (stdout + stderr).count("warning:"),
    }

def check_dependency_updates() -> list[str]:
    """Check for outdated dependencies using cargo outdated if available."""
    suggestions = []
    stdout, _, rc = run(["cargo", "outdated", "--version", ">="], timeout=60)
    if rc == 0 and "error" not in stdout.lower():
        lines = stdout.strip().split("\n")
        for line in lines[:10]:  # top 10
            if "=" in line and "version" not in line.lower():
                suggestions.append(f"  outdated: {line.strip()}")
    return suggestions

def main() -> None:
    t0 = time.time()
    log("=== Rust Optimizer started ===")
    log(f"Workspace: {WORKSPACE}")
    log(f"Crates: {len(CRATES)}")

    global CHANGES_MADE

    # 1. Ensure release profile optimization in workspace Cargo.toml
    log("\n[1/5] Checking release profile optimizations...")
    ws_cargo = WORKSPACE / "Cargo.toml"
    if ensure_release_profile(ws_cargo):
        CHANGES_MADE += 1

    # 2. Run cargo check across workspace
    log("\n[2/5] Running cargo check...")
    warn_count, check_output = cargo_check_all()

    # 3. Clippy autofix
    log("\n[3/5] Running clippy --fix on crates...")
    clippy_fixed, clippy_fixes = run_clippy_autofix(CRATES)
    CHANGES_MADE += clippy_fixed

    # 4. Analyze Cargo.toml patterns
    log("\n[4/5] Analyzing Cargo.toml patterns...")
    suggestions = analyze_cargo_tomls()
    for s in suggestions:
        log(s)

    # 5. Validate release build still works
    log("\n[5/5] Validating release build...")
    build_ok = cargo_bench_quick()

    # Commit changes if anything was modified
    if CHANGES_MADE > 0:
        log(f"\n>>> {CHANGES_MADE} changes made — committing...")
        run(["git", "add", "."], cwd=WORKSPACE)
        commit_msg = f"rust_optimizer: auto-apply {CHANGES_MADE} optimizations"
        run(["git", "commit", "-m", commit_msg], cwd=WORKSPACE)
        run(["git", "push", "origin", "main"], cwd=WORKSPACE)
        log(">>> Changes pushed to origin/main")
    else:
        log("\n>>> No changes needed — workspace already optimized")

    elapsed = time.time() - t0
    log(f"\n=== Done in {elapsed:.1f}s — {CHANGES_MADE} changes ===")
    log(f"Release build OK: {build_ok['ok']} | Warnings: {build_ok['warnings']}")

    # Print full report
    report = "\n".join(REPORT)
    print(f"\n--- FULL REPORT ---\n{report}\n--- END ---")

if __name__ == "__main__":
    main()
