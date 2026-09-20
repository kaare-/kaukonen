//! Sound in and out of a patch.
//!
//! `AudioIn` carries its own decoded samples and `AudioOut` collects its own,
//! so the renderer loads one before the run and writes the other afterwards
//! without the engine needing to know audio exists. A real-time backend would
//! refill and drain the same two buffers.

use crate::integrate::lowpass;
use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{self, params, ParamError, ParamSpec, Unit};
use crate::types::{soft_clip, Volt, DEFAULT_RAIL};
use crate::wav::Audio;

/// Fraction of full scale the converter passes untouched before it rounds off.
const CLIP_KNEE: f32 = 0.7;

const IN_NO_JACKS: &[&str] = &[];
const IN_OUT_JACKS: &[&str] = &["out", "env"];
const IN_STATE: &[&str] = &["out", "env", "position"];

/// Which channel of a stereo file feeds the patch.
pub const CHANNELS: &[&str] = &["mix", "left", "right"];

/// What happens when playback reaches the end of the file.
pub const ENDINGS: &[&str] = &["silence", "loop"];

const IN_PARAMS: &[ParamSpec] = params! {
    "gain", Unit::Ratio, 0.0 ..= 20.0, 5.0, Linear, "volts per unit of full-scale audio; 5 puts a full-scale file at 5 V";
    "channel", Unit::Enum(CHANNELS), 0.0 ..= 2.0, 0.0, Linear, "which channel of the file to take";
    "ending", Unit::Enum(ENDINGS), 0.0 ..= 1.0, 0.0, Linear, "what to do past the end of the file";
    "env_rate", Unit::Hertz, 0.1 ..= 500.0, 20.0, Log, "corner of the envelope follower on the ENV jack";
};

/// Plays a decoded file into the patch, with an envelope follower alongside.
///
/// Playback advances by elapsed time rather than by sample count, so the file's
/// own rate is independent of the engine's and oversampling costs nothing.
#[derive(Clone, Debug, Default)]
pub struct AudioIn {
    pub gain: f32,
    pub channel: usize,
    pub ending: usize,
    pub env_rate: f32,
    audio: Audio,
    /// Playback position in frames of the source file.
    position: f32,
    last: Volt,
    env: Volt,
}

impl AudioIn {
    pub fn new() -> Self {
        Self {
            gain: 5.0,
            channel: 0,
            ending: 0,
            env_rate: 20.0,
            audio: Audio::default(),
            position: 0.0,
            last: 0.0,
            env: 0.0,
        }
    }

    /// Hand this module the sound it should play.
    pub fn load(&mut self, audio: Audio) {
        self.audio = audio;
        self.position = 0.0;
    }

    pub fn audio(&self) -> &Audio {
        &self.audio
    }

    pub fn is_loaded(&self) -> bool {
        self.audio.frames() > 0
    }

    /// Linearly interpolated source sample at the current position.
    fn read(&self) -> f32 {
        let frames = self.audio.frames();
        if frames == 0 {
            return 0.0;
        }
        let at = |frame: usize| match self.channel {
            1 => self.audio.sample(frame, 0),
            2 => self.audio.sample(frame, 1.min(self.audio.channels - 1)),
            _ => self.audio.mixdown(frame),
        };
        let index = self.position.floor();
        if index < 0.0 {
            return 0.0;
        }
        let i = index as usize;
        if i >= frames {
            return 0.0;
        }
        let frac = self.position - index;
        let next = if i + 1 < frames {
            i + 1
        } else if self.ending == 1 {
            0
        } else {
            i
        };
        at(i) * (1.0 - frac) + at(next) * frac
    }
}

