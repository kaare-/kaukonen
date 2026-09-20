//! TC-3: three-channel plastic analog connectome.

use crate::integrate::leaky_integrate;
use crate::module::{AnalogModule, ModuleOutputs};
use crate::noise::{ou_increment, Rng};
use crate::param::{self, params, ParamError, ParamSpec, Unit};
use crate::plasticity::{Plasticity, PlasticityRule};
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["a", "b", "c", "learn", "forget"];
const OUT_JACKS: &[&str] = &["a", "b", "c", "activity"];
const STATE_FIELDS: &[&str] = &[
    "w00", "w01", "w02", "w10", "w11", "w12", "w20", "w21", "w22", "activity",
];

const PARAMS: &[ParamSpec] = params! {
    "natural_decay", Unit::Rate, 0.0 ..= 10.0, 0.02, Linear, "passive leak of every stored weight toward zero";
    "w_min", Unit::Ratio, -10.0 ..= 0.0, -2.0, Linear, "lower end stop of the analog weight memory";
    "w_max", Unit::Ratio, 0.0 ..= 10.0, 2.0, Linear, "upper end stop of the analog weight memory";
    "activity_gain", Unit::Ratio, 0.0 ..= 10.0, 0.05, Linear, "scales the sum of |dw/dt| into the ACTIVITY jack";
    "drift", Unit::Ratio, 0.0 ..= 1.0, 0.0, Linear, "RMS wander of each stored weight, held in check by natural_decay";
    "rule", Unit::Enum(Plasticity::RULES), 0.0 ..= 0.0, 0.0, Linear, "local plasticity rule";
    "deadzone", Unit::Ratio, 0.0 ..= 1.0, 0.0001, Linear, "ignore input*output products below this magnitude";
};

#[derive(Clone, Copy, Debug)]
pub struct Tc3Params {
    pub natural_decay: f32,
    pub w_min: Volt,
    pub w_max: Volt,
    /// Scales Σ|dw/dt| into a patchable voltage.
    pub activity_gain: f32,
    pub plasticity: Plasticity,
    /// RMS wander of each stored weight. Zero means a mathematically ideal
    /// memory; anything else makes the connectome slowly forget where it was.
    pub drift: f32,
}

impl Default for Tc3Params {
    fn default() -> Self {
        Self {
            natural_decay: 0.02,
            w_min: -2.0,
            w_max: 2.0,
            activity_gain: 0.05,
            plasticity: Plasticity::default(),
            drift: 0.0,
        }
    }
}

