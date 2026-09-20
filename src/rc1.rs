//! RC-1: three-state analog reservoir / excitable cell.
//!
//! States are membrane `v`, fast recovery `r`, and slow adaptation `s`.
//! Excitability is continuous (N-shaped / Hindmarsh–Rose-like), not a
//! digital integrate-and-fire comparator.

use crate::integrate::softplus;
use crate::module::{find_jack, AnalogModule, ModuleOutputs};
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const IN_JACKS: [&str; 1] = ["in"];
const OUT_JACKS: [&str; 1] = ["out"];

/// Knob / component characteristics.
#[derive(Clone, Copy, Debug)]
pub struct Rc1Params {
    /// Effective capacitance. Larger = slower membrane.
    pub capacitance: f32,
    /// Extra linear leak on `v`.
    pub leak: f32,
    /// Adaptation setpoint / rest-like voltage (`x_r` in HR).
    pub threshold: f32,
    /// Regenerative `v²` gain. Higher = sharper excitability.
    pub sharpness: f32,
    /// Restoring `v³` gain.
    pub cubic: f32,
    /// How strongly recovery `r` is subtracted from the membrane.
    pub discharge: f32,
    /// Fast recovery rate (1/τ_r).
    pub recovery_rate: f32,
    /// Quadratic recovery drive (`d` in HR).
    pub recovery_drive: f32,
    /// Slow adaptation rate (1/τ_s). Must be ≪ recovery_rate for bursts.
    pub adaptation_rate: f32,
    /// How strongly `v` charges the slow variable.
    pub adaptation_depth: f32,
    pub drive_gain: f32,
    pub output_gain: f32,
    /// Maps wall-clock seconds into model time.
    pub time_scale: f32,
    /// Constant bias current (HR `I` offset).
    pub bias: f32,
}

impl Default for Rc1Params {
    fn default() -> Self {
        Self::burst()
    }
}

impl Rc1Params {
    /// Weak drive, no spikes. Good for leak / slow-charge tests.
    pub fn subthreshold() -> Self {
        Self {
            capacitance: 1.0,
            leak: 0.8,
            threshold: -1.6,
            sharpness: 3.0,
            cubic: 1.0,
            discharge: 1.0,
            recovery_rate: 1.0,
            recovery_drive: 5.0,
            adaptation_rate: 0.005,
            adaptation_depth: 4.0,
            drive_gain: 1.0,
            output_gain: 5.0,
            time_scale: 80.0,
            bias: 0.0,
        }
    }

    /// Resting cell that fires an isolated pulse to a brief kick.
    pub fn pulse() -> Self {
        Self {
            bias: 0.4,
            adaptation_rate: 0.02,
            ..Self::subthreshold()
        }
    }

    /// Constant drive produces a regular spike train.
    pub fn tonic() -> Self {
        Self {
            bias: 3.2,
            adaptation_rate: 0.03,
            adaptation_depth: 1.2,
            ..Self::subthreshold()
        }
    }

    /// Slow adaptation splits spikes into bursts.
    pub fn burst() -> Self {
        Self {
            capacitance: 1.0,
            leak: 0.0,
            threshold: -1.6,
            sharpness: 3.0,
            cubic: 1.0,
            discharge: 1.0,
            recovery_rate: 1.0,
            recovery_drive: 5.0,
            adaptation_rate: 0.006,
            adaptation_depth: 4.0,
            drive_gain: 1.0,
            output_gain: 5.0,
            time_scale: 80.0,
            bias: 2.0,
        }
    }

