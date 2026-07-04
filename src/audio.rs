use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use crossbeam_channel::Sender;
use rubato::{FftFixedInOut, Resampler};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

pub const TARGET_SR: u32 = 16_000;

/// Human-readable device name (cpal 0.18 moved this into `description()`).
fn device_name(d: &Device) -> Option<String> {
    d.description().ok().map(|desc| desc.name().to_string())
}

/// The audio host to enumerate and open devices on.
///
/// On Linux the default host is ALSA, which cannot see the `*.monitor`
/// sources needed for system-audio capture — prefer the PulseAudio host
/// (a pure-Rust protocol client, also served by pipewire-pulse on PipeWire
/// systems) when a server is actually reachable, falling back to ALSA.
/// Everywhere else the default host is the right one (WASAPI / Core Audio).
fn best_host() -> cpal::Host {
    #[cfg(target_os = "linux")]
    if let Ok(host) = cpal::host_from_id(cpal::HostId::PulseAudio) {
        let usable = host
            .input_devices()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
        if usable {
            return host;
        }
    }
    cpal::default_host()
}

/// Prefix used on loopback (system-audio) sources so we can distinguish them
/// from real input devices that happen to share the same name.
pub const LOOPBACK_PREFIX: &str = "loopback:";

/// Cap the secondary buffer at 1 second of 16kHz mono so a drifting or
/// silent secondary stream can't grow unboundedly.
const SECONDARY_CAP_SAMPLES: usize = TARGET_SR as usize;

/// Best-effort auto-detection of system-audio sources for the current OS,
/// ordered by preference. The caller tries them in order — the preferred
/// native path can fail at open time (permission denied, macOS < 14.2) while
/// a later fallback still works.
///
/// - Windows: the default output device via WASAPI loopback.
/// - macOS: the default output device via a Core Audio process tap
///   (cpal 0.18, macOS 14.2+), then an installed virtual driver
///   (BlackHole / Soundflower / Loopback Audio / VB-Cable) as fallback.
/// - Linux (PipeWire/PulseAudio host): every `*.monitor` input source.
pub fn detect_system_audio_candidates() -> Vec<String> {
    let host = best_host();
    let mut out: Vec<String> = Vec::new();

    // Native loopback: tap the default output device.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if let Some(name) = host.default_output_device().as_ref().and_then(device_name) {
        out.push(format!("{}{}", LOOPBACK_PREFIX, name));
    }

    let input_names: Vec<String> = host
        .input_devices()
        .map(|devs| devs.filter_map(|d| device_name(&d)).collect())
        .unwrap_or_default();

    #[cfg(target_os = "macos")]
    {
        const NEEDLES: &[&str] = &["blackhole", "soundflower", "loopback audio", "vb-cable"];
        for needle in NEEDLES {
            if let Some(n) = input_names
                .iter()
                .find(|n| n.to_lowercase().contains(needle))
            {
                out.push(n.clone());
            }
        }
    }

    #[cfg(target_os = "linux")]
    for n in input_names.iter().filter(|n| n.ends_with(".monitor")) {
        out.push(n.clone());
    }

    let _ = input_names;
    out
}

/// Splits enumerated sources into mic-style and loopback-style lists, with
/// a leading "(none)" entry so the user can disable a slot in the picker.
/// The system-audio list also includes "(auto)" so the user can opt in to
/// OS-appropriate auto-detection without knowing the exact device name.
///
/// On Windows (WASAPI) and macOS 14.2+ (Core Audio process taps), output
/// devices can be opened as loopback inputs directly. On Linux, loopback
/// entries won't open — the PipeWire/PulseAudio `*.monitor` sources in the
/// input list are the way to capture system audio there.
pub fn list_devices_split() -> (Vec<String>, Vec<String>) {
    let mut mic = vec!["(none)".to_string(), "default".to_string()];
    let mut sys = vec!["(none)".to_string(), "(auto)".to_string()];
    let host = best_host();
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Some(name) = device_name(&d) {
                if !mic.iter().any(|n| n == &name) {
                    mic.push(name.clone());
                }
                // Also offer input devices in the system-audio column so
                // macOS users can pick BlackHole (input) and Linux users
                // can pick `*.monitor` sources explicitly.
                if !sys.iter().any(|n| n == &name) {
                    sys.push(name);
                }
            }
        }
    }
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            if let Some(name) = device_name(&d) {
                let labeled = format!("{}{}", LOOPBACK_PREFIX, name);
                if !sys.iter().any(|n| n == &labeled) {
                    sys.push(labeled);
                }
            }
        }
    }
    (mic, sys)
}