impl Tc3Params {
    pub fn vary(mut self, variation: InstanceVariation) -> Self {
        self.natural_decay = variation.scale_param(1, self.natural_decay).max(0.0);
        self.activity_gain = variation.scale_param(2, self.activity_gain);
        // Even the end stops of an analog memory are a component tolerance.
        self.w_max = variation.scale_param(3, self.w_max);
        self.w_min = variation.scale_param(4, self.w_min);
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Tc3State {
    pub weights: [[Volt; 3]; 3],
    pub last_activity: Volt,
}

impl Default for Tc3State {
    fn default() -> Self {
        Self {
            weights: [[0.35, 0.08, -0.05], [0.10, 0.30, 0.06], [-0.04, 0.12, 0.28]],
            last_activity: 0.0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Tc3 {
    pub params: Tc3Params,
    pub state: Tc3State,
    rng: Rng,
}

impl Tc3 {
    pub fn new(params: Tc3Params) -> Self {
        Self::instantiate(params, InstanceVariation::IDENTITY)
    }

    pub fn instantiate(params: Tc3Params, variation: InstanceVariation) -> Self {
        let mut state = Tc3State::default();
        if !variation.is_identity() {
            for i in 0..3 {
                for j in 0..3 {
                    state.weights[i][j] += variation.offset((i * 3 + j) as u32) * 0.05;
                }
            }
        }
        Self {
            params: params.vary(variation),
            state,
            rng: Rng::new(variation.seed),
        }
    }

    pub fn mix(&self, inputs: [Volt; 3]) -> [Volt; 3] {
        let mut out = [0.0; 3];
        for (x, row) in inputs.iter().zip(&self.state.weights) {
            for (slot, w) in out.iter_mut().zip(row) {
                *slot += x * w;
            }
        }
        out
    }
}

impl AnalogModule for Tc3 {
    fn kind_name(&self) -> &'static str {
        "tc3"
    }

    fn in_jacks(&self) -> &'static [&'static str] {
        IN_JACKS
    }

    fn out_jacks(&self) -> &'static [&'static str] {
        OUT_JACKS
    }

    fn params(&self) -> &'static [ParamSpec] {
        PARAMS
    }

    fn get_param(&self, name: &str) -> Option<f32> {
        let p = &self.params;
        Some(match name {
            "natural_decay" => p.natural_decay,
            "w_min" => p.w_min,
            "w_max" => p.w_max,
            "activity_gain" => p.activity_gain,
            "drift" => p.drift,
            "rule" => p.plasticity.rule() as f32,
            "deadzone" => p.plasticity.deadzone(),
            _ => return None,
        })
    }

    fn set_param(&mut self, name: &str, value: f32) -> Result<(), ParamError> {
        let spec = param::find_spec(PARAMS, name).ok_or_else(|| self.unknown_param(name))?;
        let v = param::checked(spec, value)?;
        let rule = self.params.plasticity.rule();
        let deadzone = self.params.plasticity.deadzone();
        let p = &mut self.params;
        match name {
            "natural_decay" => p.natural_decay = v,
            "w_min" => p.w_min = v,
            "w_max" => p.w_max = v,
            "activity_gain" => p.activity_gain = v,
            "drift" => p.drift = v,
            "rule" => p.plasticity = Plasticity::from_parts(v as usize, deadzone),
            "deadzone" => p.plasticity = Plasticity::from_parts(rule, v),
            _ => return Err(self.unknown_param(name)),
        }
        Ok(())
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn apply_variation(&mut self, variation: InstanceVariation) {
        *self = Self::instantiate(self.params, variation);
    }

    fn read_state(&self, out: &mut [Volt]) {
        let w = &self.state.weights;
        let values = [
            w[0][0],
            w[0][1],
            w[0][2],
            w[1][0],
            w[1][1],
            w[1][2],
            w[2][0],
            w[2][1],
            w[2][2],
            self.state.last_activity,
        ];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let a = inputs.first().copied().unwrap_or(0.0);
        let b = inputs.get(1).copied().unwrap_or(0.0);
        let c = inputs.get(2).copied().unwrap_or(0.0);
        let learn = inputs.get(3).copied().unwrap_or(0.0);
        let forget = inputs.get(4).copied().unwrap_or(0.0);
        let ins = [a, b, c];
        let outs = self.mix(ins);

        let p = self.params;
        let rng = &mut self.rng;
        let mut activity = 0.0;
        for (input, row) in ins.iter().zip(&mut self.state.weights) {
            for (output, w) in outs.iter().zip(row.iter_mut()) {
                let plastic = p.plasticity.dw(*input, *output, *w, learn, forget);
                let dw = plastic - p.natural_decay * *w;
                activity += dw.abs();
                // Exact leak toward the instantaneous plastic drive, then the
                // memory's own imperfection on top. The decay is already the
                // relaxation that keeps the wander bounded.
                let next = leaky_integrate(*w, plastic, p.natural_decay, dt);
                let wander = ou_increment(rng, p.drift, p.natural_decay, dt);
                *w = (next + wander).clamp(p.w_min, p.w_max);
            }
        }
        let activity_v = clamp_rail(p.activity_gain * activity, DEFAULT_RAIL);
        self.state.last_activity = activity_v;

        ModuleOutputs::new(&[
            clamp_rail(outs[0], DEFAULT_RAIL),
            clamp_rail(outs[1], DEFAULT_RAIL),
            clamp_rail(outs[2], DEFAULT_RAIL),
            activity_v,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_are_linear_mix() {
        let mut m = Tc3::new(Tc3Params::default());
        m.state.weights = [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, -1.0]];
        let y = m.step(&[1.0, 0.5, 0.25, 0.0, 0.0], 0.0005);
        assert!((y[0] - 1.0).abs() < 1e-5);
        assert!((y[1] - 1.0).abs() < 1e-5);
        assert!((y[2] + 0.25).abs() < 1e-5);
    }

    #[test]
    fn learn_grows_coincident_weight() {
        let mut m = Tc3::new(Tc3Params {
            natural_decay: 0.0,
            ..Tc3Params::default()
        });
        m.state.weights = [[0.2, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]];
        let before = m.state.weights[0][0];
        for _ in 0..2000 {
            m.step(&[2.0, 0.0, 0.0, 1.0, 0.0], 0.0005);
        }
        assert!(
            m.state.weights[0][0] > before + 0.05,
            "w00 {} did not grow from {before}",
            m.state.weights[0][0]
        );
    }

    #[test]
    fn forget_and_decay_shrink_weight() {
        let mut m = Tc3::new(Tc3Params {
            natural_decay: 2.0,
            ..Tc3Params::default()
        });
        m.state.weights = [[1.2, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]];
        for _ in 0..4000 {
            m.step(&[0.0, 0.0, 0.0, 0.0, 3.0], 0.0005);
        }
        assert!(
            m.state.weights[0][0].abs() < 0.2,
            "w00 still {}",
            m.state.weights[0][0]
        );
    }

    #[test]
    fn state_fields_track_the_weight_matrix() {
        let mut m = Tc3::default();
        m.state.weights[1][2] = 0.77;
        let snapshot = m.snapshot();
        let w12 = snapshot.iter().find(|(k, _)| *k == "w12").unwrap().1;
        assert!((w12 - 0.77).abs() < 1e-6);
    }

    #[test]
    fn drift_makes_the_memory_wander_and_zero_holds_it() {
        let quiet = |drift| Tc3Params {
            natural_decay: 0.5,
            drift,
            ..Tc3Params::default()
        };
        let mut ideal = Tc3::instantiate(quiet(0.0), InstanceVariation::new(2, 0.0));
        let mut drifting = Tc3::instantiate(quiet(0.3), InstanceVariation::new(2, 0.0));
        let start = ideal.state.weights[0][0];
        let mut wander: f32 = 0.0;
        for _ in 0..40_000 {
            // No inputs, so nothing but decay and imperfection moves a weight.
            ideal.step(&[0.0; 5], 0.0005);
            drifting.step(&[0.0; 5], 0.0005);
            wander = wander.max((drifting.state.weights[0][0] - ideal.state.weights[0][0]).abs());
        }
        assert!(
            ideal.state.weights[0][0].abs() < 1e-3,
            "ideal weight should have decayed to zero from {start}, got {}",
            ideal.state.weights[0][0]
        );
        assert!(wander > 0.05, "memory barely drifted: {wander}");
        assert!(
            drifting.state.weights[0][0].abs() < drifting.params.w_max,
            "drift pinned the weight at its end stop"
        );
    }

    #[test]
    fn mismatch_individualizes_the_end_stops() {
        let a = Tc3::instantiate(Tc3Params::default(), InstanceVariation::new(1, 0.1));
        let b = Tc3::instantiate(Tc3Params::default(), InstanceVariation::new(2, 0.1));
        assert_ne!(a.params.w_max, b.params.w_max);
        assert_ne!(a.state.weights[0][0], b.state.weights[0][0]);
        let ideal = Tc3::instantiate(Tc3Params::default(), InstanceVariation::new(1, 0.0));
        assert_eq!(ideal.params.w_max, Tc3Params::default().w_max);
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let m = Tc3::default();
        for spec in m.params() {
            let got = m.get_param(spec.name).expect(spec.name);
            assert!(
                (got - spec.default).abs() < 1e-9,
                "{} default {} but module has {got}",
                spec.name,
                spec.default
            );
        }
    }
}
