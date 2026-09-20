//! Patchbay: module instances and voltage cables.

use crate::module::{AnalogModule, Module, ModuleOutputs, MAX_PORTS};
use crate::types::Volt;

/// Stable index of a module in the network. v1 never deletes modules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub usize);

#[derive(Clone, Copy, Debug)]
pub struct Cable {
    pub from: ModuleId,
    pub from_jack: usize,
    pub to: ModuleId,
    pub to_jack: usize,
}

#[derive(Clone, Debug)]
pub struct PatchError {
    pub message: String,
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PatchError {}

#[derive(Clone, Debug)]
pub struct Network {
    modules: Vec<Module>,
    labels: Vec<String>,
    cables: Vec<Cable>,
}

impl Network {
    pub fn new() -> Self {
        Self {
            modules: Vec::new(),
            labels: Vec::new(),
            cables: Vec::new(),
        }
    }

    pub fn add(&mut self, label: impl Into<String>, module: impl Into<Module>) -> ModuleId {
        let id = ModuleId(self.modules.len());
        self.modules.push(module.into());
        self.labels.push(label.into());
        id
    }

    pub fn get(&self, id: ModuleId) -> &Module {
        &self.modules[id.0]
    }

    pub fn get_mut(&mut self, id: ModuleId) -> &mut Module {
        &mut self.modules[id.0]
    }

    pub fn label(&self, id: ModuleId) -> &str {
        &self.labels[id.0]
    }

    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    pub fn modules_mut(&mut self) -> &mut [Module] {
        &mut self.modules
    }

    pub fn cables(&self) -> &[Cable] {
        &self.cables
    }

    pub fn len(&self) -> usize {
        self.modules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    pub fn patch(
        &mut self,
        from: ModuleId,
        from_jack: &str,
        to: ModuleId,
        to_jack: &str,
    ) -> Result<(), PatchError> {
        let src = self.modules.get(from.0).ok_or_else(|| PatchError {
            message: format!("unknown source module {}", from.0),
        })?;
        let dst = self.modules.get(to.0).ok_or_else(|| PatchError {
            message: format!("unknown destination module {}", to.0),
        })?;
        let from_idx = src.jack_out(from_jack).ok_or_else(|| PatchError {
            message: format!(
                "module '{}' has no output jack '{from_jack}'",
                self.labels[from.0]
            ),
        })?;
        let to_idx = dst.jack_in(to_jack).ok_or_else(|| PatchError {
            message: format!(
                "module '{}' has no input jack '{to_jack}'",
                self.labels[to.0]
            ),
        })?;
        self.cables.push(Cable {
            from,
            from_jack: from_idx,
            to,
            to_jack: to_idx,
        });
        Ok(())
    }

    /// Zero every input buffer, then sum cables from the previous outputs.
    pub fn mix(&self, prev: &[ModuleOutputs]) -> Vec<[Volt; MAX_PORTS]> {
        let mut inputs = vec![[0.0; MAX_PORTS]; self.modules.len()];
        for cable in &self.cables {
            let v = prev
                .get(cable.from.0)
                .map(|outs| outs.get(cable.from_jack))
                .unwrap_or(0.0);
            inputs[cable.to.0][cable.to_jack] += v;
        }
        inputs
    }
}

impl Default for Network {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neuron::{Neuron, NeuronParams};
    use crate::source::Source;
    use crate::types::DEFAULT_DT;

    #[test]
    fn two_cables_into_one_jack_sum() {
        let mut net = Network::new();
        let a = net.add("a", Source::dc(1.5));
        let b = net.add("b", Source::dc(2.5));
        let n = net.add("neuron", Neuron::new(NeuronParams::default()));
        net.patch(a, "out", n, "in0").unwrap();
        net.patch(b, "out", n, "in0").unwrap();

        let mut prev = vec![ModuleOutputs::empty(); 3];
        prev[0] = ModuleOutputs::one(1.5);
        prev[1] = ModuleOutputs::one(2.5);
        let mixed = net.mix(&prev);
        assert!((mixed[2][0] - 4.0).abs() < 1e-6);
    }

    #[test]
    fn two_cycle_stays_finite() {
        let mut net = Network::new();
        let n0 = net.add(
            "n0",
            Neuron::new(NeuronParams {
                weights: [0.6, 0.0, 0.0, 0.0],
                leak: 6.0,
                gain: 3.0,
                ..NeuronParams::default()
            }),
        );
        let n1 = net.add(
            "n1",
            Neuron::new(NeuronParams {
                weights: [-0.6, 0.0, 0.0, 0.0],
                leak: 6.0,
                gain: 3.0,
                ..NeuronParams::default()
            }),
        );
        net.patch(n0, "out", n1, "in0").unwrap();
        net.patch(n1, "out", n0, "in0").unwrap();

        let mut prev = vec![ModuleOutputs::one(1.0), ModuleOutputs::one(-0.5)];
        for _ in 0..2000 {
            let mixed = net.mix(&prev);
            let mut next = Vec::new();
            for (i, module) in net.modules_mut().iter_mut().enumerate() {
                let n_in = module.input_count();
                next.push(module.step(&mixed[i][..n_in], DEFAULT_DT));
            }
            prev = next;
        }
        for outs in &prev {
            assert!(outs[0].is_finite());
        }
    }
}
