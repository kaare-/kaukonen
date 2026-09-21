//! Real hardware, via cpal. Only compiled with the `realtime` feature.
//!
//! Everything here exists to get a patch out of a socket and into an audio
//! interface, and to leave enough adjustable while it runs that a first session
//! with unfamiliar hardware is tunable rather than a guess: which device, which
//! channels of it, what rate, how big a block, and every knob in the rack.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, StreamConfig};

use crate::stream::Stream as Rack;

/// How long a fade takes when a stream starts or stops.
///
/// Interfaces are often plugged into speakers, and a rack that begins at full
/// level puts a step through them.
const FADE_SECONDS: f32 = 0.05;

/// Seconds of recording the audio thread may stage before the writer drains it.
const RECORD_STAGE_SECONDS: f32 = 2.0;

/// What a device can do, flattened for printing.
#[derive(Clone, Debug)]
pub struct DeviceReport {
    pub index: usize,
    pub name: String,
    pub is_default: bool,
    pub channels: Vec<u16>,
    pub rates: Vec<u32>,
    pub formats: Vec<String>,
    pub buffer_range: Option<(u32, u32)>,
}

/// Which way a device is being used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// Everything about the host's audio setup, for `kaukonen devices`.
pub fn survey(direction: Direction) -> Result<Vec<DeviceReport>, String> {
    let host = cpal::default_host();
    let default_name = match direction {
        Direction::Input => host.default_input_device().and_then(|d| d.name().ok()),
        Direction::Output => host.default_output_device().and_then(|d| d.name().ok()),
    };
    let devices = match direction {
        Direction::Input => host.input_devices(),
        Direction::Output => host.output_devices(),
    }
    .map_err(|e| format!("cannot list {} devices: {e}", direction.label()))?;

    let mut reports = Vec::new();
    for (index, device) in devices.enumerate() {
        let name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());
        let configs = match direction {
            Direction::Input => device.supported_input_configs().map(|c| c.collect()),
            Direction::Output => device.supported_output_configs().map(|c| c.collect()),
        };
        let configs: Vec<_> = configs.unwrap_or_default();

        let mut channels: Vec<u16> = configs.iter().map(|c| c.channels()).collect();
        channels.sort_unstable();
        channels.dedup();

        // Report the standard rates a device actually covers, rather than the
        // raw ranges, which are unreadable.
        let mut rates = Vec::new();
        for candidate in [44100, 48000, 88200, 96000, 176400, 192000] {
            if configs
                .iter()
                .any(|c| c.min_sample_rate().0 <= candidate && candidate <= c.max_sample_rate().0)
            {
                rates.push(candidate);
            }
        }
        let mut formats: Vec<String> = configs
            .iter()
            .map(|c| format!("{:?}", c.sample_format()))
            .collect();
        formats.sort();
        formats.dedup();

        let buffer_range = configs.iter().find_map(|c| match c.buffer_size() {
            cpal::SupportedBufferSize::Range { min, max } => Some((*min, *max)),
            cpal::SupportedBufferSize::Unknown => None,
        });

        reports.push(DeviceReport {
            index,
            is_default: default_name.as_deref() == Some(name.as_str()),
            name,
            channels,
            rates,
            formats,
            buffer_range,
        });
    }
    Ok(reports)
}

