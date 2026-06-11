use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub model_path: PathBuf,
    #[serde(default = "default_output")]
    pub output_path: PathBuf,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_device")]
    pub input_device: String,
    /// Optional second source mixed sample-by-sample into the input stream.
    /// Typically a `loopback:<output device>` entry to capture system audio
    /// alongside the mic. `None` or an empty string disables.
    #[serde(default)]
    pub system_audio_device: Option<String>,
    #[serde(default = "default_threads")]
    pub threads: i32,
    #[serde(default)]
    pub vad: VadConfig,
    #[serde(default)]
    pub translator: Option<TranslatorConfigToml>,
    /// Set at runtime by main.rs — not loaded from TOML.
    #[serde(default, skip)]
    pub log_dir: PathBuf,
}

#[derive(Debug, Deserialize, Clone)]
pub struct VadConfig {
    #[serde(default = "default_aggr")]
    pub aggressiveness: u8,
    #[serde(default = "default_min_speech")]
    pub min_speech_ms: u32,
    #[serde(default = "default_silence")]
    pub silence_ms: u32,
    #[serde(default = "default_max_segment")]
    pub max_segment_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            aggressiveness: default_aggr(),
            min_speech_ms: default_min_speech(),
            silence_ms: default_silence(),
            max_segment_ms: default_max_segment(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct TranslatorConfigToml {
    pub model_path: PathBuf,
    #[serde(default = "default_llama_server_binary")]
    pub binary: PathBuf,
    #[serde(default = "default_translator_port")]
    pub port: u16,
    #[serde(default = "default_translator_n_ctx")]
    pub n_ctx: u32,
    #[serde(default = "default_translator_max_new")]
    pub max_new_tokens: u32,
    #[serde(default = "default_translator_startup_timeout")]
    pub startup_timeout_secs: u64,
}

fn default_output() -> PathBuf {
    PathBuf::from("transcript.md")
}
fn default_language() -> String {
    "en".to_string()
}
fn default_device() -> String {
    "default".to_string()
}
fn default_threads() -> i32 {
    4
}
fn default_aggr() -> u8 {
    2
}
fn default_min_speech() -> u32 {
    500
}
fn default_silence() -> u32 {
    900
}
fn default_max_segment() -> u32 {
    15_000
}
fn default_llama_server_binary() -> PathBuf {
    PathBuf::from("llama-server")
}
fn default_translator_port() -> u16 {
    8787
}
fn default_translator_n_ctx() -> u32 {
    8192
}
fn default_translator_max_new() -> u32 {
    512
}
fn default_translator_startup_timeout() -> u64 {
    120
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = find_config(explicit)?;
        // Absolute-ify the config path so relative paths inside it resolve
        // against the config's directory, not whatever CWD happens to be.
        let abs_path = std::fs::canonicalize(&path).unwrap_or_else(|_| {
            std::env::current_dir()
                .map(|d| d.join(&path))
                .unwrap_or_else(|_| path.clone())
        });
        let config_dir = abs_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;

        cfg.model_path = resolve_path(&cfg.model_path, &config_dir);
        if let Some(t) = cfg.translator.as_mut() {
            t.model_path = resolve_path(&t.model_path, &config_dir);
            // Leave `binary` alone: if it's just "llama-server" we want PATH lookup.
            if t.binary.components().count() > 1 || t.binary.to_string_lossy().starts_with("~/") {
                t.binary = resolve_path(&t.binary, &config_dir);
            }
        }

        tracing::info!(config = %path.display(), "config loaded");
        Ok(cfg)
    }
}

fn find_config(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.exists() {
            return Err(anyhow!("config not found: {}", p.display()));
        }
        return Ok(p.to_path_buf());
    }

    for candidate in ["kotoma.toml", "livemd.toml"] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Ok(p);
        }
    }

    if let Some(home) = dirs::home_dir() {
        for rel in [
            ".config/kotoma/kotoma.toml",
            ".config/livemd/livemd.toml",
        ] {
            let global = home.join(rel);
            if global.exists() {
                return Ok(global);
            }
        }
    }

    Err(anyhow!(
        "no kotoma.toml found\n\
         tried: ./kotoma.toml, ~/.config/kotoma/kotoma.toml\n\
         to set up globally:\n  \
           mkdir -p ~/.config/kotoma\n  \
           cp kotoma.toml.example ~/.config/kotoma/kotoma.toml\n  \
           (then edit to point at your model files)"
    ))
}

fn resolve_path(p: &Path, base: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}
