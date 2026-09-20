//! Continuous analog neuron: weighted inputs, leaky membrane, smooth saturation.

use crate::integrate::leaky_integrate;
use crate::module::{AnalogModule, ModuleOutputs};
use crate::noise::{ou_increment, Rng};
use crate::nonlinearity::Nonlinearity;
use crate::param::{self, params, ParamError, ParamSpec, Unit};
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["in0", "in1", "in2", "in3"];
const OUT_JACKS: &[&str] = &["out"];
const STATE_FIELDS: &[&str] = &["v"];

const PARAMS: &[ParamSpec] = params! {
    "w0", Unit::Ratio, -4.0 ..= 4.0, 1.0, Linear, "signed weight on in0";
    "w1", Unit::Ratio, -4.0 ..= 4.0, 0.0, Linear, "signed weight on in1";
    "w2", Unit::Ratio, -4.0 ..= 4.0, 0.0, Linear, "signed weight on in2";
    "w3", Unit::Ratio, -4.0 ..= 4.0, 0.0, Linear, "signed weight on in3";
    "bias", Unit::Volt, -10.0 ..= 10.0, 0.0, Linear, "constant drive added to the weighted sum";
    "leak", Unit::Rate, 0.0 ..= 500.0, 8.0, Linear, "membrane leak; 1/tau of the integration";
    "gain", Unit::Ratio, 0.0 ..= 10.0, 5.0, Linear, "output scale after saturation";
    "shape", Unit::Enum(Nonlinearity::SHAPES), 0.0 ..= 2.0, 0.0, Linear, "saturation curve";
    "response", Unit::Ratio, 0.01 ..= 20.0, 0.6, Log, "slope into the saturation curve";
    "asym", Unit::Ratio, 0.05 ..= 20.0, 1.0, Log, "negative-slope ratio; 1.0 is symmetric";
    "noise", Unit::Volt, 0.0 ..= 1.0, 0.0, Linear, "RMS of the thermal fluctuation on the membrane, independent of dt";
};

/// Knob / component characteristics.
#[derive(Clone, Copy, Debug)]
pub struct NeuronParams {
    pub weights: [f32; 4],
    pub bias: Volt,
    pub leak: f32,
    pub gain: f32,
    pub nonlinearity: Nonlinearity,
    /// Thermal noise RMS in volts. Zero means a mathematically ideal module.
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
        // Saturation is a component characteristic too, so no two neurons
        // clip at quite the same place. The asymmetry only bites on a shape
        // that has separate slopes to begin with.
        self.nonlinearity = Nonlinearity::from_parts(
            self.nonlinearity.shape(),
            variation.scale_param(13, self.nonlinearity.response()),
            variation.scale_param(14, self.nonlinearity.asymmetry()),
        );
        self
    }
}

/// Persistent membrane voltage.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeuronState {
    pub v: Volt,
}

/// Four-input leaky analog neuron.
#[derive(Clone, Debug, Default)]
pub struct Neuron {
    pub params: NeuronParams,
    pub state: NeuronState,
    rng: Rng,
}

impl Neuron {
    pub fn new(params: NeuronParams) -> Self {
        Self::instantiate(params, InstanceVariation::IDENTITY)
    }

    pub fn instantiate(params: NeuronParams, variation: InstanceVariation) -> Self {
        Self {
            params: params.vary(variation),
            state: NeuronState::default(),
            rng: Rng::new(variation.seed),
        }
    }

    pub fn drive(&self, inputs: &[Volt]) -> Volt {
        let mut sum = self.params.bias;
        for (x, w) in inputs.iter().zip(&self.params.weights) {
            sum += x * w;
        }
        sum
    }
}

