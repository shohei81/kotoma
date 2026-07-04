const NOISE_TAGS: &[&str] = &[
    "[BLANK_AUDIO]",
    "[MUSIC]",
    "[Music]",
    "[music]",
    "[MÚSICA]",
    "[Music playing]",
    "[APPLAUSE]",
    "[Applause]",
    "[LAUGHTER]",
    "[Laughter]",
    "[SILENCE]",
    "[NOISE]",
    "[noise]",
    "(inaudible)",
    "(Inaudible)",
];

const HALLUCINATION_PHRASES: &[&str] = &[
    "thank you for watching",
    "thanks for watching",
    "thank you for watching!",
    "thanks for watching!",
    "please subscribe",
    "please subscribe.",
    "please subscribe to my channel",
    "subscribe to my channel",
    "like and subscribe",
    "see you in the next video",
    "see you next time",
    "thanks for listening",
];

/// Phrases whisper commonly hallucinates when audio is near-silent.
/// Dropped only when the originating segment has little actual speech.
const SILENCE_FALLBACKS: &[&str] = &[
    "thank you",
    "thanks",
    "you",
    "bye",
    "bye bye",
    "goodbye",
    "hi",
    "hello",
    "okay",
    "ok",
    "yes",
    "no",
    "uh",
    "um",
    "mm",
    "hmm",
];

/// YouTube-style suffixes whisper appends to otherwise-real speech. Stripped
/// unconditionally when present at end. Compared case-insensitively.
const TRAILING_HALLUCINATIONS: &[&str] = &[
    "thank you for watching.",
    "thanks for watching.",
    "thank you for watching!",
    "thanks for watching!",
    "thank you for watching",
    "thanks for watching",
    "please subscribe.",
    "please subscribe",
    "thanks for listening.",
    "thanks for listening",
];

/// Filler that whisper tacks onto real sentences. Stripped only when the
/// preceding content ends with sentence punctuation — avoids mangling real
/// utterances like "I wanted to say thank you".
const TRAILING_FILLER: &[&str] = &[
    "thank you so much.",
    "thank you very much.",
    "thank you.",
    "thank you",
    "thanks.",
    "thanks",
];

pub fn clean(text: &str) -> Option<String> {
    let mut s = text.trim().to_string();

    for tag in NOISE_TAGS {
        s = s.replace(tag, " ");
    }
    let s = collapse_whitespace(s.trim());

    if s.is_empty() {
        return None;
    }
    if is_only_brackets(&s) {
        return None;
    }

    let mut s = s;
    for _ in 0..3 {
        let before = s.clone();
        s = strip_trailing_stray_you(&s);
        s = strip_trailing_hallucinations(&s);
        s = strip_trailing_word_repetition(&s);
        if s == before {
            break;
        }
    }
    let s = s.trim().to_string();
    if s.is_empty() {
        return None;
    }

    let lower = s.to_lowercase();
    let bare = lower
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .to_string();
    for h in HALLUCINATION_PHRASES {
        if bare == *h || lower == *h {
            return None;
        }
    }

    if is_repetition_loop(&s) || is_word_repetition_loop(&s) {
        return None;
    }

    Some(s)
}

/// Returns true if the cleaned text is a short phrase whisper commonly
/// hallucinates on near-silent audio. Caller decides whether to drop
/// based on segment speech duration.
pub fn is_silence_fallback(text: &str) -> bool {
    let bare: String = text
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    let bare = bare.split_whitespace().collect::<Vec<_>>().join(" ");
    SILENCE_FALLBACKS.iter().any(|p| bare == *p)
}

