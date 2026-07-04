//! Merges consecutive transcript fragments into sentence-shaped lines.
//!
//! VAD segments end at pauses, not sentence boundaries — and Japanese puts
//! the verb last, so translating fragments is structurally lossy. When a
//! segment doesn't end a sentence and the next one follows quickly in the
//! same language, the two become one line (keeping the first id) and the
//! merged text is re-translated, replacing the fragment's translation.

use crate::filter::is_sentence_end;
use crate::transcribe::TranscriptLine;
use chrono::{DateTime, Local};

/// Hard cap on a merged line — a speaker who never pauses at sentence ends
/// shouldn't grow one line (and one translation request) without bound.
const MERGE_CHAR_CAP: usize = 300;

pub enum Assembled {
    /// A fresh line: append to the transcript.
    New(TranscriptLine),
    /// An existing line's text grew (fragment continuation): replace the
    /// line with this id and re-translate the whole merged text.
    Merged(TranscriptLine),
}

pub struct SentenceAssembler {
    max_gap_ms: i64,
    open: Option<TranscriptLine>,
}

impl SentenceAssembler {
    /// `max_gap_ms` is the largest pause between segments that still counts
    /// as the same sentence; 0 disables merging entirely.
    pub fn new(max_gap_ms: u32) -> Self {
        Self {
            max_gap_ms: max_gap_ms as i64,
            open: None,
        }
    }

    fn gap_ms(prev_end: DateTime<Local>, next_start: DateTime<Local>) -> i64 {
        next_start.signed_duration_since(prev_end).num_milliseconds()
    }

    pub fn push(&mut self, line: TranscriptLine) -> Assembled {
        if self.max_gap_ms > 0 {
            if let Some(open) = &mut self.open {
                let mergeable = open.src_lang == line.src_lang
                    && !is_sentence_end(&open.text)
                    && Self::gap_ms(open.ended_at, line.started_at) <= self.max_gap_ms
                    && open.text.chars().count() + line.text.chars().count() <= MERGE_CHAR_CAP;
                if mergeable {
                    let sep = if open.src_lang == "ja" { "" } else { " " };
                    open.text = format!("{}{}{}", open.text, sep, line.text);
                    open.ended_at = line.ended_at;
                    open.translated = None;
                    return Assembled::Merged(open.clone());
                }
            }
        }
        self.open = Some(line.clone());
        Assembled::New(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn line(id: u64, text: &str, lang: &str, start_ms: i64, end_ms: i64) -> TranscriptLine {
        let base = Local::now();
        TranscriptLine {
            id,
            text: text.to_string(),
            translated: None,
            src_lang: lang.to_string(),
            started_at: base + Duration::milliseconds(start_ms),
            ended_at: base + Duration::milliseconds(end_ms),
        }
    }

    fn as_merged(a: Assembled) -> TranscriptLine {
        match a {
            Assembled::Merged(l) => l,
            Assembled::New(l) => panic!("expected Merged, got New({})", l.text),
        }
    }

    fn as_new(a: Assembled) -> TranscriptLine {
        match a {
            Assembled::New(l) => l,
            Assembled::Merged(l) => panic!("expected New, got Merged({})", l.text),
        }
    }

    #[test]
    fn fragment_then_continuation_merges() {
        let mut asm = SentenceAssembler::new(2000);
        as_new(asm.push(line(0, "I went to", "en", 0, 1000)));
        let m = as_merged(asm.push(line(1, "the store.", "en", 1500, 2500)));
        assert_eq!(m.id, 0);
        assert_eq!(m.text, "I went to the store.");
        assert!(m.translated.is_none());
    }

    #[test]
    fn japanese_merges_without_space() {
        let mut asm = SentenceAssembler::new(2000);
        as_new(asm.push(line(0, "昨日の会議で決まったことを", "ja", 0, 1000)));
        let m = as_merged(asm.push(line(1, "共有します。", "ja", 1500, 2500)));
        assert_eq!(m.text, "昨日の会議で決まったことを共有します。");
    }

    #[test]
    fn completed_sentence_does_not_merge() {
        let mut asm = SentenceAssembler::new(2000);
        as_new(asm.push(line(0, "That's done.", "en", 0, 1000)));
        let n = as_new(asm.push(line(1, "Next topic", "en", 1500, 2500)));
        assert_eq!(n.id, 1);
    }

    #[test]
    fn language_switch_does_not_merge() {
        let mut asm = SentenceAssembler::new(2000);
        as_new(asm.push(line(0, "I went to", "en", 0, 1000)));
        as_new(asm.push(line(1, "お店です。", "ja", 1500, 2500)));
    }

    #[test]
    fn long_pause_does_not_merge() {
        let mut asm = SentenceAssembler::new(2000);
        as_new(asm.push(line(0, "I went to", "en", 0, 1000)));
        as_new(asm.push(line(1, "the store.", "en", 4000, 5000)));
    }

    #[test]
    fn merge_chain_extends_the_same_line() {
        let mut asm = SentenceAssembler::new(2000);
        as_new(asm.push(line(0, "one", "en", 0, 1000)));
        as_merged(asm.push(line(1, "two", "en", 1500, 2500)));
        let m = as_merged(asm.push(line(2, "three.", "en", 3000, 4000)));
        assert_eq!(m.id, 0);
        assert_eq!(m.text, "one two three.");
    }

    #[test]
    fn zero_gap_disables_merging() {
        let mut asm = SentenceAssembler::new(0);
        as_new(asm.push(line(0, "I went to", "en", 0, 1000)));
        as_new(asm.push(line(1, "the store.", "en", 1200, 2000)));
    }

    #[test]
    fn char_cap_blocks_runaway_merge() {
        let mut asm = SentenceAssembler::new(2000);
        as_new(asm.push(line(0, &"a".repeat(295), "en", 0, 1000)));
        as_new(asm.push(line(1, &"b".repeat(10), "en", 1500, 2500)));
    }
}
