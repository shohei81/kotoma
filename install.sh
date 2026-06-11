#!/usr/bin/env bash
# kotoma install / update — works both locally (cloned repo) and via curl | bash.
# Rerun the same command to update; config is preserved unless --reset-config.
set -euo pipefail

REPO_URL="https://github.com/shohei81/kotoma"
RAW_URL="https://raw.githubusercontent.com/shohei81/kotoma/main"

TIER=""
RESET_CONFIG=0
FROM_SOURCE=0

print_usage() {
    cat <<EOF >&2
usage: install.sh {standard|high} [--reset-config] [--from-source]

  standard         Whisper small + Gemma 3 4B   (~3 GB disk, ~4 GB RAM)
  high             Whisper large-v3-turbo + Gemma 3 12B   (~9 GB disk, ~10 GB RAM)
  --reset-config   Overwrite ~/.config/kotoma/kotoma.toml with the tier default
  --from-source    Skip the prebuilt binary and build with cargo

Remote install / update:
  curl -fsSL ${RAW_URL}/install.sh | bash -s -- high
EOF
}

for arg in "$@"; do
    case "$arg" in
        standard|high) TIER="$arg" ;;
        --reset-config) RESET_CONFIG=1 ;;
        --from-source) FROM_SOURCE=1 ;;
        -h|--help) print_usage; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; print_usage; exit 1 ;;
    esac
done

if [[ -z "$TIER" ]]; then
    print_usage
    exit 1
fi

if ! command -v llama-server >/dev/null 2>&1; then
    echo "NOTE: llama-server not found on PATH — translation will be disabled."
    echo "      Install with: brew install llama.cpp"
fi

CONFIG_DIR="$HOME/.config/kotoma"
MODEL_DIR="$CONFIG_DIR/models"
mkdir -p "$MODEL_DIR"

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
    echo "  installed prebuilt binary → $HOME/.local/bin/kotoma"
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
fi

fetch() {
    local url="$1"
    local dest="$2"
    if [[ -f "$dest" && -s "$dest" ]]; then
        echo "  skip (already present): $(basename "$dest")"
    else
        echo "  downloading: $(basename "$dest")"
        curl -fL --progress-bar -o "$dest" "$url"
    fi
}

write_config() {
    local example_name="$1"
    local dest="$CONFIG_DIR/kotoma.toml"
    if [[ -f "$dest" && $RESET_CONFIG -eq 0 ]]; then
        echo "  keep existing: $dest (pass --reset-config to overwrite)"
        return
    fi
    echo "  writing: $dest"
    curl -fsSL -o "$dest" "${RAW_URL}/${example_name}"
}

if [[ "$TIER" == "standard" ]]; then
    echo "==> Fetching standard-tier models"
    fetch "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin" \
          "$MODEL_DIR/ggml-small.bin"
    fetch "https://huggingface.co/ggml-org/gemma-3-4b-it-GGUF/resolve/main/gemma-3-4b-it-Q4_K_M.gguf" \
          "$MODEL_DIR/gemma-3-4b-it-Q4_K_M.gguf"
    echo "==> Config"
    write_config "kotoma.toml.standard.example"
else
    echo "==> Fetching high-tier models"
    fetch "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin" \
          "$MODEL_DIR/ggml-large-v3-turbo.bin"
    fetch "https://huggingface.co/ggml-org/gemma-3-12b-it-GGUF/resolve/main/gemma-3-12b-it-Q4_K_M.gguf" \
          "$MODEL_DIR/gemma-3-12b-it-Q4_K_M.gguf"
    echo "==> Config"
    write_config "kotoma.toml.high.example"
fi

echo ""
echo "==> Done"
echo "    Config: $CONFIG_DIR/kotoma.toml"
echo "    Models: $MODEL_DIR"
echo ""
echo "Run: kotoma notes.md"
