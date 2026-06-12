mod app;
mod audio;
mod config;
mod filter;
mod markdown;
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
#[command(version, about = "Live bilingual voice transcription TUI")]
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

    /// Skip the 要約/Summary sections on quit (transcription-only output).
    #[arg(long)]
    no_summary: bool,

    /// Disable translation for this run; transcribe only (skips llama-server).
    #[arg(long)]
    no_translate: bool,
}

fn main() -> Result<()> {
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
    if args.no_summary {
        if let Some(t) = cfg.translator.as_mut() {
            t.summarize = false;
        }
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
    if let Ok(v) = std::env::var("KOTOMA_LOG_DIR").or_else(|_| std::env::var("LIVEMD_LOG_DIR")) {
        return PathBuf::from(v);
    }
    if let Some(home) = dirs::home_dir() {
        let new_dir = home.join(".config/kotoma/logs");
        let legacy = home.join(".config/livemd/logs");
        if !new_dir.exists() && legacy.exists() {
            return legacy;
        }
        return new_dir;
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