fn auto_detect_error_message() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "system-audio auto-detect failed: no default output device. \
         Pick `[loopback] <speaker name>` explicitly from the picker."
    }
    #[cfg(target_os = "macos")]
    {
        "system-audio auto-detect failed: no default output device to tap \
         and no virtual audio driver (e.g. BlackHole) installed."
    }
    #[cfg(target_os = "linux")]
    {
        "system-audio auto-detect failed: no PulseAudio/PipeWire `*.monitor` \
         source visible. Check that PulseAudio/PipeWire is running, or pick a \
         monitor source explicitly from the picker."
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "system-audio auto-detect is not supported on this platform — \
         pick a device explicitly from the picker."
    }
}

pub struct AudioCapture {
    _streams: Vec<Stream>,
    pub input_name: String,
}

impl AudioCapture {
    /// Start capture from up to two sources, mixing them sample-by-sample at
    /// 16 kHz mono. The primary source drives the output cadence; secondary
    /// samples are pulled from a 1-second ring buffer (zero-padded if short,
    /// oldest dropped if the buffer overflows due to clock drift).
    ///
    /// Pass `"(none)"` or `""` for `primary` to capture only the secondary
    /// source (uncommon — usually mic is primary).
    pub fn start_dual(
        primary: &str,
        secondary: Option<&str>,
        tx: Sender<Vec<f32>>,
    ) -> Result<Self> {
        let primary_active = !matches!(primary, "" | "(none)");

        // Resolve "(auto)" into an ordered candidate list now; explicit
        // names become a single-entry list. Candidates are tried at open
        // time because the preferred native path can fail (permissions,
        // macOS < 14.2) while a fallback still works.
        let secondary_candidates: Vec<String> = match secondary {
            Some(s) if s == "(auto)" || s == "auto" => {
                let cands = detect_system_audio_candidates();
                if cands.is_empty() {
                    return Err(anyhow!(auto_detect_error_message()));
                }
                info!(candidates = ?cands, "auto-detected system-audio candidates");
                cands
            }
            Some(s) if !s.is_empty() && s != "(none)" => vec![s.to_string()],
            _ => Vec::new(),
        };

        // Single-source fast path: no mixer, no extra buffer.
        if secondary_candidates.is_empty() {
            if !primary_active {
                return Err(anyhow!("no audio source selected"));
            }
            let (stream, name) = build_stream(primary, Sink::Direct(tx))?;
            return Ok(Self {
                _streams: vec![stream],
                input_name: name,
            });
        }

        // If the user only configured a secondary (uncommon), treat it as
        // the primary and skip the mixer — there is nothing to mix against.
        if !primary_active {
            let (stream, name) =
                build_secondary_stream(&secondary_candidates, Sink::Direct(tx))?;
            return Ok(Self {
                _streams: vec![stream],
                input_name: name,
            });
        }

        let secondary_buf: Arc<Mutex<VecDeque<f32>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(SECONDARY_CAP_SAMPLES)));
        let (primary_stream, primary_name) = build_stream(
            primary,
            Sink::Primary {
                tx,
                secondary: secondary_buf.clone(),
            },
        )?;
        let (secondary_stream, secondary_label) = build_secondary_stream(
            &secondary_candidates,
            Sink::Secondary {
                buf: secondary_buf,
            },
        )?;

        Ok(Self {
            _streams: vec![primary_stream, secondary_stream],
            input_name: format!("{} + {}", primary_name, secondary_label),
        })
    }
}

