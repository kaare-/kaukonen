//! Small delay plus a Schroeder tank. A utility, not a neural module.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["in", "time_cv", "decay_cv"];
const OUT_JACKS: &[&str] = &["out", "wet"];
const STATE_FIELDS: &[&str] = &["wet", "env"];

const PARAMS: &[ParamSpec] = params! {
    "time", Unit::Seconds, 0.005 ..= 0.45, 0.07, Log, "pre-delay; the slap before the tank";
    "decay", Unit::Ratio, 0.05 ..= 0.94, 0.42, Linear, "how long the tank rings";
    "damp", Unit::Ratio, 0.0 ..= 0.9, 0.28, Linear, "highs die in the feedback; 0 is bright";
    "size", Unit::Ratio, 0.35 ..= 1.6, 0.85, Linear, "scales the comb lengths";
    "mix", Unit::Ratio, 0.0 ..= 1.0, 0.22, Linear, "dry/wet";
    "time_depth", Unit::Ratio, -0.4 ..= 0.4, 0.05, Linear, "seconds per volt on TIME CV";
    "decay_depth", Unit::Ratio, -1.0 ..= 1.0, 0.12, Linear, "decay change per volt on DECAY CV";
    "level", Unit::Ratio, 0.0 ..= 4.0, 1.0, Linear, "wet scale";
};

/// Long enough for 0.45 s at 192 kHz (48 kHz frames, oversample 4).
const MAX_PRE: usize = 96_000;
const MAX_COMB: usize = 24_000;
const MAX_AP: usize = 8_000;

/// Comb lengths in seconds at size=1, Freeverb-ish primes.
const COMB_T: [f32; 4] = [0.0297, 0.0371, 0.0411, 0.0437];
const AP_T: [f32; 2] = [0.0050, 0.0168];
const AP_G: f32 = 0.5;

#[derive(Clone, Debug)]
struct Line {
    buf: Vec<f32>,
    w: usize,
    filter: f32,
}

impl Line {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(8)],
            w: 0,
            filter: 0.0,
        }
    }

    fn write(&mut self, x: f32) {
        self.buf[self.w] = x;
        self.w += 1;
        if self.w == self.buf.len() {
            self.w = 0;
        }
    }

    fn tap(&self, samples: f32) -> f32 {
        let n = self.buf.len();
        let d = samples.clamp(1.0, (n - 2) as f32);
        let pos = self.w as f32 - d;
        let i0 = pos.floor();
        let frac = pos - i0;
        let a = i0.rem_euclid(n as f32) as usize;
        let b = (a + 1) % n;
        self.buf[a] + (self.buf[b] - self.buf[a]) * frac
    }

    fn comb(&mut self, x: f32, samples: f32, fb: f32, damp: f32) -> f32 {
        let y = self.tap(samples);
        self.filter += (1.0 - damp) * (y - self.filter);
        self.write(x + self.filter * fb);
        y
    }

    fn allpass(&mut self, x: f32, samples: f32, g: f32) -> f32 {
        let buf = self.tap(samples);
        let y = -g * x + buf;
        self.write(x + g * buf);
        y
    }
}

/// A short slap into four combs and two allpasses.
///
/// Time is the pre-delay. Decay is comb feedback. Size walks the tank.
/// It is a small room, not a hall — keep `mix` under 0.4 unless you want
/// the climate to sit in a cave.
#[derive(Clone, Debug)]
pub struct Verb {
    pub time: f32,
    pub decay: f32,
    pub damp: f32,
    pub size: f32,
    pub mix: f32,
    pub time_depth: f32,
    pub decay_depth: f32,
    pub level: f32,
    pre: Line,
    combs: [Line; 4],
    ap: [Line; 2],
    wet: f32,
    env: f32,
}

impl Default for Verb {
    fn default() -> Self {
        Self::new()
    }
}

impl Verb {
    pub fn new() -> Self {
        Self {
            time: 0.07,
            decay: 0.42,
            damp: 0.28,
            size: 0.85,
            mix: 0.22,
            time_depth: 0.05,
            decay_depth: 0.12,
            level: 1.0,
            pre: Line::new(MAX_PRE),
            combs: [
                Line::new(MAX_COMB),
                Line::new(MAX_COMB),
                Line::new(MAX_COMB),
                Line::new(MAX_COMB),
            ],
            ap: [Line::new(MAX_AP), Line::new(MAX_AP)],
            wet: 0.0,
            env: 0.0,
        }
    }
}

