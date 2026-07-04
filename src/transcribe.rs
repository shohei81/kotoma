use crate::filter;
use crate::msg::detect_lang;
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use crossbeam_channel::{Receiver, Sender};
use std::path::Path;
use std::sync::{Arc, RwLock};
use tracing::{debug, error, info};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct Segment {
    pub id: u64,
    pub samples: Vec<f32>,
    pub started_at: DateTime<Local>,
    pub ended_at: DateTime<Local>,
    pub speech_ms: u32,
}

#[derive(Clone)]
pub struct TranscriptLine {
    pub id: u64,
    pub text: String,
    pub translated: Option<String>,
    pub src_lang: String,
    pub started_at: DateTime<Local>,
    pub ended_at: DateTime<Local>,
}

pub struct TranscribeRunner {
    ctx: WhisperContext,
    threads: i32,
    language: Arc<RwLock<String>>,
    dynamic_audio_ctx: bool,
}

const PROMPT_CHAR_WINDOW: usize = 224;

/// Encoder frames to request for a segment of `n_samples` 16 kHz samples.
///
/// whisper always encodes a fixed 30 s window (1500 frames, 50 per second)
/// no matter how short the audio is, so a 5 s utterance pays the full-window
/// encoder cost — the dominant per-segment compute. Telling it how much
/// audio actually exists (+25% headroom, floored at 384 frames to stay well
/// clear of quality cliffs) cuts that roughly proportionally.
fn audio_ctx_for(n_samples: usize) -> i32 {
    let secs = n_samples as f32 / crate::audio::TARGET_SR as f32;
    let frames = (secs * 50.0 * 1.25).ceil() as i32;
    frames.clamp(384, 1500)
}

impl TranscribeRunner {
    pub fn new(
        model_path: &Path,
        threads: i32,
        language: Arc<RwLock<String>>,
        dynamic_audio_ctx: bool,
    ) -> Result<Self> {
        let mut params = WhisperContextParameters::default();
        // Flash attention cuts the encoder's memory traffic and compute on
        // Metal — faster inference and noticeably less heat, with no quality
        // cost. Safe here because we don't use DTW token timestamps.
        params.flash_attn(true);
        let path_str = model_path.to_str().context("model path is not valid UTF-8")?;
        let ctx = WhisperContext::new_with_params(path_str, params)
            .with_context(|| format!("loading whisper model at {}", model_path.display()))?;
        info!(path = %model_path.display(), "whisper model loaded");
        Ok(Self {
            ctx,
            threads,
            language,
            dynamic_audio_ctx,
        })
    }

    pub fn run(&self, seg_rx: Receiver<Segment>, out_tx: Sender<TranscriptLine>) {
        let mut state = match self.ctx.create_state() {
            Ok(s) => s,
            Err(e) => {
                error!("whisper create_state failed: {}", e);
                return;
            }
        };

        let mut context_chars: Vec<char> = Vec::with_capacity(PROMPT_CHAR_WINDOW * 2);

        while let Ok(seg) = seg_rx.recv() {
            let lang_snapshot = self
                .language
                .read()
                .map(|g| g.clone())
                .unwrap_or_else(|_| "en".into());
            let lang_code: &str = match lang_snapshot.as_str() {
                "en" => "en",
                "ja" => "ja",
                _ => "auto",
            };

            let prompt: String = context_chars.iter().collect();

            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_language(Some(lang_code));
            params.set_n_threads(self.threads);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_print_special(false);
            params.set_suppress_blank(true);
            params.set_suppress_non_speech_tokens(true);
            params.set_temperature(0.0);
            // Keep temperature_inc at the upstream default (0.2). When
            // entropy_thold / logprob_thold flags a degenerate segment,
            // whisper retries with temperature bumped up — this is the
            // built-in escape hatch from repetition loops. Setting this
            // to 0.0 disables the fallback entirely.
            // NOTE: whisper-rs 0.13 documents set_no_speech_thold as unimplemented
            // (see its whisper_params.rs). Keep the call for when it lands; for
            // now the silence-fallback filter in filter.rs + the speech_ms gate
            // below do the real work.
            params.set_no_speech_thold(0.8);
            params.set_translate(false);
            if self.dynamic_audio_ctx {
                let audio_ctx = audio_ctx_for(seg.samples.len());
                if audio_ctx < 1500 {
                    params.set_audio_ctx(audio_ctx);
                }
            }
            if !prompt.is_empty() {
                params.set_initial_prompt(&prompt);
            }

            if let Err(e) = state.full(params, &seg.samples) {
                error!("whisper full failed: {}", e);
                continue;
            }

            let n = state.full_n_segments().unwrap_or(0);
            let mut text = String::new();
            for i in 0..n {
                if let Ok(s) = state.full_get_segment_text(i) {
                    text.push_str(&s);
                }
            }

            let cleaned = match filter::clean(&text) {
                Some(s) => s,
                None => {
                    debug!(raw = %text.trim(), "dropped by filter");
                    continue;
                }
            };

            if seg.speech_ms < 1500 && filter::is_silence_fallback(&cleaned) {
                debug!(
                    speech_ms = seg.speech_ms,
                    text = %cleaned,
                    "dropped short-segment silence fallback"
                );
                continue;
            }

            // In auto mode whisper ran real language detection on the audio —
            // trust it over the character-class heuristic (which a romanized
            // or all-ASCII Japanese line can fool). Forced en/ja skips
            // detection, so the text heuristic stays authoritative there.
            let src_lang = if lang_code == "auto" {
                state
                    .full_lang_id_from_state()
                    .ok()
                    .and_then(whisper_rs::get_lang_str)
                    .filter(|s| *s == "ja" || *s == "en")
                    .map(str::to_string)
                    .unwrap_or_else(|| detect_lang(&cleaned).to_string())
            } else {
                detect_lang(&cleaned).to_string()
            };
            info!(
                len = cleaned.len(),
                speech_ms = seg.speech_ms,
                lang = %lang_snapshot,
                detected = %src_lang,
                "transcribed segment"
            );

            context_chars.push(' ');
            context_chars.extend(cleaned.chars());
            if context_chars.len() > PROMPT_CHAR_WINDOW {
                let drop = context_chars.len() - PROMPT_CHAR_WINDOW;
                context_chars.drain(..drop);
            }

            let _ = out_tx.send(TranscriptLine {
                id: seg.id,
                text: cleaned,
                translated: None,
                src_lang,
                started_at: seg.started_at,
                ended_at: seg.ended_at,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::audio_ctx_for;

    #[test]
    fn audio_ctx_scales_with_segment_length() {
        let sr = 16_000;
        assert_eq!(audio_ctx_for(2 * sr), 384); // short segments hit the floor
        assert_eq!(audio_ctx_for(10 * sr), 625); // 10 s → 500 frames × 1.25
        assert_eq!(audio_ctx_for(30 * sr), 1500); // never above the full window
        assert_eq!(audio_ctx_for(60 * sr), 1500);
    }
}