/// What a `ProcState` does with each batch of 16 kHz mono samples it produces.
#[derive(Clone)]
enum Sink {
    /// Single-source mode: push directly to the consumer channel.
    Direct(Sender<Vec<f32>>),
    /// Mix-mode primary: pull the same number of samples from `secondary`,
    /// sum into ours, then push to `tx`.
    Primary {
        tx: Sender<Vec<f32>>,
        secondary: Arc<Mutex<VecDeque<f32>>>,
    },
    /// Mix-mode secondary: append to the shared ring buffer.
    Secondary { buf: Arc<Mutex<VecDeque<f32>>> },
}

impl Sink {
    /// Returns true if the chunk was dropped because the consumer channel is
    /// full (the pipeline is backed up).
    fn emit(&self, mut chunk: Vec<f32>) -> bool {
        match self {
            Sink::Direct(tx) => tx.try_send(chunk).is_err(),
            Sink::Primary { tx, secondary } => {
                if let Ok(mut buf) = secondary.lock() {
                    for sample in chunk.iter_mut() {
                        if let Some(s) = buf.pop_front() {
                            // Clamp the sum: two hot sources can exceed ±1.0
                            // and feed clipped garbage into whisper.
                            *sample = (*sample + s).clamp(-1.0, 1.0);
                        }
                        // else: secondary is dry — leave primary sample as-is.
                    }
                }
                tx.try_send(chunk).is_err()
            }
            Sink::Secondary { buf } => {
                if let Ok(mut buf) = buf.lock() {
                    buf.extend(chunk.iter().copied());
                    // Drop oldest if we've exceeded the cap, so a drifting
                    // secondary clock can't grow the buffer unboundedly.
                    while buf.len() > SECONDARY_CAP_SAMPLES {
                        buf.pop_front();
                    }
                }
                false
            }
        }
    }
}

/// Try system-audio candidates in order until one opens.
fn build_secondary_stream(candidates: &[String], sink: Sink) -> Result<(Stream, String)> {
    let mut last_err: Option<anyhow::Error> = None;
    for cand in candidates {
        match build_stream(cand, sink.clone()) {
            Ok(ok) => {
                if last_err.is_some() {
                    info!(chosen = %cand, "system-audio fallback source opened");
                }
                return Ok(ok);
            }
            Err(e) => {
                warn!(candidate = %cand, error = %e, "system-audio candidate failed to open");
                last_err = Some(e);
            }
        }
    }
    let base = last_err.unwrap_or_else(|| anyhow!(auto_detect_error_message()));
    if cfg!(target_os = "macos") {
        Err(base.context(
            "opening system audio failed — on macOS 14.2+ grant your terminal app \
             permission under System Settings → Privacy & Security → Screen & \
             System Audio Recording; on older macOS install BlackHole \
             (https://github.com/ExistentialAudio/BlackHole)",
        ))
    } else {
        Err(base)
    }
}

fn build_stream(wanted: &str, sink: Sink) -> Result<(Stream, String)> {
    let host = best_host();
    let (device, is_loopback) = if let Some(name) = wanted.strip_prefix(LOOPBACK_PREFIX) {
        let dev = host
            .output_devices()?
            .find(|d| device_name(d).is_some_and(|n| n == name))
            .ok_or_else(|| anyhow!("output device not found: {}", name))?;
        if cfg!(target_os = "linux") {
            warn!(
                device = %name,
                "loopback entries are not natively supported on Linux — \
                 select the corresponding *.monitor source from the input \
                 list instead."
            );
        }
        (dev, true)
    } else if wanted == "default" {
        (
            host.default_input_device()
                .ok_or_else(|| anyhow!("no default input device"))?,
            false,
        )
    } else {
        let dev = host
            .input_devices()?
            .find(|d| device_name(d).is_some_and(|n| n == wanted))
            .ok_or_else(|| anyhow!("input device not found: {}", wanted))?;
        (dev, false)
    };

    open_stream(device, is_loopback, sink)
}