fn strip_trailing_hallucinations(s: &str) -> String {
    let mut out = s.trim().to_string();
    loop {
        let mut stripped = false;

        // YouTube-style suffixes — strip unconditionally, case-insensitive.
        for phrase in TRAILING_HALLUCINATIONS {
            if ends_with_ignore_ascii_case(&out, phrase) && out.len() > phrase.len() {
                let new_len = out.len() - phrase.len();
                out = out[..new_len].trim_end().to_string();
                stripped = true;
                break;
            }
        }
        if stripped {
            continue;
        }

        // Short filler (Thank you., Thanks.) — only strip if the text before
        // already ends with a sentence boundary. Standalone "Thank you." is
        // handled upstream by filter::is_silence_fallback.
        for phrase in TRAILING_FILLER {
            if ends_with_ignore_ascii_case(&out, phrase) && out.len() > phrase.len() {
                let new_len = out.len() - phrase.len();
                let before = out[..new_len].trim_end();
                if is_sentence_end(before) {
                    out = before.to_string();
                    stripped = true;
                    break;
                }
            }
        }

        if !stripped {
            break;
        }
    }
    out
}

fn ends_with_ignore_ascii_case(s: &str, suffix: &str) -> bool {
    if s.len() < suffix.len() {
        return false;
    }
    let start = s.len() - suffix.len();
    if !s.is_char_boundary(start) {
        return false;
    }
    s[start..].eq_ignore_ascii_case(suffix)
}

fn is_sentence_end(s: &str) -> bool {
    let t = s.trim_end();
    t.ends_with('.')
        || t.ends_with('?')
        || t.ends_with('!')
        || t.ends_with('…')
        || t.ends_with("...")
        || t.ends_with('。')
        || t.ends_with('？')
        || t.ends_with('！')
}

/// Whisper often appends a stray " you" or " you." after sentences. Strip it
/// if the preceding token already ends with sentence punctuation.
fn strip_trailing_stray_you(s: &str) -> String {
    let t = s.trim_end();
    for suffix in [" you.", " you", ". you.", ". you"] {
        if t.ends_with(suffix) {
            let cut = t.len() - suffix.len();
            let before = t[..cut].trim_end();
            if before.ends_with('.') || before.ends_with('?') || before.ends_with('!') {
                return before.to_string();
            }
        }
    }
    t.to_string()
}

fn is_repetition_loop(s: &str) -> bool {
    let chars: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.len() < 15 {
        return false;
    }
    let mut counts = std::collections::HashMap::new();
    for c in &chars {
        *counts.entry(*c).or_insert(0usize) += 1;
    }
    let max_count = counts.values().copied().max().unwrap_or(0);
    max_count * 100 / chars.len() >= 70
}

/// Detects word-level / token-level loops like "Great. Great. Great." or
/// "the the the". Returns true if a single whitespace-delimited token
/// accounts for at least 60% of a string of 20+ words.
fn is_word_repetition_loop(s: &str) -> bool {
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() < 20 {
        return false;
    }
    let mut counts = std::collections::HashMap::new();
    for w in &words {
        *counts.entry(*w).or_insert(0usize) += 1;
    }
    let max_count = counts.values().copied().max().unwrap_or(0);
    max_count * 100 / words.len() >= 60
}