/// Find a device by index, by a substring of its name, or take the default.
///
/// Substring matching is deliberate: interface names are long and inconsistent
/// across drivers, so `--device scarlett` should be enough.
pub fn find_device(direction: Direction, wanted: Option<&str>) -> Result<Device, String> {
    let host = cpal::default_host();
    let Some(wanted) = wanted else {
        return match direction {
            Direction::Input => host.default_input_device(),
            Direction::Output => host.default_output_device(),
        }
        .ok_or_else(|| {
            format!(
                "no default {} device. Run `kaukonen devices` to see what is available.",
                direction.label()
            )
        });
    };

    let devices: Vec<Device> = match direction {
        Direction::Input => host.input_devices(),
        Direction::Output => host.output_devices(),
    }
    .map_err(|e| format!("cannot list {} devices: {e}", direction.label()))?
    .collect();

    if let Ok(index) = wanted.parse::<usize>() {
        return devices.into_iter().nth(index).ok_or_else(|| {
            format!(
                "there is no {} device {index}. Run `kaukonen devices`.",
                direction.label()
            )
        });
    }

    let needle = wanted.to_lowercase();
    let mut matches: Vec<Device> = devices
        .into_iter()
        .filter(|d| {
            d.name()
                .map(|n| n.to_lowercase().contains(&needle))
                .unwrap_or(false)
        })
        .collect();
    match matches.len() {
        0 => Err(format!(
            "no {} device matches '{wanted}'. Run `kaukonen devices`.",
            direction.label()
        )),
        1 => Ok(matches.remove(0)),
        _ => {
            let names: Vec<String> = matches
                .iter()
                .map(|d| d.name().unwrap_or_default())
                .collect();
            Err(format!(
                "'{wanted}' matches {} {} devices: {}. Be more specific.",
                names.len(),
                direction.label(),
                names.join(", ")
            ))
        }
    }
}

/// What a stream ended up running at, after the device had its say.
#[derive(Clone, Debug)]
pub struct Negotiated {
    pub config: StreamConfig,
    pub format: SampleFormat,
    pub device_name: String,
    /// Set when the device refused the rate that was asked for.
    pub rate_changed_from: Option<u32>,
}

impl Negotiated {
    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate.0
    }

    pub fn channels(&self) -> usize {
        self.config.channels as usize
    }

    /// Frames per callback, when the device accepted a fixed size.
    pub fn block(&self) -> Option<u32> {
        match self.config.buffer_size {
            cpal::BufferSize::Fixed(frames) => Some(frames),
            cpal::BufferSize::Default => None,
        }
    }

    pub fn latency_ms(&self) -> Option<f32> {
        self.block()
            .map(|frames| frames as f32 / self.sample_rate() as f32 * 1000.0)
    }
}

