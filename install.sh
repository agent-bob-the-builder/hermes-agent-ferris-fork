#!/usr/bin/env bash
# =============================================================================
# Hermes Agent — Ferris Fork Install Script
# =============================================================================
# Sets up the full development environment for the Rust-accelerated fork:
#   - Python dependencies (uv / pip)
#   - Rust toolchain + PyO3 extensions (maturin develop)
#   - .env from .env.example
#
# Usage:
#   ./install.sh          # full install (deps + Rust build)
#   ./install.sh --deps   # Python deps only, skip Rust build
#   ./install.sh --rust   # Rust build only, skip Python deps
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# ── Colour helpers ────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'
BOLD='\033[1m'; RESET='\033[0m'
info()  { echo -e "${BLUE}[info]${RESET}  $*"; }
ok()    { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn()  { echo -e "${YELLOW}[warn]${RESET}  $*"; }
fail()  { echo -e "${RED}[FAIL]${RESET}  $*" >&2; }

need()  { command -v "$1" &>/dev/null; }
require() { need "$1" || { fail "needed: $1"; exit 1; }; }

# ── Argument parsing ──────────────────────────────────────────────────────────
SKIP_DEPS=0; SKIP_RUST=0
for arg in "$@"; do
  case "$arg" in
    --deps)  SKIP_DEPS=1 ;;
    --rust)  SKIP_RUST=1 ;;
    --help)  echo "Usage: $0 [--deps] [--rust]"; exit 0 ;;
    *)       fail "unknown argument: $arg" ;;
  esac
done

echo ""
echo -e "${BOLD}Hermes Agent — Ferris Fork Install${RESET}"
echo -e "─────────────────────────────────────────────"

# ── 1. Dependency checks ─────────────────────────────────────────────────────
info "Checking dependencies..."
require python3 && require git
ok "python3, git"

if need uv; then
  PYINSTALL="uv pip install"
  ok "uv"
elif need pip3; then
  PYINSTALL="pip3 install"
  ok "pip3"
else
  fail "neither uv nor pip3 found"
  exit 1
fi

# ── 2. Rust toolchain ────────────────────────────────────────────────────────
install_rust() {
  if need rustc; then
    ok "rustc"
    return 0
  fi
  info "Installing Rust toolchain..."
  if curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable; then
    ok "Rust installed"
    # shellcheck source=/dev/null
    source "${HOME}/.cargo/env" 2>/dev/null || true
  else
    fail "Rust toolchain install failed"
    exit 1
  fi
}

# ── 3. Python dependencies ────────────────────────────────────────────────────
install_python_deps() {
  info "Installing Python dependencies..."
  if [[ -f "pyproject.toml" ]]; then
    $PYINSTALL -e . 2>/dev/null || $PYINSTALL -e . --quiet
    ok "Python packages (pyproject.toml)"
  elif [[ -f "requirements.txt" ]]; then
    $PYINSTALL -r requirements.txt 2>/dev/null || $PYINSTALL -r requirements.txt --quiet
    ok "Python packages (requirements.txt)"
  else
    warn "no pyproject.toml or requirements.txt — skipping Python deps"
  fi
}

# ── 4. .env setup ────────────────────────────────────────────────────────────
setup_env() {
  if [[ -f ".env" ]]; then
    ok ".env exists"
  elif [[ -f ".env.example" ]]; then
    cp .env.example .env
    ok "created .env from .env.example"
    warn "  → fill in your API keys / tokens in .env"
  else
    warn ".env.example not found"
  fi
}

# ── 5. Skills symlink ────────────────────────────────────────────────────────
setup_skills() {
  SKILLS_DIR="${HOME}/.hermes/skills"
  if [[ -d "$SKILLS_DIR" ]] || [[ -L "$SKILLS_DIR" ]]; then
    ok "skills dir exists: $SKILLS_DIR"
  elif [[ -d "skills" ]]; then
    mkdir -p "$(dirname "$SKILLS_DIR")"
    ln -sf "$(pwd)/skills" "$SKILLS_DIR"
    ok "linked skills/ → $SKILLS_DIR"
  fi
}

# ── 6. Rust extensions (PyO3) ────────────────────────────────────────────────
build_rust_extensions() {
  if [[ ! -f "rust/Cargo.toml" ]]; then
    warn "rust/Cargo.toml not found — skipping Rust build"
    return 0
  fi

  install_rust

  if ! need maturin; then
    info "Installing maturin..."
    pip3 install maturin || { fail "maturin install failed"; exit 1; }
  fi
  ok "maturin"

  # Build each crate
  declare -a CRATES=("rust-compressor" "model_tools_rs" "prompt_builder_rs")
  for CRATE in "${CRATES[@]}"; do
    info "Building ${CRATE}..."
    if maturin develop --release \
         --manifest-path "rust/Cargo.toml" \
         -p "$CRATE" 2>&1 | grep -vE "^(  Compiling|   Finished|Running|warning:)"; then
      ok "  ${CRATE}"
    else
      # maturin exits non-zero on warnings — check .so actually landed
      if [[ -f "rust/target/release/${CRATE}.so" ]] || \
         [[ -f "rust/target/release/lib${CRATE}.so" ]] || \
         python3 -c "import ${CRATE//-/_}" 2>/dev/null; then
        ok "  ${CRATE} (built)"
      else
        warn "  ${CRATE}: build may have failed — check above"
      fi
    fi
  done

  # Verify all three extensions load
  info "Verifying Rust extensions..."
  python3 -c \
    "import rust_compressor, _model_tools_rust, _prompt_builder_rust; print('All Rust extensions loaded OK')" \
    2>/dev/null && ok "extensions importable" || warn "could not verify imports"
}

# ── Main ──────────────────────────────────────────────────────────────────────
[[ $SKIP_DEPS -eq 0 ]] && setup_env
[[ $SKIP_DEPS -eq 0 ]] && install_python_deps
[[ $SKIP_RUST -eq 0 ]] && build_rust_extensions
[[ $SKIP_DEPS -eq 0 ]] && setup_skills

echo ""
echo -e "${BOLD}Install complete!${RESET}"
echo ""
echo "  Next steps:"
echo "    1. Edit .env and fill in your API keys"
echo "    2. Run: python3 cli.py"
echo "       or: python3 run_agent.py"
echo ""
