//! Patchbay: module instances and voltage cables.

use crate::module::{AnalogModule, Module};

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
pub struct CableError {
    pub message: String,
}

impl std::fmt::Display for CableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CableError {}

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
    ) -> Result<(), CableError> {
        let src = self.modules.get(from.0).ok_or_else(|| CableError {
            message: format!("unknown source module {}", from.0),
        })?;
        let dst = self.modules.get(to.0).ok_or_else(|| CableError {
            message: format!("unknown destination module {}", to.0),
        })?;
        let from_idx = src.jack_out(from_jack).ok_or_else(|| CableError {
            message: format!(
                "module '{}' has no output jack '{from_jack}'",
                self.labels[from.0]
            ),
        })?;
        let to_idx = dst.jack_in(to_jack).ok_or_else(|| CableError {
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
}

impl Default for Network {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::neuron::{Neuron, NeuronParams};
    use crate::source::Source;
    use crate::types::SimConfig;

    #[test]
    fn two_cables_into_one_jack_sum() {
        let mut net = Network::new();
        let a = net.add("a", Source::dc(1.5));
        let b = net.add("b", Source::dc(2.5));
        let n = net.add(
            "neuron",
            Neuron::new(NeuronParams {
                leak: 1.0,
                ..NeuronParams::default()
            }),
        );
        net.patch(a, "out", n, "in0").unwrap();
        net.patch(b, "out", n, "in0").unwrap();

        let mut engine = Engine::new(net, SimConfig::control_rate());
        for _ in 0..40_000 {
            engine.step();
        }
        // Both sources reach the same jack, so the membrane settles at their
        // sum over the leak.
        let v = engine.lookup_channel("neuron.v").unwrap();
        assert!((v - 4.0).abs() < 0.01, "membrane settled at {v}");
    }

    #[test]
    fn unknown_jacks_are_refused_by_name() {
        let mut net = Network::new();
        let a = net.add("a", Source::dc(1.0));
        let n = net.add("n", Neuron::new(NeuronParams::default()));
        assert!(net.patch(a, "gate", n, "in0").is_err());
        assert!(net.patch(a, "out", n, "in9").is_err());
        let err = net.patch(a, "out", n, "in9").unwrap_err();
        assert!(err.message.contains("input jack"), "{err}");
    }
}
