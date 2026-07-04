use crate::audio::TARGET_SR;
use crate::msg::DraftState;
use crate::transcribe::Segment;
use anyhow::Result;
use chrono::{DateTime, Local};
use crossbeam_channel::{Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, info};
use webrtc_vad::{SampleRate as VadSr, Vad, VadMode};

const FRAME_MS: u32 = 30;
pub const FRAME_SAMPLES: usize = (TARGET_SR as usize * FRAME_MS as usize) / 1000; // 480

/// Speech/non-speech decision per 30 ms frame. Injectable so the segmenting
/// state machine can be unit-tested with a scripted detector (webrtc-vad
/// needs real speech audio, which synthetic test signals can't provide).
trait SpeechDetector {
    fn is_speech(&mut self, frame_i16: &[i16]) -> bool;
}

struct WebRtcDetector {
    vad: Vad,
}

impl SpeechDetector for WebRtcDetector {
    fn is_speech(&mut self, frame_i16: &[i16]) -> bool {
        self.vad.is_voice_segment(frame_i16).unwrap_or(false)
    }
}

pub struct VadRunner {
    aggressiveness: u8,
    min_speech_ms: u32,
    silence_ms: u32,
    max_segment_ms: u32,
}

impl VadRunner {
    pub fn new(aggr: u8, min_speech_ms: u32, silence_ms: u32, max_segment_ms: u32) -> Self {
        Self {
            aggressiveness: aggr,
            min_speech_ms,
            silence_ms,
            max_segment_ms,
        }
    }

    pub fn run(
        &self,
        audio_rx: Receiver<Vec<f32>>,
        seg_tx: Sender<Segment>,
        level_tx: Sender<f32>,
        draft_tx: Sender<DraftState>,
        paused: Arc<AtomicBool>,
    ) -> Result<()> {
        let mode = match self.aggressiveness {
            0 => VadMode::Quality,
            1 => VadMode::LowBitrate,
            2 => VadMode::Aggressive,
            _ => VadMode::VeryAggressive,
        };
        let vad = Vad::new_with_rate_and_mode(VadSr::Rate16kHz, mode);
        self.run_with_detector(WebRtcDetector { vad }, audio_rx, seg_tx, level_tx, draft_tx, paused)
    }

