# kotoma

Live bilingual voice transcription TUI in Rust.

- **ASR**: `cpal` → `webrtc-vad` → `whisper.cpp` (Metal) via `whisper-rs`
- **Translation** (optional): Gemma 3 via `llama-server` subprocess (Metal), queried over HTTP (OpenAI-compatible `/v1/chat/completions`)
- **UI**: `ratatui` two-column display (English ↔ 日本語)
- **Output**: timestamped Markdown table

## Requirements

- `llama-server` on PATH → `brew install llama.cpp` (only for translation)
- Apple Silicon Mac: nothing else — the installer fetches a prebuilt binary
- Other platforms / `--from-source`: Rust (stable) + CMake + a C/C++ toolchain
  (needed to build whisper.cpp)

## Model tiers

Pick the tier that matches your machine.

| Tier | Whisper | Translator | Disk | RAM |
|------|---------|------------|------|-----|
| **standard** | `small` (500 MB) | Gemma 3 4B Q4_K_M (2.5 GB) | ~3 GB | ~4 GB |
| **high** | `large-v3-turbo` (1.6 GB) | Gemma 3 12B Q4_K_M (7.3 GB) | ~9 GB | ~10 GB |

- **standard** is comfortable on any 8–16 GB M-series Mac.
- **high** shines on 32 GB Macs and produces noticeably better JA ↔ EN translation
  and ASR accuracy.
- Gemma 3 is strong at Japanese and, unlike the previously bundled Qwen2.5,
  does not tend to leak Chinese into JA output.

## Install / update

One command from anywhere — no clone needed:

```sh
curl -fsSL https://raw.githubusercontent.com/shohei81/kotoma/main/install.sh | bash -s -- high
# or
curl -fsSL https://raw.githubusercontent.com/shohei81/kotoma/main/install.sh | bash -s -- standard
```

The installer:
- Downloads the latest prebuilt binary from GitHub Releases into
  `~/.local/bin/kotoma` (Apple Silicon macOS). Elsewhere — or with
  `--from-source` — it falls back to `cargo install --git … --force kotoma`.
- Downloads tier-appropriate models into `~/.config/kotoma/models/`
  (skips anything already present).
- Writes `~/.config/kotoma/kotoma.toml` from the tier example **only if
  the file doesn't exist yet**. Pass `--reset-config` at the end to force
  overwrite.

### Update

Run the exact same command — it swaps in the latest binary (or rebuilds
from `main` when building from source). Existing models and your edited
config are preserved.

### From a cloned repo (dev)

```sh
./install.sh high                  # same flow, cargo install --git
# or, to build the currently checked-out code:
./setup.sh high                    # cargo install --path .
```

### Manual setup

If you prefer to drive it yourself, `kotoma.toml.standard.example` and
`kotoma.toml.high.example` list the exact model paths expected. Download
the corresponding models into `~/.config/kotoma/models/` and copy the
example to `~/.config/kotoma/kotoma.toml`. Relative paths resolve against
the config file's directory, so `models/foo` → `~/.config/kotoma/models/foo`.

To disable translation permanently, delete or comment out the `[translator]`
section. To disable it for a single run, pass `--no-translate` — both take the
transcript-only path (no `llama-server`).

## Run

```sh
# from anywhere
kotoma notes.md

# or use the default output path from config
kotoma

# append a new session to an existing file
kotoma --resume notes.md

# override language at launch
kotoma -l auto meeting.md

# transcribe only, no translation (skips llama-server) for this run
kotoma --no-translate notes.md

# transcribe only in a single language (no translation)
kotoma --no-translate -l ja notes.md

# explicit config file
kotoma -c ./project-specific.toml notes.md
```

### Output modes

- **Default (overwrite)**: writes `---` frontmatter + `## start – end` session
  header + transcript table. Existing file is replaced.
- **`--resume` / `-r`**: existing file content is preserved verbatim, a new
  `## start – end` session block is appended below it. Multiple `s`
  (save-now) presses during a session rewrite the same block, never
  duplicate.

### Config search order

1. `-c / --config` CLI flag (if given)
2. `./kotoma.toml` in the current directory
3. `~/.config/kotoma/kotoma.toml`

Legacy `livemd.toml` paths (from previous versions) are still picked up as a
fallback if no `kotoma.toml` is present.

### Log location

- App log: `~/.config/kotoma/logs/kotoma.log`
- llama-server log: `~/.config/kotoma/logs/llama-server.log`
- Override: `KOTOMA_LOG_DIR=/some/path kotoma notes.md`

## Development

