//! Uniform module contract and stack-allocated jack buffers.

use crate::neuron::Neuron;
use crate::rc1::Rc1;
use crate::source::Source;
use crate::tc3::Tc3;
use crate::types::Volt;

/// Maximum jacks on any v1 module (TC-3 has 5 inputs / 4 outputs).
pub const MAX_PORTS: usize = 8;

/// Small output vector produced by `step`. No heap in the hot path.
#[derive(Clone, Copy, Debug)]
pub struct ModuleOutputs {
    data: [Volt; MAX_PORTS],
    len: usize,
}

impl ModuleOutputs {
    pub fn new(values: &[Volt]) -> Self {
        debug_assert!(values.len() <= MAX_PORTS);
        let mut data = [0.0; MAX_PORTS];
        let len = values.len().min(MAX_PORTS);
        data[..len].copy_from_slice(&values[..len]);
        Self { data, len }
    }

    pub fn empty() -> Self {
        Self {
            data: [0.0; MAX_PORTS],
            len: 0,
        }
    }

    pub fn one(v: Volt) -> Self {
        Self::new(&[v])
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn as_slice(&self) -> &[Volt] {
        &self.data[..self.len]
    }

    pub fn get(&self, index: usize) -> Volt {
        self.data.get(index).copied().unwrap_or(0.0)
    }
}

impl std::ops::Index<usize> for ModuleOutputs {
    type Output = Volt;

    fn index(&self, index: usize) -> &Self::Output {
        &self.data[index]
    }
}

/// Continuous-time analog module.
pub trait AnalogModule {
    fn input_count(&self) -> usize;
    fn output_count(&self) -> usize;
    fn jack_in(&self, name: &str) -> Option<usize>;
    fn jack_out(&self, name: &str) -> Option<usize>;
    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs;
    fn snapshot(&self) -> Vec<(&'static str, Volt)>;
}

/// Concrete modules the engine can host.
#[derive(Clone, Debug)]
pub enum Module {
    Neuron(Neuron),
    Rc1(Rc1),
    Tc3(Tc3),
    Source(Source),
}

impl AnalogModule for Module {
    fn input_count(&self) -> usize {
        match self {
            Self::Neuron(m) => m.input_count(),
            Self::Rc1(m) => m.input_count(),
            Self::Tc3(m) => m.input_count(),
            Self::Source(m) => m.input_count(),
        }
    }

    fn output_count(&self) -> usize {
        match self {
            Self::Neuron(m) => m.output_count(),
            Self::Rc1(m) => m.output_count(),
            Self::Tc3(m) => m.output_count(),
            Self::Source(m) => m.output_count(),
        }
    }

    fn jack_in(&self, name: &str) -> Option<usize> {
        match self {
            Self::Neuron(m) => m.jack_in(name),
            Self::Rc1(m) => m.jack_in(name),
            Self::Tc3(m) => m.jack_in(name),
            Self::Source(m) => m.jack_in(name),
        }
    }

    fn jack_out(&self, name: &str) -> Option<usize> {
        match self {
            Self::Neuron(m) => m.jack_out(name),
            Self::Rc1(m) => m.jack_out(name),
            Self::Tc3(m) => m.jack_out(name),
            Self::Source(m) => m.jack_out(name),
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        match self {
            Self::Neuron(m) => m.step(inputs, dt),
            Self::Rc1(m) => m.step(inputs, dt),
            Self::Tc3(m) => m.step(inputs, dt),
            Self::Source(m) => m.step(inputs, dt),
        }
    }

    fn snapshot(&self) -> Vec<(&'static str, Volt)> {
        match self {
            Self::Neuron(m) => m.snapshot(),
            Self::Rc1(m) => m.snapshot(),
            Self::Tc3(m) => m.snapshot(),
            Self::Source(m) => m.snapshot(),
        }
    }
}

impl From<Neuron> for Module {
    fn from(m: Neuron) -> Self {
        Self::Neuron(m)
    }
}

impl From<Rc1> for Module {
    fn from(m: Rc1) -> Self {
        Self::Rc1(m)
    }
}

impl From<Tc3> for Module {
    fn from(m: Tc3) -> Self {
        Self::Tc3(m)
    }
}

impl From<Source> for Module {
    fn from(m: Source) -> Self {
        Self::Source(m)
    }
}

pub fn find_jack(names: &[&str], name: &str) -> Option<usize> {
    names.iter().position(|n| *n == name)
}