/// Pick a configuration the device will accept, preferring what was asked for.
pub fn negotiate(
    device: &Device,
    direction: Direction,
    wanted_rate: Option<u32>,
    wanted_block: Option<u32>,
    wanted_channels: Option<u16>,
) -> Result<Negotiated, String> {
    let device_name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());
    let supported: Vec<_> = match direction {
        Direction::Input => device.supported_input_configs().map(|c| c.collect()),
        Direction::Output => device.supported_output_configs().map(|c| c.collect()),
    }
    .map_err(|e| format!("{device_name}: cannot read supported configs: {e}"))?;
    if supported.is_empty() {
        return Err(format!(
            "{device_name} reports no usable {} configurations",
            direction.label()
        ));
    }

    let default = match direction {
        Direction::Input => device.default_input_config(),
        Direction::Output => device.default_output_config(),
    }
    .map_err(|e| format!("{device_name}: no default config: {e}"))?;

    let target_rate = wanted_rate.unwrap_or(default.sample_rate().0);
    let target_channels = wanted_channels.unwrap_or(default.channels());

    // A device — especially one behind ALSA's plug layer — offers far more
    // formats than are worth using, so pick deliberately rather than taking
    // whatever is listed first.
    let usable: Vec<_> = supported
        .iter()
        .filter(|c| matches!(c.sample_format(), SampleFormat::F32 | SampleFormat::I16))
        .cloned()
        .collect();
    if usable.is_empty() {
        let offered: Vec<String> = {
            let mut names: Vec<String> = supported
                .iter()
                .map(|c| format!("{:?}", c.sample_format()))
                .collect();
            names.sort();
            names.dedup();
            names
        };
        return Err(format!(
            "{device_name} offers only {} samples, and this handles f32 and i16. \
             If the device can do either, ask for it explicitly in its driver settings.",
            offered.join(", ")
        ));
    }

    let supports = |c: &cpal::SupportedStreamConfigRange, rate: u32| {
        c.min_sample_rate().0 <= rate && rate <= c.max_sample_rate().0
    };
    // Lower is better: float over integer, the asked-for channel count over
    // anything else, and as few channels as will do.
    let score = |c: &cpal::SupportedStreamConfigRange| {
        let format_rank = if c.sample_format() == SampleFormat::F32 {
            0
        } else {
            1
        };
        let channel_rank = if c.channels() == target_channels {
            0
        } else {
            1
        };
        (format_rank, channel_rank, c.channels())
    };

    let exact: Vec<_> = usable
        .iter()
        .filter(|c| supports(c, target_rate))
        .cloned()
        .collect();

    let (range, rate, rate_changed_from) =
        if let Some(best) = exact.iter().min_by_key(|c| score(c)).cloned() {
            (best, target_rate, None)
        } else {
            // Nothing runs at the rate asked for, so take the nearest instead of
            // refusing the device outright.
            let best = usable
                .iter()
                .min_by_key(|c| {
                    let clamped = target_rate.clamp(c.min_sample_rate().0, c.max_sample_rate().0);
                    (clamped.abs_diff(target_rate), score(c))
                })
                .cloned()
                .ok_or_else(|| format!("{device_name} has no usable configuration"))?;
            let rate = target_rate.clamp(best.min_sample_rate().0, best.max_sample_rate().0);
            (best, rate, Some(target_rate))
        };

    let format = range.sample_format();

    let buffer_size = match (wanted_block, range.buffer_size()) {
        (Some(frames), cpal::SupportedBufferSize::Range { min, max }) => {
            cpal::BufferSize::Fixed(frames.clamp(*min, *max))
        }
        (Some(frames), cpal::SupportedBufferSize::Unknown) => cpal::BufferSize::Fixed(frames),
        (None, _) => cpal::BufferSize::Default,
    };

    Ok(Negotiated {
        config: StreamConfig {
            channels: range.channels(),
            sample_rate: cpal::SampleRate(rate),
            buffer_size,
        },
        format,
        device_name,
        rate_changed_from,
    })
}

/// Where the rack's two output channels land on the device.
#[derive(Clone, Copy, Debug)]
pub struct Routing {
    pub left: usize,
    pub right: usize,
    /// Which device input channel feeds the rack.
    pub input: usize,
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            left: 0,
            right: 1,
            input: 0,
        }
    }
}

/// Counters the audio thread publishes for the console to read.
#[derive(Debug, Default)]
pub struct Stats {
    pub blocks: AtomicU64,
    /// Callbacks that took longer than their own deadline. Each one is a
    /// dropout waiting to happen.
    pub late: AtomicU64,
    pub worst_micros: AtomicU64,
    /// Output peak since the counter was last read, as f32 bits.
    peak_bits: AtomicU32,
    pub input_starved: AtomicU64,
    pub record_dropped: AtomicU64,
}

impl Stats {
    fn observe_peak(&self, value: f32) {
        let mut current = f32::from_bits(self.peak_bits.load(Ordering::Relaxed));
        if value <= current {
            return;
        }
        // Only the audio thread writes, so a plain store is enough.
        current = value;
        self.peak_bits.store(current.to_bits(), Ordering::Relaxed);
    }

    /// Read and reset the peak, so each report covers one interval.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak_bits.swap(0.0f32.to_bits(), Ordering::Relaxed))
    }
}

/// A change the console asks the audio thread to make.
#[derive(Clone, Copy, Debug)]
pub enum Control {
    /// Set one knob. The name is `'static` because it comes from a ParamSpec,
    /// which keeps the message allocation-free.
    Knob {
        module: usize,
        knob: &'static str,
        value: f32,
    },
    /// Master output trim.
    Gain(f32),
    /// Fade to a level over [`FADE_SECONDS`], then optionally stop.
    Fade { to: f32, then_stop: bool },
}