impl AnalogModule for Verb {
    fn kind_name(&self) -> &'static str {
        "verb"
    }

    fn in_jacks(&self) -> &'static [&'static str] {
        IN_JACKS
    }

    fn out_jacks(&self) -> &'static [&'static str] {
        OUT_JACKS
    }

    knob_accessors! {
        PARAMS;
        floats {
            "time" => time,
            "decay" => decay,
            "damp" => damp,
            "size" => size,
            "mix" => mix,
            "time_depth" => time_depth,
            "decay_depth" => decay_depth,
            "level" => level,
        }
        choices {}
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.wet, self.env];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let x = inputs.first().copied().unwrap_or(0.0);
        let time_cv = inputs.get(1).copied().unwrap_or(0.0);
        let decay_cv = inputs.get(2).copied().unwrap_or(0.0);
        let time = (self.time + self.time_depth * time_cv).clamp(0.005, 0.45);
        let decay = (self.decay + self.decay_depth * decay_cv).clamp(0.05, 0.94);
        let size = self.size.clamp(0.35, 1.6);
        let damp = self.damp.clamp(0.0, 0.9);
        let mix = self.mix.clamp(0.0, 1.0);

        self.pre.write(x);
        let delayed = self.pre.tap(time / dt.max(1e-9));

        let mut acc = 0.0;
        for (comb, &t) in self.combs.iter_mut().zip(COMB_T.iter()) {
            acc += comb.comb(delayed, (t * size) / dt.max(1e-9), decay, damp);
        }
        acc *= 0.28;
        acc = self.ap[0].allpass(acc, AP_T[0] * size / dt.max(1e-9), AP_G);
        acc = self.ap[1].allpass(acc, AP_T[1] * size / dt.max(1e-9), AP_G);
        self.wet = acc * self.level;
        self.env += 40.0 * dt * (self.wet.abs() - self.env);
        let out = (1.0 - mix) * x + mix * self.wet;

        ModuleOutputs::new(&[
            clamp_rail(out, DEFAULT_RAIL),
            clamp_rail(self.wet, DEFAULT_RAIL),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 48_000.0;

    #[test]
    fn an_impulse_has_a_tail() {
        let mut v = Verb::new();
        v.mix = 1.0;
        v.time = 0.01;
        v.decay = 0.7;
        v.size = 1.0;
        v.step(&[1.0, 0.0, 0.0], DT);
        let mut peak_early = 0.0f32;
        let mut peak_late = 0.0f32;
        for i in 0..9000 {
            let y = v.step(&[0.0, 0.0, 0.0], DT)[1].abs();
            // Predelay plus the shortest comb lands near 2k samples.
            if i < 3500 {
                peak_early = peak_early.max(y);
            }
            if i > 6000 {
                peak_late = peak_late.max(y);
            }
        }
        assert!(peak_early > 0.02, "no early wet {peak_early}");
        assert!(peak_late > 0.002, "the tail died: late {peak_late}");
        assert!(peak_late < peak_early, "the tail did not decay");
    }

    #[test]
    fn mix_of_zero_is_dry() {
        let mut v = Verb::new();
        v.mix = 0.0;
        v.step(&[0.8, 0.0, 0.0], DT);
        let mut err = 0.0f32;
        for _ in 0..2000 {
            let y = v.step(&[0.0, 0.0, 0.0], DT)[0];
            err = err.max(y.abs());
        }
        assert!(err < 0.02, "dry leaked {err}");
    }

    #[test]
    fn more_decay_rings_longer() {
        fn late(decay: f32) -> f32 {
            let mut v = Verb::new();
            v.mix = 1.0;
            v.decay = decay;
            v.time = 0.01;
            v.step(&[1.0, 0.0, 0.0], DT);
            let mut peak = 0.0f32;
            for i in 0..9000 {
                let y = v.step(&[0.0, 0.0, 0.0], DT)[1].abs();
                if i > 5000 {
                    peak = peak.max(y);
                }
            }
            peak
        }
        let short = late(0.2);
        let long = late(0.85);
        assert!(
            long > 2.0 * short,
            "decay did nothing: short {short} long {long}"
        );
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let v = Verb::new();
        for spec in v.params() {
            let got = v.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