impl AnalogModule for Neuron {
    fn kind_name(&self) -> &'static str {
        "neuron"
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
        Some(match name {
            "w0" => self.params.weights[0],
            "w1" => self.params.weights[1],
            "w2" => self.params.weights[2],
            "w3" => self.params.weights[3],
            "bias" => self.params.bias,
            "leak" => self.params.leak,
            "gain" => self.params.gain,
            "shape" => self.params.nonlinearity.shape() as f32,
            "response" => self.params.nonlinearity.response(),
            "asym" => self.params.nonlinearity.asymmetry(),
            "noise" => self.params.noise,
            _ => return None,
        })
    }

    fn set_param(&mut self, name: &str, value: f32) -> Result<(), ParamError> {
        let spec = param::find_spec(PARAMS, name).ok_or_else(|| self.unknown_param(name))?;
        let v = param::checked(spec, value)?;
        let shape = self.params.nonlinearity.shape();
        let response = self.params.nonlinearity.response();
        let asym = self.params.nonlinearity.asymmetry();
        match name {
            "w0" => self.params.weights[0] = v,
            "w1" => self.params.weights[1] = v,
            "w2" => self.params.weights[2] = v,
            "w3" => self.params.weights[3] = v,
            "bias" => self.params.bias = v,
            "leak" => self.params.leak = v,
            "gain" => self.params.gain = v,
            "shape" => {
                self.params.nonlinearity = Nonlinearity::from_parts(v as usize, response, asym)
            }
            "response" => self.params.nonlinearity = Nonlinearity::from_parts(shape, v, asym),
            "asym" => self.params.nonlinearity = Nonlinearity::from_parts(shape, response, v),
            "noise" => self.params.noise = v,
            _ => return Err(self.unknown_param(name)),
        }
        Ok(())
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn apply_variation(&mut self, variation: InstanceVariation) {
        self.params = self.params.vary(variation);
        // Reseeded even when tolerance is zero, so identical modules still
        // have independent noise streams.
        self.rng = Rng::new(variation.seed);
    }

    fn read_state(&self, out: &mut [Volt]) {
        if let Some(slot) = out.first_mut() {
            *slot = self.state.v;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let drive = self.drive(inputs);
        self.state.v = leaky_integrate(self.state.v, drive, self.params.leak, dt);
        self.state.v += ou_increment(&mut self.rng, self.params.noise, self.params.leak, dt);
        self.state.v = clamp_rail(self.state.v, DEFAULT_RAIL);
        let out = self.params.gain * self.params.nonlinearity.apply(self.state.v);
        ModuleOutputs::one(clamp_rail(out, DEFAULT_RAIL))
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

    #[test]
    fn knobs_reach_the_nonlinearity() {
        let mut n = Neuron::default();
        n.set_param("shape", 2.0).unwrap();
        n.set_param("response", 3.0).unwrap();
        n.set_param("asym", 0.25).unwrap();
        assert_eq!(n.get_param("shape"), Some(2.0));
        assert!((n.get_param("asym").unwrap() - 0.25).abs() < 1e-6);
        match n.params.nonlinearity {
            Nonlinearity::AsymTanh { pos, neg } => {
                assert!((pos - 3.0).abs() < 1e-6);
                assert!((neg - 0.75).abs() < 1e-6);
            }
            other => panic!("expected asym_tanh, got {other:?}"),
        }
    }

    #[test]
    fn noise_holds_its_rms_whatever_the_timestep() {
        for dt in [0.001, 0.00025] {
            let mut n = Neuron::instantiate(
                NeuronParams {
                    weights: [0.0; 4],
                    leak: 20.0,
                    noise: 0.08,
                    ..NeuronParams::default()
                },
                InstanceVariation::new(5, 0.0),
            );
            let steps = (30.0 / dt) as usize;
            let mut samples = Vec::new();
            for i in 0..steps {
                n.step(&[0.0; 4], dt);
                if i > steps / 10 {
                    samples.push(n.state.v);
                }
            }
            let mean = samples.iter().sum::<f32>() / samples.len() as f32;
            let std = (samples.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>()
                / samples.len() as f32)
                .sqrt();
            assert!((std - 0.08).abs() < 0.012, "dt={dt} gave RMS {std}");
        }
    }

    #[test]
    fn silent_neurons_stay_exactly_deterministic() {
        let run = || {
            let mut n =
                Neuron::instantiate(NeuronParams::default(), InstanceVariation::new(3, 0.0));
            (0..500)
                .map(|_| n.step(&[0.4; 4], 0.0005)[0])
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn instances_get_independent_noise_streams() {
        let params = NeuronParams {
            weights: [0.0; 4],
            noise: 0.05,
            ..NeuronParams::default()
        };
        let mut a = Neuron::instantiate(params, InstanceVariation::new(1, 0.0));
        let mut b = Neuron::instantiate(params, InstanceVariation::new(2, 0.0));
        let mut differed = false;
        for _ in 0..200 {
            let x = a.step(&[0.0; 4], 0.0005)[0];
            let y = b.step(&[0.0; 4], 0.0005)[0];
            differed |= (x - y).abs() > 1e-9;
        }
        assert!(differed, "two seeds produced the same noise");
    }

    #[test]
    fn mismatch_individualizes_the_saturation() {
        let params = NeuronParams {
            nonlinearity: Nonlinearity::AsymTanh { pos: 1.0, neg: 0.5 },
            ..NeuronParams::default()
        };
        let a = Neuron::instantiate(params, InstanceVariation::new(1, 0.1));
        let b = Neuron::instantiate(params, InstanceVariation::new(2, 0.1));
        assert_ne!(
            a.params.nonlinearity.asymmetry(),
            b.params.nonlinearity.asymmetry()
        );
        assert_eq!(
            a.params.nonlinearity.shape(),
            2,
            "shape must survive mismatch"
        );
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let n = Neuron::default();
        for spec in n.params() {
            let got = n.get_param(spec.name).expect(spec.name);
            assert!(
                (got - spec.default).abs() < 1e-6,
                "{} default {} but module has {got}",
                spec.name,
                spec.default
            );
        }
    }
}
