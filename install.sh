#!/bin/bash
set -euo pipefail

# ── Colours ──────────────────────────────────────────────────────────────────
BOLD="\033[1m"; RESET="\033[0m"; BLUE="\033[34m"; GREEN="\033[32m"; YELLOW="\033[33m"; RED="\033[31m"
info()  { echo -e "${BLUE}[info]${RESET}  $*" >&2; }
ok()    { echo -e "${GREEN}[ ok ]${RESET}  $*" >&2; }
warn()  { echo -e "${YELLOW}[warn]${RESET}  $*" >&2; }
fail()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

need()  { command -v "$1" &>/dev/null; }

SKIP_DEPS=0; SKIP_RUST=0; FORCE_REBUILD=0
for arg in "$@"; do
    case "$arg" in
        --deps)        SKIP_DEPS=1 ;;
        --rust)        SKIP_RUST=1 ;;
        --force-rebuild) FORCE_REBUILD=1 ;;
        --help)  echo "Usage: $0 [--deps] [--rust] [--force-rebuild]"; exit 0 ;;
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

# ── Detect platform tag for pre-built wheels ───────────────────────────────────
# Returns: manylinux_2_34_x86_64 | manylinux_2_34_aarch64 | macosx_14_0_arm64 |
#          macosx_13_x86_64 | macosx_11_0_x86_64
detect_wheel_tag() {
    local os_name uname_m arch

    os_name=$(uname -s)
    uname_m=$(uname -m)

    case "$os_name" in
        Linux*)
            case "$uname_m" in
                x86_64)  echo "manylinux_2_34_x86_64" ;;
                aarch64) echo "manylinux_2_34_aarch64" ;;
                arm64)   echo "manylinux_2_34_aarch64" ;;
                *)       echo "" ;;
            esac
            ;;
        Darwin*)
            case "$uname_m" in
                arm64)   echo "macosx_14_0_arm64" ;;
                x86_64)  echo "macosx_13_x86_64" ;;
                *)       echo "" ;;
            esac
            ;;
        *)  echo "" ;;
    esac
}

# ── Fetch and apply a pre-built combined wheel ────────────────────────────────
# Tries: 1) local file  2) GitHub Release  3) source build
install_rust_extensions() {
    install_build_deps
    reload_path

    local wheel_tag
    wheel_tag=$(detect_wheel_tag)
    info "Platform tag: ${wheel_tag:-unknown} ($(uname -s) $(uname -m))"

    # ── 1. Try local wheel ─────────────────────────────────────────────────────
    local local_wheel=""
    if [[ -n "$wheel_tag" ]]; then
        local_wheel="$HERMES_DIR/target/wheels-all/compressor/hermes_agent-0.6.0-cp311-abi3-${wheel_tag}.whl"
    fi
    if [[ -z "$local_wheel" || ! -f "$local_wheel" ]]; then
        # Scan for any local combined wheel
        local_wheel=$(find "$HERMES_DIR/target" -name "hermes_agent-0.6.0-cp311-abi3-*.whl" 2>/dev/null | head -1)
    fi

    if [[ -f "${local_wheel:-}" ]] && [[ $FORCE_REBUILD -eq 0 ]]; then
        info "Using local wheel: $local_wheel"
        if install_wheel "$local_wheel"; then
            return 0
        fi
        warn "Local wheel failed, trying GitHub Release..."
    fi

    # ── 2. Try GitHub Release ─────────────────────────────────────────────────────
    if [[ -n "$wheel_tag" ]] && [[ $FORCE_REBUILD -eq 0 ]]; then
        local gh_wheel="hermes_agent-0.6.0-cp311-abi3-${wheel_tag}.whl"
        local tmp_wheel
        tmp_wheel=$(mktemp "$gh_wheel.XXXXXX") || fail "mktemp failed"

        info "Downloading $gh_wheel from GitHub Release..."
        local repo="${REPO_URL##https://github.com/}"
        if curl -fsSL \
            "https://github.com/${repo}/releases/latest/download/${gh_wheel}" \
            -o "$tmp_wheel" 2>/dev/null; then
            if install_wheel "$tmp_wheel"; then
                rm -f "$tmp_wheel"
                return 0
            fi
            rm -f "$tmp_wheel"
        fi
        warn "No pre-built wheel on GitHub for this platform — building from source..."
    fi

    # ── 3. Build from source ────────────────────────────────────────────────────
    build_rust_from_source
}

