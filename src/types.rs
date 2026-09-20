//! Shared scalar types, rails, and per-instance variation.

/// Eurorack-style voltage in volts. Nominal operating range is about ±5 V.
pub type Volt = f32;

/// Default soft rail used to keep patched voltages from running away.
pub const DEFAULT_RAIL: Volt = 10.0;

/// Default integration step. 0.5 ms is audio-ish and cheap for small nets.
pub const DEFAULT_DT: f32 = 0.0005;

/// Engine / integrator configuration.
#[derive(Clone, Copy, Debug)]
pub struct SimConfig {
    /// Fixed timestep in seconds.
    pub dt: f32,
    /// Symmetric clamp rail in volts (`±rail`).
    pub rail: Volt,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            dt: DEFAULT_DT,
            rail: DEFAULT_RAIL,
        }
    }
}

impl SimConfig {
    pub fn clamp(&self, v: Volt) -> Volt {
        clamp_rail(v, self.rail)
    }
}

/// Hard clamp used as a last-resort rail protector.
pub fn clamp_rail(v: Volt, rail: Volt) -> Volt {
    v.clamp(-rail, rail)
}

/// Soft saturation toward `±rail` without a hard corner.
pub fn soft_rail(v: Volt, rail: Volt) -> Volt {
    if rail <= 0.0 {
        return 0.0;
    }
    rail * (v / rail).tanh()
}

/// How a newly instantiated module differs from its nominal params.
///
/// v1 only applies a tiny seeded offset. Noise and drift stay off unless a
/// caller turns them on later.
#[derive(Clone, Copy, Debug)]
pub struct InstanceVariation {
    pub seed: u64,
    pub scale: f32,
}

impl InstanceVariation {
    pub const IDENTITY: Self = Self {
        seed: 0,
        scale: 0.0,
    };

    pub fn new(seed: u64, scale: f32) -> Self {
        Self { seed, scale }
    }

    pub fn is_identity(self) -> bool {
        self.scale == 0.0
    }

    /// Deterministic offset in `[-scale, scale]` for parameter slot `slot`.
    pub fn offset(self, slot: u32) -> f32 {
        if self.scale == 0.0 {
            return 0.0;
        }
        let mixed = self
            .seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(u64::from(slot).wrapping_mul(0xBF58_476D_1CE4_E5B9));
        let unit = (mixed >> 11) as f32 / ((1u64 << 53) as f32);
        (unit * 2.0 - 1.0) * self.scale
    }

    pub fn scale_param(self, slot: u32, value: f32) -> f32 {
        value * (1.0 + self.offset(slot))
    }
}

impl Default for InstanceVariation {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_variation_is_zero() {
        let v = InstanceVariation::IDENTITY;
        assert_eq!(v.offset(0), 0.0);
        assert_eq!(v.scale_param(3, 2.5), 2.5);
    }

    #[test]
    fn seeded_variation_is_deterministic() {
        let v = InstanceVariation::new(7, 0.1);
        assert_eq!(v.offset(1), v.offset(1));
        assert!(v.offset(1).abs() <= 0.1);
        assert_ne!(v.offset(1), v.offset(2));
    }
}