```sh
cargo run --release          # uses ./kotoma.toml
cargo run --release -- notes.md
```

### Keybindings

| Key           | Action                          |
|---------------|---------------------------------|
| `q` / `Ctrl+C`| Save transcript and quit (writes `### 要約` + `### Summary` sections when the translator is running; disable with `--no-summary` or `summarize = false` in `[translator]`) |
| `s`           | Save transcript now             |
| `l`           | Cycle Whisper language (en → ja → auto) |
| `space`       | Pause / resume UI               |
| `m`           | Toggle system-audio mix (auto-detected) on / off       |
| `d`           | Pick mic + system-audio source (mix both, or either alone) |
| `Tab`         | (in picker) switch between mic and system-audio columns |
| `?`           | Toggle the keybinding help overlay |
| `↑` / `↓`     | Scroll transcript up / down one line |
| `PgUp` / `PgDn` | Scroll by half a page          |
| `Home` / `End` | Jump to oldest line / resume live tail |
| mouse wheel   | Scroll transcript               |

### UI

```
┌ kotoma · REC · lang=en · in=MacBook Pro Mic · model=ggml-small.bin · tr=ready ┐
│ ██████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░                              │
├─ English ────────────────────────────┬─ 日本語 ──────────────────────────────┤
│ [10:31:03] ▶ Hello, how are you?     │ [10:31:03]   こんにちは、お元気ですか？│
│ [10:31:10]   I'm fine, thanks.        │ [10:31:10] ▶ 元気です、ありがとう。    │
└──────────────────────────────────────┴───────────────────────────────────────┘
 q quit&save · s save · l cycle lang · space pause
```

`▶` marks the source-language side (the one the speaker actually used).
The opposite column shows the machine-translated version (or `…` while pending).

### Audio sources

**Quick toggle:** press `m` to enable system-audio mixing using OS-appropriate
auto-detection (WASAPI loopback on Windows, BlackHole-style virtual driver on
macOS, `*.monitor` source on Linux/PulseAudio). Press `m` again to turn it
off. If auto-detection fails (e.g. no virtual driver installed on macOS),
the status line shows an actionable error.

**Manual override:** press `d` to open a two-column picker — **Microphone**
on the left, **System audio** on the right. `↑/↓` chooses within a column,
`Tab` switches columns, `Enter` applies. `(auto)` in the system-audio column
runs the same detection as `m`; `(none)` disables that slot.

When both slots are set, the streams are mixed sample-by-sample at 16 kHz
mono before transcription, so your voice and the meeting/browser audio land
in the same transcript. The mic drives the cadence; if the system-audio
source goes silent, the mix degrades to mic-only automatically.

Persistent config in `kotoma.toml`:

```toml
input_device = "default"
system_audio_device = "auto"     # or an explicit device name, see examples
```

Platform support for system-audio capture:

- **Windows** — natively supported via WASAPI. Pick `[loopback] Speakers`
  (or similar) and system audio is captured directly.
- **macOS** — cpal has no native loopback. Install a virtual audio driver
  such as [BlackHole](https://github.com/ExistentialAudio/BlackHole), route
  system audio through it (a Multi-Output Device lets you hear and capture
  simultaneously), and select BlackHole from the regular input list.
- **Linux (PulseAudio/PipeWire)** — pick the sink's `*.monitor` entry from
  the input list.

### Markdown output

```markdown
| time | English | 日本語 |
|------|---------|--------|
| 10:31:03 | Hello, how are you? | こんにちは、お元気ですか？ |
| 10:31:10 | I'm fine, thanks. | 元気です、ありがとう。 |
```

## Memory & performance (M-series Mac)

On a 32 GB M4 MacBook Air with the recommended stack:
- Whisper large-v3-turbo: transcribes faster than realtime on Metal
- Gemma 3 12B Q4_K_M: ~20–30 tok/s, with prompt caching across segments
- Peak RSS: ~10 GB; the standard tier stays around ~4 GB

For lighter setups: use `ggml-small.bin` + gemma-3-4b-it-Q4_K_M.

The UI only re-renders when something changed and only wraps the lines
visible in the viewport, so idle CPU usage is near zero and scrolling stays
smooth in long sessions.

Note: the translator now launches `llama-server` with `--jinja` (uses the
GGUF's bundled chat template), which requires a llama.cpp release from 2025
or later — `brew upgrade llama.cpp` if yours is older.

## Logs

Diagnostic logs are written to `kotoma.log` (keeps the TUI clean). Set
`RUST_LOG=debug` for verbose output.
