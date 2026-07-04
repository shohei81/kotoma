# kotoma

Live bilingual voice transcription TUI in Rust.

- **ASR**: `cpal` → `webrtc-vad` → `whisper.cpp` (Metal) via `whisper-rs`
- **Translation** (optional): Gemma 4 via `llama-server` subprocess (Metal), queried over HTTP (OpenAI-compatible `/v1/chat/completions`)
- **UI**: `ratatui` two-column display (English ↔ 日本語)
- **Output**: timestamped Markdown table

## Requirements

- `llama-server` on PATH → `brew install llama.cpp` (only for translation)
- Apple Silicon Mac: nothing else — the installer fetches a prebuilt binary
- Other platforms / `--from-source`: Rust (stable) + CMake + a C/C++ toolchain
  (needed to build whisper.cpp)

## Model presets

Models are kotoma's only heavy dependency. Two ready-made presets bundle an
ASR + a translator model; pick one (or `both`, or any mix — see
[Managing models](#managing-models)).

| Preset | Whisper | Translator | Disk | RAM |
|------|---------|------------|------|-----|
| **standard** | `small` (500 MB) | Gemma 4 E4B Q4_K_M (5.3 GB) | ~6 GB | ~8 GB |
| **high** | `large-v3-turbo` (1.6 GB) | Gemma 4 12B Q4_K_M (7.4 GB) | ~9 GB | ~12 GB |

- **standard** is comfortable on any 8–16 GB M-series Mac.
- **high** shines on 16–32 GB Macs and produces noticeably better JA ↔ EN translation
  and ASR accuracy.
- Gemma 4 improves on Japanese translation stability and instruction-following
  compared to Gemma 3.

## Install / update

One command from anywhere — no clone needed:

```sh
curl -fsSL https://raw.githubusercontent.com/shohei81/kotoma/main/install.sh | bash -s -- high
# or: standard · both · (omit the preset to install the binary only)
curl -fsSL https://raw.githubusercontent.com/shohei81/kotoma/main/install.sh | bash
```

The installer always:
- Downloads the latest prebuilt binary from GitHub Releases into
  `~/.local/bin/kotoma` (Apple Silicon macOS). Elsewhere — or with
  `--from-source` — it falls back to `cargo install --git … --force kotoma`.

When you pass a preset (`standard` / `high` / `both`) it then runs
`kotoma model preset <name>` to download the models and write
`~/.config/kotoma/kotoma.toml`. Omit the preset and the binary is installed
alone, with guidance on setting up models afterwards.

The models are kotoma's only heavy dependency; the binary itself updates
independently of them (see [Update](#update)).

### Update

Updates target the **binary**; your models and edited config are a separate
dependency and are never touched. From an already-installed kotoma:

```sh
kotoma update          # update the binary only
kotoma update high     # also (re)fetch the high-tier model preset
```

This re-runs `install.sh` for you — building from source when the current
binary is a `cargo install` one (`~/.cargo/bin`), otherwise downloading the
latest prebuilt binary. Running the original `curl … | bash` command (with or
without a tier) does the same thing.

### Managing models

Models are the only heavy dependency, and you pick them independently of the
tiers. `kotoma model` works off an embedded catalog of known-good
whisper.cpp / llama.cpp models:

```sh
kotoma model list                         # catalog + what's installed/active
kotoma model preset both                  # install a whole preset (standard|high|both)
kotoma model pull whisper-large-v3-turbo   # download one model into ~/.config/kotoma/models/
kotoma model use  whisper-large-v3-turbo   # point kotoma.toml at it
kotoma model rm   whisper-small            # delete a downloaded file
```

`preset` downloads a bundle and writes the config (creating it from the
template if missing); `both` installs everything and selects the high preset.
`use` rewrites the relevant path in `~/.config/kotoma/kotoma.toml` (top-level
`model_path` for ASR models, `[translator] model_path` for translators),
preserving the rest of the file. Mixing tiers is fine — e.g. a large ASR model
with the 4B translator. You can still point the config at any compatible
`ggml-*.bin` / `*.gguf` by hand; the catalog is just a convenience.

### From a cloned repo (dev)

```sh
./install.sh high                  # same flow, cargo install --git
# or, to build the currently checked-out code:
./setup.sh high                    # cargo install --path .
```

### Manual setup

If you prefer to drive it yourself, copy `kotoma.toml.example` to
`~/.config/kotoma/kotoma.toml` and point `model_path` (and, for translation,
`[translator] model_path`) at the models you downloaded into
`~/.config/kotoma/models/`. Relative paths resolve against the config file's
directory, so `models/foo` → `~/.config/kotoma/models/foo`. `kotoma model
list` / `pull` / `use` automate the same thing.

To disable translation permanently, delete or comment out the `[translator]`
section. To disable it for a single run, pass `--no-translate` — both take the
transcript-only path (no `llama-server`). With translation off the UI collapses
to a single full-width column showing each line's transcribed language (so a
`-l ja` run shows only 日本語, `-l auto` shows whatever each line was detected
as) instead of the English ↔ 日本語 split.

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
| `q` / `Ctrl+C`| Save transcript and quit          |
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
- Gemma 4 12B Q4_K_M: ~20–30 tok/s, with prompt caching across segments
- Peak RSS: ~10 GB; the standard tier stays around ~6 GB

For lighter setups: use the standard preset (`ggml-small.bin` +
`gemma-4-E4B-it-Q4_K_M.gguf`).

Whisper runs with flash attention on Metal (faster, cooler, no quality cost).
If the machine still runs hot during long transcription, switch to a quantized
turbo model — same accuracy class at a third to half the compute/heat:

```sh
kotoma model use whisper-large-v3-turbo-q8_0   # ~half size, near-zero loss
kotoma model use whisper-large-v3-turbo-q5_0   # ~1/3 size, a touch more loss
```

The UI only re-renders when something changed and only wraps the lines
visible in the viewport, so idle CPU usage is near zero and scrolling stays
smooth in long sessions. The audio level meter is deadbanded and its repaints
throttled to ~10 Hz, so ambient noise doesn't keep the terminal and window
compositor busy when nothing is being said.

Note: the translator now launches `llama-server` with `--jinja` (uses the
GGUF's bundled chat template), which requires a llama.cpp release from 2025
or later — `brew upgrade llama.cpp` if yours is older.

## Logs

Diagnostic logs are written to `kotoma.log` (keeps the TUI clean). Set
`RUST_LOG=debug` for verbose output.
