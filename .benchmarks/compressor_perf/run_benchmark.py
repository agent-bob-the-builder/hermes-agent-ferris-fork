#!/usr/bin/env python3
"""
Benchmark: Rust compressor vs Python compressor (context preflight estimation)

Tests the token estimation hotpath — no LLM calls needed.
"""
import pathlib
import sys
import timeit

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "rust-compressor/target/debug"))

# Python compressor
from agent.context_compressor import ContextCompressor
from agent.model_metadata import estimate_messages_tokens_rough

# Rust compressor
import rust_compressor

# Shared test data
MESSAGES = [{"role": "system", "content": "You are Hermes."}]
for i in range(120):
    MESSAGES.append({"role": "user", "content": f"Question {i}: " + ("x" * 400)})
    MESSAGES.append({"role": "assistant", "content": f"Answer {i}: " + ("y" * 600)})

print(f"Test data: {len(MESSAGES)} messages")
print(f"Python rough estimate: {estimate_messages_tokens_rough(MESSAGES):,} tokens")

def rust_estimate():
    rust_compressor.estimate_messages_tokens(MESSAGES)

def python_estimate():
    estimate_messages_tokens_rough(MESSAGES)

# Quick timing (5 iterations to warm up, 20 to measure)
N_WARMUP = 3
N_RUN = 20

for name, fn in [("rust_estimate", rust_estimate), ("python_estimate", python_estimate)]:
    for _ in range(N_WARMUP):
        fn()
    
    t = timeit.timeit(fn, number=N_RUN)
    avg_ms = (t / N_RUN) * 1000
    print(f"{name}: {avg_ms:.3f} ms avg over {N_RUN} runs")
