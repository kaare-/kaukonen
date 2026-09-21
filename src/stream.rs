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
}
