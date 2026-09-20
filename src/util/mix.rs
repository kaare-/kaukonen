//! Four-input attenuverting mixer with an offset. A utility module.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["in0", "in1", "in2", "in3"];
const OUT_JACKS: &[&str] = &["out", "inv"];
const STATE_FIELDS: &[&str] = &["out"];

const PARAMS: &[ParamSpec] = params! {
    "a0", Unit::Ratio, -2.0 ..= 2.0, 1.0, Linear, "attenuverter on in0; negative inverts";
    "a1", Unit::Ratio, -2.0 ..= 2.0, 1.0, Linear, "attenuverter on in1";
    "a2", Unit::Ratio, -2.0 ..= 2.0, 1.0, Linear, "attenuverter on in2";
    "a3", Unit::Ratio, -2.0 ..= 2.0, 1.0, Linear, "attenuverter on in3";
    "offset", Unit::Volt, -10.0 ..= 10.0, 0.0, Linear, "constant voltage added to the sum";
    "level", Unit::Ratio, 0.0 ..= 4.0, 1.0, Linear, "output scale";
};

/// Scales, inverts, offsets and sums. The CV utility the rack was missing.
#[derive(Clone, Debug)]
pub struct Mix {
    pub a0: f32,
    pub a1: f32,
    pub a2: f32,
    pub a3: f32,
    pub offset: Volt,
    pub level: f32,
    last: Volt,
}

impl Default for Mix {
    fn default() -> Self {
        Self::new()
    }
}

impl Mix {
    pub fn new() -> Self {
        Self {
            a0: 1.0,
            a1: 1.0,
            a2: 1.0,
            a3: 1.0,
            offset: 0.0,
            level: 1.0,
            last: 0.0,
        }
    }
}

impl AnalogModule for Mix {
    fn kind_name(&self) -> &'static str {
        "mix"
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
            "a0" => a0,
            "a1" => a1,
            "a2" => a2,
            "a3" => a3,
            "offset" => offset,
            "level" => level,
        }
        choices {}
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        if let Some(slot) = out.first_mut() {
            *slot = self.last;
        }
    }

    fn step(&mut self, inputs: &[Volt], _dt: f32) -> ModuleOutputs {
        let gains = [self.a0, self.a1, self.a2, self.a3];
        let mut sum = self.offset;
        for (x, gain) in inputs.iter().zip(gains) {
            sum += x * gain;
        }
        self.last = clamp_rail(self.level * sum, DEFAULT_RAIL);
        ModuleOutputs::new(&[self.last, -self.last])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_sums_its_inputs() {
        let mut mix = Mix::new();
        let out = mix.step(&[1.0, 2.0, 3.0, -1.0], 0.001);
        assert!((out[0] - 5.0).abs() < 1e-6);
        assert!((out[1] + 5.0).abs() < 1e-6, "inverted output");
    }

    #[test]
    fn attenuverters_scale_and_invert() {
        let mut mix = Mix::new();
        mix.set_param("a0", 0.5).unwrap();
        mix.set_param("a1", -1.0).unwrap();
        mix.set_param("a2", 0.0).unwrap();
        mix.set_param("a3", 0.0).unwrap();
        let out = mix.step(&[4.0, 3.0, 99.0, 99.0], 0.001)[0];
        assert!((out - (-1.0)).abs() < 1e-6, "got {out}");
    }

    #[test]
    fn the_offset_knob_makes_it_a_voltage_source() {
        let mut mix = Mix::new();
        mix.set_param("offset", 2.5).unwrap();
        assert!((mix.step(&[], 0.001)[0] - 2.5).abs() < 1e-6);
    }

    #[test]
    fn the_output_respects_the_rail() {
        let mut mix = Mix::new();
        mix.set_param("offset", 10.0).unwrap();
        let out = mix.step(&[9.0, 9.0, 9.0, 9.0], 0.001)[0];
        assert!((out - DEFAULT_RAIL).abs() < 1e-6, "got {out}");
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let mix = Mix::new();
        for spec in mix.params() {
            let got = mix.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