# ── Verify all Rust extensions from a wheel are present ───────────────────────
# Returns 0 + prints "all N present" if complete; returns 1 if incomplete.
verify_wheel_complete() {
    local venv_python="$HERMES_DIR/venv/bin/python3"
    local result
    result=$($venv_python - << 'PYEOF'
import sys
expected = [
    "compressor_rs", "_model_tools_rs", "_prompt_builder_rs", "_skin_engine_rs",
    "_hermes_state_rs", "_fuzzy_match_rs", "_subprocess_rs", "_file_ops_rs",
    "_patch_parser_rs", "_ansi_strip_rs", "redact_rs", "run_agent_loop_rs",
    "_tool_dispatch_rs", "_retry_state_machine_rs", "_honcho_http_rs",
    "_context_refs_rs", "approval_rs",
]
missing = []
for m in expected:
    try:
        __import__(m)
    except ImportError:
        missing.append(m)
if missing:
    print(f"[verify] MISSING modules: {missing}", file=sys.stderr)
    sys.exit(1)
print(f"[verify] All {len(expected)} Rust extensions present")
PYEOF
)
    local rc=$?
    if [[ $rc -eq 0 ]]; then
        ok "$result"
        return 0
    else
        warn "pre-built wheel is incomplete — falling back to source build"
        return 1
    fi
}

# ── Install a wheel file into the venv ────────────────────────────────────────
# NOTE: pre-built wheels on GitHub only contain compressor_rs.  The full bundle
# (all 17 _rs extensions) is built from source.  We verify ALL expected modules
# are present — if the wheel is incomplete we fall through to source build.
install_wheel() {
    local wheel_path="$1"
    info "Installing wheel: $wheel_path"
    if ! "$HERMES_DIR/venv/bin/pip" install --no-deps "$wheel_path" 2>&1; then
        warn "pip install failed for $wheel_path"
        return 1
    fi
    verify_wheel_complete
}

# ── Build all _rs crates from source and install the combined wheel ─────────────
build_rust_from_source() {
    install_rust
    reload_path

    if ! "$HERMES_DIR/venv/bin/python3" -c "import maturin" 2>/dev/null; then
        info "Installing maturin..."
        uv tool install maturin -q || fail "maturin install failed"
    fi
    ok "maturin"

    info "Building all Rust extensions from source..."

    # Ensure rust sources are present
    if [[ ! -f "$HERMES_DIR/rust/Cargo.toml" ]]; then
        ensure_rust_sources
    fi

    local tmp_wheel_dir
    tmp_wheel_dir=$(mktemp -d) || fail "mktemp failed"

    # ── Build all crates into the tmp dir ───────────────────────────────────
    local crates=(
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
        "rust/run_agent_loop_rs/Cargo.toml"
        "rust/tool_dispatch_rs/Cargo.toml"
        "rust/_retry_state_machine_rs/Cargo.toml"
        "rust/honcho_http_rs/Cargo.toml"
        "rust/context_refs_rs/Cargo.toml"
        "rust/approval_rs/Cargo.toml"
    )

    for manifest in "${crates[@]}"; do
        local crate_label="${manifest##*/}"  # e.g. compressor
        info "  Building $crate_label..."

        if ! maturin build \
            --release \
            --manifest-path "$manifest" \
            --out "$tmp_wheel_dir" 2>&1; then
            rm -rf "$tmp_wheel_dir"
            fail "maturin build failed for $crate_label"
        fi
    done

    # ── Combine all per-crate wheels into one ──────────────────────────────────
    # Export vars so the Python heredoc can read them (double-quoted heredoc)
    export HERMES_DIR WHEEL_TAG="$wheel_tag"
    python3 - << PYEOF
import zipfile, os, sys, shutil, tempfile

hermes_dir = os.environ.get("HERMES_DIR", ".")
wheel_tag  = os.environ.get("WHEEL_TAG", "manylinux_2_34_x86_64")
tmp_wheel_dir = sys.argv[1] if len(sys.argv) > 1 else None

if not tmp_wheel_dir:
    print("[build] ERROR: tmp_wheel_dir not provided", file=sys.stderr)
    sys.exit(1)

crates = [
    "compressor_rs", "_model_tools_rs", "_prompt_builder_rs", "_skin_engine_rs",
    "_hermes_state_rs", "_fuzzy_match_rs", "_subprocess_rs", "_file_ops_rs",
    "_patch_parser_rs", "_ansi_strip_rs", "redact_rs", "run_agent_loop_rs",
    "_tool_dispatch_rs", "_retry_state_machine_rs", "_honcho_http_rs", "_context_refs_rs",
    "approval_rs",
]

dest_pkg_dir = os.path.join(hermes_dir, "target", "wheels-all", "compressor")
os.makedirs(dest_pkg_dir, exist_ok=True)

# Each wheel is a separate package. Extract each crate's .so into the shared
# dest_pkg_dir so they sit next to each other like a combined wheel.
for whl in sorted(os.listdir(tmp_wheel_dir)):
    if not whl.endswith(".whl"):
        continue
    whl_path = os.path.join(tmp_wheel_dir, whl)
    print(f"  Combining: {whl}", flush=True)
    with zipfile.ZipFile(whl_path) as z:
        for f in z.namelist():
            if not f.endswith(".so"):
                continue
            z.extract(f, dest_pkg_dir)
            extracted = os.path.join(dest_pkg_dir, f)
            base = os.path.basename(f)
            flat = os.path.join(dest_pkg_dir, base)
            if extracted != flat:
                os.replace(extracted, flat)

# Build the combined wheel
combined_dir = tempfile.mkdtemp()
combined_whl_base = f"hermes_agent-0.6.0-cp311-abi3-{wheel_tag}.whl"
combined_whl_path = os.path.join(combined_dir, combined_whl_base)

with zipfile.ZipFile(combined_whl_path, "w", zipfile.ZIP_DEFLATED) as zout:
    for root, dirs, files in os.walk(dest_pkg_dir):
        for file in files:
            full = os.path.join(root, file)
            arcname = os.path.relpath(full, dest_pkg_dir)
            zout.write(full, arcname)
            print(f"  Added: {arcname}", flush=True)

    dist_info_base = "hermes_agent-0.6.0.dist-info"
    zout.writestr(f"{dist_info_base}/WHEEL",
        "Wheel-Version: 1.0\nGenerator: hermes-build\nRoot-Is-Purelib: true\nTag: py3-abi3-cp311\n")
    zout.writestr(f"{dist_info_base}/METADATA",
        "Metadata-Version: 2.1\nName: hermes-agent\nVersion: 0.6.0\nSummary: hermes-agent Rust extensions\n")
    zout.writestr(f"{dist_info_base}/RECORD", "")

shutil.copy(combined_whl_path, os.path.join(dest_pkg_dir, combined_whl_base))
print(f"Combined wheel: {dest_pkg_dir}/{combined_whl_base}", flush=True)
PYEOF

    local combined="$HERMES_DIR/target/wheels-all/compressor/hermes_agent-0.6.0-cp311-abi3-${wheel_tag:-manylinux_2_34_x86_64}.whl"
    info "Installing combined wheel..."
    if ! "$HERMES_DIR/venv/bin/pip" install --no-deps "$combined" 2>&1; then
        rm -rf "$tmp_wheel_dir"
        fail "combined wheel install failed"
    fi
    rm -rf "$tmp_wheel_dir"
    verify_rust_extensions
}