    fn run_with_detector(
        &self,
        mut detector: impl SpeechDetector,
        audio_rx: Receiver<Vec<f32>>,
        seg_tx: Sender<Segment>,
        level_tx: Sender<f32>,
        draft_tx: Sender<DraftState>,
        paused: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut leftover: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
        let mut segment: Vec<f32> = Vec::new();
        let mut in_speech = false;
        let mut silence_frames = 0usize;
        let mut speech_frames = 0usize;
        let silence_limit = (self.silence_ms / FRAME_MS).max(1) as usize;
        let min_speech_frames = (self.min_speech_ms / FRAME_MS).max(1) as usize;
        let max_segment_frames = (self.max_segment_ms / FRAME_MS).max(1) as usize;
        let mut segment_start: Option<DateTime<Local>> = None;
        let mut next_id: u64 = 0;

        let mut frames_since_draft = 0usize;
        let emit_draft_every: usize = 10; // ~300 ms at 30 ms/frame

        while let Ok(chunk) = audio_rx.recv() {
            if paused.load(Ordering::Relaxed) {
                // Discard audio, reset segment state, flatten the UI gauge.
                leftover.clear();
                segment.clear();
                in_speech = false;
                speech_frames = 0;
                silence_frames = 0;
                let _ = level_tx.try_send(0.0);
                let _ = draft_tx.try_send(DraftState::default());
                continue;
            }

            leftover.extend_from_slice(&chunk);

            while leftover.len() >= FRAME_SAMPLES {
                let frame_f: Vec<f32> = leftover.drain(..FRAME_SAMPLES).collect();
                let frame_i16: Vec<i16> = frame_f
                    .iter()
                    .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                    .collect();

                let rms = (frame_f.iter().map(|s| s * s).sum::<f32>() / frame_f.len() as f32).sqrt();
                let _ = level_tx.try_send(rms);

                let is_speech = detector.is_speech(&frame_i16);

                if is_speech {
                    if !in_speech {
                        in_speech = true;
                        speech_frames = 0;
                        segment_start = Some(Local::now());
                        debug!("speech start");
                    }
                    silence_frames = 0;
                    speech_frames += 1;
                    segment.extend_from_slice(&frame_f);
                    frames_since_draft += 1;
                    if frames_since_draft >= emit_draft_every {
                        let _ = draft_tx.try_send(DraftState {
                            active: true,
                            speech_ms: (speech_frames as u32) * FRAME_MS,
                            elapsed_ms: segment_start
                                .map(|s| {
                                    Local::now()
                                        .signed_duration_since(s)
                                        .num_milliseconds()
                                        .max(0) as u32
                                })
                                .unwrap_or(0),
                        });
                        frames_since_draft = 0;
                    }
                } else if in_speech {
                    silence_frames += 1;
                    segment.extend_from_slice(&frame_f);
                    if silence_frames >= silence_limit {
                        if speech_frames >= min_speech_frames {
                            let speech_ms = (speech_frames as u32) * FRAME_MS;
                            let id = next_id;
                            next_id += 1;
                            let seg = Segment {
                                id,
                                samples: std::mem::take(&mut segment),
                                started_at: segment_start.unwrap_or_else(Local::now),
                                ended_at: Local::now(),
                                speech_ms,
                            };
                            info!(
                                id,
                                speech_frames,
                                speech_ms,
                                len = seg.samples.len(),
                                "flushing segment"
                            );
                            let _ = seg_tx.send(seg);
                        } else {
                            debug!(speech_frames, "discarded short segment");
                            segment.clear();
                        }
                        in_speech = false;
                        silence_frames = 0;
                        speech_frames = 0;
                        frames_since_draft = 0;
                        let _ = draft_tx.try_send(DraftState::default());
                    }
                }

                if in_speech && (speech_frames + silence_frames) >= max_segment_frames {
                    let speech_ms = (speech_frames as u32) * FRAME_MS;
                    let id = next_id;
                    next_id += 1;
                    let seg = Segment {
                        id,
                        samples: std::mem::take(&mut segment),
                        started_at: segment_start.unwrap_or_else(Local::now),
                        ended_at: Local::now(),
                        speech_ms,
                    };
                    info!(id, "max segment reached, force-flushing");
                    let _ = seg_tx.send(seg);
                    in_speech = false;
                    silence_frames = 0;
                    speech_frames = 0;
                    frames_since_draft = 0;
                    let _ = draft_tx.try_send(DraftState::default());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{bounded, unbounded};

    /// Plays back a fixed per-frame speech/non-speech script; false once
    /// the script runs out.
    struct Scripted {
        script: Vec<bool>,
        i: usize,
    }

    impl SpeechDetector for Scripted {
        fn is_speech(&mut self, _frame: &[i16]) -> bool {
            let v = self.script.get(self.i).copied().unwrap_or(false);
            self.i += 1;
            v
        }
    }

    /// Runs the segmenter over `script.len()` frames of audio with the given
    /// per-frame speech script and returns the flushed segments.
    fn run_segmenter(runner: &VadRunner, script: Vec<bool>, paused: bool) -> Vec<Segment> {
        let (audio_tx, audio_rx) = unbounded::<Vec<f32>>();
        let (seg_tx, seg_rx) = bounded::<Segment>(64);
        let (level_tx, _level_rx) = unbounded::<f32>();
        let (draft_tx, _draft_rx) = unbounded::<DraftState>();

        audio_tx
            .send(vec![0.1f32; FRAME_SAMPLES * script.len()])
            .unwrap();
        drop(audio_tx); // close the channel so run_with_detector returns

        runner
            .run_with_detector(
                Scripted { script, i: 0 },
                audio_rx,
                seg_tx,
                level_tx,
                draft_tx,
                Arc::new(AtomicBool::new(paused)),
            )
            .unwrap();
        seg_rx.try_iter().collect()
    }

    /// min_speech = 3 frames, silence flush = 2 frames, max segment = 10 frames.
    fn test_runner() -> VadRunner {
        VadRunner::new(2, 3 * FRAME_MS, 2 * FRAME_MS, 10 * FRAME_MS)
    }

    #[test]
    fn short_burst_below_min_speech_is_discarded() {
        let mut script = vec![true, true]; // 2 < 3 min_speech frames
        script.extend([false; 4]);
        let segs = run_segmenter(&test_runner(), script, false);
        assert!(segs.is_empty());
    }

    #[test]
    fn speech_then_silence_flushes_one_segment() {
        let mut script = vec![true; 5];
        script.extend([false; 4]);
        let segs = run_segmenter(&test_runner(), script, false);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].speech_ms, 5 * FRAME_MS);
        // Segment includes the trailing silence frames buffered before flush.
        assert_eq!(segs[0].samples.len(), 7 * FRAME_SAMPLES);
    }

    #[test]
    fn long_speech_is_force_flushed_at_max_segment() {
        // 12 frames of continuous speech with a 10-frame cap: force flush at
        // 10, and the 2-frame tail (below min_speech) never flushes.
        let segs = run_segmenter(&test_runner(), vec![true; 12], false);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].speech_ms, 10 * FRAME_MS);
        assert_eq!(segs[0].samples.len(), 10 * FRAME_SAMPLES);
    }

    #[test]
    fn two_utterances_produce_two_segments() {
        let mut script = vec![true; 4];
        script.extend([false; 3]);
        script.extend([true; 4]);
        script.extend([false; 3]);
        let segs = run_segmenter(&test_runner(), script, false);
        assert_eq!(segs.len(), 2);
        assert!(segs[1].id > segs[0].id);
    }

    #[test]
    fn paused_discards_everything() {
        let segs = run_segmenter(&test_runner(), vec![true; 12], true);
        assert!(segs.is_empty());
    }
}