impl AnalogModule for AudioIn {
    fn kind_name(&self) -> &'static str {
        "audioin"
    }

    fn in_jacks(&self) -> &'static [&'static str] {
        IN_NO_JACKS
    }

    fn out_jacks(&self) -> &'static [&'static str] {
        IN_OUT_JACKS
    }

    fn params(&self) -> &'static [ParamSpec] {
        IN_PARAMS
    }

    fn get_param(&self, name: &str) -> Option<f32> {
        Some(match name {
            "gain" => self.gain,
            "channel" => self.channel as f32,
            "ending" => self.ending as f32,
            "env_rate" => self.env_rate,
            _ => return None,
        })
    }

    fn set_param(&mut self, name: &str, value: f32) -> Result<(), ParamError> {
        let spec = param::find_spec(IN_PARAMS, name).ok_or_else(|| self.unknown_param(name))?;
        let v = param::checked(spec, value)?;
        match name {
            "gain" => self.gain = v,
            "channel" => self.channel = param::choice(v),
            "ending" => self.ending = param::choice(v),
            "env_rate" => self.env_rate = v,
            _ => return Err(self.unknown_param(name)),
        }
        Ok(())
    }

    fn state_fields(&self) -> &'static [&'static str] {
        IN_STATE
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.last, self.env, self.position];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, _inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let out = self.gain * self.read();
        // A rectifier into a lowpass, which is all an envelope follower is.
        self.env = lowpass(self.env, out.abs(), self.env_rate, dt);
        self.last = out;

        let frames = self.audio.frames();
        if frames > 0 {
            self.position += dt * self.audio.sample_rate as f32;
            if self.position >= frames as f32 {
                if self.ending == 1 {
                    self.position %= frames as f32;
                } else {
                    self.position = frames as f32;
                }
            }
        }
        ModuleOutputs::new(&[out.clamp(-DEFAULT_RAIL, DEFAULT_RAIL), self.env])
    }
}

const OUT_IN_JACKS: &[&str] = &["left", "right"];
const OUT_OUT_JACKS: &[&str] = &["mix"];
const OUT_STATE: &[&str] = &["left", "right", "peak"];

const OUT_PARAMS: &[ParamSpec] = params! {
    "level", Unit::Ratio, 0.0 ..= 2.0, 0.2, Linear, "full scale per volt; 0.2 puts 5 V at full scale";
    "rate", Unit::Hertz, 1000.0 ..= 192000.0, 48000.0, Log, "sample rate of the captured file";
    "headroom", Unit::Ratio, 0.1 ..= 4.0, 1.0, Linear, "soft-clip ceiling; the converter saturates rather than wrapping";
};

/// The DAC: takes patched voltages and collects a file.
///
/// Emission is driven by elapsed time at the `rate` knob, and the signal is
/// lowpassed just under that rate's Nyquist first, so an oversampled engine
/// decimates cleanly instead of folding its extra bandwidth back down.
#[derive(Clone, Debug)]
pub struct AudioOut {
    pub level: f32,
    pub rate: f32,
    pub headroom: f32,
    left: Vec<f32>,
    right: Vec<f32>,
    /// Two cascaded one-poles, so the decimation filter is -12 dB/octave.
    filter: [[f32; 2]; 2],
    /// Fractional output samples owed.
    phase: f32,
    last: [Volt; 2],
    peak: f32,
}

impl Default for AudioOut {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioOut {
    pub fn new() -> Self {
        Self {
            level: 0.2,
            rate: 48000.0,
            headroom: 1.0,
            left: Vec::new(),
            right: Vec::new(),
            filter: [[0.0; 2]; 2],
            phase: 0.0,
            last: [0.0; 2],
            peak: 0.0,
        }
    }

    /// Everything captured so far, as stereo audio ready to write.
    pub fn captured(&self) -> Audio {
        Audio::stereo(self.rate as u32, &self.left, &self.right)
    }

    pub fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    /// Largest absolute value seen before soft clipping, so a renderer can
    /// report how hard the converter was driven.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    pub fn clear(&mut self) {
        self.left.clear();
        self.right.clear();
        self.phase = 0.0;
        self.peak = 0.0;
    }
}