# ── Verify all _rs extensions load ───────────────────────────────────────────
verify_rust_extensions() {
    info "Verifying _rs extensions..."
    local failed=0
    python3 - << 'PYEOF'
import os, sys, subprocess

hermes = os.environ.get("HERMES_DIR", "/root/.hermes/hermes-agent-ferris-fork")
venv_python = os.path.join(hermes, "venv/bin/python3")

result = subprocess.run([venv_python, "-c", """
import compressor_rs, _model_tools_rs, _prompt_builder_rs, _skin_engine_rs,
       _hermes_state_rs, _fuzzy_match_rs, _subprocess_rs, _file_ops_rs,
       _patch_parser_rs, _ansi_strip_rs, redact_rs, run_agent_loop_rs,
       _tool_dispatch_rs, _retry_state_machine_rs, _honcho_http_rs,
       _context_refs_rs, approval_rs
print('All Rust extensions loaded OK')
"""], capture_output=True, text=True)

if result.returncode != 0:
    print(f"[build] ERROR: extension load failure", file=sys.stderr)
    print((result.stderr or "")[:1000], file=sys.stderr)
    sys.exit(1)
print((result.stdout or "").strip())
PYEOF
    ok "_rs extensions verified"
}

# ── Resolve HERMES_DIR ─────────────────────────────────────────────────────────
# Try: (1) pwd, (2) script location via BASH_SOURCE[0], (3) known install paths.
# When running via `curl | bash` the script lives in /tmp so we need (3).
_hermes_pwd=$(pwd)
_hermes_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"

_resolve_hermes_dir() {
    local dir="$1"
    [[ -f "$dir/pyproject.toml" || -f "$dir/setup.py" || -f "$dir/rust/Cargo.toml" ]] && echo "$dir" && return 0
    return 1
}

# Known install locations to try as fallbacks
_known_hermes_dirs=(
    "$HOME/.hermes/hermes-agent-ferris-fork"
    "$HOME/.local/hermes"
    "/opt/hermes-agent-ferris-fork"
)

if _resolve_hermes_dir "$_hermes_pwd" > /dev/null; then
    export HERMES_DIR="$_hermes_pwd"
