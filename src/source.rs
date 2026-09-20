//! Test-only stimulus sources. Not a Eurorack hardware module.

use crate::module::{find_jack, AnalogModule, ModuleOutputs};
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const OUT_JACKS: [&str; 1] = ["out"];

#[derive(Clone, Copy, Debug)]
pub enum SourceKind {
    Dc { volts: Volt },
    Sine { freq_hz: f32, amp: Volt, offset: Volt },
    Pulse {
        period: f32,
        width: f32,
        amp: Volt,
        offset: Volt,
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SourceState {
    pub phase: f32,
}

#[derive(Clone, Debug)]
pub struct Source {
    pub kind: SourceKind,
    pub state: SourceState,
}

impl Source {
    pub fn dc(volts: Volt) -> Self {
        Self::instantiate(SourceKind::Dc { volts }, InstanceVariation::IDENTITY)
    }

    pub fn sine(freq_hz: f32, amp: Volt, offset: Volt) -> Self {
        Self::instantiate(
            SourceKind::Sine {
                freq_hz,
                amp,
                offset,
            },
            InstanceVariation::IDENTITY,
        )
    }

    pub fn pulse(period: f32, width: f32, amp: Volt, offset: Volt) -> Self {
        Self::instantiate(
            SourceKind::Pulse {
                period,
                width,
                amp,
                offset,
            },
            InstanceVariation::IDENTITY,
        )
    }

    pub fn instantiate(kind: SourceKind, _variation: InstanceVariation) -> Self {
        Self {
            kind,
            state: SourceState::default(),
        }
    }

    fn sample(&self) -> Volt {
        match self.kind {
            SourceKind::Dc { volts } => volts,
            SourceKind::Sine {
                freq_hz: _,
                amp,
                offset,
            } => offset + amp * (self.state.phase * std::f32::consts::TAU).sin(),
            SourceKind::Pulse {
                period,
                width,
                amp,
                offset,
            } => {
                let t = if period > 0.0 {
                    self.state.phase % period
                } else {
                    0.0
                };
                if t < width {
                    offset + amp
                } else {
                    offset
                }
            }
        }
    }
}

impl AnalogModule for Source {
    fn input_count(&self) -> usize {
        0
    }

    fn output_count(&self) -> usize {
        1
    }

    fn jack_in(&self, _name: &str) -> Option<usize> {
        None
    }

    fn jack_out(&self, name: &str) -> Option<usize> {
        find_jack(&OUT_JACKS, name)
    }

    fn step(&mut self, _inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let out = clamp_rail(self.sample(), DEFAULT_RAIL);
        match &self.kind {
            SourceKind::Dc { .. } => {}
            SourceKind::Sine { freq_hz, .. } => {
                self.state.phase = (self.state.phase + freq_hz * dt) % 1.0;
            }
            SourceKind::Pulse { period, .. } => {
                let p = period.max(dt);
                self.state.phase = (self.state.phase + dt) % p;
            }
        }
        ModuleOutputs::one(out)
    }

    fn snapshot(&self) -> Vec<(&'static str, Volt)> {
        vec![("phase", self.state.phase), ("out", self.sample())]
    }
}
