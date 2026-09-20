//! TC-3: three-channel plastic analog connectome.

use crate::integrate::leaky_integrate;
use crate::module::{find_jack, AnalogModule, ModuleOutputs};
use crate::plasticity::{Plasticity, PlasticityRule};
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const IN_JACKS: [&str; 5] = ["a", "b", "c", "learn", "forget"];
const OUT_JACKS: [&str; 4] = ["a", "b", "c", "activity"];

#[derive(Clone, Copy, Debug)]
pub struct Tc3Params {
    pub natural_decay: f32,
    pub w_min: Volt,
    pub w_max: Volt,
    /// Scales Σ|dw/dt| into a patchable voltage.
    pub activity_gain: f32,
    pub plasticity: Plasticity,
    /// Reserved. Stay zero in v1.
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
            weights: [
                [0.35, 0.08, -0.05],
                [0.10, 0.30, 0.06],
                [-0.04, 0.12, 0.28],
            ],
            last_activity: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Tc3 {
    pub params: Tc3Params,
    pub state: Tc3State,
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
        }
    }

    pub fn mix(&self, inputs: [Volt; 3]) -> [Volt; 3] {
        let mut out = [0.0; 3];
        for j in 0..3 {
            for i in 0..3 {
                out[j] += inputs[i] * self.state.weights[i][j];
            }
        }
        out
    }
}

impl AnalogModule for Tc3 {
    fn input_count(&self) -> usize {
        5
    }

    fn output_count(&self) -> usize {
        4
    }

    fn jack_in(&self, name: &str) -> Option<usize> {
        find_jack(&IN_JACKS, name)
    }

    fn jack_out(&self, name: &str) -> Option<usize> {
        find_jack(&OUT_JACKS, name)
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let a = inputs.first().copied().unwrap_or(0.0);
        let b = inputs.get(1).copied().unwrap_or(0.0);
        let c = inputs.get(2).copied().unwrap_or(0.0);
        let learn = inputs.get(3).copied().unwrap_or(0.0);
        let forget = inputs.get(4).copied().unwrap_or(0.0);
        let ins = [a, b, c];
        let outs = self.mix(ins);

        let mut activity = 0.0;
        for i in 0..3 {
            for j in 0..3 {
                let w = self.state.weights[i][j];
                let plastic = self
                    .params
                    .plasticity
                    .dw(ins[i], outs[j], w, learn, forget);
                let dw = plastic - self.params.natural_decay * w;
                activity += dw.abs();
                // Exact leak toward the instantaneous plastic drive.
                let next = leaky_integrate(w, plastic, self.params.natural_decay, dt);
                self.state.weights[i][j] = next.clamp(self.params.w_min, self.params.w_max);
            }
        }
        let activity_v = clamp_rail(self.params.activity_gain * activity, DEFAULT_RAIL);
        self.state.last_activity = activity_v;

        ModuleOutputs::new(&[
            clamp_rail(outs[0], DEFAULT_RAIL),
            clamp_rail(outs[1], DEFAULT_RAIL),
            clamp_rail(outs[2], DEFAULT_RAIL),
            activity_v,
        ])
    }

    fn snapshot(&self) -> Vec<(&'static str, Volt)> {
        vec![
            ("w00", self.state.weights[0][0]),
            ("w01", self.state.weights[0][1]),
            ("w02", self.state.weights[0][2]),
            ("w10", self.state.weights[1][0]),
            ("w11", self.state.weights[1][1]),
            ("w12", self.state.weights[1][2]),
            ("w20", self.state.weights[2][0]),
            ("w21", self.state.weights[2][1]),
            ("w22", self.state.weights[2][2]),
            ("activity", self.state.last_activity),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_are_linear_mix() {
        let mut m = Tc3::new(Tc3Params::default());
        m.state.weights = [
            [1.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.0, 0.0, -1.0],
        ];
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
}
