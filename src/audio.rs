use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use crossbeam_channel::Sender;
use rubato::{FftFixedInOut, Resampler};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

pub const TARGET_SR: u32 = 16_000;

/// Prefix used on loopback (system-audio) sources so we can distinguish them
/// from real input devices that happen to share the same name.
pub const LOOPBACK_PREFIX: &str = "loopback:";

/// Cap the secondary buffer at 1 second of 16kHz mono so a drifting or
/// silent secondary stream can't grow unboundedly.
const SECONDARY_CAP_SAMPLES: usize = TARGET_SR as usize;

/// Best-effort auto-detection of a system-audio source for the current OS.
///
/// - Windows: the default output device, accessed via WASAPI loopback.
/// - macOS: an installed virtual driver (BlackHole / Soundflower / Loopback
///   Audio / VB-Cable) exposed as a regular input device.
/// - Linux (PulseAudio/PipeWire): the first `*.monitor` input source.
///
/// Returns the device string ready to pass into [`AudioCapture::start_dual`],
/// or `None` if nothing suitable was found.
pub fn detect_system_audio_source() -> Option<String> {
    let host = cpal::default_host();

    #[cfg(target_os = "windows")]
    {
        use cpal::traits::DeviceTrait;
        if let Some(dev) = host.default_output_device() {
            if let Ok(name) = dev.name() {
                return Some(format!("{}{}", LOOPBACK_PREFIX, name));
            }
        }
    }

    let input_names: Vec<String> = host
        .input_devices()
        .ok()?
        .filter_map(|d| d.name().ok())
        .collect();

    #[cfg(target_os = "macos")]
    {
        const NEEDLES: &[&str] = &["blackhole", "soundflower", "loopback audio", "vb-cable"];
        for needle in NEEDLES {
            if let Some(n) = input_names
                .iter()
                .find(|n| n.to_lowercase().contains(needle))
            {
                return Some(n.clone());
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(n) = input_names.iter().find(|n| n.ends_with(".monitor")) {
            return Some(n.clone());
        }
    }

    let _ = input_names;
    None
}

/// Splits enumerated sources into mic-style and loopback-style lists, with
/// a leading "(none)" entry so the user can disable a slot in the picker.
/// The system-audio list also includes "(auto)" so the user can opt in to
/// OS-appropriate auto-detection without knowing the exact device name.
///
/// On Windows, output devices can be opened as input via WASAPI loopback.
/// On macOS/Linux, loopback entries are enumerated too, but opening them
/// will fail unless a virtual driver (e.g. BlackHole) is in use — in that
/// case the virtual driver already appears as a regular input device.
pub fn list_devices_split() -> (Vec<String>, Vec<String>) {
    let mut mic = vec!["(none)".to_string(), "default".to_string()];
    let mut sys = vec!["(none)".to_string(), "(auto)".to_string()];
    let host = cpal::default_host();
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Ok(name) = d.name() {
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
            if let Ok(name) = d.name() {
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
        "system-audio auto-detect failed: no virtual audio driver found. \
         Install BlackHole (https://github.com/ExistentialAudio/BlackHole) \
         and route system output through it, then retry."
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

        // Resolve "(auto)" to a concrete device name now, so the rest of
        // the function deals only with explicit names. A failed resolution
        // produces an actionable error instead of silently disabling.
        let resolved_secondary: Option<String> = match secondary {
            Some(s) if s == "(auto)" || s == "auto" => match detect_system_audio_source() {
                Some(name) => {
                    info!(detected = %name, "auto-detected system-audio source");
                    Some(name)
                }
                None => {
                    return Err(anyhow!(auto_detect_error_message()));
                }
            },
            Some(s) if !s.is_empty() && s != "(none)" => Some(s.to_string()),
            _ => None,
        };
        let secondary_active = resolved_secondary.is_some();

        // Single-source fast path: no mixer, no extra buffer.
        if !secondary_active {
            if !primary_active {
                return Err(anyhow!("no audio source selected"));
            }
            let (stream, name) = build_stream(primary, Sink::Direct(tx))?;
            return Ok(Self {
                _streams: vec![stream],
                input_name: name,
            });
        }

        let secondary_name = resolved_secondary.unwrap();
        let secondary_buf: Arc<Mutex<VecDeque<f32>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(SECONDARY_CAP_SAMPLES)));

        // If the user only configured a secondary (uncommon), treat it as
        // the primary and skip the mixer — there is nothing to mix against.
        if !primary_active {
            let (stream, name) = build_stream(&secondary_name, Sink::Direct(tx))?;
            return Ok(Self {
                _streams: vec![stream],
                input_name: name,
            });
        }

        let (primary_stream, primary_name) = build_stream(
            primary,
            Sink::Primary {
                tx,
                secondary: secondary_buf.clone(),
            },
        )?;
        let (secondary_stream, secondary_label) = build_stream(
            &secondary_name,
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
    fn emit(&self, mut chunk: Vec<f32>) {
        match self {
            Sink::Direct(tx) => {
                let _ = tx.try_send(chunk);
            }
            Sink::Primary { tx, secondary } => {
                if let Ok(mut buf) = secondary.lock() {
                    for sample in chunk.iter_mut() {
                        if let Some(s) = buf.pop_front() {
                            *sample += s;
                        }
                        // else: secondary is dry — leave primary sample as-is.
                    }
                }
                let _ = tx.try_send(chunk);
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
            }
        }
    }
}

fn build_stream(device_name: &str, sink: Sink) -> Result<(Stream, String)> {
    let host = cpal::default_host();
    let (device, is_loopback) = if let Some(name) = device_name.strip_prefix(LOOPBACK_PREFIX) {
        let dev = host
            .output_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| anyhow!("output device not found: {}", name))?;
        if !cfg!(target_os = "windows") {
            warn!(
                device = %name,
                "loopback capture is only natively supported on Windows \
                 (WASAPI). On macOS, install a virtual audio driver \
                 (e.g. BlackHole) and select it as a regular input device. \
                 On Linux (PulseAudio), select the *.monitor source from \
                 the input list."
            );
        }
        (dev, true)
    } else if device_name == "default" {
        (
            host.default_input_device()
                .ok_or_else(|| anyhow!("no default input device"))?,
            false,
        )
    } else {
        let dev = host
            .input_devices()?
            .find(|d| d.name().map(|n| n == device_name).unwrap_or(false))
            .ok_or_else(|| anyhow!("input device not found: {}", device_name))?;
        (dev, false)
    };

    open_stream(device, is_loopback, sink)
}

fn open_stream(device: Device, is_loopback: bool, sink: Sink) -> Result<(Stream, String)> {
    let input_name = if is_loopback {
        format!(
            "{}{}",
            LOOPBACK_PREFIX,
            device.name().unwrap_or_else(|_| "unknown".into())
        )
    } else {
        device.name().unwrap_or_else(|_| "unknown".into())
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
    let input_sr = config.sample_rate.0;
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
                &config,
                move |data: &[f32], _| state.push(data),
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let mut state = ProcState::new(input_channels as usize, input_sr, sink)?;
            device.build_input_stream(
                &config,
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
                &config,
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
        })
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

        if let Some(resampler) = self.resampler.as_mut() {
            let in_size = resampler.input_frames_next();
            while self.mono_buf.len() >= in_size {
                let input: Vec<f32> = self.mono_buf.drain(..in_size).collect();
                match resampler.process(&[input], None) {
                    Ok(mut out) => {
                        if let Some(first) = out.pop() {
                            if !first.is_empty() {
                                self.sink.emit(first);
                            }
                        }
                    }
                    Err(e) => warn!("resample error: {}", e),
                }
            }
        } else if !self.mono_buf.is_empty() {
            let chunk: Vec<f32> = std::mem::take(&mut self.mono_buf);
            self.sink.emit(chunk);
        }
    }
}
