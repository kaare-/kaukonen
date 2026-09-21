//! Driving a patch from a host audio callback.
//!
//! The offline renderer in [`crate::render`] owns the clock. A real-time host
//! owns it instead: it hands over a buffer and expects it filled before the
//! deadline. This module is that side of the arrangement — block based, free of
//! allocation and locks once built, and driven one buffer at a time.
//!
//! There is no audio backend here on purpose. The crate stays dependency free,
//! and a host wanting cpal, JACK or an AudioWorklet writes a dozen lines of
//! glue around [`Stream::process`]. See `docs/realtime.md`.

use crate::engine::Engine;
use crate::module::AnalogModule;
use crate::network::ModuleId;
use crate::patch::BuiltPatch;
use crate::types::Volt;

/// A stream is handed to a host's audio thread, so it has to be movable there.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Stream>();
};

/// A patch wired up for buffer-at-a-time playback.
pub struct Stream {
    engine: Engine,
    /// Module indices of every `audioin`, resolved once.
    inputs: Vec<usize>,
    /// Module indices of every `audioout`, resolved once.
    outputs: Vec<usize>,
}

impl Stream {
    /// Prepare a built patch for a host running at the patch's own frame rate.
    ///
    /// Capture is switched off on every converter, because a stream runs
    /// indefinitely and a growing file buffer would be a leak with a long fuse.
    pub fn new(built: BuiltPatch) -> Self {
        let rate = built.config.sample_rate;
        let mut engine = Engine::new(built.network, built.config);
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for index in 0..engine.network.len() {
            let module = engine.network.get_mut(ModuleId(index));
            if module.as_audio_in_mut().is_some() {
                inputs.push(index);
            }
            if let Some(out) = module.as_audio_out_mut() {
                out.set_capturing(false);
                // The host consumes one frame per engine frame, so the
                // converter's anti-alias corner has to sit below the host's
                // Nyquist, not below whatever its knob happened to say.
                out.rate = rate;
                outputs.push(index);
            }
        }
        Self {
            engine,
            inputs,
            outputs,
        }
    }

    /// Set a knob on one module by index, for a host changing it mid-stream.
    ///
    /// Takes a `&'static str` because it is meant to be called from an audio
    /// callback: the name comes from a [`crate::param::ParamSpec`], so passing
    /// it costs no allocation.
    pub fn set_knob(&mut self, module: usize, knob: &'static str, value: f32) -> bool {
        if module >= self.engine.network.len() {
            return false;
        }
        self.engine
            .network
            .get_mut(ModuleId(module))
            .set_param(knob, value)
            .is_ok()
    }

