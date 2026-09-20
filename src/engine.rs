//! Fixed-timestep simulation engine.

use crate::module::{AnalogModule, ModuleOutputs, MAX_STATE};
use crate::network::{ModuleId, Network};
use crate::record::Recorder;
use crate::types::{SimConfig, Volt};

pub struct Engine {
    pub network: Network,
    pub config: SimConfig,
    pub time: f32,
    prev: Vec<ModuleOutputs>,
}

impl Engine {
    pub fn new(network: Network, config: SimConfig) -> Self {
        let prev = vec![ModuleOutputs::empty(); network.len()];
        Self {
            network,
            config,
            time: 0.0,
            prev,
        }
    }

    pub fn step(&mut self) -> &[ModuleOutputs] {
        let mixed = self.network.mix(&self.prev);
        let mut next = Vec::with_capacity(self.network.len());
        for (i, module) in self.network.modules_mut().iter_mut().enumerate() {
            let n_in = module.input_count();
            let mut outs = module.step(&mixed[i][..n_in], self.config.dt);
            // Rail-protect every jack voltage.
            let clamped: Vec<Volt> = outs
                .as_slice()
                .iter()
                .map(|v| self.config.clamp(*v))
                .collect();
            outs = ModuleOutputs::new(&clamped);
            next.push(outs);
        }
        self.prev = next;
        self.time += self.config.dt;
        &self.prev
    }

    pub fn outputs(&self) -> &[ModuleOutputs] {
        &self.prev
    }

    /// Advance `seconds` of simulated time, recording a snapshot each step.
    pub fn run(&mut self, seconds: f32, recorder: &mut Recorder) {
        let steps = (seconds / self.config.dt).max(0.0) as usize;
        let names = recorder.names.clone();
        for _ in 0..steps {
            self.step();
            let sample = self.snapshot_sample(&names);
            recorder.push(self.time, &sample);
        }
    }

    pub fn snapshot_sample(&self, names: &[String]) -> Vec<Volt> {
        names
            .iter()
            .map(|name| self.lookup_channel(name).unwrap_or(0.0))
            .collect()
    }

    /// Channel names are `label.field`, e.g. `rc1_0.v` or `tc3.activity`.
    ///
    /// Internal states win over jacks of the same name, then output jack
    /// voltages are searched, so `neuron0.out` is recordable too.
    pub fn lookup_channel(&self, name: &str) -> Option<Volt> {
        let (label, field) = name.split_once('.')?;
        let idx = (0..self.network.len()).find(|&i| self.network.label(ModuleId(i)) == label)?;
        let module = self.network.get(ModuleId(idx));
        let fields = module.state_fields();
        if let Some(slot) = fields.iter().position(|f| *f == field) {
            let mut values = [0.0; MAX_STATE];
            module.read_state(&mut values[..fields.len()]);
            return Some(values[slot]);
        }
        let jack = module.jack_out(field)?;
        Some(self.prev.get(idx).map_or(0.0, |outs| outs.get(jack)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch;

    #[test]
    fn channels_read_states_and_output_jacks() {
        let built = patch::parse(
            "module src source shape=dc volts=2\n\
             module n neuron w0=1 leak=1 gain=3\n\
             patch src.out -> n.in0\n",
        )
        .unwrap()
        .build()
        .unwrap();
        let mut engine = Engine::new(built.network, built.config);
        for _ in 0..20_000 {
            engine.step();
        }
        let v = engine.lookup_channel("n.v").expect("state");
        let out = engine.lookup_channel("n.out").expect("jack");
        // Equilibrium of dV/dt = drive - leak*V is drive/leak.
        assert!((v - 2.0).abs() < 0.01, "membrane settled at {v}");
        assert!((out - 3.0 * (v * 0.6).tanh()).abs() < 1e-4, "out={out}");
        assert!(engine.lookup_channel("n.nope").is_none());
        assert!(engine.lookup_channel("nope.v").is_none());
    }
}
