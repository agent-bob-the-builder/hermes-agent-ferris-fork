#!/usr/bin/env bash
# =============================================================================
# Hermes Agent — Install Script
# =============================================================================
# Sets up the full development environment:
#   - Rust extensions (maturin build → .so files)
#   - Python dependencies (uv / pip)
#   - .env from .env.example
#   - git submodules (if any)
#
# Usage:
#   ./install.sh          # full install
#   ./install.sh --deps   # dependency checks + Rust builds only
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

echo ""
echo -e "${BOLD}Hermes Agent — Install${RESET}"
echo -e "─────────────────────────────────────────────"

# ── 1. Dependency checks ──────────────────────────────────────────────────────
info "Checking dependencies..."
require python3 && require git
ok "python3, git"

if command -v uv &>/dev/null; then
  PYINSTALL="uv pip install"
elif command -v pip3 &>/dev/null; then
  PYINSTALL="pip3 install"
else
  fail "neither uv nor pip3 found"
  exit 1
fi
ok "$PYINSTALL"

if need maturin; then
  ok "maturin"
else
  warn "maturin not found — installing..."
  pip3 install maturin || warn "maturin install failed"
fi

# ── 2. .env setup ─────────────────────────────────────────────────────────────
if [[ -f ".env" ]]; then
  ok ".env exists"
elif [[ -f ".env.example" ]]; then
  cp .env.example .env
  ok "created .env from .env.example"
  warn "  → fill in your API keys / tokens in .env"
else
  warn ".env.example not found"
fi

# ── 3. Git submodules ──────────────────────────────────────────────────────────
if [[ -f ".gitmodules" ]] && [[ -s ".gitmodules" ]]; then
  info "Initialising submodules..."
  git submodule update --init --recursive 2>/dev/null || warn "submodule init failed"
  ok "submodules"
else
  ok "no submodules"
fi

# ── 4. Python dependencies ─────────────────────────────────────────────────────
info "Installing Python dependencies..."
if [[ -f "pyproject.toml" ]]; then
  $PYINSTALL -e . 2>/dev/null || $PYINSTALL -e . --quiet
  ok "Python packages (pyproject.toml)"
elif [[ -f "requirements.txt" ]]; then
  $PYINSTALL -r requirements.txt 2>/dev/null || $PYINSTALL -r requirements.txt --quiet
  ok "Python packages (requirements.txt)"
else
  warn "no pyproject.toml or requirements.txt"
fi

# ── 5. Rust extensions (Ferris Fork) ─────────────────────────────────────────
if [[ ! -f "rust/Cargo.toml" ]]; then
  warn "rust/Cargo.toml not found — skipping Rust"
else
  info "Building Rust extensions (Ferris Fork)..."

  # Check maturin is available
  if ! command -v maturin &>/dev/null; then
    warn "maturin not found — installing..."
    pip3 install maturin || fail "maturin install failed"
  fi

  declare -a CRATES=("rust-compressor" "model_tools_rs" "prompt_builder_rs")

  for CRATE in "${CRATES[@]}"; do
    CRATE_ARG=""
    # For workspace builds, -p specifies the package name within the workspace
    if [[ -f "rust/Cargo.toml" ]]; then
      CRATE_ARG="-p ${CRATE}"
    fi
    info "  building ${CRATE}..."
    if maturin develop --release \
         --manifest-path "rust/Cargo.toml" \
         ${CRATE_ARG} 2>&1 | grep -v "^  "; then
      ok "  ${CRATE} built"
    else
      # Fallback: try building just this crate's manifest
      warn "  trying fallback build for ${CRATE}..."
      if maturin develop --release \
           --manifest-path "rust/${CRATE}/Cargo.toml" 2>&1 | grep -v "^  "; then
        ok "  ${CRATE} built (fallback)"
      else
        warn "  ${CRATE}: build may have failed — check above"
      fi
    fi
  done

  # Verify all three extensions load
  info "Verifying Rust extensions..."
  python3 -c "import rust_compressor, _model_tools_rust, _prompt_builder_rust; print('All Rust extensions loaded OK')" 2>/dev/null && ok "extensions importable" || warn "could not verify imports"
fi

# ── 6. Skills symlink ─────────────────────────────────────────────────────────
SKILLS_DIR="${HOME}/.hermes/skills"
if [[ -d "$SKILLS_DIR" ]] || [[ -L "$SKILLS_DIR" ]]; then
  ok "skills dir exists: $SKILLS_DIR"
elif [[ -d "skills" ]]; then
  mkdir -p "$(dirname "$SKILLS_DIR")"
  ln -sf "$(pwd)/skills" "$SKILLS_DIR"
  ok "linked skills/ → $SKILLS_DIR"
fi

echo ""
echo -e "${BOLD}Install complete!${RESET}"
echo ""
echo "  Next steps:"
echo "    1. Edit .env and fill in your API keys"
echo "    2. Run: python3 cli.py"
echo "       or: python3 run_agent.py"
echo ""