fn open_stream(device: Device, is_loopback: bool, sink: Sink) -> Result<(Stream, String)> {
    let bare_name = device_name(&device).unwrap_or_else(|| "unknown".into());
    let input_name = if is_loopback {
        format!("{}{}", LOOPBACK_PREFIX, bare_name)
    } else {
        bare_name
    };
    let supported = if is_loopback {
        device
            .default_output_config()
            .context("default output config (loopback)")?
    } else {
        device
            .default_input_config()
            .context("default input config")?
    };
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let input_sr = config.sample_rate;
    let input_channels = config.channels;

    info!(
        %input_name,
        input_sr,
        input_channels,
        ?sample_format,
        is_loopback,
        "opening input stream"
    );

    let err_fn = |e| warn!("audio stream error: {}", e);

    let stream = match sample_format {
        SampleFormat::F32 => {
            let mut state = ProcState::new(input_channels as usize, input_sr, sink)?;
            device.build_input_stream(
                config,
                move |data: &[f32], _| state.push(data),
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let mut state = ProcState::new(input_channels as usize, input_sr, sink)?;
            device.build_input_stream(
                config,
                move |data: &[i16], _| {
                    let f: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                    state.push(&f);
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let mut state = ProcState::new(input_channels as usize, input_sr, sink)?;
            device.build_input_stream(
                config,
                move |data: &[u16], _| {
                    let f: Vec<f32> = data
                        .iter()
                        .map(|s| (*s as f32 - 32768.0) / 32768.0)
                        .collect();
                    state.push(&f);
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported sample format: {:?}", other)),
    };

    stream.play()?;
    Ok((stream, input_name))
}

struct ProcState {
    channels: usize,
    mono_buf: Vec<f32>,
    resampler: Option<FftFixedInOut<f32>>,
    sink: Sink,
    /// Chunks silently discarded because the consumer channel was full.
    dropped_chunks: u32,
}

impl ProcState {
    fn new(channels: usize, input_sr: u32, sink: Sink) -> Result<Self> {
        let resampler = if input_sr != TARGET_SR {
            Some(FftFixedInOut::<f32>::new(
                input_sr as usize,
                TARGET_SR as usize,
                1024,
                1,
            )?)
        } else {
            None
        };
        Ok(Self {
            channels,
            mono_buf: Vec::with_capacity(8192),
            resampler,
            sink,
            dropped_chunks: 0,
        })
    }

    /// Record dropped chunks and warn, rate-limited so a stalled pipeline
    /// doesn't flood the log.
    fn note_dropped(&mut self, n: u32) {
        let before = self.dropped_chunks;
        self.dropped_chunks += n;
        if before == 0 || self.dropped_chunks / 100 > before / 100 {
            warn!(
                dropped = self.dropped_chunks,
                "audio chunks dropped — pipeline backed up"
            );
        }
    }

    fn push(&mut self, data: &[f32]) {
        if self.channels <= 1 {
            self.mono_buf.extend_from_slice(data);
        } else {
            for frame in data.chunks_exact(self.channels) {
                let s: f32 = frame.iter().sum::<f32>() / self.channels as f32;
                self.mono_buf.push(s);
            }
        }

        let mut newly_dropped = 0u32;
        if let Some(resampler) = self.resampler.as_mut() {
            let in_size = resampler.input_frames_next();
            while self.mono_buf.len() >= in_size {
                let input: Vec<f32> = self.mono_buf.drain(..in_size).collect();
                match resampler.process(&[input], None) {
                    Ok(mut out) => {
                        if let Some(first) = out.pop() {
                            if !first.is_empty() && self.sink.emit(first) {
                                newly_dropped += 1;
                            }
                        }
                    }
                    Err(e) => warn!("resample error: {}", e),
                }
            }
        } else if !self.mono_buf.is_empty() {
            let chunk: Vec<f32> = std::mem::take(&mut self.mono_buf);
            if self.sink.emit(chunk) {
                newly_dropped += 1;
            }
        }
        if newly_dropped > 0 {
            self.note_dropped(newly_dropped);
        }
    }
}