/// Lock-free single-producer, single-consumer ring, for input to output.
///
/// cpal's input and output are separate streams with separate clocks, so
/// something has to sit between them. Overflow drops the oldest audio and
/// underflow inserts silence; both are counted rather than hidden.
pub struct Ring {
    slots: Vec<AtomicU32>,
    read: AtomicUsize,
    write: AtomicUsize,
    overflowed: AtomicU64,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self {
            slots: (0..capacity.max(2)).map(|_| AtomicU32::new(0)).collect(),
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
            overflowed: AtomicU64::new(0),
        }
    }

    pub fn push(&self, value: f32) {
        let write = self.write.load(Ordering::Relaxed);
        let next = (write + 1) % self.slots.len();
        if next == self.read.load(Ordering::Acquire) {
            self.overflowed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.slots[write].store(value.to_bits(), Ordering::Relaxed);
        self.write.store(next, Ordering::Release);
    }

    pub fn pop(&self) -> Option<f32> {
        let read = self.read.load(Ordering::Relaxed);
        if read == self.write.load(Ordering::Acquire) {
            return None;
        }
        let value = f32::from_bits(self.slots[read].load(Ordering::Relaxed));
        self.read
            .store((read + 1) % self.slots.len(), Ordering::Release);
        Some(value)
    }

    pub fn overflowed(&self) -> u64 {
        self.overflowed.load(Ordering::Relaxed)
    }

    /// Samples waiting to be read.
    pub fn len(&self) -> usize {
        let write = self.write.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Relaxed);
        (write + self.slots.len() - read) % self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Fixed-capacity staging area for recording, drained by the writer thread.
///
/// Fixed so the audio thread never allocates. If the writer falls behind, audio
/// is dropped from the recording and counted — never from the output.
pub struct RecordStage {
    pub samples: Mutex<Vec<f32>>,
    pub capacity: usize,
}

/// A running stream and the handles for talking to it.
pub struct Playback {
    _output: cpal::Stream,
    _input: Option<cpal::Stream>,
    control: Sender<Control>,
    pub stats: Arc<Stats>,
    pub record: Option<Arc<RecordStage>>,
    pub negotiated: Negotiated,
    pub finished: Arc<AtomicBool>,
}

impl Playback {
    pub fn send(&self, control: Control) -> Result<(), String> {
        self.control
            .send(control)
            .map_err(|_| "the audio thread has stopped".to_string())
    }

    /// Fade out and let the tail through, so stopping does not click.
    pub fn fade_out(&self) {
        let _ = self.send(Control::Fade {
            to: 0.0,
            then_stop: true,
        });
        std::thread::sleep(std::time::Duration::from_secs_f32(FADE_SECONDS * 1.5));
    }
}

/// Settings for one live session.
pub struct PlayOptions {
    pub output: Negotiated,
    pub routing: Routing,
    pub gain: f32,
    /// Capture from a device as well as playing.
    pub input: Option<(Device, Negotiated)>,
    pub record: bool,
}

/// Start playing a rack through a device.
pub fn play(mut rack: Rack, device: &Device, options: PlayOptions) -> Result<Playback, String> {
    let negotiated = options.output.clone();
    let channels = negotiated.channels();
    let rate = negotiated.sample_rate() as f32;
    if (rack.sample_rate() - rate).abs() > 0.5 {
        return Err(format!(
            "the rack runs at {} Hz and the device at {rate} Hz. Rebuild the rack at the \
             device's rate rather than resampling.",
            rack.sample_rate()
        ));
    }

    let stats = Arc::new(Stats::default());
    let finished = Arc::new(AtomicBool::new(false));
    let (control_tx, control_rx) = mpsc::channel::<Control>();

    let record = options.record.then(|| {
        let capacity = (rate * RECORD_STAGE_SECONDS) as usize * 2;
        Arc::new(RecordStage {
            samples: Mutex::new(Vec::with_capacity(capacity)),
            capacity,
        })
    });

    // Input, if asked for, arrives through a ring a few blocks deep.
    let ring = options.input.as_ref().map(|(_, input_config)| {
        let depth = input_config.block().unwrap_or(1024) as usize * 8;
        Arc::new(Ring::new(depth.max(4096)))
    });

    let input_stream = match (&options.input, &ring) {
        (Some((input_device, input_config)), Some(ring)) => Some(build_input(
            input_device,
            input_config,
            options.routing.input,
            Arc::clone(ring),
        )?),
        _ => None,
    };

    let mut writer = Writer {
        rack_channels: channels,
        routing: options.routing,
        gain: options.gain,
        fade: 0.0,
        fade_step: 1.0 / (FADE_SECONDS * rate).max(1.0),
        fade_target: 1.0,
        stop_after_fade: false,
        control: control_rx,
        stats: Arc::clone(&stats),
        record: record.clone(),
        ring: ring.clone(),
        finished: Arc::clone(&finished),
        scratch: vec![0.0; 4096],
        captured: vec![0.0; 4096],
    };

    let error_stats = Arc::clone(&stats);
    let on_error = move |err| {
        // An error callback fires off the audio thread, so printing is fine.
        error_stats.late.fetch_add(1, Ordering::Relaxed);
        eprintln!("audio stream error: {err}");
    };

    let output = match negotiated.format {
        SampleFormat::F32 => device.build_output_stream(
            &negotiated.config,
            move |buffer: &mut [f32], _| writer.fill(&mut rack, buffer),
            on_error,
            None,
        ),
        SampleFormat::I16 => {
            let mut staging = vec![0.0f32; 8192];
            device.build_output_stream(
                &negotiated.config,
                move |buffer: &mut [i16], _| {
                    let needed = buffer.len();
                    if staging.len() < needed {
                        // Grows once, on the first callback, then never again.
                        staging.resize(needed, 0.0);
                    }
                    let slice = &mut staging[..needed];
                    writer.fill(&mut rack, slice);
                    for (out, sample) in buffer.iter_mut().zip(slice.iter()) {
                        *out = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                },
                on_error,
                None,
            )
        }
        other => return Err(format!("unhandled sample format {other:?}")),
    }
    .map_err(|e| format!("{}: cannot open the stream: {e}", negotiated.device_name))?;

    output
        .play()
        .map_err(|e| format!("{}: cannot start the stream: {e}", negotiated.device_name))?;
    if let Some(stream) = &input_stream {
        stream
            .play()
            .map_err(|e| format!("cannot start the input stream: {e}"))?;
    }

    Ok(Playback {
        _output: output,
        _input: input_stream,
        control: control_tx,
        stats,
        record,
        negotiated,
        finished,
    })
}

fn build_input(
    device: &Device,
    config: &Negotiated,
    channel: usize,
    ring: Arc<Ring>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels();
    let take = channel.min(channels.saturating_sub(1));
    let on_error = |err| eprintln!("audio input error: {err}");
    let stream = match config.format {
        SampleFormat::F32 => device.build_input_stream(
            &config.config,
            move |buffer: &[f32], _| {
                for frame in buffer.chunks(channels) {
                    ring.push(frame.get(take).copied().unwrap_or(0.0));
                }
            },
            on_error,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &config.config,
            move |buffer: &[i16], _| {
                for frame in buffer.chunks(channels) {
                    let sample = frame.get(take).copied().unwrap_or(0) as f32 / 32768.0;
                    ring.push(sample);
                }
            },
            on_error,
            None,
        ),
        other => return Err(format!("unhandled input sample format {other:?}")),
    }
    .map_err(|e| format!("{}: cannot open the input stream: {e}", config.device_name))?;
    Ok(stream)
}

/// The audio-thread side: drains control messages, runs the rack, fills a block.
struct Writer {
    rack_channels: usize,
    routing: Routing,
    gain: f32,
    fade: f32,
    fade_step: f32,
    fade_target: f32,
    stop_after_fade: bool,
    control: Receiver<Control>,
    stats: Arc<Stats>,
    record: Option<Arc<RecordStage>>,
    ring: Option<Arc<Ring>>,
    finished: Arc<AtomicBool>,
    /// Reused buffer for the rack's own stereo output.
    scratch: Vec<f32>,
    /// Reused buffer for whatever arrived from the input device.
    captured: Vec<f32>,
}

impl Writer {
    fn fill(&mut self, rack: &mut Rack, buffer: &mut [f32]) {
        let started = Instant::now();
        let channels = self.rack_channels.max(1);
        let frames = buffer.len() / channels;

        // Apply anything the console asked for since the last block.
        while let Ok(control) = self.control.try_recv() {
            match control {
                Control::Knob {
                    module,
                    knob,
                    value,
                } => {
                    rack.set_knob(module, knob, value);
                }
                Control::Gain(value) => self.gain = value,
                Control::Fade { to, then_stop } => {
                    self.fade_target = to;
                    self.stop_after_fade = then_stop;
                }
            }
        }

        let needed = frames * 2;
        // Grows once, on the first block a device asks for more than the
        // initial guess, and never again.
        if self.scratch.len() < needed {
            self.scratch.resize(needed, 0.0);
            self.captured.resize(needed, 0.0);
        }

        // Pull input through the ring, padding with silence if it is behind.
        let live = match &self.ring {
            Some(ring) => {
                let mut starved = 0u64;
                for frame in 0..frames {
                    let sample = match ring.pop() {
                        Some(sample) => sample,
                        None => {
                            starved += 1;
                            0.0
                        }
                    };
                    self.captured[frame * 2] = sample;
                    self.captured[frame * 2 + 1] = sample;
                }
                if starved > 0 {
                    self.stats
                        .input_starved
                        .fetch_add(starved, Ordering::Relaxed);
                }
                true
            }
            None => false,
        };

        // Disjoint fields, so the rack can read one buffer and write the other.
        let captured = &self.captured[..needed];
        let stereo = &mut self.scratch[..needed];
        stereo.fill(0.0);
        if live {
            rack.process_duplex(captured, stereo, 2);
        } else {
            rack.process(stereo, 2);
        }

        buffer.fill(0.0);
        let mut peak = 0.0f32;
        for frame in 0..frames {
            // A short fade at each end, so plugging this into speakers does not
            // put a step through them.
            if self.fade < self.fade_target {
                self.fade = (self.fade + self.fade_step).min(self.fade_target);
            } else if self.fade > self.fade_target {
                self.fade = (self.fade - self.fade_step).max(self.fade_target);
            }
            let level = self.gain * self.fade;
            let left = (stereo[frame * 2] * level).clamp(-1.0, 1.0);
            let right = (stereo[frame * 2 + 1] * level).clamp(-1.0, 1.0);
            peak = peak.max(left.abs()).max(right.abs());

            let base = frame * channels;
            if let Some(slot) = buffer.get_mut(base + self.routing.left.min(channels - 1)) {
                *slot = left;
            }
            if channels > 1 {
                if let Some(slot) = buffer.get_mut(base + self.routing.right.min(channels - 1)) {
                    *slot = right;
                }
            }
            // Keep what the device is actually being sent, so a recording shows
            // the master gain, any fade and any clipping rather than the rack's
            // output before all three.
            stereo[frame * 2] = left;
            stereo[frame * 2 + 1] = right;
        }
        self.stats.observe_peak(peak);

        if self.stop_after_fade && self.fade <= 0.0 {
            self.finished.store(true, Ordering::Relaxed);
        }

        // Stage the recording. try_lock so a slow writer can never stall audio.
        if let Some(record) = &self.record {
            if let Ok(mut staged) = record.samples.try_lock() {
                if staged.len() + needed <= record.capacity {
                    staged.extend_from_slice(&stereo[..needed]);
                } else {
                    self.stats
                        .record_dropped
                        .fetch_add(frames as u64, Ordering::Relaxed);
                }
            } else {
                self.stats
                    .record_dropped
                    .fetch_add(frames as u64, Ordering::Relaxed);
            }
        }

        let taken = started.elapsed();
        let micros = taken.as_micros() as u64;
        self.stats.blocks.fetch_add(1, Ordering::Relaxed);
        self.stats.worst_micros.fetch_max(micros, Ordering::Relaxed);
        let deadline = frames as f64 / rack.sample_rate() as f64;
        if taken.as_secs_f64() > deadline {
            self.stats.late.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_returns_what_was_put_in_it() {
        let ring = Ring::new(16);
        assert!(ring.is_empty());
        assert_eq!(ring.pop(), None);
        for i in 0..8 {
            ring.push(i as f32 * 0.25);
        }
        assert_eq!(ring.len(), 8);
        for i in 0..8 {
            assert_eq!(ring.pop(), Some(i as f32 * 0.25));
        }
        assert!(ring.is_empty());
        assert_eq!(ring.overflowed(), 0);
    }

    #[test]
    fn a_full_ring_drops_and_counts_rather_than_blocking() {
        // An audio callback cannot wait, so overflow has to be visible instead.
        let ring = Ring::new(4);
        for i in 0..10 {
            ring.push(i as f32);
        }
        assert_eq!(ring.overflowed(), 7, "capacity is one less than the slots");
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.pop(), Some(0.0));
    }

    #[test]
    fn a_ring_wraps_indefinitely() {
        let ring = Ring::new(8);
        for round in 0..1000 {
            ring.push(round as f32);
            assert_eq!(ring.pop(), Some(round as f32));
        }
        assert_eq!(ring.overflowed(), 0);
        assert!(ring.is_empty());
    }

    #[test]
    fn a_ring_survives_two_threads() {
        // The real arrangement: the input callback writes while the output
        // callback reads, with no lock between them.
        let ring = Arc::new(Ring::new(512));
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let ring = Arc::clone(&ring);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                for i in 0..20_000 {
                    ring.push(i as f32);
                    if i % 128 == 0 {
                        std::thread::yield_now();
                    }
                }
                done.store(true, Ordering::Release);
            })
        };
        let reader = {
            let ring = Arc::clone(&ring);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let mut seen = 0usize;
                let mut last = -1.0f32;
                // The writer may outrun the reader and drop samples, so the
                // reader cannot wait for a particular count: it drains until
                // the writer has finished and nothing is left.
                loop {
                    match ring.pop() {
                        Some(value) => {
                            // Order holds even where samples went missing.
                            assert!(value > last, "{value} arrived after {last}");
                            last = value;
                            seen += 1;
                        }
                        None => {
                            if done.load(Ordering::Acquire) {
                                break;
                            }
                            std::thread::yield_now();
                        }
                    }
                }
                seen
            })
        };
        writer.join().expect("writer");
        let seen = reader.join().expect("reader");
        assert!(seen > 0, "nothing crossed between the threads");
        assert_eq!(
            seen as u64 + ring.overflowed(),
            20_000,
            "every sample was either read or counted as dropped"
        );
    }

    #[test]
    fn peak_reporting_resets_each_time_it_is_read() {
        let stats = Stats::default();
        stats.observe_peak(0.4);
        stats.observe_peak(0.9);
        stats.observe_peak(0.2);
        assert!((stats.take_peak() - 0.9).abs() < 1e-6);
        assert_eq!(stats.take_peak(), 0.0);
    }
}
