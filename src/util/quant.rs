//! 1 V/oct quantizer. A utility, not a neural module.

use super::scale::{self, SCALES};
use super::volts_to_ratio;
use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["v_oct", "root_cv"];
const OUT_JACKS: &[&str] = &["v_oct", "hz"];
const STATE_FIELDS: &[&str] = &["hz", "snapped", "v_oct"];

const PARAMS: &[ParamSpec] = params! {
    "tune", Unit::Hertz, 20.0 ..= 4000.0, 55.0, Log, "the oscillator this jack feeds; 0 V is this pitch";
    "root", Unit::Hertz, 20.0 ..= 2000.0, 55.0, Log, "scale root; 55 is A1";
    "scale", Unit::Enum(SCALES), 0.0 ..= 6.0, 6.0, Linear, "how the pitch snaps; phrygian is the flat 2";
    "amount", Unit::Ratio, 0.0 ..= 1.0, 1.0, Linear, "0 is the raw CV, 1 is the scale";
    "glide", Unit::Seconds, 0.001 ..= 0.8, 0.08, Log, "how slowly a new degree is reached";
};

/// Snaps a 1 V/oct jack onto a scale and writes the same jack back out.
///
/// `tune` is the rest pitch of the oscillator this feeds, so 0 V on the
/// output is that note. Incoming volts are applied to `tune`, snapped to
/// `root`'s scale, then turned back into volts the oscillator can eat.
#[derive(Clone, Debug)]
pub struct Quant {
    pub tune: f32,
    pub root: f32,
    pub scale: usize,
    pub amount: f32,
    pub glide: f32,
    hz: f32,
    snapped: f32,
    v_oct: f32,
    sung_log: f32,
}

impl Default for Quant {
    fn default() -> Self {
        Self::new()
    }
}

impl Quant {
    pub fn new() -> Self {
        Self {
            tune: 55.0,
            root: 55.0,
            scale: 6,
            amount: 1.0,
            glide: 0.08,
            hz: 55.0,
            snapped: 55.0,
            v_oct: 0.0,
            sung_log: 55.0f32.ln(),
        }
    }

    pub fn hz(&self) -> f32 {
        self.hz
    }
}

impl AnalogModule for Quant {
    fn kind_name(&self) -> &'static str {
        "quant"
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
            "tune" => tune,
            "root" => root,
            "amount" => amount,
            "glide" => glide,
        }
        choices {
            "scale" => scale,
        }
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.hz, self.snapped, self.v_oct];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let v_in = inputs.first().copied().unwrap_or(0.0);
        let root_cv = inputs.get(1).copied().unwrap_or(0.0);
        let tune = self.tune.clamp(20.0, 4000.0);
        let root = (self.root * volts_to_ratio(root_cv)).clamp(20.0, 2000.0);
        let amount = self.amount.clamp(0.0, 1.0);
        let glide = self.glide.clamp(0.001, 0.8);

        let raw = (tune * volts_to_ratio(v_in)).clamp(20.0, 4000.0);
        self.snapped = scale::snap(raw, root, self.scale);
        let target = raw + amount * (self.snapped - raw);
        let coeff = 1.0 - (-dt / glide).exp();
        self.sung_log += coeff * (target.max(20.0).ln() - self.sung_log);
        self.hz = self.sung_log.exp().clamp(20.0, 4000.0);
        self.v_oct = clamp_rail((self.hz / tune).log2(), DEFAULT_RAIL);
        ModuleOutputs::new(&[self.v_oct, self.hz])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 48_000.0;

    fn settle(q: &mut Quant, v_oct: f32, seconds: f32) -> (f32, f32) {
        let mut out = (0.0, 0.0);
        for _ in 0..(seconds / DT) as usize {
            let y = q.step(&[v_oct, 0.0], DT);
            out = (y[0], y[1]);
        }
        out
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let q = Quant::new();
        for spec in q.params() {
            let got = q.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }

    #[test]
    fn zero_volts_stays_on_the_root() {
        let mut q = Quant::new();
        let (v, hz) = settle(&mut q, 0.0, 0.3);
        assert!(v.abs() < 0.02, "rest was {v} V");
        assert!((hz - 55.0).abs() < 1.0, "rest hz {hz}");
    }

    #[test]
    fn phrygian_snaps_the_flat_two() {
        let mut q = Quant::new();
        q.set_param("scale", 6.0).unwrap();
        q.set_param("glide", 0.008).unwrap();
        // A little over a semitone up from A is Bb, not B.
        let (_v, hz) = settle(&mut q, 1.5 / 12.0, 0.25);
        let bb = 55.0 * (1.0f32 / 12.0).exp2();
        assert!(
            (hz - bb).abs() < 0.8,
            "a +1.5 st walk snapped to {hz}, not Bb {bb}"
        );
        assert!(scale::in_scale(hz, 55.0, 6, 0.25));
    }

    #[test]
    fn amount_of_zero_is_the_raw_jack() {
        let mut q = Quant::new();
        q.set_param("amount", 0.0).unwrap();
        q.set_param("glide", 0.005).unwrap();
        let (v, _hz) = settle(&mut q, 0.3, 0.2);
        assert!((v - 0.3).abs() < 0.02, "pass-through was {v}");
    }

    #[test]
    fn a_fifth_oscillator_stays_in_a_phrygian() {
        let mut q = Quant::new();
        q.set_param("tune", 82.41).unwrap();
        q.set_param("root", 55.0).unwrap();
        q.set_param("scale", 6.0).unwrap();
        q.set_param("glide", 0.008).unwrap();
        let (_v, hz) = settle(&mut q, 0.2, 0.25);
        assert!(
            scale::in_scale(hz, 55.0, 6, 0.3),
            "E-rest + 0.2 V left A Phrygian: {hz}"
        );
    }
}
