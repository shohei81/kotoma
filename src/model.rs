//! `kotoma model` — manage local LLM models (kotoma's only heavy dependency).
//!
//! The catalog (`models.toml`) is embedded at build time, so `kotoma update`
//! ships the latest list. Commands operate on the global config + models dir
//! (`~/.config/kotoma/`), which is the canonical install location.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

const CATALOG_TOML: &str = include_str!("../models.toml");

#[derive(Deserialize)]
struct Catalog {
    model: Vec<ModelEntry>,
}

#[derive(Deserialize, Clone)]
struct ModelEntry {
    name: String,
    /// "asr" (whisper.cpp) or "translator" (llama.cpp GGUF).
    role: String,
    file: String,
    url: String,
    size: String,
    #[serde(default)]
    note: String,
}

impl ModelEntry {
    fn is_asr(&self) -> bool {
        self.role == "asr"
    }
}

pub fn run(args: &[String]) -> Result<()> {
    let catalog: Catalog =
        toml::from_str(CATALOG_TOML).context("parsing embedded models.toml")?;
    match args.first().map(String::as_str) {
        None | Some("list") => list(&catalog),
        Some("pull") => pull(&catalog, args.get(1).map(String::as_str)),
        Some("use") => use_model(&catalog, args.get(1).map(String::as_str)),
        Some("rm") => rm(&catalog, args.get(1).map(String::as_str)),
        Some("-h") | Some("--help") => {
            print_usage();
            Ok(())
        }
        Some(other) => {
            bail!("unknown subcommand 'model {other}' (expected: list | pull | use | rm)")
        }
    }
}

fn print_usage() {
    println!(
        "usage: kotoma model <command>\n\n  \
         list                 Show catalog models and which are installed/active\n  \
         pull <name>          Download a model into ~/.config/kotoma/models/\n  \
         use <name>           Point the config at an installed model\n  \
         rm <name>            Delete a downloaded model file"
    );
}

fn config_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("cannot locate home directory"))?
        .join(".config/kotoma"))
}

fn models_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("models"))
}

fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("kotoma.toml"))
}

fn find<'a>(catalog: &'a Catalog, name: Option<&str>) -> Result<&'a ModelEntry> {
    let name = name.ok_or_else(|| anyhow!("missing model name (see `kotoma model list`)"))?;
    catalog
        .model
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow!("unknown model '{name}' (see `kotoma model list`)"))
}

/// Basename of each active model path in the config, if it exists.
fn active_files() -> (Option<String>, Option<String>) {
    let Ok(path) = config_path() else {
        return (None, None);
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (None, None);
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return (None, None);
    };
    let basename = |v: Option<&toml::Value>| {
        v.and_then(|v| v.as_str())
            .and_then(|s| s.rsplit('/').next())
            .map(str::to_string)
    };
    let asr = basename(value.get("model_path"));
    let tr = basename(
        value
            .get("translator")
            .and_then(|t| t.get("model_path")),
    );
    (asr, tr)
}

fn list(catalog: &Catalog) -> Result<()> {
    let dir = models_dir()?;
    let (active_asr, active_tr) = active_files();

    for (role, header) in [("asr", "ASR (transcription)"), ("translator", "Translation")] {
        println!("{header}:");
        for m in catalog.model.iter().filter(|m| m.role == role) {
            let installed = dir.join(&m.file).exists();
            let active = match role {
                "asr" => active_asr.as_deref() == Some(m.file.as_str()),
                _ => active_tr.as_deref() == Some(m.file.as_str()),
            };
            let tag = match (installed, active) {
                (true, true) => "● active   ",
                (true, false) => "✓ installed",
                (false, _) => "  available",
            };
            println!("  {tag}  {:<24} {:>8}  {}", m.name, m.size, m.note);
        }
    }
    println!("\n  pull: kotoma model pull <name>   ·   use: kotoma model use <name>");
    Ok(())
}

fn pull(catalog: &Catalog, name: Option<&str>) -> Result<()> {
    let m = find(catalog, name)?;
    let dir = models_dir()?;
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(&m.file);
    if dest.exists() && std::fs::metadata(&dest).map(|md| md.len() > 0).unwrap_or(false) {
        println!("already present: {}", dest.display());
        return Ok(());
    }
    println!("==> Downloading {} ({})", m.name, m.size);
    // Shell out to curl for a progress bar and resilient large-file handling,
    // matching install.sh.
    let status = std::process::Command::new("curl")
        .args(["-fL", "--progress-bar", "-o"])
        .arg(&dest)
        .arg(&m.url)
        .status()
        .context("running curl (is it installed?)")?;
    if !status.success() {
        // Don't leave a truncated file behind.
        let _ = std::fs::remove_file(&dest);
        bail!("download failed (curl exited with {status})");
    }
    println!("installed → {}", dest.display());
    println!("activate with: kotoma model use {}", m.name);
    Ok(())
}

fn use_model(catalog: &Catalog, name: Option<&str>) -> Result<()> {
    let m = find(catalog, name)?;
    let dir = models_dir()?;
    if !dir.join(&m.file).exists() {
        bail!(
            "'{}' is not downloaded yet — run `kotoma model pull {}` first",
            m.name,
            m.name
        );
    }

    let path = config_path()?;
    if !path.exists() {
        bail!(
            "no config at {} — run `install.sh standard|high` once to create it",
            path.display()
        );
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;

    // Store relative to the config dir, matching the example configs.
    let rel = format!("models/{}", m.file);
    if m.is_asr() {
        doc["model_path"] = toml_edit::value(rel);
    } else {
        if doc.get("translator").and_then(|t| t.as_table()).is_none() {
            // No [translator] section yet (translation was off): create one.
            // Other fields fall back to their serde defaults at load time.
            doc["translator"] = toml_edit::table();
        }
        doc["translator"]["model_path"] = toml_edit::value(rel);
    }

    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("writing {}", path.display()))?;
    let role = if m.is_asr() { "ASR" } else { "translation" };
    println!("{} model set to {} in {}", role, m.name, path.display());
    Ok(())
}

fn rm(catalog: &Catalog, name: Option<&str>) -> Result<()> {
    let m = find(catalog, name)?;
    let dest = models_dir()?.join(&m.file);
    if !dest.exists() {
        println!("not installed: {}", m.name);
        return Ok(());
    }
    let (active_asr, active_tr) = active_files();
    let active = if m.is_asr() {
        active_asr.as_deref() == Some(m.file.as_str())
    } else {
        active_tr.as_deref() == Some(m.file.as_str())
    };
    std::fs::remove_file(&dest)
        .with_context(|| format!("removing {}", dest.display()))?;
    println!("removed {}", dest.display());
    if active {
        println!(
            "warning: this model was active in the config — pick another with `kotoma model use <name>`"
        );
    }
    Ok(())
}