    /// The rate a host must run at for this stream to be in time.
    ///
    /// One engine frame becomes one host frame, so nothing resamples. A host
    /// whose rate differs should be given a patch with a matching `rate`.
    pub fn sample_rate(&self) -> f32 {
        self.engine.config.sample_rate
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    pub fn has_output(&self) -> bool {
        !self.outputs.is_empty()
    }

    pub fn has_input(&self) -> bool {
        !self.inputs.is_empty()
    }

    /// Fill one interleaved output buffer. Allocates nothing.
    ///
    /// A patch with no `audioout` fills the buffer with silence rather than
    /// leaving whatever the host had there.
    pub fn process(&mut self, output: &mut [f32], channels: usize) {
        self.process_duplex(&[], output, channels);
    }

    /// Fill one interleaved output buffer while feeding an input buffer.
    ///
    /// `input` may be shorter than `output`, or empty; missing samples read as
    /// silence, the same as an unpatched jack. Allocates nothing.
    pub fn process_duplex(&mut self, input: &[f32], output: &mut [f32], channels: usize) {
        let channels = channels.max(1);
        let frames = output.len() / channels;
        let input_channels = if input.is_empty() {
            0
        } else {
            channels.min(input.len().max(1))
        };

        // A leftover live sample from a previous duplex block would otherwise
        // keep playing after the host stopped feeding us.
        if input_channels == 0 {
            self.clear_live();
        }

        for frame in 0..frames {
            if input_channels > 0 {
                // Mono sum of whatever the host gave us for this frame.
                let base = frame * channels;
                let mut sum = 0.0;
                let mut counted = 0;
                for channel in 0..channels {
                    if let Some(sample) = input.get(base + channel) {
                        sum += *sample;
                        counted += 1;
                    }
                }
                // A short input buffer is silence for that frame, not a repeat
                // of the previous one.
                let live = if counted > 0 {
                    sum / counted as f32
                } else {
                    0.0
                };
                self.set_live(live);
            }

            self.engine.frame();

            // Live overrides the file for one host frame, including every
            // oversampled step inside it. After the frame it has to go, or
            // the next process() without input would freeze on the last
            // sample.
            if input_channels > 0 {
                self.clear_live();
            }

            let (left, right) = self.converter_frame();
            let base = frame * channels;
            for channel in 0..channels {
                if let Some(slot) = output.get_mut(base + channel) {
                    *slot = if channel % 2 == 0 { left } else { right };
                }
            }
        }
    }

    fn set_live(&mut self, sample: f32) {
        for index in &self.inputs {
            if let Some(module) = self
                .engine
                .network
                .get_mut(ModuleId(*index))
                .as_audio_in_mut()
            {
                module.set_live(sample);
            }
        }
    }

    fn clear_live(&mut self) {
        for index in &self.inputs {
            if let Some(module) = self
                .engine
                .network
                .get_mut(ModuleId(*index))
                .as_audio_in_mut()
            {
                module.clear_live();
            }
        }
    }

    /// Sum of every converter's current output.
    fn converter_frame(&self) -> (Volt, Volt) {
        let mut left = 0.0;
        let mut right = 0.0;
        for index in &self.outputs {
            if let Some(out) = self.engine.network.get(ModuleId(*index)).as_audio_out() {
                let (l, r) = out.current_frame();
                left += l;
                right += r;
            }
        }
        (left.clamp(-1.0, 1.0), right.clamp(-1.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch;
    use crate::render::{render, RenderOptions};
    use crate::wav::Audio;

    const VOICE: &str = "\
        rate 16000\n\
        seconds 1\n\
        module cells rc1[2] preset=burst rate=400\n\
        module osc  vco  tune=180\n\
        module filt vcf  cutoff=500 resonance=0.8 cv_depth=1.1\n\
        module amp  vca  gain=0.6\n\
        module dac  audioout rate=16000 level=0.3\n\
        patch cells[0].out -> filt.cutoff_cv\n\
        patch osc.saw -> filt.in\n\
        patch filt.out -> amp.in\n\
        patch amp.out -> dac.left\n\
        patch amp.out -> dac.right\n\
        record cells[0].v amp.out\n";

    fn built(text: &str) -> BuiltPatch {
        patch::parse(text).unwrap().build().unwrap()
    }

    /// Pull a whole stream through in fixed-size blocks, as a host would.
    fn pull(text: &str, block: usize, frames: usize, channels: usize) -> Vec<f32> {
        let mut stream = Stream::new(built(text));
        let mut buffer = vec![0.0; block * channels];
        let mut collected = Vec::with_capacity(frames * channels);
        while collected.len() < frames * channels {
            buffer.fill(0.0);
            stream.process(&mut buffer, channels);
            collected.extend_from_slice(&buffer);
        }
        collected.truncate(frames * channels);
        collected
    }

    #[test]
    fn streaming_matches_the_offline_render() {
        // The whole point: the real-time path and the file path are the same
        // simulation, so a patch cannot sound different when played live.
        let (_, _, report) = render(built(VOICE), &RenderOptions::default());
        let offline = report.audio.expect("captured audio");
        let live = pull(VOICE, 256, offline.frames(), 2);

        assert!(offline.frames() > 15_000, "too short to be meaningful");
        let mut worst = 0.0f32;
        for frame in 0..offline.frames() {
            for channel in 0..2 {
                let a = offline.sample(frame, channel);
                let b = live[frame * 2 + channel];
                worst = worst.max((a - b).abs());
            }
        }
        assert!(worst < 1e-6, "live and offline diverged by {worst}");
    }

    #[test]
    fn the_block_size_does_not_change_the_result() {
        let a = pull(VOICE, 64, 4000, 2);
        let b = pull(VOICE, 1024, 4000, 2);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-6, "{x} vs {y}");
        }
    }

    #[test]
    fn a_stream_reports_what_the_host_needs_to_match() {
        let stream = Stream::new(built(VOICE));
        assert_eq!(stream.sample_rate(), 16000.0);
        assert!(stream.has_output());
        assert!(!stream.has_input());
    }

    #[test]
    fn live_input_reaches_the_patch() {
        let text = "\
            rate 8000\n\
            module adc audioin gain=5\n\
            module dac audioout rate=8000 level=0.2\n\
            patch adc.out -> dac.left\npatch adc.out -> dac.right\n";
        let mut stream = Stream::new(built(text));
        assert!(stream.has_input());
        // A tone, not a steady level: the converter blocks DC, as a real one
        // does, so a constant would correctly vanish.
        let rate = stream.sample_rate();
        let mut phase = 0.0f32;
        let mut input = vec![0.0; 512];
        let mut output = vec![0.0; 512];
        let mut peak = 0.0f32;
        for block in 0..16 {
            for sample in input.iter_mut() {
                phase = (phase + 200.0 / rate).fract();
                *sample = 0.5 * (phase * std::f32::consts::TAU).sin();
            }
            stream.process_duplex(&input, &mut output, 1);
            if block >= 8 {
                peak = output.iter().fold(peak, |m, s| m.max(s.abs()));
            }
        }
        assert!((peak - 0.5).abs() < 0.05, "half scale in gave {peak} out");
    }

    #[test]
    fn a_patch_with_no_converter_yields_silence_not_leftovers() {
        let mut stream = Stream::new(built("rate 8000\nmodule n neuron\n"));
        assert!(!stream.has_output());
        let mut output = vec![0.7; 128];
        stream.process(&mut output, 2);
        assert!(
            output.iter().all(|s| *s == 0.0),
            "stale host buffer left in"
        );
    }

    #[test]
    fn a_stream_does_not_grow_while_it_runs() {
        // Capture is off, so an indefinite stream cannot accumulate a file.
        let mut stream = Stream::new(built(VOICE));
        let mut output = vec![0.0; 512];
        for _ in 0..200 {
            stream.process(&mut output, 2);
        }
        for index in 0..stream.engine.network.len() {
            if let Some(out) = stream.engine.network.get(ModuleId(index)).as_audio_out() {
                assert!(!out.is_capturing());
                assert_eq!(
                    out.frames(),
                    0,
                    "the converter buffered {} frames",
                    out.frames()
                );
            }
        }
    }

    #[test]
    fn a_file_can_still_be_played_into_a_live_stream() {
        let text = "\
            rate 8000\n\
            module adc audioin gain=5\n\
            module dac audioout rate=8000 level=0.2\n\
            patch adc.out -> dac.left\npatch adc.out -> dac.right\n";
        let mut stream = Stream::new(built(text));
        for index in 0..stream.engine.network.len() {
            if let Some(module) = stream
                .engine
                .network
                .get_mut(ModuleId(index))
                .as_audio_in_mut()
            {
                let samples: Vec<f32> = (0..8000)
                    .map(|i| 0.5 * (i as f32 / 8000.0 * 200.0 * std::f32::consts::TAU).sin())
                    .collect();
                module.load(Audio::mono(8000, samples));
            }
        }
        let mut output = vec![0.0; 512];
        let mut peak = 0.0f32;
        for _ in 0..8 {
            stream.process(&mut output, 2);
            peak = output.iter().fold(peak, |m, s| m.max(s.abs()));
        }
        assert!(
            peak > 0.4,
            "the loaded file never reached the converter (peak {peak})"
        );
    }

    #[test]
    fn a_live_sample_does_not_stick_after_the_block_ends() {
        // Live overrides the file for one host frame. After the host stops
        // feeding us, the converter has to fall silent (or back to the file),
        // not hold the last sample forever.
        let text = "\
            rate 8000\n\
            module adc audioin gain=1\n\
            module dac audioout rate=8000 level=1 dc_block=0\n\
            patch adc.out -> dac.left\npatch adc.out -> dac.right\n";
        let mut stream = Stream::new(built(text));
        let input = vec![0.4; 64];
        let mut output = vec![0.0; 64];
        stream.process_duplex(&input, &mut output, 1);
        let during = output.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            during > 0.2,
            "live input never reached the converter: {during}"
        );

        let mut later = vec![0.0; 64];
        stream.process(&mut later, 1);
        let leftover = later.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            leftover < 0.05,
            "the last live sample kept playing: {leftover}"
        );
    }

