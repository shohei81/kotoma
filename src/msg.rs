use crate::transcribe::TranscriptLine;

#[derive(Clone)]
pub enum UiMsg {
    NewLine(TranscriptLine),
    TranslationReady { id: u64, translated: String },
    TranslatorStatus(TranslatorStatus),
}

#[derive(Clone, Copy, Debug)]
pub enum TranslatorStatus {
    Loading,
    Ready,
    Failed,
}

/// Snapshot of VAD's in-progress segment, emitted a few times per second.
#[derive(Clone, Copy, Debug, Default)]
pub struct DraftState {
    pub active: bool,
    /// Cumulative milliseconds that registered as speech.
    pub speech_ms: u32,
    /// Wall-clock milliseconds since the segment started.
    pub elapsed_ms: u32,
}

/// Binary classifier for column/translation-direction routing: "en" vs "ja"
/// (there is no third bucket). Hangul deliberately maps to "ja": it only shows
/// up as a whisper hallucination, and routing it down the ja → en path — where
/// translate.rs's Hangul-leak guard rejects it — is the least harmful choice
/// (treating it as "en" would poison the en → ja context history instead).
pub fn detect_lang(text: &str) -> &'static str {
    let has_cjk = text.chars().any(|c| {
        let n = c as u32;
        (0x3040..=0x309F).contains(&n)   // Hiragana
            || (0x30A0..=0x30FF).contains(&n) // Katakana
            || (0x4E00..=0x9FFF).contains(&n) // CJK Unified Ideographs
            || (0xFF66..=0xFF9D).contains(&n) // Halfwidth Katakana
            || (0xAC00..=0xD7A3).contains(&n) // Hangul Syllables
            || (0x1100..=0x11FF).contains(&n) // Hangul Jamo
            || (0x3130..=0x318F).contains(&n) // Hangul Compatibility Jamo
    });
    if has_cjk {
        "ja"
    } else {
        "en"
    }
}

#[cfg(test)]
mod tests {
    use super::detect_lang;

    #[test]
    fn kana_is_ja() {
        assert_eq!(detect_lang("こんにちは"), "ja");
    }

    #[test]
    fn kanji_is_ja() {
        assert_eq!(detect_lang("会議"), "ja");
    }

    #[test]
    fn ascii_is_en() {
        assert_eq!(detect_lang("Hello, world"), "en");
    }

    #[test]
    fn mixed_with_kana_is_ja() {
        assert_eq!(detect_lang("OKです"), "ja");
    }

    #[test]
    fn hangul_is_ja() {
        assert_eq!(detect_lang("안녕하세요"), "ja");
    }
}
