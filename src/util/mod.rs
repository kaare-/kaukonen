//! Utility rack: the ordinary modules that surround the interesting ones.
//!
//! None of this is the point of the project. The three neural modules are, and
//! these exist so their dynamics can be heard, scaled, gated and mixed — the
//! same supporting role [`crate::source::Source`] plays for tests. Anything
//! here is a conventional analog building block with no plasticity, no
//! excitability, and nothing to discover.

pub mod cochlea;
pub mod comp;
pub mod env;
pub mod fbank;
pub mod mix;
pub mod noise;
pub mod svf;
pub mod vca;
pub mod vcf;
pub mod vco;
pub mod verb;
pub mod vocode;

pub use cochlea::Cochlea;
pub use comp::Comp;
pub use env::Env;
pub use fbank::Fbank;
pub use mix::Mix;
pub use noise::Noise;
pub use svf::Svf;
pub use vca::Vca;
pub use vcf::Vcf;
pub use vco::Vco;
pub use verb::Verb;
pub use vocode::Vocode;

/// Correction that removes most of the aliasing from a waveform's jump.
///
/// A saw or pulse steps discontinuously, which a sampled system cannot
/// represent; this subtracts a polynomial approximation of the band-limited
/// step around the jump. `t` is the phase and `dt` the phase increment, both in
/// cycles.
pub fn poly_blep(t: f32, dt: f32) -> f32 {
    if dt <= 0.0 {
        return 0.0;
    }
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// Exponential 1 V per octave, the Eurorack pitch convention.
pub fn volts_to_ratio(volts: f32) -> f32 {
    // Keep the exponent sane so a runaway CV cannot produce an infinity.
    (volts.clamp(-20.0, 20.0)).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blep_only_acts_next_to_the_jump() {
        let dt = 0.01;
        assert_eq!(poly_blep(0.5, dt), 0.0);
        assert!(poly_blep(0.001, dt) != 0.0);
        assert!(poly_blep(0.999, dt) != 0.0);
        assert_eq!(poly_blep(0.5, 0.0), 0.0);
    }

    #[test]
    fn blep_is_continuous_across_the_wrap() {
        // Just before and just after the jump the correction should meet at the
        // same magnitude, otherwise it adds a discontinuity of its own.
        let dt = 0.01;
        let before = poly_blep(1.0 - 1e-6, dt);
        let after = poly_blep(1e-6, dt);
        assert!((before - 1.0).abs() < 1e-3, "before={before}");
        assert!((after + 1.0).abs() < 1e-3, "after={after}");
    }

    #[test]
    fn one_volt_is_one_octave() {
        assert!((volts_to_ratio(0.0) - 1.0).abs() < 1e-6);
        assert!((volts_to_ratio(1.0) - 2.0).abs() < 1e-6);
        assert!((volts_to_ratio(-1.0) - 0.5).abs() < 1e-6);
        assert!(volts_to_ratio(1e9).is_finite());
    }
}
