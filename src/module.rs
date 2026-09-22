//! Uniform module contract and stack-allocated jack buffers.

use crate::audio::{AudioIn, AudioOut};
use crate::neuron::Neuron;
use crate::param::{ParamError, ParamSpec};
use crate::rc1::Rc1;
use crate::source::Source;
use crate::tc3::Tc3;
use crate::types::{clamp_rail, InstanceVariation, Volt};
use crate::util::{Cochlea, Comp, Env, Fbank, Mix, Noise, Svf, Vca, Vcf, Vco, Verb};

/// Maximum jacks on any one module (TC-3 has 5 inputs / 4 outputs).
pub const MAX_PORTS: usize = 8;

/// Maximum internal state fields any one module reports (TC-3 reports 10).
pub const MAX_STATE: usize = 16;

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

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[Volt] {
        &self.data[..self.len]
    }

    pub fn get(&self, index: usize) -> Volt {
        self.data.get(index).copied().unwrap_or(0.0)
    }

    /// Apply a rail in place, avoiding the rebuild-through-`Vec` the engine
    /// used to do once per module per step.
    pub fn clamp_all(&mut self, rail: Volt) {
        for v in &mut self.data[..self.len] {
            *v = clamp_rail(*v, rail);
        }
    }
}

impl std::ops::Index<usize> for ModuleOutputs {
    type Output = Volt;

    fn index(&self, index: usize) -> &Self::Output {
        &self.data[index]
    }
}

/// Continuous-time analog module.
///
/// Implementors describe their panel (jacks and knobs) as static data, so the
/// patch language and CLI can drive any module without knowing its type.
pub trait AnalogModule {
    /// Name used in patch files, e.g. `rc1`.
    fn kind_name(&self) -> &'static str;

    fn in_jacks(&self) -> &'static [&'static str];
    fn out_jacks(&self) -> &'static [&'static str];

    /// The module's knobs.
    fn params(&self) -> &'static [ParamSpec];
    fn get_param(&self, name: &str) -> Option<f32>;
    fn set_param(&mut self, name: &str, value: f32) -> Result<(), ParamError>;

    /// Names of the internal states this module exposes for recording.
    fn state_fields(&self) -> &'static [&'static str];

    /// Make this instance physically individual.
    ///
    /// Called once after knobs are set, so component tolerance perturbs the
    /// value the user dialled in rather than the factory default. Utility
    /// modules that represent no physical hardware leave this alone.
    fn apply_variation(&mut self, variation: InstanceVariation) {
        let _ = variation;
    }

    /// Write the current internal state into `out`, in `state_fields` order.
    fn read_state(&self, out: &mut [Volt]);

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs;

    fn input_count(&self) -> usize {
        self.in_jacks().len()
    }

    fn output_count(&self) -> usize {
        self.out_jacks().len()
    }

    fn jack_in(&self, name: &str) -> Option<usize> {
        find_jack(self.in_jacks(), name)
    }

    fn jack_out(&self, name: &str) -> Option<usize> {
        find_jack(self.out_jacks(), name)
    }

    /// Convenience view of the internal state. Allocates; not for the hot path.
    fn snapshot(&self) -> Vec<(&'static str, Volt)> {
        let fields = self.state_fields();
        let mut values = [0.0; MAX_STATE];
        self.read_state(&mut values[..fields.len()]);
        fields.iter().copied().zip(values).collect()
    }

    /// Helper for implementors: reject an unknown knob name with the known list.
    fn unknown_param(&self, name: &str) -> ParamError {
        ParamError::Unknown {
            kind: self.kind_name(),
            name: name.to_string(),
            known: self.params().iter().map(|s| s.name).collect(),
        }
    }
}

/// Concrete modules the engine can host.
#[derive(Clone, Debug)]
pub enum Module {
    Neuron(Neuron),
    Rc1(Rc1),
    Tc3(Tc3),
    Source(Source),
    AudioIn(AudioIn),
    AudioOut(AudioOut),
    Vco(Vco),
    Vcf(Vcf),
    Svf(Svf),
    Comp(Comp),
    Fbank(Fbank),
    Verb(Verb),
    Cochlea(Cochlea),
    Vca(Vca),
    Mix(Mix),
    Env(Env),
    Noise(Noise),
}

