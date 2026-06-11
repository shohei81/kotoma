use crate::transcribe::TranscriptLine;
use anyhow::Result;
use chrono::{DateTime, Local};
use std::io::Write;
use std::path::Path;

pub fn write(
    path: &Path,
    lines: &[TranscriptLine],
    existing: Option<&str>,
    summary: Option<&str>,
) -> Result<()> {
    if lines.is_empty() && existing.is_none() {
        return Ok(());
    }

    let mut f = std::fs::File::create(path)?;

    match existing {
        Some(prev) => {
            f.write_all(prev.as_bytes())?;
            if !prev.ends_with('\n') {
                writeln!(f)?;
            }
            writeln!(f)?;
            if !lines.is_empty() {
                write_session(&mut f, lines, summary)?;
            }
        }
        None => {
            write_frontmatter(&mut f)?;
            if !lines.is_empty() {
                write_session(&mut f, lines, summary)?;
            }
        }
    }
    Ok(())
}

fn write_frontmatter(f: &mut std::fs::File) -> Result<()> {
    let now = Local::now();
    writeln!(f, "---")?;
    writeln!(f, "title: Transcript")?;
    writeln!(f, "created: {}", now.format("%Y-%m-%d %H:%M:%S"))?;
    writeln!(f, "---")?;
    writeln!(f)?;
    Ok(())
}

fn write_session(
    f: &mut std::fs::File,
    lines: &[TranscriptLine],
    summary: Option<&str>,
) -> Result<()> {
    let started: DateTime<Local> = lines
        .first()
        .map(|l| l.started_at)
        .unwrap_or_else(Local::now);
    let ended: DateTime<Local> = lines
        .last()
        .map(|l| l.ended_at)
        .unwrap_or_else(Local::now);

    let same_day = started.date_naive() == ended.date_naive();
    if same_day {
        writeln!(
            f,
            "## {} – {}",
            started.format("%Y-%m-%d %H:%M"),
            ended.format("%H:%M")
        )?;
    } else {
        writeln!(
            f,
            "## {} – {}",
            started.format("%Y-%m-%d %H:%M"),
            ended.format("%Y-%m-%d %H:%M")
        )?;
    }
    writeln!(f)?;
    if let Some(s) = summary {
        writeln!(f, "### 要約")?;
        writeln!(f)?;
        writeln!(f, "{}", s.trim())?;
        writeln!(f)?;
    }
    writeln!(f, "| time | English | 日本語 |")?;
    writeln!(f, "|------|---------|--------|")?;

    for line in lines {
        let ts = line.started_at.format("%H:%M:%S");
        let (en, ja) = if line.src_lang == "ja" {
            (line.translated.as_deref().unwrap_or(""), line.text.as_str())
        } else {
            (line.text.as_str(), line.translated.as_deref().unwrap_or(""))
        };
        writeln!(
            f,
            "| {} | {} | {} |",
            ts,
            escape_cell(en),
            escape_cell(ja)
        )?;
    }
    Ok(())
}

fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::escape_cell;

    #[test]
    fn escapes_pipes_and_newlines() {
        assert_eq!(escape_cell("a|b\nc"), "a\\|b c");
    }
}
