mod app;
mod audio;
mod config;
mod filter;
mod markdown;
mod model;
mod msg;
mod transcribe;
mod translate;
mod ui;
mod vad;

use anyhow::{bail, Result};
use clap::Parser;
use std::path::{Path, PathBuf};

const ALLOWED_OUTPUT_EXTS: &[&str] = &["md", "markdown", "mdown", "mkd", "txt", "log"];

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Live bilingual voice transcription TUI",
    after_help = "Commands:\n  \
        update [standard|high|both]  Update the kotoma binary in place; pass a tier to also refetch that model preset\n  \
        model <list|pull|use|preset|rm>  Manage local LLM models (run `kotoma model` to list)\n  \
        devices  List audio sources (mic + system audio) without starting the TUI"
)]
struct Args {
    /// Output markdown file. Example: `kotoma notes.md`
    #[arg(value_name = "OUTPUT.md")]
    output: Option<PathBuf>,

    /// Path to config file (default: ./kotoma.toml or ~/.config/kotoma/kotoma.toml)
    #[arg(short = 'c', long)]
    config: Option<PathBuf>,

    /// Override whisper model path
    #[arg(short = 'm', long)]
    model: Option<PathBuf>,

    /// Override starting language (en | ja | auto)
    #[arg(short = 'l', long)]
    language: Option<String>,

    /// Append a new session to an existing markdown file instead of overwriting.
    #[arg(short = 'r', long)]
    resume: bool,

    /// Disable translation for this run; transcribe only (skips llama-server).
    #[arg(long)]
    no_translate: bool,
}

const INSTALL_URL: &str = "https://raw.githubusercontent.com/shohei81/kotoma/main/install.sh";
const INSTALL_PS1_URL: &str = "https://raw.githubusercontent.com/shohei81/kotoma/main/install.ps1";

fn main() -> Result<()> {
    // `kotoma update [tier]` self-updates by re-running install.sh. Handle it
    // before clap so it doesn't collide with the positional OUTPUT argument.
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).map(String::as_str) == Some("update") {
        return run_update(raw.get(2).map(String::as_str));
    }
    if raw.get(1).map(String::as_str) == Some("model") {
        return model::run(&raw[2..]);
    }
    if raw.get(1).map(String::as_str) == Some("devices") {
        return run_devices(&raw[2..]);
    }

    let args = Args::parse();

    let log_dir = log_dir_path();
    std::fs::create_dir_all(&log_dir).ok();
    init_logging(&log_dir)?;
    whisper_rs::install_whisper_tracing_trampoline();

    let mut cfg = config::Config::load(args.config.as_deref())?;
    if let Some(m) = args.model {
        cfg.model_path = m;
    }
    if let Some(o) = args.output {
        cfg.output_path = normalize_output_path(o)?;
    }
    if let Some(l) = args.language {
        cfg.language = l;
    }
    if args.no_translate {
        cfg.translator = None;
    }
    cfg.log_dir = log_dir;

    let existing = if args.resume && cfg.output_path.exists() {
        let content = std::fs::read_to_string(&cfg.output_path)?;
        tracing::info!(
            path = %cfg.output_path.display(),
            bytes = content.len(),
            "resuming: snapshot taken"
        );
        Some(content)
    } else {
        None
    };

    app::run(cfg, existing)
}

/// Re-run install.sh to update the binary in place. Models are a separate
/// dependency and are left untouched; pass an explicit tier only if you also
/// want to (re)fetch that preset's models.
fn run_update(tier: Option<&str>) -> Result<()> {
    let tier = match tier {
        Some(t @ ("standard" | "high" | "both")) => Some(t),
        Some(other) => bail!("unknown tier '{other}' (expected: standard | high | both)"),
        None => None,
    };

    // Match the binary we're replacing: a cargo-installed kotoma lives under
    // ~/.cargo/bin (or \.cargo\ on Windows), so build from source to update
    // it in place; a prebuilt binary takes the default download path.
    let from_source = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.to_str()
                .map(|s| s.contains("/.cargo/") || s.contains("\\.cargo\\"))
        })
        .unwrap_or(false);

    println!(
        "==> Updating kotoma binary{}{}",
        tier.map(|t| format!(" (+ {t} models)")).unwrap_or_default(),
        if from_source { ", from source" } else { "" }
    );

    let status = if cfg!(windows) {
        let mut script =
            format!("& ([scriptblock]::Create((irm -useb '{INSTALL_PS1_URL}')))");
        if let Some(t) = tier {
            script.push_str(&format!(" -Tier {t}"));
        }
        if from_source {
            script.push_str(" -FromSource");
        }
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
            .status()?
    } else {
        let mut script = format!("curl -fsSL '{INSTALL_URL}' | bash -s --");
        if let Some(t) = tier {
            script.push(' ');
            script.push_str(t);
        }
        if from_source {
            script.push_str(" --from-source");
        }
        std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .status()?
    };
    if !status.success() {
        bail!("update failed (install script exited with {status})");
    }
    Ok(())
}

/// Print enumerated audio sources without starting the TUI — for writing
/// config files and for cross-platform bug reports. `devices probe <name>
/// [secs]` additionally opens one source and reports its signal level.
fn run_devices(args: &[String]) -> Result<()> {
    if args.first().map(String::as_str) == Some("probe") {
        let name = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: kotoma devices probe <device name> [secs]"))?;
        let secs = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
        return audio::probe(name, secs);
    }
    println!("Microphone sources:");
    let (mic, sys) = audio::list_devices_split();
    for m in &mic {
        println!("  {m}");
    }
    println!("\nSystem-audio sources:");
    for s in &sys {
        println!("  {s}");
    }
    println!("\nSystem-audio auto-detect candidates (tried in order):");
    let cands = audio::detect_system_audio_candidates();
    if cands.is_empty() {
        println!("  (none found)");
    }
    for c in &cands {
        println!("  {c}");
    }
    Ok(())
}

fn normalize_output_path(path: PathBuf) -> Result<PathBuf> {
    match path.extension() {
        None => Ok(path.with_extension("md")),
        Some(ext) => {
            let lower = ext.to_string_lossy().to_ascii_lowercase();
            if ALLOWED_OUTPUT_EXTS.iter().any(|e| *e == lower) {
                Ok(path)
            } else {
                bail!(
                    "unsupported output extension: .{lower} (allowed: {})",
                    ALLOWED_OUTPUT_EXTS
                        .iter()
                        .map(|e| format!(".{e}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
}

fn log_dir_path() -> PathBuf {
    if let Ok(v) = std::env::var("KOTOMA_LOG_DIR") {
        return PathBuf::from(v);
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".config/kotoma/logs");
    }
    PathBuf::from(".")
}

fn init_logging(preferred: &Path) -> Result<()> {
    use tracing_subscriber::{fmt, EnvFilter};

    let log_path = if preferred.exists() {
        preferred.join("kotoma.log")
    } else {
        PathBuf::from("kotoma.log")
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(file)
        .with_ansi(false)
        .init();
    Ok(())
}