    #[test]
    fn a_short_input_buffer_is_silence_not_a_repeat() {
        let text = "\
            rate 8000\n\
            module adc audioin gain=1\n\
            module dac audioout rate=8000 level=1 dc_block=0\n\
            patch adc.out -> dac.left\npatch adc.out -> dac.right\n";
        let mut stream = Stream::new(built(text));
        // One frame of input for a 4-frame, 1-channel output. The rest of
        // the block has to read as zero, not as that first sample again.
        let input = vec![0.5];
        let mut output = vec![-1.0; 4];
        stream.process_duplex(&input, &mut output, 1);
        // The converter's decimation filter colours the first sample and
        // leaves a short tail. A stuck live sample would hold near 0.5
        // for the rest of the block; a decaying tail is the filter.
        assert!(
            output[0] > 0.3,
            "first frame should see the live sample: {:?}",
            output
        );
        assert!(
            output[1].abs() < output[0].abs() * 0.3,
            "second frame held the live sample: {:?}",
            output
        );
        assert!(
            output[2..].iter().all(|s| s.abs() < 0.05),
            "later frames repeated the live sample: {:?}",
            output
        );
    }

    #[test]
    fn odd_channel_counts_are_handled() {
        for channels in [1, 2, 4] {
            let mut stream = Stream::new(built(VOICE));
            let mut output = vec![0.0; 128 * channels];
            stream.process(&mut output, channels);
            assert!(
                output.iter().any(|s| *s != 0.0),
                "{channels} channels gave silence"
            );
            assert!(output.iter().all(|s| s.is_finite()));
        }
    }

