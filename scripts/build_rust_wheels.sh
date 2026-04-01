#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${1:-$ROOT_DIR/dist/rust-wheels}"
PYTHON_INTERPRETER="${PYTHON_INTERPRETER:-}"

if ! command -v maturin >/dev/null 2>&1; then
    echo "maturin is required to build Rust extension wheels" >&2
    exit 1
fi

mkdir -p "$OUT_DIR"

MANIFESTS=(
    "rust/compressor/Cargo.toml"
    "rust/model_tools_rs/Cargo.toml"
    "rust/prompt_builder_rs/Cargo.toml"
    "rust/skin_engine_rs/Cargo.toml"
    "rust/hermes_state_rs/Cargo.toml"
    "rust/fuzzy_match_rs/Cargo.toml"
    "rust/subprocess_rs/Cargo.toml"
    "rust/file_ops_rs/Cargo.toml"
    "rust/patch_parser_rs/Cargo.toml"
    "rust/ansi_strip_rs/Cargo.toml"
    "rust/redact_rs/Cargo.toml"
    "rust/tool_dispatch_rs/Cargo.toml"
    "rust/_retry_state_machine_rs/Cargo.toml"
    "rust/honcho_http_rs/Cargo.toml"
    "rust/context_refs_rs/Cargo.toml"
    "rust/approval_rs/Cargo.toml"
    "rust/url_safety_rs/Cargo.toml"
    "rust/url_safety_python_rs/Cargo.toml"
    "rust/hermes_time_rs/Cargo.toml"
    "rust/checkpoint_manager_rs/Cargo.toml"
    "rust/title_generator_rs/Cargo.toml"
    "rust/model_metadata_rs/Cargo.toml"
    "rust/skill_utils_rs/Cargo.toml"
    "rust/usage_pricing_rs/Cargo.toml"
    "rust/insights_rs/Cargo.toml"
)

for manifest in "${MANIFESTS[@]}"; do
    crate_dir="$(dirname "$manifest")"
    echo "Building ${crate_dir#rust/}"
    args=(
        build
        --release
        --manifest-path "$ROOT_DIR/$manifest"
        --out "$OUT_DIR"
    )
    if [[ -n "$PYTHON_INTERPRETER" ]]; then
        args+=(--interpreter "$PYTHON_INTERPRETER")
    fi
    maturin "${args[@]}"
done