    pub fn vary(mut self, variation: InstanceVariation) -> Self {
        self.threshold += variation.offset(1) * 0.05;
        self.sharpness = variation.scale_param(2, self.sharpness);
        self.adaptation_rate = variation.scale_param(3, self.adaptation_rate).max(1e-6);
        self.bias += variation.offset(4) * 0.05;
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Rc1State {
    pub v: Volt,
    pub r: Volt,
    pub s: Volt,
}

impl Default for Rc1State {
    fn default() -> Self {
        Self::rest_like(-1.5, 5.0, 4.0, -1.6)
    }
}

impl Rc1State {
    /// Equilibrium of the linear recovery/adaptation equations at voltage `v`.
    pub fn rest_like(v: f32, recovery_drive: f32, adaptation_depth: f32, threshold: f32) -> Self {
        Self {
            v,
            r: 1.0 - recovery_drive * v * v,
            s: adaptation_depth * (v - threshold),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Rc1 {
    pub params: Rc1Params,
    pub state: Rc1State,
}

impl Rc1 {
    pub fn new(params: Rc1Params) -> Self {
        Self::instantiate(params, InstanceVariation::IDENTITY)
    }

    pub fn instantiate(params: Rc1Params, variation: InstanceVariation) -> Self {
        let params = params.vary(variation);
        let state = Rc1State::rest_like(
            params.threshold + 0.15,
            params.recovery_drive,
            params.adaptation_depth,
            params.threshold,
        );
        Self { params, state }
    }

    fn derivatives(&self, v: f32, r: f32, s: f32, i_in: f32) -> (f32, f32, f32) {
        let p = &self.params;
        let c = p.capacitance.max(1e-4);
        // Smooth regenerative current: quadratic / cubic N-shape, plus a
        // softplus kick that grows as v approaches the firing region.
        let exc = p.sharpness * v * v - p.cubic * v * v * v
            + 0.15 * softplus(p.sharpness * (v - 0.0));
        let drive = p.drive_gain * i_in + p.bias;
        // Hindmarsh–Rose-like: recovery `r` is added, adaptation `s` subtracted.
        // `discharge` scales the recovery contribution (1.0 = standard HR).
        let dv = (p.discharge * r + exc - s + drive - p.leak * v) / c;
        let dr = p.recovery_rate * (1.0 - p.recovery_drive * v * v - r);
        let ds = p.adaptation_rate * (p.adaptation_depth * (v - p.threshold) - s);
        (dv, dr, ds)
    }

    fn rk2_step(&mut self, i_in: f32, dt_model: f32) {
        let (v, r, s) = (self.state.v, self.state.r, self.state.s);
        let (k1v, k1r, k1s) = self.derivatives(v, r, s, i_in);
        let (k2v, k2r, k2s) = self.derivatives(
            v + dt_model * k1v,
            r + dt_model * k1r,
            s + dt_model * k1s,
            i_in,
        );
        self.state.v += 0.5 * dt_model * (k1v + k2v);
        self.state.r += 0.5 * dt_model * (k1r + k2r);
        self.state.s += 0.5 * dt_model * (k1s + k2s);
        self.state.v = clamp_rail(self.state.v, 20.0);
        self.state.r = clamp_rail(self.state.r, 40.0);
        self.state.s = clamp_rail(self.state.s, 40.0);
    }
}

impl AnalogModule for Rc1 {
    fn input_count(&self) -> usize {
        1
    }

    fn output_count(&self) -> usize {
        1
    }

    fn jack_in(&self, name: &str) -> Option<usize> {
        find_jack(&IN_JACKS, name)
    }

    fn jack_out(&self, name: &str) -> Option<usize> {
        find_jack(&OUT_JACKS, name)
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let i_in = inputs.first().copied().unwrap_or(0.0);
        let dt_model = dt * self.params.time_scale;
        self.rk2_step(i_in, dt_model);
        let out = self.params.output_gain * self.state.v.tanh();
        ModuleOutputs::one(clamp_rail(out, DEFAULT_RAIL))
    }

    fn snapshot(&self) -> Vec<(&'static str, Volt)> {
        vec![("v", self.state.v), ("r", self.state.r), ("s", self.state.s)]
    }
}

/// Upward crossings of `level`. Used by tests and the demo summary.
pub fn count_crossings(trace: &[f32], level: f32) -> usize {
    trace
        .windows(2)
        .filter(|w| w[0] < level && w[1] >= level)
        .count()
}

/// Count silent-separated spike clusters.
pub fn count_bursts(spike_times: &[f32], min_isi_gap: f32) -> usize {
    if spike_times.is_empty() {
        return 0;
    }
    let mut bursts = 1;
    for w in spike_times.windows(2) {
        if w[1] - w[0] > min_isi_gap {
            bursts += 1;
        }
    }
    bursts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(params: Rc1Params, drive: impl Fn(f32) -> f32, seconds: f32) -> (Vec<f32>, Vec<f32>) {
        let mut cell = Rc1::new(params);
        let dt = 0.0005;
        let steps = (seconds / dt) as usize;
        let mut v = Vec::with_capacity(steps);
        let mut t = 0.0;
        let mut times = Vec::new();
        for _ in 0..steps {
            cell.step(&[drive(t)], dt);
            v.push(cell.state.v);
            t += dt;
        }
        let mut prev = v[0];
        t = 0.0;
        for &x in &v {
            if prev < 0.0 && x >= 0.0 {
                times.push(t);
            }
            prev = x;
            t += dt;
        }
        (v, times)
    }

    #[test]
    fn weak_drive_stays_subthreshold() {
        let (v, spikes) = run(Rc1Params::subthreshold(), |_| 0.0, 1.5);
        assert!(spikes.is_empty(), "unexpected spikes: {spikes:?}");
        let max_v = v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(max_v < 0.0, "max v={max_v}");
    }

    #[test]
    fn brief_kick_makes_a_pulse() {
        let drive = |t: f32| {
            if t >= 0.20 && t < 0.215 {
                6.0
            } else {
                0.0
            }
        };
        let (_v, spikes) = run(Rc1Params::pulse(), drive, 0.8);
        assert!(
            (1..=3).contains(&spikes.len()),
            "expected an isolated pulse, got {} spikes at {spikes:?}",
            spikes.len()
        );
    }

    #[test]
    fn tonic_drive_oscillates() {
        let (_v, spikes) = run(Rc1Params::tonic(), |_| 0.0, 1.5);
        assert!(
            spikes.len() >= 4,
            "expected a spike train, got {} spikes",
            spikes.len()
        );
        // Regular-ish: first-to-last span should contain several intervals.
        if spikes.len() >= 3 {
            let isis: Vec<f32> = spikes.windows(2).map(|w| w[1] - w[0]).collect();
            let mean = isis.iter().sum::<f32>() / isis.len() as f32;
            assert!(mean > 0.0);
            let max = isis.iter().copied().fold(0.0, f32::max);
            assert!(max < 8.0 * mean, "tonic ISI blew up: {isis:?}");
        }
    }

    #[test]
    fn slow_adaptation_produces_bursts() {
        let (_v, spikes) = run(Rc1Params::burst(), |_| 0.0, 4.0);
        let bursts = count_bursts(&spikes, 0.25);
        assert!(
            spikes.len() >= 6,
            "not enough spikes for bursting: {}",
            spikes.len()
        );
        assert!(
            bursts >= 2,
            "expected ≥2 bursts, got {bursts} from spikes {spikes:?}"
        );
    }
}