/// Forward one expression to whichever variant is held.
macro_rules! dispatch {
    ($self:ident, $inner:ident => $body:expr) => {
        match $self {
            Module::Neuron($inner) => $body,
            Module::Rc1($inner) => $body,
            Module::Tc3($inner) => $body,
            Module::Source($inner) => $body,
            Module::AudioIn($inner) => $body,
            Module::AudioOut($inner) => $body,
            Module::Vco($inner) => $body,
            Module::Vcf($inner) => $body,
            Module::Svf($inner) => $body,
            Module::Comp($inner) => $body,
            Module::Fbank($inner) => $body,
            Module::Verb($inner) => $body,
            Module::Cochlea($inner) => $body,
            Module::Vca($inner) => $body,
            Module::Mix($inner) => $body,
            Module::Env($inner) => $body,
            Module::Noise($inner) => $body,
        }
    };
}

/// Builds a default instance of one module kind.
pub type ModuleFactory = fn() -> Module;

impl Module {
    /// Every module kind the patch language can instantiate, with a factory.
    pub const KINDS: &'static [(&'static str, ModuleFactory)] = &[
        ("neuron", || Module::Neuron(Neuron::default())),
        ("rc1", || Module::Rc1(Rc1::default())),
        ("tc3", || Module::Tc3(Tc3::default())),
        ("source", || Module::Source(Source::default())),
        ("audioin", || Module::AudioIn(AudioIn::new())),
        ("audioout", || Module::AudioOut(AudioOut::new())),
        ("vco", || Module::Vco(Vco::new())),
        ("vcf", || Module::Vcf(Vcf::new())),
        ("svf", || Module::Svf(Svf::new())),
        ("comp", || Module::Comp(Comp::new())),
        ("fbank", || Module::Fbank(Fbank::new())),
        ("verb", || Module::Verb(Verb::new())),
        ("cochlea", || Module::Cochlea(Cochlea::new())),
        ("vca", || Module::Vca(Vca::new())),
        ("mix", || Module::Mix(Mix::new())),
        ("env", || Module::Env(Env::new())),
        ("noise", || Module::Noise(Noise::new())),
    ];

    /// Build a default instance of a kind named in a patch file.
    pub fn from_kind_name(kind: &str) -> Option<Module> {
        Self::KINDS
            .iter()
            .find(|(name, _)| *name == kind)
            .map(|(_, make)| make())
    }

    pub fn kind_names() -> Vec<&'static str> {
        Self::KINDS.iter().map(|(name, _)| *name).collect()
    }

    /// The sound-input module, so a renderer can hand it a file to play.
    pub fn as_audio_in_mut(&mut self) -> Option<&mut AudioIn> {
        match self {
            Self::AudioIn(m) => Some(m),
            _ => None,
        }
    }

    /// The sound-output module, so a renderer can collect what it captured.
    pub fn as_audio_out(&self) -> Option<&AudioOut> {
        match self {
            Self::AudioOut(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_audio_out_mut(&mut self) -> Option<&mut AudioOut> {
        match self {
            Self::AudioOut(m) => Some(m),
            _ => None,
        }
    }
}

impl AnalogModule for Module {
    fn kind_name(&self) -> &'static str {
        dispatch!(self, m => m.kind_name())
    }

    fn in_jacks(&self) -> &'static [&'static str] {
        dispatch!(self, m => m.in_jacks())
    }

    fn out_jacks(&self) -> &'static [&'static str] {
        dispatch!(self, m => m.out_jacks())
    }

    fn params(&self) -> &'static [ParamSpec] {
        dispatch!(self, m => m.params())
    }

    fn get_param(&self, name: &str) -> Option<f32> {
        dispatch!(self, m => m.get_param(name))
    }

    fn set_param(&mut self, name: &str, value: f32) -> Result<(), ParamError> {
        dispatch!(self, m => m.set_param(name, value))
    }

    fn state_fields(&self) -> &'static [&'static str] {
        dispatch!(self, m => m.state_fields())
    }

    fn apply_variation(&mut self, variation: InstanceVariation) {
        dispatch!(self, m => m.apply_variation(variation))
    }

    fn read_state(&self, out: &mut [Volt]) {
        dispatch!(self, m => m.read_state(out))
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        dispatch!(self, m => m.step(inputs, dt))
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

impl From<AudioIn> for Module {
    fn from(m: AudioIn) -> Self {
        Self::AudioIn(m)
    }
}

impl From<AudioOut> for Module {
    fn from(m: AudioOut) -> Self {
        Self::AudioOut(m)
    }
}

impl From<Vco> for Module {
    fn from(m: Vco) -> Self {
        Self::Vco(m)
    }
}

impl From<Vcf> for Module {
    fn from(m: Vcf) -> Self {
        Self::Vcf(m)
    }
}

impl From<Svf> for Module {
    fn from(m: Svf) -> Self {
        Self::Svf(m)
    }
}

impl From<Comp> for Module {
    fn from(m: Comp) -> Self {
        Self::Comp(m)
    }
}

impl From<Fbank> for Module {
    fn from(m: Fbank) -> Self {
        Self::Fbank(m)
    }
}

impl From<Verb> for Module {
    fn from(m: Verb) -> Self {
        Self::Verb(m)
    }
}

impl From<Cochlea> for Module {
    fn from(m: Cochlea) -> Self {
        Self::Cochlea(m)
    }
}

impl From<Vca> for Module {
    fn from(m: Vca) -> Self {
        Self::Vca(m)
    }
}

impl From<Mix> for Module {
    fn from(m: Mix) -> Self {
        Self::Mix(m)
    }
}

impl From<Env> for Module {
    fn from(m: Env) -> Self {
        Self::Env(m)
    }
}

impl From<Noise> for Module {
    fn from(m: Noise) -> Self {
        Self::Noise(m)
    }
}

pub fn find_jack(names: &[&str], name: &str) -> Option<usize> {
    names.iter().position(|n| *n == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_its_knobs() {
        for name in Module::kind_names() {
            let mut m = Module::from_kind_name(name).expect("kind");
            assert_eq!(m.kind_name(), name);
            for spec in m.params() {
                let mid = spec.from_norm(0.5);
                m.set_param(spec.name, mid)
                    .unwrap_or_else(|e| panic!("{name}.{}: {e}", spec.name));
                let got = m.get_param(spec.name).expect("readable");
                let expected = if spec.choices().is_some() {
                    crate::param::choice(mid) as f32
                } else {
                    mid
                };
                assert!(
                    (got - expected).abs() <= 1e-4 * expected.abs().max(1.0),
                    "{name}.{} set {mid} read {got}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn unknown_knobs_and_out_of_range_are_rejected() {
        let mut m = Module::from_kind_name("neuron").unwrap();
        assert!(m.set_param("wobble", 1.0).is_err());
        let spec = m.params().iter().find(|s| s.name == "leak").unwrap();
        assert!(m.set_param("leak", spec.max + 10.0).is_err());
    }

    #[test]
    fn state_fields_match_read_state_width() {
        for name in Module::kind_names() {
            let m = Module::from_kind_name(name).unwrap();
            assert!(m.state_fields().len() <= MAX_STATE);
            assert_eq!(m.snapshot().len(), m.state_fields().len(), "{name}");
            assert!(m.input_count() <= MAX_PORTS);
            assert!(m.output_count() <= MAX_PORTS);
        }
    }

    #[test]
    fn jack_names_resolve_to_their_own_indices() {
        for name in Module::kind_names() {
            let m = Module::from_kind_name(name).unwrap();
            for (i, jack) in m.in_jacks().iter().enumerate() {
                assert_eq!(m.jack_in(jack), Some(i), "{name} in {jack}");
            }
            for (i, jack) in m.out_jacks().iter().enumerate() {
                assert_eq!(m.jack_out(jack), Some(i), "{name} out {jack}");
            }
        }
    }
}
