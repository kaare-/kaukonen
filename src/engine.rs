//! Fixed-timestep simulation engine.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::network::Network;
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
    pub fn lookup_channel(&self, name: &str) -> Option<Volt> {
        let (label, field) = name.split_once('.')?;
        let idx = (0..self.network.len())
            .find(|&i| self.network.label(crate::network::ModuleId(i)) == label)?;
        self.network
            .get(crate::network::ModuleId(idx))
            .snapshot()
            .into_iter()
            .find(|(k, _)| *k == field)
            .map(|(_, v)| v)
    }
}
