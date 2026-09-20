//! Local plasticity rules. No global loss, no backpropagation.

use crate::types::Volt;

/// Local analog memory update for one TC-3 route.
pub trait PlasticityRule {
    fn dw(&self, input: Volt, output: Volt, weight: Volt, learn: Volt, forget: Volt) -> f32;
}

/// Hebbian-like local rule with a small activity dead-zone.
#[derive(Clone, Copy, Debug)]
pub struct Hebbian {
    /// Ignore products whose magnitude is below this so residual noise
    /// does not integrate forever.
    pub deadzone: f32,
}

impl Default for Hebbian {
    fn default() -> Self {
        Self { deadzone: 1e-4 }
    }
}

impl PlasticityRule for Hebbian {
    fn dw(&self, input: Volt, output: Volt, weight: Volt, learn: Volt, forget: Volt) -> f32 {
        let product = input * output;
        let hebb = if product.abs() < self.deadzone {
            0.0
        } else {
            learn.max(0.0) * product
        };
        hebb - forget.max(0.0) * weight
    }
}

/// Swappable built-in rules. Prefer this over trait objects in v1.
#[derive(Clone, Copy, Debug)]
pub enum Plasticity {
    Hebbian(Hebbian),
}

impl Default for Plasticity {
    fn default() -> Self {
        Self::Hebbian(Hebbian::default())
    }
}

impl Plasticity {
    /// Rule names addressable from a patch file, in index order.
    pub const RULES: &'static [&'static str] = &["hebbian"];

    pub fn rule(self) -> usize {
        match self {
            Self::Hebbian(_) => 0,
        }
    }

    pub fn deadzone(self) -> f32 {
        match self {
            Self::Hebbian(rule) => rule.deadzone,
        }
    }

    /// Rebuild from flat knob values. Unknown rules fall back to Hebbian.
    pub fn from_parts(rule: usize, deadzone: f32) -> Self {
        match rule {
            0 => Self::Hebbian(Hebbian { deadzone }),
            _ => Self::Hebbian(Hebbian { deadzone }),
        }
    }
}

impl PlasticityRule for Plasticity {
    fn dw(&self, input: Volt, output: Volt, weight: Volt, learn: Volt, forget: Volt) -> f32 {
        match self {
            Self::Hebbian(rule) => rule.dw(input, output, weight, learn, forget),
        }
    }
}