    #[test]
    fn duet_makes_sound_without_a_microphone() {
        let text = std::fs::read_to_string("patches/duet.kpatch").unwrap();
        let mut stream = Stream::new(built(&text));
        assert!(stream.has_input(), "duet should take a microphone");
        assert!(stream.has_output(), "duet should play");
        let mut buffer = vec![0.0f32; 512 * 2];
        let mut peak = 0.0f32;
        for _ in 0..80 {
            stream.process(&mut buffer, 2);
            peak = buffer.iter().fold(peak, |m, s| m.max(s.abs()));
        }
        assert!(
            peak > 0.01,
            "the internal voice was silent without a mic: {peak}"
        );
        assert!(peak.is_finite());
    }

    #[test]
    fn duet_mic_steers_the_rack_not_the_speakers() {
        // Speech is supposed to change the connectome and open a second take
        // on the room, not leave the waveform on the converter. A 220 Hz
        // tone through a dry path would stay highly correlated with the
        // output; through this patch it should barely show, while the
        // live and silent runs still diverge.
        let text = std::fs::read_to_string("patches/duet.kpatch").unwrap();
        let mut dry = Stream::new(built(&text));
        let mut live = Stream::new(built(&text));
        let rate = live.sample_rate();
        let block = 512;
        let channels = 2;
        let warmup = 40;
        let heard = 80;
        let mut phase = 0.0f32;
        let mut input = vec![0.0f32; block * channels];
        let mut dry_buf = vec![0.0f32; block * channels];
        let mut live_buf = vec![0.0f32; block * channels];
        let mut in_left = Vec::with_capacity(heard * block);
        let mut dry_left = Vec::with_capacity(heard * block);
        let mut live_left = Vec::with_capacity(heard * block);

        for block_i in 0..warmup + heard {
            for frame in 0..block {
                phase = (phase + 220.0 / rate).fract();
                let sample = 0.5 * (phase * std::f32::consts::TAU).sin();
                input[frame * channels] = sample;
                input[frame * channels + 1] = sample;
            }
            dry.process(&mut dry_buf, channels);
            live.process_duplex(&input, &mut live_buf, channels);
            if block_i < warmup {
                continue;
            }
            for frame in 0..block {
                in_left.push(input[frame * channels]);
                dry_left.push(dry_buf[frame * channels]);
                live_left.push(live_buf[frame * channels]);
            }
        }

        let r = pearson(&in_left, &live_left);
        assert!(
            r.abs() < 0.25,
            "live input leaked onto the output (r = {r})"
        );

        let mut diff_energy = 0.0f32;
        let mut dry_energy = 0.0f32;
        for (a, b) in dry_left.iter().zip(&live_left) {
            let d = a - b;
            diff_energy += d * d;
            dry_energy += a * a;
        }
        assert!(
            diff_energy > dry_energy * 0.01,
            "the microphone never moved the rack (diff {diff_energy} vs dry {dry_energy})"
        );

        let names = [
            "adc.env",
            "brain.norm",
            "room.cutoff",
            "air.cutoff",
            "micamp.gain",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
        let dry_state = dry.engine_mut().snapshot_sample(&names);
        let live_state = live.engine_mut().snapshot_sample(&names);
        assert!(
            live_state[0] > 0.05,
            "the envelope never rose: {}",
            live_state[0]
        );
        let moved = dry_state
            .iter()
            .zip(&live_state)
            .skip(1)
            .any(|(a, b)| (a - b).abs() > 0.02);
        assert!(
            moved,
            "envelope rose but the rack state did not: dry {dry_state:?} live {live_state:?}"
        );
        assert!(live_left.iter().any(|s| s.abs() > 0.01));
    }

    #[test]
    fn weather_makes_sound_without_a_microphone() {
        let text = std::fs::read_to_string("patches/weather.kpatch").unwrap();
        let mut stream = Stream::new(built(&text));
        assert!(stream.has_input(), "weather should take a microphone");
        assert!(stream.has_output(), "weather should play");
        let mut buffer = vec![0.0f32; 512 * 2];
        let mut peak = 0.0f32;
        for _ in 0..200 {
            stream.process(&mut buffer, 2);
            peak = buffer.iter().fold(peak, |m, s| m.max(s.abs()));
        }
        assert!(peak > 0.02, "the climate never opened the room: {peak}");
        assert!(peak.is_finite());
    }

    #[test]
    fn weather_has_quiet_and_bursty_windows() {
        // The point of this patch: long quiet, then a squall, then quiet
        // again — not a constant drone. Six seconds is enough for the sky
        // to turn over at least once.
        let text = std::fs::read_to_string("patches/weather.kpatch").unwrap();
        let mut stream = Stream::new(built(&text));
        let block = 512;
        let channels = 2;
        let windows = 24; // 0.25 s each at 48 kHz
        let frames_per_window = 12_000;
        let mut buffer = vec![0.0f32; block * channels];
        let mut left = Vec::with_capacity(windows * frames_per_window);
        let pair_names = ["a.v", "b.v"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        let mut pair_peak = 0.0f32;
        while left.len() < windows * frames_per_window {
            stream.process(&mut buffer, channels);
            for frame in 0..block {
                left.push(buffer[frame * channels]);
            }
            let pair = stream.engine_mut().snapshot_sample(&pair_names);
            pair_peak = pair_peak.max(pair[0].abs()).max(pair[1].abs());
        }
        left.truncate(windows * frames_per_window);

        let mut rms = Vec::with_capacity(windows);
        for w in 0..windows {
            let slice = &left[w * frames_per_window..(w + 1) * frames_per_window];
            let e: f32 = slice.iter().map(|s| s * s).sum();
            rms.push((e / slice.len() as f32).sqrt());
        }
        let quiet = rms.iter().filter(|r| **r < 0.008).count();
        let hot = rms.iter().filter(|r| **r > 0.03).count();
        let peak = left.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            quiet >= 2,
            "never went quiet: {quiet} quiet windows, rms={rms:?}"
        );
        assert!(hot >= 2, "never burst: {hot} hot windows, rms={rms:?}");
        assert!(peak > 0.05, "the squalls were too small: {peak}");
        assert!(
            pair_peak > 0.8,
            "the oscillating pair never left rest: {pair_peak}"
        );
    }

    #[test]
    fn weather_grains_tick_finer_than_the_sky() {
        // The granulator is RC-1 clocks into short neuron-gated windows.
        // Those clocks have to fire more often than the climate, or the
        // dust filter is just another squall VCA.
        let text = std::fs::read_to_string("patches/weather.kpatch").unwrap();
        let mut stream = Stream::new(built(&text));
        let names = ["sky[0].v", "flake[0].out", "dustamp.gain"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        let mut sky_rises = 0;
        let mut flake_rises = 0;
        let mut last = [0.0f32; 3];
        let mut buffer = vec![0.0f32; 64 * 2];
        let mut grain_peak = 0.0f32;
        // 64-frame blocks so an 8–30 ms flake is several samples, not one
        // missed edge in a 512-frame window.
        for _ in 0..1600 {
            stream.process(&mut buffer, 2);
            let now = stream.engine_mut().snapshot_sample(&names);
            if last[0] < 0.8 && now[0] >= 0.8 {
                sky_rises += 1;
            }
            if last[1] < 0.5 && now[1] >= 0.5 {
                flake_rises += 1;
            }
            grain_peak = grain_peak.max(now[2].abs());
            last = [now[0], now[1], now[2]];
        }
        assert!(
            flake_rises > sky_rises,
            "grains no finer than the sky: flake {flake_rises} sky {sky_rises}"
        );
        assert!(grain_peak > 0.05, "the dust VCA never opened: {grain_peak}");
    }

    #[test]
    fn weather_sidechain_splits_the_doors() {
        // Room, pair and grains used to ride one swell. The sidechain
        // should pull them apart: a grain cloud ducks the room, and
        // the pair should not be a copy of the room.
        let text = std::fs::read_to_string("patches/weather.kpatch").unwrap();
        let mut stream = Stream::new(built(&text));
        let names = ["roomamp.gain", "pairamp.gain", "dustamp.gain", "cloud.out"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        let mut max_door_sep = 0.0f32;
        let mut grain_without_room = 0usize;
        let mut room_without_grain = 0usize;
        let mut cloud_peak = 0.0f32;
        let mut buffer = vec![0.0f32; 64 * 2];
        for _ in 0..6000 {
            stream.process(&mut buffer, 2);
            let now = stream.engine_mut().snapshot_sample(&names);
            max_door_sep = max_door_sep.max((now[0] - now[1]).abs());
            cloud_peak = cloud_peak.max(now[3].abs());
            if now[2] > 0.04 && now[0] < 0.14 {
                grain_without_room += 1;
            }
            if now[0] > 0.12 && now[2] < 0.05 {
                room_without_grain += 1;
            }
        }
        assert!(cloud_peak > 0.5, "the grain cloud never rose: {cloud_peak}");
        assert!(
            max_door_sep > 0.04,
            "room and pair stayed locked: sep {max_door_sep}"
        );
        assert!(
            grain_without_room >= 1,
            "grains never appeared without the room: {grain_without_room}"
        );
        assert!(
            room_without_grain >= 1,
            "the room never opened without grains: {room_without_grain}"
        );
    }

    #[test]
    fn weather_later_stages_write_back() {
        // Sift readings have to echo into the sky and kick the cell they
        // came from. If those mixes stay at rest the climate never feels
        // the grains, and every squall is the last one again.
        let text = std::fs::read_to_string("patches/weather.kpatch").unwrap();
        let mut stream = Stream::new(built(&text));
        let names = [
            "echo[0].out",
            "write[0].out",
            "recoil.out",
            "sky[1].v",
            "sky[3].v",
            "sieve.out",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
        let mut echo_peak = 0.0f32;
        let mut write_peak = 0.0f32;
        let mut recoil_peak = 0.0f32;
        let mut sieve_peak = 0.0f32;
        let mut sky_spread = 0.0f32;
        let mut buffer = vec![0.0f32; 64 * 2];
        for _ in 0..6000 {
            stream.process(&mut buffer, 2);
            let now = stream.engine_mut().snapshot_sample(&names);
            echo_peak = echo_peak.max(now[0].abs());
            write_peak = write_peak.max(now[1].abs());
            recoil_peak = recoil_peak.max(now[2].abs());
            sieve_peak = sieve_peak.max(now[5].abs());
            sky_spread = sky_spread.max((now[3] - now[4]).abs());
        }
        assert!(
            echo_peak > 0.06,
            "echo never wrote into the sky: {echo_peak}"
        );
        assert!(
            write_peak > 0.06,
            "sift never kicked the cell it read: {write_peak}"
        );
        assert!(
            recoil_peak > 0.04,
            "the pair never felt the grains: {recoil_peak}"
        );
        assert!(
            sieve_peak > 0.2,
            "the grain sieve stayed shut: {sieve_peak}"
        );
        assert!(
            sky_spread > 0.3,
            "sky cells stayed locked: spread {sky_spread}"
        );
    }

    #[test]
    fn weather_bandpass_walks_with_the_climate() {
        // The final window has to move. If cutoff sits still the bandpass
        // is just a fixed tone control.
        let text = std::fs::read_to_string("patches/weather.kpatch").unwrap();
        let mut stream = Stream::new(built(&text));
        let names = ["band[0].cutoff", "band[0].q"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        let mut lo = f32::MAX;
        let mut hi = 0.0f32;
        let mut q_hi = 0.0f32;
        let mut buffer = vec![0.0f32; 64 * 2];
        for _ in 0..4000 {
            stream.process(&mut buffer, 2);
            let now = stream.engine_mut().snapshot_sample(&names);
            lo = lo.min(now[0]);
            hi = hi.max(now[0]);
            q_hi = q_hi.max(now[1]);
        }
        assert!(hi > 1.35 * lo, "the band never walked: {lo} .. {hi}");
        assert!(q_hi > 0.7, "grains never peaked the Q: {q_hi}");
    }

    #[test]
    fn weather_mic_steers_the_sky_not_the_speakers() {
        let text = std::fs::read_to_string("patches/weather.kpatch").unwrap();
        let mut dry = Stream::new(built(&text));
        let mut live = Stream::new(built(&text));
        let rate = live.sample_rate();
        let block = 512;
        let channels = 2;
        let warmup = 30;
        let heard = 50;
        let mut phase = 0.0f32;
        let mut input = vec![0.0f32; block * channels];
        let mut dry_buf = vec![0.0f32; block * channels];
        let mut live_buf = vec![0.0f32; block * channels];
        let mut in_left = Vec::with_capacity(heard * block);
        let mut live_left = Vec::with_capacity(heard * block);

        for block_i in 0..warmup + heard {
            for frame in 0..block {
                phase = (phase + 220.0 / rate).fract();
                let sample = 0.5 * (phase * std::f32::consts::TAU).sin();
                input[frame * channels] = sample;
                input[frame * channels + 1] = sample;
            }
            dry.process(&mut dry_buf, channels);
            live.process_duplex(&input, &mut live_buf, channels);
            if block_i < warmup {
                continue;
            }
            for frame in 0..block {
                in_left.push(input[frame * channels]);
                live_left.push(live_buf[frame * channels]);
            }
        }

        let r = pearson(&in_left, &live_left);
        assert!(
            r.abs() < 0.25,
            "live input leaked onto the output (r = {r})"
        );
        let names = ["adc.env", "sky[0].v", "roomamp.gain"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        let dry_state = dry.engine_mut().snapshot_sample(&names);
        let live_state = live.engine_mut().snapshot_sample(&names);
        assert!(
            live_state[0] > 0.05,
            "the envelope never rose: {}",
            live_state[0]
        );
        let moved = dry_state
            .iter()
            .zip(&live_state)
            .any(|(a, b)| (a - b).abs() > 0.02);
        assert!(
            moved,
            "speech never turned the climate: dry {dry_state:?} live {live_state:?}"
        );
    }

    fn pearson(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len() as f32;
        if n == 0.0 {
            return 0.0;
        }
        let ma = a.iter().sum::<f32>() / n;
        let mb = b.iter().sum::<f32>() / n;
        let mut num = 0.0;
        let mut da = 0.0;
        let mut db = 0.0;
        for (x, y) in a.iter().zip(b) {
            let dx = x - ma;
            let dy = y - mb;
            num += dx * dy;
            da += dx * dx;
            db += dy * dy;
        }
        if da == 0.0 || db == 0.0 {
            0.0
        } else {
            num / (da.sqrt() * db.sqrt())
        }
    }
}