elif _resolve_hermes_dir "$_hermes_script_dir" > /dev/null; then
    export HERMES_DIR="$_hermes_script_dir"
else
    # Search known paths — this handles `curl | bash` from any directory
    local found=0
    for d in "${_known_hermes_dirs[@]}"; do
        if [[ -d "$d" ]] && _resolve_hermes_dir "$d" > /dev/null; then
            export HERMES_DIR="$d"
            found=1
            break
        fi
    done
    if [[ $found -eq 0 ]]; then
        # Last resort: use pwd and let ensure_hermes_sources sort it out
        export HERMES_DIR="$_hermes_pwd"
    fi
fi
reload_path

# ── Install hermes CLI wrapper ─────────────────────────────────────────────────
install_hermes_cli() {
    mkdir -p "$HOME/.local/bin"

    # Determine where the actual hermes script lives — we write the wrapper
    # to this file (not to a separate location) so readlink-f works correctly.
    # HERMES_DIR is already set by the time this runs; use it directly so the
    # wrapper is always correct even when BASH_SOURCE[0] is relative/broken.
    local hermes_wrapper="$HOME/.local/bin/hermes"

    cat > "$hermes_wrapper" << WRAPPER
#!/bin/bash
# hermes CLI wrapper — finds the hermes repo and runs cli.py via venv Python.
set -euo pipefail

# Known install locations — checked first, most reliable.
if [[ -d "\$HOME/.hermes/hermes-agent-ferris-fork" && \\
      -f "\$HOME/.hermes/hermes-agent-ferris-fork/pyproject.toml" ]]; then
    HERMES_DIR="\$HOME/.hermes/hermes-agent-ferris-fork"
elif [[ -d "\$HOME/.local/hermes" && \\
        -f "\$HOME/.local/hermes/pyproject.toml" ]]; then
    HERMES_DIR="\$HOME/.local/hermes"
elif [[ -L "\${BASH_SOURCE[0]}" && -f "\${BASH_SOURCE[0]}" ]]; then
    # Resolve symlink via ls -l (more portable than readlink -f).
    # Handles both absolute and relative symlink targets.
    _target="\$(ls -l "\${BASH_SOURCE[0]}" 2>/dev/null | awk '{print \$NF}')"
    if [[ -n "\$_target" ]]; then
        if [[ "\$_target" = /* ]]; then
            HERMES_DIR="\$(dirname "\$_target")"
        else
            HERMES_DIR="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && cd "\$(dirname "\$_target")" && pwd)"
        fi
    else
        HERMES_DIR="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
    fi
else
    echo "ERROR: could not determine HERMES_DIR" >&2
    exit 1
fi

# Sanity check — if venv doesn't exist, fail clearly
if [[ ! -f "\$HERMES_DIR/venv/bin/python3" ]]; then
    echo "ERROR: hermes venv not found at \$HERMES_DIR/venv/bin/python3" >&2
    echo "Run the install script again to fix." >&2
    exit 1
fi

export HERMES_DIR
export PATH="\$HOME/.local/bin:\$PATH"

exec "\$HERMES_DIR/venv/bin/python3" "\$HERMES_DIR/cli.py" "\$@"
WRAPPER
    chmod +x "$hermes_wrapper"
    # Detect shell rc files — add both PATH and hermes export so the install
    # takes effect immediately in new shells without requiring a re-source.
    for rc in "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.profile"; do
        if [[ -f "$rc" ]]; then
            # Avoid duplicate PATH exports (check for the specific pattern)
            if ! grep -q 'export PATH="\$HOME/.local/bin:\$PATH"' "$rc" 2>/dev/null; then
                echo "" >> "$rc"
                echo "# hermes-agent (ferris-fork)" >> "$rc"
                echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$rc"
                ok "Added PATH export to $rc"
            fi
            # Avoid duplicate hermes exports
            if ! grep -q "export hermes=" "$rc" 2>/dev/null; then
                echo "export hermes=\"$HOME/.local/bin/hermes\"" >> "$rc"
                ok "Added hermes export to $rc"
            fi
        fi
    done
    ok "hermes CLI installed to $HOME/.local/bin/hermes"
}

install_hermes_cli

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

[[ $SKIP_RUST -eq 0 ]] && install_rust_extensions

echo ""
echo -e "${BOLD}Install complete!${RESET}"
echo ""
echo "  ✅ hermes installed to $HOME/.local/bin/hermes"
echo "  ✅ PATH updated in ~/.bashrc (and ~/.zshrc, ~/.profile if present)"
echo ""
echo "  Next steps:"
echo "    1. Edit $HERMES_DIR/.env and fill in your API keys"
echo "    2. Run: hermes"
echo ""
echo "  Run this NOW to activate hermes in the current shell:"
echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
echo ""
