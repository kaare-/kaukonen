//! Deterministic per-instance noise.
//!
//! Physical imperfection has to be reproducible: a run has to repeat exactly
//! given the same patch seed, or comparing two nearly-identical runs to detect
//! chaos means nothing. Every noisy module therefore owns its own stream,
//! seeded from the patch seed and its position in the rack, rather than drawing
//! from a shared global generator.

/// splitmix64. Small, fast, good enough for thermal noise, and reproducible
/// across platforms because it is all integer arithmetic.
#[derive(Clone, Copy, Debug)]
pub struct Rng {
    state: u64,
}

impl Default for Rng {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self {
            // Offset so seed 0 is not a degenerate stream.
            state: seed ^ 0x2545_F491_4F6C_DD1D,
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn uniform(&mut self) -> f32 {
        // Top 24 bits give an exactly representable f32 mantissa.
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }

    /// Uniform in `[-1, 1)`.
    pub fn bipolar(&mut self) -> f32 {
        self.uniform() * 2.0 - 1.0
    }

    /// Unit-variance noise, bounded at about ±3.5 sigma.
    ///
    /// Irwin–Hall rather than Box–Muller: no transcendentals, and bounded
    /// tails are arguably a better fit for a real circuit than a Gaussian's
    /// unbounded ones.
    pub fn normal(&mut self) -> f32 {
        let sum = self.uniform() + self.uniform() + self.uniform() + self.uniform();
        // Four summed uniforms have variance 1/3, so sqrt(3) normalizes it.
        (sum - 2.0) * SQRT_3
    }
}

const SQRT_3: f32 = 1.732_050_8;

/// Increment of an Ornstein–Uhlenbeck process whose stationary RMS is `rms`.
///
/// A module's own relaxation sets how much diffusion is needed to hold a given
/// fluctuation size, so `rms` stays in volts and stays independent of `dt`.
/// With no relaxation to push against there is no stationary distribution, so
/// this falls back to a plain random walk of `rms * sqrt(dt)` per step.
pub fn ou_increment(rng: &mut Rng, rms: f32, relaxation: f32, dt: f32) -> f32 {
    if rms == 0.0 || dt <= 0.0 {
        return 0.0;
    }
    let sigma = if relaxation > 1e-6 {
        rms * (2.0 * relaxation).sqrt()
    } else {
        rms
    };
    sigma * dt.sqrt() * rng.normal()
}

/// One step of an Ornstein–Uhlenbeck process: relax toward zero, then diffuse.
///
/// This is the shape slow parameter drift takes — a wander that stays bounded
/// because something pulls it back, rather than a random walk that eventually
/// leaves the panel.
pub fn ou_step(rng: &mut Rng, x: f32, rms: f32, relaxation: f32, dt: f32) -> f32 {
    x - relaxation * x * dt + ou_increment(rng, rms, relaxation, dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moments(values: &[f32]) -> (f32, f32) {
        let n = values.len() as f32;
        let mean = values.iter().sum::<f32>() / n;
        let var = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
        (mean, var.sqrt())
    }

    #[test]
    fn uniform_covers_the_unit_interval() {
        let mut rng = Rng::new(1);
        let xs: Vec<f32> = (0..20_000).map(|_| rng.uniform()).collect();
        let (mean, std) = moments(&xs);
        assert!(xs.iter().all(|x| (0.0..1.0).contains(x)));
        assert!((mean - 0.5).abs() < 0.01, "mean={mean}");
        // Uniform on [0,1) has std 1/sqrt(12).
        assert!((std - 0.2887).abs() < 0.01, "std={std}");
    }

    #[test]
    fn normal_has_unit_variance() {
        let mut rng = Rng::new(2);
        let xs: Vec<f32> = (0..50_000).map(|_| rng.normal()).collect();
        let (mean, std) = moments(&xs);
        assert!(mean.abs() < 0.02, "mean={mean}");
        assert!((std - 1.0).abs() < 0.02, "std={std}");
        assert!(xs.iter().all(|x| x.abs() < 3.5));
    }

    #[test]
    fn streams_are_reproducible_and_independent() {
        let take = |seed| {
            let mut rng = Rng::new(seed);
            (0..8).map(|_| rng.next_u64()).collect::<Vec<_>>()
        };
        assert_eq!(take(42), take(42));
        assert_ne!(take(42), take(43));
        assert_ne!(take(0), take(1));
    }

    #[test]
    fn ou_noise_holds_its_rms_across_timesteps() {
        // Integrate dV = -leak*V*dt + noise, and check the stationary spread
        // matches the requested RMS whatever the step size.
        for dt in [0.002, 0.0005, 0.000125] {
            let leak = 20.0;
            let mut rng = Rng::new(7);
            let mut v = 0.0;
            let mut samples = Vec::new();
            let steps = (40.0 / dt) as usize;
            for i in 0..steps {
                v += -leak * v * dt + ou_increment(&mut rng, 0.1, leak, dt);
                if i > steps / 10 {
                    samples.push(v);
                }
            }
            let (_, std) = moments(&samples);
            assert!(
                (std - 0.1).abs() < 0.015,
                "dt={dt} gave RMS {std}, wanted 0.1"
            );
        }
    }

    #[test]
    fn zero_rms_is_exactly_silent() {
        let mut rng = Rng::new(9);
        assert_eq!(ou_increment(&mut rng, 0.0, 10.0, 0.001), 0.0);
        // And it must not advance the stream, so an ideal module stays
        // bit-identical to one with no noise support at all.
        let mut a = Rng::new(9);
        let mut b = Rng::new(9);
        ou_increment(&mut a, 0.0, 10.0, 0.001);
        assert_eq!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn ou_step_wanders_but_stays_bounded() {
        let mut rng = Rng::new(11);
        let (rms, relaxation, dt) = (0.2, 0.5, 0.001);
        let mut x = 0.0;
        let mut samples = Vec::new();
        for i in 0..200_000 {
            x = ou_step(&mut rng, x, rms, relaxation, dt);
            if i > 20_000 {
                samples.push(x);
            }
        }
        let (mean, std) = moments(&samples);
        // The point is that it stays put; the mean of a finite OU sample
        // still wanders a fraction of the spread.
        assert!(mean.abs() < 0.5 * rms, "mean={mean}");
        assert!((std - rms).abs() < 0.03, "std={std}, wanted {rms}");
    }
}
