#!/bin/bash
set -euo pipefail

# ── Colours ──────────────────────────────────────────────────────────────────
BOLD="\033[1m"; RESET="\033[0m"; BLUE="\033[34m"; GREEN="\033[32m"; YELLOW="\033[33m"; RED="\033[31m"
info()  { echo -e "${BLUE}[info]${RESET}  $*" >&2; }
ok()    { echo -e "${GREEN}[ ok ]${RESET}  $*" >&2; }
warn()  { echo -e "${YELLOW}[warn]${RESET}  $*" >&2; }
fail()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

need()  { command -v "$1" &>/dev/null; }

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

# ── Env helpers ───────────────────────────────────────────────────────────────
reload_path() {
    export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
    # Source rustup env if present
    [[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"
    [[ -f "$HOME/.local/share/uv/uv.toml" ]] && . "$HOME/.local/bin/uv.env" 2>/dev/null || true
}

# ── Install uv ────────────────────────────────────────────────────────────────
install_uv() {
    if need uv; then
        ok "uv"
        return
    fi
    info "Installing uv..."
    if ! need curl && ! need wget; then
        fail "uv install requires curl or wget"
    fi

    local installer
    if need curl; then
        installer="curl -fsSL https://astral.sh/uv/install.sh"
    else
        installer="wget -qO- https://astral.sh/uv/install.sh"
    fi
    # Run the uv installer; it creates ~/.local/bin/uv and sources ~/.local/bin/env
    eval "$installer | sh" || fail "uv install failed"
    # uv installer should have added ~/.local/bin to PATH; verify
    reload_path
    need uv || fail "uv install failed (uv not in PATH after install)"
    ok "uv"
}

# ── Install system build dependencies ───────────────────────────────────────
install_build_deps() {
    if need cc; then
        ok "cc"
        return
    fi
    info "Installing build dependencies (cc, libc-dev)..."

    # ── Linux ──────────────────────────────────────────────────────────────
    if need apt-get; then
        # Debian / Ubuntu / Linux Mint / WSL
        apt-get install -y --no-install-recommends build-essential || fail "apt-get install build deps failed"

    elif need yum; then
        # CentOS 7 / older RHEL — yum is an alias for dnf in CentOS 8+
        yum install -y gcc gcc-c++ make || fail "yum install build deps failed"

    elif need dnf; then
        # Fedora / CentOS 8+ / RHEL 8+
        dnf install -y gcc make || fail "dnf install build deps failed"

    elif need apk; then
        # Alpine Linux / Docker Alpine
        apk add --no-cache gcc musl-dev make || fail "apk install build deps failed"

    # ── macOS ──────────────────────────────────────────────────────────────
    elif need brew; then
        # Homebrew — auto-installs gcc (creates /usr/local/bin/gcc-13 etc.)
        brew install gcc || fail "brew install gcc failed"

    # ── Windows (Git Bash / MSYS2 / Cygwin) ─────────────────────────────────
    elif need pacman; then
        # MSYS2 / Git Bash on Windows
        pacman -Sy --noconfirm gcc make || fail "pacman install gcc failed"

    # ── Windows native (PowerShell / cmd) ──────────────────────────────────
    elif need choco; then
        # Chocolatey
        choco install -y visualstudio2022buildtools --package-parameters "--add Microsoft.VisualStudio.Workload.VCTools" || \
        choco install -y mingw || fail "choco install failed"

    elif need scoop; then
        scoop install gcc make || fail "scoop install failed"

    else
        fail "No package manager found — cannot install build tools." \
             "Supported: apt/yum/dnf/apk (Linux), brew (macOS), pacman/choco/scoop (Windows)"
    fi

    need cc || fail "cc not found after install"
    ok "cc"
}

# ── Install Rust toolchain ───────────────────────────────────────────────────
install_rust() {
    # Source cargo env if already installed
    [[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"

    if need rustc; then
        ok "rustc"
        return
    fi
    info "Installing Rust..."
    if ! need curl; then
        fail "rust install requires curl"
    fi
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable \
        || fail "rustup install failed"
    . "$HOME/.cargo/env"
    need rustc || fail "rustc not found after install"
    ok "rustc"
}

# ── Ensure hermes sources present ─────────────────────────────────────────────
ensure_hermes_sources() {
    if [[ -f "$HERMES_DIR/pyproject.toml" || -f "$HERMES_DIR/setup.py" ]]; then
        ok "hermes sources present"
        return
    fi
    info "Downloading hermes-agent-ferris-fork from ${REPO_URL}..."
    local tmp
    tmp=$(mktemp -d) || fail "mktemp failed"
    git clone --depth=1 --branch "$GITREF" "$REPO_URL" "$tmp" \
        || fail "git clone failed — check your network / repo URL"
    if [[ ! -f "$tmp/pyproject.toml" && ! -f "$tmp/setup.py" ]]; then
        rm -rf "$tmp"
        fail "Neither pyproject.toml nor setup.py found in ${REPO_URL}@${GITREF} — repo may be incomplete"
    fi
    # Move all cloned contents into HERMES_DIR (may be a fresh dir)
    mv "$tmp"/* "$HERMES_DIR/" 2>/dev/null || true
    mv "$tmp"/.[!.]* "$HERMES_DIR/" 2>/dev/null || true
    rm -rf "$tmp"
    ok "hermes sources downloaded"
}

# ── Patch cli.py to fix asyncio event loop deprecation ─────────────────────────
patch_cli_event_loop() {
    local cli="$HERMES_DIR/cli.py"
    if [[ ! -f "$cli" ]]; then
        warn "cli.py not found — skipping event loop patch"
        return
    fi
    # Fix: replace deprecated asyncio.get_event_loop() with new_event_loop()
    # This prevents DeprecationWarning and crash on fresh install in non-TTY env
    if grep -q '_aio.get_event_loop()' "$cli" 2>/dev/null; then
        info "Patching cli.py asyncio event loop (fixes installer crash)..."
        sed -i 's/_loop = _aio\.get_event_loop()/_loop = _aio.new_event_loop()\n                    _aio.set_event_loop(_loop)/' "$cli"
        ok "cli.py patched"
    else
        ok "cli.py event loop already patched"
    fi
}

# ── Ensure rust sources present ────────────────────────────────────────────────
ensure_rust_sources() {
    if [[ -f "$HERMES_DIR/rust/Cargo.toml" ]]; then
        ok "rust sources present"
        return
    fi
    info "Downloading Rust sources from ${REPO_URL}..."
    local tmp
    tmp=$(mktemp -d) || fail "mktemp failed"
    git clone --depth=1 --branch "$GITREF" "$REPO_URL" "$tmp" \
        || fail "git clone failed — check your network / repo URL"
    if [[ ! -d "$tmp/rust" || ! -f "$tmp/rust/Cargo.toml" ]]; then
        rm -rf "$tmp"
        fail "rust/ not found in ${REPO_URL}@${GITREF} — repo may be incomplete"
    fi
    mv "$tmp/rust" "$HERMES_DIR/rust/"
    rm -rf "$tmp"
    ok "rust sources downloaded"
}

# ── Build Rust extensions ────────────────────────────────────────────────────
build_rust_extensions() {
    [[ -f "$HERMES_DIR/rust/Cargo.toml" ]] || ensure_rust_sources
    install_build_deps
    install_rust

    reload_path
    if ! "$HERMES_DIR/venv/bin/python3" -c "import maturin" 2>/dev/null; then
        info "Installing maturin..."
        # Install maturin as a uv tool (not into the project venv) so it doesn't
        # need pip present in the hermes venv.
        uv tool install maturin -q || fail "maturin install failed"
    fi
    ok "maturin"

    info "Building Rust extensions..."
    python3 - << 'PYEOF'
import zipfile, shutil, os, sys, subprocess, tempfile

def log(msg):
    print(f"[build] {msg}", flush=True)

hermes = os.environ.get("HERMES_DIR", "/root/.hermes/hermes-agent")
venv_python = os.path.join(hermes, "venv/bin/python3")

# Detect Python version from the venv
result = subprocess.run([venv_python, "-c", "import sys; print(f'python{sys.version_info.major}.{sys.version_info.minor}')"], capture_output=True, text=True)
python_version = result.stdout.strip()
site = os.path.join(hermes, f"venv/lib/{python_version}/site-packages")
log(f"Detected venv Python: {python_version}")
log(f"Site-packages: {site}")

# Find maturin (installed as uv tool, not in project venv)
import shutil as _shutil
maturin = _shutil.which("maturin") or (hermes + "/.local/bin/maturin")
log(f"maturin: {maturin}")

# Verify maturin works
result = subprocess.run([maturin, "--version"], capture_output=True, text=True)
if result.returncode == 0:
    log(f"maturin version: {result.stdout.strip()}")
else:
    print(f"[build] WARN: maturin --version failed: {result.stderr.strip()}", flush=True)

crates = [
    ("rust/compressor/Cargo.toml",       "rust_compressor"),
    ("rust/model_tools_rs/Cargo.toml",  "_model_tools_rust"),
    ("rust/prompt_builder_rs/Cargo.toml","_prompt_builder_rust"),
]

with tempfile.TemporaryDirectory() as tmpdir:
    for manifest, module in crates:
        crate_name = manifest.split("/")[1]
        log(f"Building crate {crate_name} ({module})...")
        out = os.path.join(tmpdir, os.path.dirname(manifest))
        log(f"  manifest: {manifest}")
        log(f"  output dir: {out}")

        result = subprocess.run(
            [maturin, "build", "--release", "--manifest-path", manifest, "-o", out],
            cwd=hermes, capture_output=True, text=True
        )

        if result.returncode != 0:
            print(f"[build] ERROR: maturin build failed for {module}", file=sys.stderr)
            print(f"[build] --- stdout ---", file=sys.stderr)
            print(result.stdout[-1200:], file=sys.stderr)
            print(f"[build] --- stderr ---", file=sys.stderr)
            print(result.stderr[-1200:], file=sys.stderr)
            sys.exit(1)

        log(f"  build succeeded")

        whl_files = [f for f in os.listdir(out) if f.endswith(".whl")]
        if not whl_files:
            print(f"[build] ERROR: no .whl file found in {out}", file=sys.stderr)
            print(f"[build] Contents: {os.listdir(out)}", file=sys.stderr)
            sys.exit(1)

        whl = whl_files[0]
        log(f"  wheel: {whl}")

        with zipfile.ZipFile(os.path.join(out, whl)) as z:
            log(f"  wheel contains {len(z.namelist())} files")
            for f in z.namelist():
                if f.endswith(".so") and module in f:
                    dest = os.path.join(site, module)
                    log(f"  extracting {f} -> {dest}")
                    shutil.rmtree(dest, ignore_errors=True)
                    z.extract(f, site)
                    extracted = os.path.join(site, f)
                    if os.path.isdir(extracted):
                        os.rename(extracted, dest)
                    log(f"  installed {module}")
                    break
            else:
                print(f"[build] WARN: no .so file for {module} in wheel", file=sys.stderr)
                print(f"[build] Files in wheel: {[f for f in z.namelist() if module in f]}", file=sys.stderr)

log("All crates built successfully")

# Verify all extensions load
log("Verifying extensions...")
result = subprocess.run([venv_python, "-c",
    "import rust_compressor, _model_tools_rust, _prompt_builder_rust; "
    "print('rust_compressor ok'); "
    "print('_model_tools_rust ok'); "
    "print('_prompt_builder_rust ok'); "
    "print('All Rust extensions loaded OK')"])
if result.returncode != 0:
    print(f"[build] ERROR: extension load failure", file=sys.stderr)
    print((result.stderr or "")[-800:], file=sys.stderr)
    sys.exit(1)
for line in (result.stdout or "").strip().split("\n"):
    log(f"  {line}")
PYEOF
}

# ── Main ──────────────────────────────────────────────────────────────────────
# Resolve HERMES_DIR: prefer pwd (works for curl|bash from cloned dir),
# fall back to the script's own location via BASH_SOURCE[0].
_hermes_pwd=$(pwd)
_hermes_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
if [[ -f "$_hermes_pwd/pyproject.toml" || -f "$_hermes_pwd/setup.py" || -f "$_hermes_pwd/rust/Cargo.toml" ]]; then
    export HERMES_DIR="$_hermes_pwd"
elif [[ -f "$_hermes_script_dir/pyproject.toml" || -f "$_hermes_script_dir/setup.py" || -f "$_hermes_script_dir/rust/Cargo.toml" ]]; then
    export HERMES_DIR="$_hermes_script_dir"
else
    export HERMES_DIR="$_hermes_pwd"
fi
reload_path

[[ $SKIP_DEPS -eq 0 ]] && {
    info "Checking dependencies..."
    need python3 || fail "needed: python3"
    need git || fail "needed: git"
    ok "python3, git"

    install_uv

    # Clone repo if source files are missing (handles fresh/empty clone scenario)
    ensure_hermes_sources

    # Patch cli.py event loop before any python code runs
    patch_cli_event_loop

    info "Creating virtual environment..."
    uv venv "$HERMES_DIR/venv" --python python3 --seed || fail "uv venv failed"
    ok "venv created"

    info "Installing Python dependencies..."
    cd "$HERMES_DIR" && uv pip install --python "$HERMES_DIR/venv/bin/python3" -e . -q || fail "Python deps install failed"
    ok "Python packages (pyproject.toml)"

    mkdir -p ~/.hermes/skills
    [[ -L ~/.hermes/skills/hermes-agent ]] || ln -s "$HERMES_DIR" ~/.hermes/skills/hermes-agent
    ok "skills dir"

    [[ -f .env ]] || cp .env.example .env 2>/dev/null || true
    [[ -s .env ]] || warn "  → fill in your API keys / tokens in .env"
}

[[ $SKIP_RUST -eq 0 ]] && build_rust_extensions

echo ""
echo -e "${BOLD}Install complete!${RESET}"
echo ""
echo "  Next steps:"
echo "    1. Edit .env and fill in your API keys"
echo "    2. Run: python3 cli.py"
echo ""
read -p "  → Run hermes now? [Y/n] " -r </dev/tty && echo ""
if [[ ! "$REPLY" =~ ^[Nn]$ ]]; then
    echo "  Launching hermes... (Ctrl+C to exit)"
    sleep 1
    cd "$HERMES_DIR" && exec "$HERMES_DIR/venv/bin/python3" "$HERMES_DIR/cli.py"
fi
