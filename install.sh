#!/usr/bin/env bash
# kotoma install / update — works both locally (cloned repo) and via curl | bash.
# Installs the binary; local LLM models are managed separately via `kotoma model`.
set -euo pipefail

REPO_URL="https://github.com/shohei81/kotoma"
RAW_URL="https://raw.githubusercontent.com/shohei81/kotoma/main"

TIER=""
FROM_SOURCE=0

print_usage() {
    cat <<EOF >&2
usage: install.sh [standard|high|both] [--from-source]

Installs/updates the kotoma binary. The local LLM models are a separate
dependency — pass a preset to also fetch its models, or omit it to install
the binary only and follow the model guidance printed at the end.

  standard         Whisper small + Gemma 4 E4B   (~6 GB disk, ~8 GB RAM)
  high             Whisper large-v3-turbo + Gemma 4 12B   (~9 GB disk, ~12 GB RAM)
  both             Both presets (selects high; switch with 'kotoma model use')
  --from-source    Skip the prebuilt binary and build with cargo

Remote install / update:
  curl -fsSL ${RAW_URL}/install.sh | bash -s -- high   # binary + high preset
  curl -fsSL ${RAW_URL}/install.sh | bash              # binary only
EOF
}

for arg in "$@"; do
    case "$arg" in
        standard|high|both) TIER="$arg" ;;
        --from-source) FROM_SOURCE=1 ;;
        -h|--help) print_usage; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; print_usage; exit 1 ;;
    esac
done

if ! command -v llama-server >/dev/null 2>&1; then
    echo "NOTE: llama-server not found on PATH — translation will be disabled."
    echo "      Install with: brew install llama.cpp"
fi

CONFIG_DIR="$HOME/.config/kotoma"
INSTALLED_BIN=""

# Prebuilt binary (Apple Silicon macOS only, no Rust toolchain needed).
# Falls back to cargo when unavailable.
try_binary_install() {
    [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]] || return 1
    local url="${REPO_URL}/releases/latest/download/kotoma-aarch64-apple-darwin.tar.gz"
    local tmp
    tmp=$(mktemp -d)
    curl -fsSL "$url" -o "$tmp/kotoma.tar.gz" || { rm -rf "$tmp"; return 1; }
    tar -xzf "$tmp/kotoma.tar.gz" -C "$tmp" || { rm -rf "$tmp"; return 1; }
    mkdir -p "$HOME/.local/bin"
    mv "$tmp/kotoma" "$HOME/.local/bin/kotoma"
    chmod +x "$HOME/.local/bin/kotoma"
    rm -rf "$tmp"
    INSTALLED_BIN="$HOME/.local/bin/kotoma"
    echo "  installed prebuilt binary → $INSTALLED_BIN"
    if ! command -v kotoma >/dev/null 2>&1; then
        echo "  NOTE: add ~/.local/bin to PATH, e.g.:"
        echo "        echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.zshrc"
    fi
}

echo "==> Installing / updating kotoma"
if [[ $FROM_SOURCE -eq 0 ]] && try_binary_install; then
    :
else
    if ! command -v cargo >/dev/null 2>&1; then
        echo "ERROR: no prebuilt binary available for this platform and cargo not found." >&2
        echo "       Install Rust first: https://rustup.rs" >&2
        exit 1
    fi
    echo "  building from source via cargo (${REPO_URL})"
    cargo install --git "$REPO_URL" --force kotoma
    INSTALLED_BIN="$(command -v kotoma || echo "$HOME/.cargo/bin/kotoma")"
fi

# Models are a separate dependency, owned by the binary's catalog-driven
# `model preset` command. Only touch them when a preset was requested.
if [[ -n "$TIER" ]]; then
    echo "==> Installing the '$TIER' model preset"
    "$INSTALLED_BIN" model preset "$TIER"
fi

echo ""
echo "==> Done"
if [[ -z "$TIER" ]]; then
    if [[ ! -f "$CONFIG_DIR/kotoma.toml" ]] || ! ls "$CONFIG_DIR/models"/ggml-*.bin >/dev/null 2>&1; then
        cat <<EOF

Binary installed. kotoma also needs local models (its only heavy dependency).
Install a preset — downloads the models and writes the config:
  kotoma model preset standard   # ~3 GB
  kotoma model preset high       # ~9 GB
  kotoma model preset both       # both; switch with 'kotoma model use'

Or browse and pick individually: kotoma model list
EOF
    fi
else
    echo "    Config: $CONFIG_DIR/kotoma.toml"
    echo "    Models: $CONFIG_DIR/models"
fi
echo ""
echo "Run: kotoma notes.md"
