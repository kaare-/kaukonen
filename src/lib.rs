//! Kaukonen — continuous-time simulator for patchable analog neural modules.
//!
//! This is a phenomenological model of physical / analog hardware, not a
//! machine-learning framework. Computation arises from leaky state, local
//! plasticity, nonlinear excitability, and feedback.

pub mod audio;
pub mod engine;
pub mod integrate;
pub mod module;
pub mod network;
pub mod neuron;
pub mod noise;
pub mod nonlinearity;
pub mod param;
pub mod patch;
pub mod plasticity;
pub mod rc1;
pub mod record;
pub mod render;
pub mod schedule;
pub mod source;
pub mod tc3;
pub mod types;
pub mod wav;

pub use audio::{AudioIn, AudioOut};
pub use engine::Engine;
pub use module::{AnalogModule, Module, ModuleOutputs};
pub use network::{Cable, CableError, ModuleId, Network};
pub use neuron::{Neuron, NeuronParams, NeuronState};
pub use noise::Rng;
pub use nonlinearity::Nonlinearity;
pub use param::{ParamError, ParamSpec, Taper, Unit};
pub use patch::{BuiltPatch, Patch, PatchError};
pub use plasticity::{Plasticity, PlasticityRule, Rule};
pub use rc1::{Rc1, Rc1Params, Rc1State};
pub use record::Recorder;
pub use render::{render, RenderOptions, RenderReport};
pub use schedule::Schedule;
pub use source::{Source, SourceKind, SourceParams};
pub use tc3::{Tc3, Tc3Params, Tc3State};
pub use types::{
    InstanceVariation, SimConfig, Volt, DEFAULT_DT, DEFAULT_RAIL, DEFAULT_SAMPLE_RATE,
};
pub use wav::{Audio, SampleFormat};
