#!/usr/bin/env bash
# kotoma dev setup — builds the checked-out code and installs a model preset.
# Usage: ./setup.sh [standard|high|both]   (omit to install the binary only)
set -euo pipefail

TIER="${1:-}"
case "$TIER" in
    ""|standard|high|both) ;;
    *)
        echo "usage: $0 [standard|high|both]" >&2
        echo "  standard  Whisper small + Gemma 3 4B" >&2
        echo "  high      Whisper large-v3-turbo + Gemma 3 12B" >&2
        echo "  both      both presets (selects high)" >&2
        echo "  (omit)    binary only — set up models later with 'kotoma model'" >&2
        exit 1
        ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "==> Building & installing kotoma from $SCRIPT_DIR"
(cd "$SCRIPT_DIR" && cargo install --path . --force)

BIN="$(command -v kotoma || echo "$HOME/.cargo/bin/kotoma")"

if [[ -n "$TIER" ]]; then
    echo "==> Installing the '$TIER' model preset"
    "$BIN" model preset "$TIER"
else
    echo ""
    echo "Binary installed. Set up models with:"
    echo "  kotoma model preset standard|high|both"
fi

echo ""
echo "Try: kotoma notes.md"