/// Strips trailing runs of an identical word (>=5 occurrences in a row).
/// Matches Whisper's habit of appending "Great. Great. Great." tails.
fn strip_trailing_word_repetition(s: &str) -> String {
    let trimmed = s.trim_end();
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() < 6 {
        return trimmed.to_string();
    }
    let last = words[words.len() - 1];
    let mut run = 1usize;
    for i in (0..words.len() - 1).rev() {
        if words[i] == last {
            run += 1;
        } else {
            break;
        }
    }
    if run < 5 {
        return trimmed.to_string();
    }
    let keep = words.len() - run;
    if keep == 0 {
        return String::new();
    }
    words[..keep].join(" ")
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn is_only_brackets(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return true;
    }
    // Only annotation-style brackets count: 「」 are quotation marks, so a
    // fully-quoted line is real speech, not a noise caption.
    let wrapped = (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('(') && t.ends_with(')'))
        || (t.starts_with('《') && t.ends_with('》'));
    if !wrapped {
        return false;
    }
    let total = t.chars().count();
    if total <= 2 {
        return true;
    }
    let inner_len = t
        .chars()
        .skip(1)
        .take(total - 2)
        .collect::<String>()
        .trim()
        .chars()
        .count();
    inner_len <= 30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_blank_audio_only() {
        assert_eq!(clean("[BLANK_AUDIO]"), None);
    }

    #[test]
    fn strips_trailing_noise_tag() {
        assert_eq!(
            clean("Hello world. [BLANK_AUDIO]").as_deref(),
            Some("Hello world.")
        );
    }

    #[test]
    fn drops_thanks_for_watching() {
        assert_eq!(clean("Thanks for watching!"), None);
    }

    #[test]
    fn keeps_real_speech() {
        assert_eq!(
            clean("  This is real speech.  ").as_deref(),
            Some("This is real speech.")
        );
    }

    #[test]
    fn drops_bracket_only_nonlatin() {
        assert_eq!(clean("《Soucaity Astrolabe》"), None);
    }

    #[test]
    fn keeps_fully_quoted_speech() {
        assert_eq!(
            clean("「こんにちは、元気ですか」").as_deref(),
            Some("「こんにちは、元気ですか」")
        );
    }

    #[test]
    fn drops_repetition_loop() {
        let runaway: String = "ん".repeat(300);
        assert_eq!(clean(&runaway), None);
    }

    #[test]
    fn keeps_normal_repetition() {
        assert_eq!(
            clean("yeah yeah yeah it's fine").as_deref(),
            Some("yeah yeah yeah it's fine")
        );
    }

    #[test]
    fn silence_fallback_detects_thank_you() {
        assert!(is_silence_fallback("Thank you."));
        assert!(is_silence_fallback("thank you"));
        assert!(is_silence_fallback("you"));
        assert!(is_silence_fallback(" Bye. "));
    }

    #[test]
    fn silence_fallback_ignores_real_speech() {
        assert!(!is_silence_fallback("Thank you for joining today."));
        assert!(!is_silence_fallback("I don't know."));
    }

    #[test]
    fn strips_trailing_youtube_tail() {
        // Case now strips the dangling "Thank you." too since it follows a
        // sentence boundary — hallucination tails are the overwhelming cause.
        assert_eq!(
            clean("I don't know. Thank you. Thank you for watching. you").as_deref(),
            Some("I don't know.")
        );
    }

    #[test]
    fn strips_trailing_stray_you() {
        assert_eq!(
            clean("Oh, that looks great. Alright. you").as_deref(),
            Some("Oh, that looks great. Alright.")
        );
    }

    #[test]
    fn drops_word_repetition_loop() {
        let runaway = "Great. ".repeat(100);
        assert_eq!(clean(&runaway), None);
    }

    #[test]
    fn strips_trailing_word_repetition_tail() {
        let input = format!(
            "Let me explain this properly. {}",
            "Great. ".repeat(40)
        );
        assert_eq!(
            clean(&input).as_deref(),
            Some("Let me explain this properly.")
        );
    }

    #[test]
    fn keeps_short_natural_repetition() {
        assert_eq!(
            clean("no no no, that's not what I meant").as_deref(),
            Some("no no no, that's not what I meant")
        );
    }

    #[test]
    fn strips_trailing_thank_you_after_sentence() {
        assert_eq!(
            clean("I can speak English. Thank you. Thank you.").as_deref(),
            Some("I can speak English.")
        );
    }

    #[test]
    fn strips_trailing_thank_you_very_much() {
        assert_eq!(
            clean("That was a great talk. Thank you very much.").as_deref(),
            Some("That was a great talk.")
        );
    }

    #[test]
    fn keeps_thank_you_in_middle() {
        // Without sentence-ending punctuation before, the strip bails out.
        assert_eq!(
            clean("I wanted to say thank you").as_deref(),
            Some("I wanted to say thank you")
        );
    }

    #[test]
    fn strips_case_insensitive_youtube_tail() {
        assert_eq!(
            clean("That concludes today's lecture. thank you for watching").as_deref(),
            Some("That concludes today's lecture.")
        );
    }
}
