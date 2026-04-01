//! Benchmark: get_tool_definitions (Rust-native, zero JSON roundtrip)
//!
//! Run with: cargo run --release --bin bm_get_tool_definitions
//!
//! Profiling: perf record -g -p $(pgrep -f bm_get_tool_definitions) sleep 10
//!
#![allow(unused_imports)]

use pyo3::types::{PyAnyMethods, PyModuleMethods};
use std::time::{Duration, Instant};

fn main() {
    let mut init_time = Duration::ZERO;
    let mut per_call_us = 0.0;

    unsafe {
        pyo3::with_embedded_python_interpreter(|py| {
            let module = py.import("_model_tools_rs").expect(
                "Failed to import _model_tools_rs — is the extension built? Run: maturin develop -r",
            );

            // --- Cold-run: initialize (imports all tools, builds registry cache) ---
            let t0 = Instant::now();
            module
                .call_method0("initialize")
                .expect("initialize() failed");
            init_time = t0.elapsed();

            // --- Benchmark: get_tool_definitions ---
            let n = 1000;
            let start = Instant::now();
            for _ in 0..n {
                module
                    .call_method0("get_tool_definitions")
                    .expect("get_tool_definitions() failed");
            }
            let elapsed = start.elapsed();
            per_call_us = elapsed.as_secs_f64() / n as f64 * 1_000_000.0;

            eprintln!("initialize() took: {:.3}s", init_time.as_secs_f64());
            eprintln!(
                "get_tool_definitions x {}: {:.1}ms total ({:.1}µs/call)",
                n,
                elapsed.as_secs_f64() * 1000.0,
                per_call_us
            );
            eprintln!(
                "Throughput: {:.0} calls/sec",
                n as f64 / elapsed.as_secs_f64()
            );

            // --- Verify result shape ---
            let result = module.call_method0("get_tool_definitions").unwrap();
            let list_len = result.len().unwrap_or(0);
            eprintln!("Tool definitions returned: {} tools", list_len);
        });
    }
}
