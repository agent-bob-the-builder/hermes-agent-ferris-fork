# Performance Benchmarks
Measured on the actual codebase. Lower is better.
## Tool Dispatch (`model_tools_rs.sanitize()`)
6 runs each, cold start excluded:
- **Rust**: avg=440.9ms  (min=30.0, max=584.2, n=6)
- **Python**: avg=451.0ms  (min=33.4, max=580.0, n=6)

**Result:** Rust is ~2.2% faster on tool dispatch hot path.

## Compressor Sub-operations (`compressor_rs`)
Note: Rust and Python implement different algorithms here — not direct 1:1 comparisons.
| Operation | Rust avg (ms) | Python avg (ms) | Notes |
|-----------|--------------|-----------------|-------|
| `est8` | 0.3148 | 0.005537 | Python 57x faster |
| `est80` | 3.0225 | 0.048558 | Python 62x faster |
| `tail` | 0.2209 | 0.000836 | Python 264x faster |
| `prune` | 0.0913 | 0.000769 | Python 119x faster |
| `ser` | 0.0879 | 0.007781 | Python 11x faster |

## Subprocess Orchestration (`subprocess_rs`)
- **Rust**: avg=2.6606ms  (n=50)
- **Python**: avg=2.4662ms  (n=50)

**Result:** Within 7.3% of each other — essentially equivalent.

---
*Run your own: `python -m benchmarks.run`*  
*Results collected: `bench_results.json`*
