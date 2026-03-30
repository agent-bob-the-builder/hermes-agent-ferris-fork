#!/bin/bash
set -euo pipefail

# ── Colours ──────────────────────────────────────────────────────────────────
BOLD="\033[1m"; RESET="\033[0m"; BLUE="\033[34m"; GREEN="\033[32m"; YELLOW="\033[33m"; RED="\033[31m"
info()  { echo -e "${BLUE}[info]${RESET}  $*"; }
ok()    { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn()  { echo -e "${YELLOW}[warn]${RESET}  $*"; }
fail()  { echo -e "${RED}[fail]${RESET}  $*" >&2; }

need()  { command -v "$1" &>/dev/null; }
require() { need "$1" || { fail "needed: $1"; exit 1; }; }

SKIP_DEPS=0; SKIP_RUST=0
for arg in "$@"; do
    case "$arg" in
        --deps)  SKIP_DEPS=1 ;;
        --rust)  SKIP_RUST=1 ;;
        --help)  echo "Usage: $0 [--deps] [--rust]"; exit 0 ;;
        *)       fail "unknown argument: $arg" ;;
    esac
done

# ── Init ───────────────────────────────────────────────────────────────────────
[[ $SKIP_DEPS -eq 0 ]] && setup_env() {
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
    fi

    [[ -f .env ]] || cp .env.example .env 2>/dev/null || true
    [[ -s .env ]] || warn "  → fill in your API keys / tokens in .env"
}

[[ $SKIP_DEPS -eq 0 ]] && install_python_deps() {
    info "Installing Python dependencies..."
    $PYINSTALL -e . -q 2>&1 | grep -vE "^$" || true
    ok "Python packages (pyproject.toml)"
    mkdir -p ~/.hermes/skills
    ok "skills dir exists: ~/.hermes/skills"
}

[[ $SKIP_DEPS -eq 0 ]] && setup_skills() {
    mkdir -p ~/.hermes/skills
    [[ -L ~/.hermes/skills/hermes-agent ]] || ln -s "$(pwd)" ~/.hermes/skills/hermes-agent
}

install_rust() {
    if need rustc; then
        ok "rustc"
    else
        info "Installing Rust..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable \
            && ok "rustc" || warn "Rust install may have failed"
    fi
}

build_rust_extensions() {
    if [[ ! -f "rust/Cargo.toml" ]]; then
        warn "rust/Cargo.toml not found — skipping Rust build"
        return 0
    fi

    install_rust

    if ! need maturin; then
        info "Installing maturin..."
        pip3 install maturin -q || { fail "maturin install failed"; exit 1; }
    fi
    ok "maturin"

    info "Building Rust extensions..."
    python3 - << 'PYEOF'
import zipfile, shutil, os, sys, subprocess, tempfile

hermes = "/root/.hermes/hermes-agent"
venv_python = os.path.join(hermes, "venv/bin/python3")
site = os.path.join(hermes, "venv/lib/python3.11/site-packages")
maturin = os.path.join(hermes, "venv/bin/maturin")

crates = [
    ("rust/compressor/Cargo.toml",      "rust_compressor"),
    ("rust/model_tools_rs/Cargo.toml",  "_model_tools_rust"),
    ("rust/prompt_builder_rs/Cargo.toml","_prompt_builder_rust"),
]

with tempfile.TemporaryDirectory() as tmpdir:
    for manifest, module in crates:
        out = os.path.join(tmpdir, os.path.dirname(manifest))
        subprocess.run(
            [maturin, "build", "--release", "--manifest-path", manifest, "-o", out],
            cwd=hermes, capture_output=True
        )
        whl = next(f for f in os.listdir(out) if f.endswith(".whl"))
        with zipfile.ZipFile(os.path.join(out, whl)) as z:
            for f in z.namelist():
                if f.endswith(".so") and module in f:
                    dest = os.path.join(site, module)
                    shutil.rmtree(dest, ignore_errors=True)
                    z.extract(f, site)
                    extracted = os.path.join(site, f)
                    if os.path.isdir(extracted):
                        os.rename(extracted, dest)
                    print(f"Installed {module}")

result = subprocess.run([venv_python, "-c",
    "import rust_compressor, _model_tools_rust, _prompt_builder_rust; print('All Rust extensions loaded OK')"])
if result.returncode == 0:
    print("Extensions verified OK")
else:
    print("WARNING: Extensions may not all be loadable", file=sys.stderr)
    sys.exit(1)
PYEOF
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
