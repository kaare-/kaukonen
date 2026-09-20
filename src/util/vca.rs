//! Voltage-controlled amplifier. A utility, not one of the neural modules.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["in", "cv"];
const OUT_JACKS: &[&str] = &["out"];
const STATE_FIELDS: &[&str] = &["gain"];

/// How control voltage maps to gain.
pub const RESPONSES: &[&str] = &["linear", "exponential"];

/// Control voltage that means fully open.
const FULL_SCALE_CV: f32 = 5.0;

/// Decades of range an exponential VCA covers before it shuts off.
const EXP_DECADES: f32 = 3.0;

const PARAMS: &[ParamSpec] = params! {
    "gain", Unit::Ratio, 0.0 ..= 2.0, 0.0, Linear, "offset gain with no CV patched";
    "cv_depth", Unit::Ratio, -2.0 ..= 2.0, 1.0, Linear, "gain per 5 V on the CV jack";
    "response", Unit::Enum(RESPONSES), 0.0 ..= 1.0, 0.0, Linear, "how CV maps to gain";
    "level", Unit::Ratio, 0.0 ..= 4.0, 1.0, Linear, "output scale";
};

/// Multiplies a signal by a control voltage.
#[derive(Clone, Debug)]
pub struct Vca {
    pub gain: f32,
    pub cv_depth: f32,
    pub response: usize,
    pub level: f32,
    last_gain: f32,
}

impl Default for Vca {
    fn default() -> Self {
        Self::new()
    }
}

impl Vca {
    pub fn new() -> Self {
        Self {
            gain: 0.0,
            cv_depth: 1.0,
            response: 0,
            level: 1.0,
            last_gain: 0.0,
        }
    }

    /// Gain for a given control amount, where 1.0 is fully open.
    fn curve(&self, amount: f32) -> f32 {
        if amount <= 0.0 {
            return 0.0;
        }
        match self.response {
            // Exponential is what a real VCA does and what suits an envelope;
            // it reaches unity at 1.0 and falls away by decades below that.
            1 => (EXP_DECADES * (amount - 1.0) * std::f32::consts::LN_10).exp(),
            _ => amount,
        }
    }
}

impl AnalogModule for Vca {
    fn kind_name(&self) -> &'static str {
        "vca"
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
            "gain" => gain,
            "cv_depth" => cv_depth,
            "level" => level,
        }
        choices {
            "response" => response,
        }
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        if let Some(slot) = out.first_mut() {
            *slot = self.last_gain;
        }
    }

    fn step(&mut self, inputs: &[Volt], _dt: f32) -> ModuleOutputs {
        let x = inputs.first().copied().unwrap_or(0.0);
        let cv = inputs.get(1).copied().unwrap_or(0.0);
        let amount = self.gain + self.cv_depth * cv / FULL_SCALE_CV;
        self.last_gain = self.curve(amount);
        ModuleOutputs::one(clamp_rail(self.level * self.last_gain * x, DEFAULT_RAIL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_vca_is_silent() {
        let mut vca = Vca::new();
        assert_eq!(vca.step(&[5.0, 0.0], 0.001)[0], 0.0);
    }

    #[test]
    fn linear_response_is_proportional_to_cv() {
        let mut vca = Vca::new();
        for (cv, expected) in [(5.0, 4.0), (2.5, 2.0), (0.0, 0.0)] {
            let out = vca.step(&[4.0, cv], 0.001)[0];
            assert!((out - expected).abs() < 1e-4, "cv {cv} gave {out}");
        }
    }

    #[test]
    fn negative_cv_does_not_invert_the_signal() {
        let mut vca = Vca::new();
        assert_eq!(vca.step(&[4.0, -5.0], 0.001)[0], 0.0);
    }

    #[test]
    fn exponential_response_reaches_unity_and_falls_by_decades() {
        let mut vca = Vca::new();
        vca.set_param("response", 1.0).unwrap();
        let full = vca.step(&[1.0, 5.0], 0.001)[0];
        let half = vca.step(&[1.0, 2.5], 0.001)[0];
        assert!((full - 1.0).abs() < 1e-4, "full open gave {full}");
        // Half the CV is 1.5 decades down.
        assert!(
            (half - 10f32.powf(-1.5)).abs() < 1e-3,
            "half open gave {half}"
        );
    }

    #[test]
    fn the_offset_knob_opens_it_without_cv() {
        let mut vca = Vca::new();
        vca.set_param("gain", 0.5).unwrap();
        assert!((vca.step(&[2.0, 0.0], 0.001)[0] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn gain_is_reported_as_state() {
        let mut vca = Vca::new();
        vca.step(&[1.0, 2.5], 0.001);
        let snapshot = vca.snapshot();
        assert!((snapshot[0].1 - 0.5).abs() < 1e-4);
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let vca = Vca::new();
        for spec in vca.params() {
            let got = vca.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
