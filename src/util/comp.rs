//! Feed-forward compressor / limiter. A utility, not a neural module.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["in"];
const OUT_JACKS: &[&str] = &["out", "gr", "env"];
const STATE_FIELDS: &[&str] = &["gain", "env", "gr"];

const PARAMS: &[ParamSpec] = params! {
    "threshold", Unit::Volt, 0.2 ..= 8.0, 2.5, Linear, "level where gain reduction starts";
    "ratio", Unit::Ratio, 1.0 ..= 20.0, 4.0, Log, "compression above the threshold; 20 is a limiter";
    "attack", Unit::Seconds, 0.0002 ..= 0.5, 0.005, Log, "how fast the detector follows a rising edge";
    "release", Unit::Seconds, 0.01 ..= 3.0, 0.15, Log, "how fast the detector falls";
    "knee", Unit::Volt, 0.0 ..= 4.0, 0.4, Linear, "soft knee width; zero is a hard corner";
    "makeup", Unit::Ratio, 0.0 ..= 4.0, 1.0, Linear, "gain after reduction";
    "mix", Unit::Ratio, 0.0 ..= 1.0, 1.0, Linear, "dry/wet; 1 is fully compressed";
};

/// Envelope follower into a downward VCA.
///
/// Ratio 1 is bypass. Ratio near 20 with a fast attack is a limiter. The GR
/// jack reports how far the VCA has closed, so it can be patched as a control.
#[derive(Clone, Debug)]
pub struct Comp {
    pub threshold: Volt,
    pub ratio: f32,
    pub attack: f32,
    pub release: f32,
    pub knee: Volt,
    pub makeup: f32,
    pub mix: f32,
    env: f32,
    gain: f32,
}

impl Default for Comp {
    fn default() -> Self {
        Self::new()
    }
}

impl Comp {
    pub fn new() -> Self {
        Self {
            threshold: 2.5,
            ratio: 4.0,
            attack: 0.005,
            release: 0.15,
            knee: 0.4,
            makeup: 1.0,
            mix: 1.0,
            env: 0.0,
            gain: 1.0,
        }
    }

    fn coeff(time: f32, dt: f32) -> f32 {
        1.0 - (-dt / time.max(1e-6)).exp()
    }

    fn reduction(env: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
        if env <= 1e-6 {
            return 1.0;
        }
        let slope = 1.0 - 1.0 / ratio.max(1.0);
        if slope <= 0.0 {
            return 1.0;
        }
        let over = env - threshold;
        if over <= 0.0 {
            return 1.0;
        }
        let used = if knee > 1e-6 && over < knee {
            slope * (over / knee).powi(2)
        } else {
            slope
        };
        (threshold / env).powf(used).clamp(0.0, 1.0)
    }
}

impl AnalogModule for Comp {
    fn kind_name(&self) -> &'static str {
        "comp"
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
            "threshold" => threshold,
            "ratio" => ratio,
            "attack" => attack,
            "release" => release,
            "knee" => knee,
            "makeup" => makeup,
            "mix" => mix,
        }
        choices {}
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.gain, self.env, (1.0 - self.gain) * 5.0];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let x = inputs.first().copied().unwrap_or(0.0);
        let level = x.abs();
        let coeff = if level > self.env {
            Self::coeff(self.attack, dt)
        } else {
            Self::coeff(self.release, dt)
        };
        self.env += (level - self.env) * coeff;
        self.gain = Self::reduction(self.env, self.threshold, self.ratio, self.knee);
        let wet = self.gain * x;
        let y = self.makeup * (self.mix * wet + (1.0 - self.mix) * x);
        let gr = (1.0 - self.gain) * 5.0;
        ModuleOutputs::new(&[
            clamp_rail(y, DEFAULT_RAIL),
            clamp_rail(gr, DEFAULT_RAIL),
            clamp_rail(self.env, DEFAULT_RAIL),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 48_000.0;

    fn comp(threshold: f32, ratio: f32) -> Comp {
        let mut c = Comp::new();
        c.set_param("threshold", threshold).unwrap();
        c.set_param("ratio", ratio).unwrap();
        c.set_param("attack", 0.001).unwrap();
        c.set_param("release", 0.05).unwrap();
        c.set_param("knee", 0.0).unwrap();
        c.set_param("makeup", 1.0).unwrap();
        c
    }

    fn settle(c: &mut Comp, x: f32, seconds: f32) -> f32 {
        let mut y = 0.0;
        for _ in 0..(seconds / DT) as usize {
            y = c.step(&[x], DT)[0];
        }
        y
    }

    #[test]
    fn below_threshold_is_unity() {
        let mut c = comp(3.0, 8.0);
        let y = settle(&mut c, 1.0, 0.05);
        assert!((y - 1.0).abs() < 0.02, "below threshold gained {y}");
    }

    #[test]
    fn above_threshold_it_turns_down() {
        let mut c = comp(2.0, 8.0);
        let y = settle(&mut c, 6.0, 0.08);
        assert!(y < 4.0, "6 V only came down to {y}");
        assert!(y > 1.5, "it crushed a 6 V tone to {y}");
    }

    #[test]
    fn a_limiter_holds_near_the_threshold() {
        let mut c = comp(2.0, 20.0);
        let y = settle(&mut c, 8.0, 0.1);
        assert!(
            (y - 2.0).abs() < 0.6,
            "limiter let 8 V become {y} (want near 2)"
        );
    }

    #[test]
    fn higher_ratio_reduces_more() {
        let soft = settle(&mut comp(2.0, 2.0), 6.0, 0.08);
        let hard = settle(&mut comp(2.0, 12.0), 6.0, 0.08);
        assert!(hard < soft * 0.9, "ratio did nothing: {soft} vs {hard}");
    }

    #[test]
    fn mix_of_zero_is_dry() {
        let mut c = comp(1.0, 20.0);
        c.set_param("mix", 0.0).unwrap();
        let y = settle(&mut c, 5.0, 0.05);
        assert!((y - 5.0).abs() < 0.05, "dry mix changed the signal: {y}");
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let c = Comp::new();
        for spec in c.params() {
            let got = c.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
