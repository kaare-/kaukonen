//! Noise source. A utility, not one of the neural modules.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::noise::Rng;
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &[];
const OUT_JACKS: &[&str] = &["white", "pink"];
const STATE_FIELDS: &[&str] = &["white", "pink"];

const PARAMS: &[ParamSpec] = params! {
    "level", Unit::Volt, 0.0 ..= 10.0, 5.0, Linear, "output amplitude";
};

/// White and pink noise from one deterministic stream.
///
/// Seeded from the patch, like every other imperfection in the rack, so a run
/// with noise in it still repeats exactly.
#[derive(Clone, Debug)]
pub struct Noise {
    pub level: Volt,
    rng: Rng,
    /// Kellett's three-pole approximation of a -3 dB/octave slope.
    poles: [f32; 3],
    last: [Volt; 2],
}

impl Default for Noise {
    fn default() -> Self {
        Self::new()
    }
}

impl Noise {
    pub fn new() -> Self {
        Self {
            level: 5.0,
            rng: Rng::new(0),
            poles: [0.0; 3],
            last: [0.0; 2],
        }
    }
}

impl AnalogModule for Noise {
    fn kind_name(&self) -> &'static str {
        "noise"
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
            "level" => level,
        }
        choices {}
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn apply_variation(&mut self, variation: InstanceVariation) {
        self.rng = Rng::new(variation.seed);
    }

    fn read_state(&self, out: &mut [Volt]) {
        for (slot, value) in out.iter_mut().zip(self.last) {
            *slot = value;
        }
    }

    fn step(&mut self, _inputs: &[Volt], _dt: f32) -> ModuleOutputs {
        let white = self.rng.bipolar();
        // Three one-poles at decade spacing sum to a fair -3 dB/octave slope.
        self.poles[0] = 0.99765 * self.poles[0] + white * 0.0990460;
        self.poles[1] = 0.96300 * self.poles[1] + white * 0.2965164;
        self.poles[2] = 0.57000 * self.poles[2] + white * 1.0526913;
        let pink = (self.poles[0] + self.poles[1] + self.poles[2] + white * 0.1848) / 3.0;

        self.last = [
            clamp_rail(self.level * white, DEFAULT_RAIL),
            clamp_rail(self.level * pink, DEFAULT_RAIL),
        ];
        ModuleOutputs::new(&self.last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(noise: &mut Noise, jack: usize, count: usize) -> Vec<f32> {
        (0..count).map(|_| noise.step(&[], 0.001)[jack]).collect()
    }

    fn rms(values: &[f32]) -> f32 {
        (values.iter().map(|v| v * v).sum::<f32>() / values.len() as f32).sqrt()
    }

    #[test]
    fn white_noise_fills_its_level() {
        let mut noise = Noise::new();
        let xs = collect(&mut noise, 0, 20_000);
        // Uniform over the level has RMS level/sqrt(3).
        assert!((rms(&xs) - 5.0 / 1.732).abs() < 0.2, "rms {}", rms(&xs));
        assert!(xs.iter().all(|v| v.abs() <= 5.0));
    }

    #[test]
    fn pink_noise_has_more_energy_down_low() {
        let mut noise = Noise::new();
        let pink = collect(&mut noise, 1, 40_000);
        // A pink spectrum correlates step to step; white does not.
        let correlation = |xs: &[f32]| {
            let pairs: f32 = xs.windows(2).map(|w| w[0] * w[1]).sum();
            pairs / (xs.len() as f32 * rms(xs) * rms(xs))
        };
        let mut white_noise = Noise::new();
        let white = collect(&mut white_noise, 0, 40_000);
        assert!(
            correlation(&pink) > 0.3,
            "pink was not correlated: {}",
            correlation(&pink)
        );
        assert!(
            correlation(&white).abs() < 0.05,
            "white was correlated: {}",
            correlation(&white)
        );
    }

    #[test]
    fn instances_are_seeded_from_the_patch() {
        let make = |seed| {
            let mut noise = Noise::new();
            noise.apply_variation(InstanceVariation::new(seed, 0.0));
            collect(&mut noise, 0, 32)
        };
        assert_eq!(make(1), make(1));
        assert_ne!(make(1), make(2));
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let noise = Noise::new();
        for spec in noise.params() {
            let got = noise.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