impl AnalogModule for AudioOut {
    fn kind_name(&self) -> &'static str {
        "audioout"
    }

    fn in_jacks(&self) -> &'static [&'static str] {
        OUT_IN_JACKS
    }

    fn out_jacks(&self) -> &'static [&'static str] {
        OUT_OUT_JACKS
    }

    fn params(&self) -> &'static [ParamSpec] {
        OUT_PARAMS
    }

    fn get_param(&self, name: &str) -> Option<f32> {
        Some(match name {
            "level" => self.level,
            "rate" => self.rate,
            "headroom" => self.headroom,
            _ => return None,
        })
    }

    fn set_param(&mut self, name: &str, value: f32) -> Result<(), ParamError> {
        let spec = param::find_spec(OUT_PARAMS, name).ok_or_else(|| self.unknown_param(name))?;
        let v = param::checked(spec, value)?;
        match name {
            "level" => self.level = v,
            "rate" => self.rate = v,
            "headroom" => self.headroom = v,
            _ => return Err(self.unknown_param(name)),
        }
        Ok(())
    }

    fn state_fields(&self) -> &'static [&'static str] {
        OUT_STATE
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.last[0], self.last[1], self.peak];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let left = inputs.first().copied().unwrap_or(0.0);
        let right = inputs.get(1).copied().unwrap_or(left);
        self.last = [left, right];

        // Keep the anti-alias corner just under the output Nyquist.
        let corner = 0.45 * self.rate;
        let mut scaled = [left * self.level, right * self.level];
        for (channel, poles) in self.filter.iter_mut().enumerate() {
            poles[0] = lowpass(poles[0], scaled[channel], corner, dt);
            poles[1] = lowpass(poles[1], poles[0], corner, dt);
            scaled[channel] = poles[1];
        }

        self.phase += dt * self.rate;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.peak = self.peak.max(scaled[0].abs()).max(scaled[1].abs());
            self.left
                .push(soft_clip(scaled[0], self.headroom, CLIP_KNEE));
            self.right
                .push(soft_clip(scaled[1], self.headroom, CLIP_KNEE));
        }

        let mix = 0.5 * (left + right);
        ModuleOutputs::one(mix.clamp(-DEFAULT_RAIL, DEFAULT_RAIL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::Audio;

    /// Run a module for `seconds` of simulated time at `dt`.
    fn drive(module: &mut impl AnalogModule, inputs: &[Volt], dt: f32, seconds: f32) {
        for _ in 0..(seconds / dt) as usize {
            module.step(inputs, dt);
        }
    }

    #[test]
    fn playback_follows_the_files_own_rate() {
        // A 100 Hz square at 8 kHz, played by an engine running at 20 kHz.
        let frames = 8000;
        let samples: Vec<f32> = (0..frames)
            .map(|i| if (i / 40) % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let mut input = AudioIn::new();
        input.load(Audio::mono(8000, samples));
        assert!(input.is_loaded());

        let dt = 1.0 / 20_000.0;
        let mut crossings = 0;
        let mut prev = 0.0;
        for _ in 0..20_000 {
            let out = input.step(&[], dt)[0];
            if prev < 0.0 && out >= 0.0 {
                crossings += 1;
            }
            prev = out;
        }
        // One second of playback of a 100 Hz square.
        assert!(
            (99..=101).contains(&crossings),
            "expected about 100 cycles, counted {crossings}"
        );
    }

    #[test]
    fn gain_puts_full_scale_where_the_knob_says() {
        let mut input = AudioIn::new();
        input.load(Audio::mono(8000, vec![1.0; 800]));
        input.set_param("gain", 5.0).unwrap();
        let out = input.step(&[], 1.0 / 8000.0)[0];
        assert!((out - 5.0).abs() < 1e-4, "full scale gave {out} V");
    }

    #[test]
    fn the_envelope_follower_rises_and_falls() {
        let mut input = AudioIn::new();
        // Half a second of tone, then silence.
        let rate = 8000;
        let samples: Vec<f32> = (0..rate)
            .map(|i| {
                if i < rate / 2 {
                    (i as f32 * 0.2).sin()
                } else {
                    0.0
                }
            })
            .collect();
        input.load(Audio::mono(rate as u32, samples));
        input.set_param("env_rate", 20.0).unwrap();

        let dt = 1.0 / rate as f32;
        drive(&mut input, &[], dt, 0.45);
        let loud = input.step(&[], dt)[1];
        drive(&mut input, &[], dt, 0.45);
        let quiet = input.step(&[], dt)[1];
        assert!(loud > 1.0, "envelope stayed low during the tone: {loud}");
        assert!(quiet < 0.1 * loud, "envelope did not fall: {quiet}");
    }

    #[test]
    fn playback_can_loop_or_fall_silent() {
        let make = |ending: f32| {
            let mut input = AudioIn::new();
            input.load(Audio::mono(1000, vec![1.0; 100]));
            input.set_param("ending", ending).unwrap();
            input
        };
        let dt = 1.0 / 1000.0;

        let mut once = make(0.0);
        drive(&mut once, &[], dt, 0.15);
        assert_eq!(once.step(&[], dt)[0], 0.0, "should have fallen silent");

        let mut looping = make(1.0);
        drive(&mut looping, &[], dt, 0.15);
        assert!(looping.step(&[], dt)[0] > 4.0, "should have wrapped around");
    }

    #[test]
    fn capture_emits_at_the_rate_knob_whatever_the_step_size() {
        for dt in [1.0 / 48_000.0, 1.0 / 192_000.0] {
            let mut out = AudioOut::new();
            out.set_param("rate", 16_000.0).unwrap();
            drive(&mut out, &[1.0, 1.0], dt, 0.5);
            let frames = out.frames();
            assert!(
                (7990..=8010).contains(&frames),
                "dt={dt} captured {frames} frames, wanted about 8000"
            );
            assert_eq!(out.captured().sample_rate, 16_000);
            assert_eq!(out.captured().channels, 2);
        }
    }

    #[test]
    fn capture_is_transparent_below_the_knee_and_never_exceeds_the_ceiling() {
        let dt = 1.0 / 96_000.0;
        // 3 V at level 0.2 is 0.6 full scale, below the 0.7 knee.
        let mut clean = AudioOut::new();
        clean.set_param("rate", 48_000.0).unwrap();
        drive(&mut clean, &[3.0, 3.0], dt, 0.05);
        let settled = *clean.captured().samples.last().unwrap();
        assert!(
            (settled - 0.6).abs() < 1e-3,
            "quiet signal was coloured: {settled}"
        );

        // 20 V is far past full scale and must still land inside the ceiling.
        let mut hot = AudioOut::new();
        hot.set_param("rate", 48_000.0).unwrap();
        drive(&mut hot, &[20.0, -20.0], dt, 0.05);
        let audio = hot.captured();
        assert!(audio.peak() <= 1.0, "converter exceeded full scale");
        assert!(audio.peak() > 0.9, "converter gave up too early");
        assert!(hot.peak() > 1.0, "peak meter should see the overdrive");
    }

    #[test]
    fn capture_lowpasses_before_it_decimates() {
        // A 6 kHz tone captured at 8 kHz would alias to 2 kHz if nothing
        // filtered it, so the decimation filter has to hold it down.
        let dt = 1.0 / 96_000.0;
        let mut out = AudioOut::new();
        out.set_param("rate", 8000.0).unwrap();
        out.set_param("level", 1.0).unwrap();
        let mut phase = 0.0f32;
        for _ in 0..(0.2 / dt) as usize {
            phase = (phase + 6000.0 * dt).fract();
            let v = (phase * std::f32::consts::TAU).sin();
            out.step(&[v, v], dt);
        }
        let captured = out.captured();
        let tail: Vec<f32> = captured.samples.iter().copied().skip(400).collect();
        let peak = tail.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak < 0.4,
            "a tone above the output Nyquist came through at {peak}"
        );
    }

    #[test]
    fn param_defaults_match_the_default_modules() {
        for module in [
            &AudioIn::new() as &dyn AnalogModule,
            &AudioOut::new() as &dyn AnalogModule,
        ] {
            for spec in module.params() {
                let got = module.get_param(spec.name).expect(spec.name);
                assert!(
                    (got - spec.default).abs() < 1e-9,
                    "{}.{} default {} but module has {got}",
                    module.kind_name(),
                    spec.name,
                    spec.default
                );
            }
        }
    }
}
