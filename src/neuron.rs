//! Continuous analog neuron: weighted inputs, leaky membrane, smooth saturation.

use crate::integrate::leaky_integrate;
use crate::module::{find_jack, AnalogModule, ModuleOutputs};
use crate::nonlinearity::Nonlinearity;
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const IN_JACKS: [&str; 4] = ["in0", "in1", "in2", "in3"];
const OUT_JACKS: [&str; 1] = ["out"];

/// Knob / component characteristics.
#[derive(Clone, Copy, Debug)]
pub struct NeuronParams {
    pub weights: [f32; 4],
    pub bias: Volt,
    pub leak: f32,
    pub gain: f32,
    pub nonlinearity: Nonlinearity,
    /// Reserved. Stay zero in v1.
    pub noise: f32,
}

impl Default for NeuronParams {
    fn default() -> Self {
        Self {
            weights: [1.0, 0.0, 0.0, 0.0],
            bias: 0.0,
            leak: 8.0,
            gain: 5.0,
            nonlinearity: Nonlinearity::Tanh { response: 0.6 },
            noise: 0.0,
        }
    }
}

impl NeuronParams {
    pub fn vary(mut self, variation: InstanceVariation) -> Self {
        for (i, w) in self.weights.iter_mut().enumerate() {
            *w = variation.scale_param(i as u32, *w);
        }
        self.bias += variation.offset(10);
        self.leak = variation.scale_param(11, self.leak).max(0.0);
        self.gain = variation.scale_param(12, self.gain);
        self
    }
}

/// Persistent membrane voltage.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeuronState {
    pub v: Volt,
}

/// Four-input leaky analog neuron.
#[derive(Clone, Debug)]
pub struct Neuron {
    pub params: NeuronParams,
    pub state: NeuronState,
}

impl Neuron {
    pub fn new(params: NeuronParams) -> Self {
        Self::instantiate(params, InstanceVariation::IDENTITY)
    }

    pub fn instantiate(params: NeuronParams, variation: InstanceVariation) -> Self {
        Self {
            params: params.vary(variation),
            state: NeuronState::default(),
        }
    }

    pub fn drive(&self, inputs: &[Volt]) -> Volt {
        let mut sum = self.params.bias;
        for i in 0..4 {
            let x = inputs.get(i).copied().unwrap_or(0.0);
            sum += x * self.params.weights[i];
        }
        sum
    }
}

impl AnalogModule for Neuron {
    fn input_count(&self) -> usize {
        4
    }

    fn output_count(&self) -> usize {
        1
    }

    fn jack_in(&self, name: &str) -> Option<usize> {
        find_jack(&IN_JACKS, name)
    }

    fn jack_out(&self, name: &str) -> Option<usize> {
        find_jack(&OUT_JACKS, name)
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let drive = self.drive(inputs);
        self.state.v = leaky_integrate(self.state.v, drive, self.params.leak, dt);
        self.state.v = clamp_rail(self.state.v, DEFAULT_RAIL);
        let out = self.params.gain * self.params.nonlinearity.apply(self.state.v);
        ModuleOutputs::one(clamp_rail(out, DEFAULT_RAIL))
    }

    fn snapshot(&self) -> Vec<(&'static str, Volt)> {
        vec![("v", self.state.v)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_drive_leaks_toward_zero() {
        let mut n = Neuron::new(NeuronParams {
            leak: 20.0,
            ..NeuronParams::default()
        });
        n.state.v = 3.0;
        for _ in 0..4000 {
            n.step(&[0.0, 0.0, 0.0, 0.0], 0.0005);
        }
        assert!(n.state.v.abs() < 0.05, "v={}", n.state.v);
    }

    #[test]
    fn large_drive_saturates_output() {
        let mut n = Neuron::new(NeuronParams {
            weights: [1.0, 0.0, 0.0, 0.0],
            bias: 0.0,
            leak: 5.0,
            gain: 5.0,
            nonlinearity: Nonlinearity::Tanh { response: 2.0 },
            noise: 0.0,
        });
        let mut last = 0.0;
        for _ in 0..4000 {
            last = n.step(&[8.0, 0.0, 0.0, 0.0], 0.0005)[0];
        }
        assert!(last > 4.5, "saturated out={last}");
        assert!(last <= 5.0 + 1e-3);
    }

    #[test]
    fn negative_weight_inverts() {
        let mut n = Neuron::new(NeuronParams {
            weights: [-1.0, 0.0, 0.0, 0.0],
            ..NeuronParams::default()
        });
        let mut last = 0.0;
        for _ in 0..2000 {
            last = n.step(&[3.0, 0.0, 0.0, 0.0], 0.0005)[0];
        }
        assert!(last < 0.0, "out={last}");
    }
}
