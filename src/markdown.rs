use crate::transcribe::TranscriptLine;
use anyhow::Result;
use chrono::{DateTime, Local};
use std::io::Write;
use std::path::Path;

pub fn write(
    path: &Path,
    lines: &[TranscriptLine],
    existing: Option<&str>,
) -> Result<()> {
    if lines.is_empty() && existing.is_none() {
        return Ok(());
    }

    // Write to a sibling temp file and rename into place, so a mid-write
    // failure (disk full, crash) can never truncate an existing transcript.
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "transcript.md".to_string());
    let tmp_path = path.with_file_name(format!("{file_name}.tmp"));
    let mut f = std::fs::File::create(&tmp_path)?;

    let res = (|| -> Result<()> {
        match existing {
            Some(prev) => {
                f.write_all(prev.as_bytes())?;
                if !prev.ends_with('\n') {
                    writeln!(f)?;
                }
                writeln!(f)?;
                if !lines.is_empty() {
                    write_session(&mut f, lines)?;
                }
            }
            None => {
                write_frontmatter(&mut f)?;
                if !lines.is_empty() {
                    write_session(&mut f, lines)?;
                }
            }
        }
        f.sync_all()?;
        Ok(())
    })();

    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    std::fs::rename(&tmp_path, path)?;
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
    use super::*;

    #[test]
    fn escapes_pipes_and_newlines() {
        assert_eq!(escape_cell("a|b\nc"), "a\\|b c");
    }

    fn sample_line() -> TranscriptLine {
        TranscriptLine {
            id: 0,
            text: "hello".into(),
            translated: Some("こんにちは".into()),
            src_lang: "en".into(),
            started_at: Local::now(),
            ended_at: Local::now(),
        }
    }

    #[test]
    fn writes_transcript_and_leaves_no_tmp_file() {
        let dir = std::env::temp_dir().join(format!("kotoma-md-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.md");

        write(&path, &[sample_line()], None).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("| hello | こんにちは |"));
        assert!(!dir.join("t.md.tmp").exists());

        // Resume: prior content is preserved verbatim above the new session.
        write(&path, &[sample_line()], Some("# old notes\n")).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.starts_with("# old notes\n"));
        assert!(out.contains("| hello | こんにちは |"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
