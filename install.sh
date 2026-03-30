#!/bin/bash
set -euo pipefail

# ── Colours ──────────────────────────────────────────────────────────────────
BOLD="\033[1m"; RESET="\033[0m"; BLUE="\033[34m"; GREEN="\033[32m"; YELLOW="\033[33m"; RED="\033[31m"
info()  { echo -e "${BLUE}[info]${RESET}  $*"; }
ok()    { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn()  { echo -e "${YELLOW}[warn]${RESET}  $*" >&2; }
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

REPO_URL="https://github.com/agent-bob-the-builder/hermes-agent-ferris-fork"
GITREF="${GITREF:-main}"

# ── Helpers ───────────────────────────────────────────────────────────────────
PYINSTALL=""
setup_python() {
    if need uv; then
        PYINSTALL="uv pip install"
    elif need pip3; then
        PYINSTALL="pip3 install"
    elif need python3; then
        PYINSTALL="python3 -m pip install"
    fi
}

install_uv() {
    if need uv; then
        ok "uv"
        return
    fi
    info "Installing uv..."
    (
        if need curl; then
            curl -fsSL https://astral.sh/uv/install.sh | sh 2>&1 | grep -vE "^$"
        elif need wget; then
            wget -qO- https://astral.sh/uv/install.sh | sh 2>&1 | grep -vE "^$"
        else
            exit 1
        fi
    ) || fail "uv install failed"
    # Reload PATH so uv is available immediately
    export PATH="$HOME/.local/bin:$PATH"
    need uv || fail "uv install failed"
    ok "uv"
}

ensure_rust_sources() {
    if [[ -d "rust" && -f "rust/Cargo.toml" ]]; then
        ok "rust sources present"
        return
    fi

    info "Downloading Rust sources from ${REPO_URL}..."
    local tmp=$(mktemp -d)
    git clone --depth=1 --branch "$GITREF" "$REPO_URL" "$tmp" 2>&1 | tail -2
    if [[ ! -d "$tmp/rust" || ! -f "$tmp/rust/Cargo.toml" ]]; then
        rm -rf "$tmp"
        fail "failed to fetch rust sources from ${REPO_URL}@${GITREF}"
    fi
    mv "$tmp/rust" rust/
    rm -rf "$tmp"
    ok "rust sources downloaded"
}

# ── Init ───────────────────────────────────────────────────────────────────────
[[ $SKIP_DEPS -eq 0 ]] && setup_env() {
    info "Checking dependencies..."
    require python3 && require git
    ok "python3, git"

    install_uv
    setup_python
    ok "${PYINSTALL%% *}"

    [[ -f .env ]] || cp .env.example .env 2>/dev/null || true
    [[ -s .env ]] || warn "  → fill in your API keys / tokens in .env"
}

[[ $SKIP_DEPS -eq 0 ]] && install_python_deps() {
    info "Installing Python dependencies..."
    if [[ -z "$PYINSTALL" ]]; then
        fail "no Python package manager found (tried uv, pip3, python3 -m pip)"
    fi
    $PYINSTALL -e . -q 2>&1 | grep -vE "^$" || true
    ok "Python packages (pyproject.toml)"
    mkdir -p ~/.hermes/skills
    ok "skills dir exists: ~/.hermes/skills"
}

[[ $SKIP_DEPS -eq 0 ]] && setup_skills() {
    mkdir -p ~/.hermes/skills
    [[ -L ~/.hermes/skills/hermes-agent ]] || ln -s "$(pwd)" ~/.hermes/skills/hermes-agent
}

install_rust_toolchain() {
    if need rustc; then
        ok "rustc"
    else
        info "Installing Rust..."
        if need curl; then
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable 2>&1 | tail -3
        else
            fail "rust install requires curl"
        fi
        ok "rustc"
    fi
}

build_rust_extensions() {
    [[ -d "rust" && -f "rust/Cargo.toml" ]] || ensure_rust_sources
    install_rust_toolchain

    if ! need maturin; then
        info "Installing maturin..."
        $PYINSTALL install maturin -q || { fail "maturin install failed"; exit 1; }
    fi
    ok "maturin"

    info "Building Rust extensions..."
    python3 - << 'PYEOF'
import zipfile, shutil, os, sys, subprocess, tempfile

hermes = os.environ.get("HERMES_DIR", "/root/.hermes/hermes-agent")
venv_python = os.path.join(hermes, "venv/bin/python3")
site = os.path.join(hermes, "venv/lib/python3.11/site-packages")
maturin = os.path.join(hermes, "venv/bin/maturin")

crates = [
    ("rust/compressor/Cargo.toml",       "rust_compressor"),
    ("rust/model_tools_rs/Cargo.toml",  "_model_tools_rust"),
    ("rust/prompt_builder_rs/Cargo.toml","_prompt_builder_rust"),
]

with tempfile.TemporaryDirectory() as tmpdir:
    for manifest, module in crates:
        out = os.path.join(tmpdir, os.path.dirname(manifest))
        result = subprocess.run(
            [maturin, "build", "--release", "--manifest-path", manifest, "-o", out],
            cwd=hermes, capture_output=True, text=True
        )
        if result.returncode != 0:
            print(f"maturin build failed for {module}: {result.stderr[-500:]}", file=sys.stderr)
            sys.exit(1)
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
if result.returncode != 0:
    print("WARNING: Extensions may not all be loadable", file=sys.stderr)
    sys.exit(1)
print("Extensions verified OK")
PYEOF
}

# ── Main ──────────────────────────────────────────────────────────────────────
export HERMES_DIR="$(pwd)"
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
echo ""
read -p "  → Run hermes now? [Y/n] " -r; echo ""
if [[ ! "$REPLY" =~ ^[Nn]$ ]]; then
    echo "  Launching hermes... (Ctrl+C to exit)"
    sleep 1
    exec python3 cli.py
fi
