//! RC-1: three-state analog reservoir / excitable cell.
//!
//! States are membrane `v`, fast recovery `r`, and slow adaptation `s`.
//! Excitability is continuous (N-shaped / Hindmarsh–Rose-like), not a
//! digital integrate-and-fire comparator.

use crate::integrate::softplus;
use crate::module::{AnalogModule, ModuleOutputs};
use crate::noise::{ou_increment, ou_step, Rng};
use crate::param::{self, params, ParamError, ParamSpec, Unit};
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["in"];
const OUT_JACKS: &[&str] = &["out"];
const STATE_FIELDS: &[&str] = &["v", "r", "s", "drift"];

const PARAMS: &[ParamSpec] = params! {
    "capacitance", Unit::Ratio, 0.01 ..= 100.0, 1.0, Log, "effective capacitance; larger is a slower membrane";
    "leak", Unit::Rate, 0.0 ..= 50.0, 0.0, Linear, "extra linear leak on v";
    "threshold", Unit::Volt, -10.0 ..= 10.0, -1.6, Linear, "adaptation setpoint / rest-like voltage";
    "sharpness", Unit::Ratio, 0.0 ..= 20.0, 3.0, Linear, "regenerative v^2 gain; higher is sharper excitability";
    "cubic", Unit::Ratio, 0.0 ..= 10.0, 1.0, Linear, "restoring v^3 gain";
    "discharge", Unit::Ratio, 0.0 ..= 10.0, 1.0, Linear, "how strongly recovery feeds back into the membrane";
    "recovery_rate", Unit::Rate, 0.001 ..= 100.0, 1.0, Log, "fast recovery rate (1/tau_r)";
    "recovery_drive", Unit::Ratio, 0.0 ..= 20.0, 5.0, Linear, "quadratic recovery drive";
    "adaptation_rate", Unit::Rate, 0.00001 ..= 10.0, 0.006, Log, "slow adaptation rate (1/tau_s); keep well below recovery_rate to burst";
    "adaptation_depth", Unit::Ratio, 0.0 ..= 20.0, 4.0, Linear, "how strongly v charges the slow variable";
    "drive_gain", Unit::Ratio, -10.0 ..= 10.0, 1.0, Linear, "scale on the patched input current";
    "output_gain", Unit::Ratio, 0.0 ..= 10.0, 5.0, Linear, "output scale after saturation";
    "rate", Unit::Ratio, 0.01 ..= 20000.0, 80.0, Log, "model time per second of wall clock; raise into the thousands to hear the cell";
    "bias", Unit::Volt, -10.0 ..= 10.0, 2.0, Linear, "constant bias current";
    "noise", Unit::Volt, 0.0 ..= 1.0, 0.0, Linear, "RMS of the thermal fluctuation on the membrane, independent of dt";
    "drift", Unit::Volt, 0.0 ..= 1.0, 0.0, Linear, "RMS of the slow wander of the threshold, on the adaptation timescale";
};

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
    /// Model time per second of wall clock. 80 keeps the cell biological;
    /// thousands put it in the audio band.
    pub rate: f32,
    /// Constant bias current (HR `I` offset).
    pub bias: f32,
    /// Thermal noise RMS in volts. Zero means a mathematically ideal module.
    pub noise: f32,
    /// RMS of the slow wander of `threshold`. Zero holds the panel setting
    /// exactly; a small value is what puts a cell in and out of its
    /// near-threshold region over minutes.
    pub drift: f32,
}

impl Default for Rc1Params {
    fn default() -> Self {
        Self::burst()
    }
}

/// Builds one named factory preset.
pub type Rc1Preset = fn() -> Rc1Params;

impl Rc1Params {
    /// Factory presets addressable by name from a patch file.
    pub const PRESETS: &'static [(&'static str, Rc1Preset)] = &[
        ("subthreshold", Rc1Params::subthreshold),
        ("pulse", Rc1Params::pulse),
        ("tonic", Rc1Params::tonic),
        ("burst", Rc1Params::burst),
    ];

    pub fn from_preset(name: &str) -> Option<Self> {
        Self::PRESETS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, make)| make())
    }

    pub fn preset_names() -> Vec<&'static str> {
        Self::PRESETS.iter().map(|(n, _)| *n).collect()
    }

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
            rate: 80.0,
            bias: 0.0,
            noise: 0.0,
            drift: 0.0,
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
            leak: 0.0,
            adaptation_rate: 0.006,
            bias: 2.0,
            ..Self::subthreshold()
        }
    }

    pub fn vary(mut self, variation: InstanceVariation) -> Self {
        self.threshold += variation.offset(1) * 0.05;
        self.sharpness = variation.scale_param(2, self.sharpness);
        self.adaptation_rate = variation.scale_param(3, self.adaptation_rate).max(1e-6);
        self.bias += variation.offset(4) * 0.05;
        self.capacitance = variation.scale_param(5, self.capacitance).max(1e-4);
        self.recovery_rate = variation.scale_param(6, self.recovery_rate).max(0.0);
        self.output_gain = variation.scale_param(7, self.output_gain);
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Rc1State {
    pub v: Volt,
    pub r: Volt,
    pub s: Volt,
    /// Accumulated slow wander added to the threshold knob.
    pub drift: Volt,
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
            drift: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Rc1 {
    pub params: Rc1Params,
    pub state: Rc1State,
    rng: Rng,
}

impl Default for Rc1 {
    fn default() -> Self {
        Self::new(Rc1Params::default())
    }
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
        Self {
            params,
            state,
            rng: Rng::new(variation.seed),
        }
    }

    /// Threshold actually in force, including any accumulated drift.
    pub fn effective_threshold(&self) -> Volt {
        self.params.threshold + self.state.drift
    }

    fn derivatives(&self, v: f32, r: f32, s: f32, i_in: f32) -> (f32, f32, f32) {
        let p = &self.params;
        let c = p.capacitance.max(1e-4);
        // Smooth regenerative current: quadratic / cubic N-shape, plus a
        // softplus kick that grows as v approaches the firing region.
        let exc = p.sharpness * v * v - p.cubic * v * v * v + 0.15 * softplus(p.sharpness * v);
        let drive = p.drive_gain * i_in + p.bias;
        // Hindmarsh–Rose-like: recovery `r` is added, adaptation `s` subtracted.
        // `discharge` scales the recovery contribution (1.0 = standard HR).
        let dv = (p.discharge * r + exc - s + drive - p.leak * v) / c;
        let dr = p.recovery_rate * (1.0 - p.recovery_drive * v * v - r);
        let ds = p.adaptation_rate * (p.adaptation_depth * (v - self.effective_threshold()) - s);
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
    fn kind_name(&self) -> &'static str {
        "rc1"
    }

    fn in_jacks(&self) -> &'static [&'static str] {
        IN_JACKS
    }

    fn out_jacks(&self) -> &'static [&'static str] {
        OUT_JACKS
    }

    fn params(&self) -> &'static [ParamSpec] {
        PARAMS
    }

    fn get_param(&self, name: &str) -> Option<f32> {
        let p = &self.params;
        Some(match name {
            "capacitance" => p.capacitance,
            "leak" => p.leak,
            "threshold" => p.threshold,
            "sharpness" => p.sharpness,
            "cubic" => p.cubic,
            "discharge" => p.discharge,
            "recovery_rate" => p.recovery_rate,
            "recovery_drive" => p.recovery_drive,
            "adaptation_rate" => p.adaptation_rate,
            "adaptation_depth" => p.adaptation_depth,
            "drive_gain" => p.drive_gain,
            "output_gain" => p.output_gain,
            "rate" => p.rate,
            "bias" => p.bias,
            "noise" => p.noise,
            "drift" => p.drift,
            _ => return None,
        })
    }

    fn set_param(&mut self, name: &str, value: f32) -> Result<(), ParamError> {
        let spec = param::find_spec(PARAMS, name).ok_or_else(|| self.unknown_param(name))?;
        let v = param::checked(spec, value)?;
        let p = &mut self.params;
        match name {
            "capacitance" => p.capacitance = v,
            "leak" => p.leak = v,
            "threshold" => p.threshold = v,
            "sharpness" => p.sharpness = v,
            "cubic" => p.cubic = v,
            "discharge" => p.discharge = v,
            "recovery_rate" => p.recovery_rate = v,
            "recovery_drive" => p.recovery_drive = v,
            "adaptation_rate" => p.adaptation_rate = v,
            "adaptation_depth" => p.adaptation_depth = v,
            "drive_gain" => p.drive_gain = v,
            "output_gain" => p.output_gain = v,
            "rate" => p.rate = v,
            "bias" => p.bias = v,
            "noise" => p.noise = v,
            "drift" => p.drift = v,
            _ => return Err(self.unknown_param(name)),
        }
        Ok(())
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn apply_variation(&mut self, variation: InstanceVariation) {
        *self = Self::instantiate(self.params, variation);
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.state.v, self.state.r, self.state.s, self.state.drift];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let i_in = inputs.first().copied().unwrap_or(0.0);
        let dt_model = dt * self.params.rate;
        self.rk2_step(i_in, dt_model);
        // Recovery sets how fast the membrane is pulled back, so it is the
        // relaxation the thermal fluctuation has to push against.
        self.state.v += ou_increment(
            &mut self.rng,
            self.params.noise,
            self.params.recovery_rate,
            dt_model,
        );
        // The threshold wanders on the slowest timescale the cell has.
        self.state.drift = ou_step(
            &mut self.rng,
            self.state.drift,
            self.params.drift,
            self.params.adaptation_rate,
            dt_model,
        );
        let out = self.params.output_gain * self.state.v.tanh();
        ModuleOutputs::one(clamp_rail(out, DEFAULT_RAIL))
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
            if (0.20..0.215).contains(&t) {
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

    #[test]
    fn presets_are_addressable_by_name() {
        for name in Rc1Params::preset_names() {
            assert!(Rc1Params::from_preset(name).is_some(), "{name}");
        }
        assert!(Rc1Params::from_preset("nope").is_none());
    }

    #[test]
    fn rate_knob_scales_model_time() {
        let slow = run(
            Rc1Params {
                rate: 40.0,
                ..Rc1Params::tonic()
            },
            |_| 0.0,
            1.0,
        );
        let fast = run(
            Rc1Params {
                rate: 160.0,
                ..Rc1Params::tonic()
            },
            |_| 0.0,
            1.0,
        );
        assert!(
            fast.1.len() > 2 * slow.1.len(),
            "rate did not speed the cell up: {} vs {}",
            slow.1.len(),
            fast.1.len()
        );
    }

    /// Upward zero crossings of a freely running cell.
    fn spike_count(params: Rc1Params, variation: InstanceVariation, seconds: f32) -> usize {
        let mut cell = Rc1::instantiate(params, variation);
        let dt = 0.0005;
        let mut spikes = 0;
        let mut prev = cell.state.v;
        for _ in 0..(seconds / dt) as usize {
            cell.step(&[0.0], dt);
            if prev < 0.0 && cell.state.v >= 0.0 {
                spikes += 1;
            }
            prev = cell.state.v;
        }
        spikes
    }

    #[test]
    fn noise_pushes_a_resting_cell_over_threshold() {
        // Near-threshold instability: silent when ideal, firing once thermal
        // noise is on, with nothing else on the panel changed.
        let quiet = Rc1Params {
            bias: 0.1,
            ..Rc1Params::subthreshold()
        };
        let ideal = InstanceVariation::new(1, 0.0);
        assert_eq!(spike_count(quiet, ideal, 8.0), 0, "ideal cell fired");
        for seed in [1, 32, 63, 94] {
            let spikes = spike_count(
                Rc1Params {
                    noise: 0.6,
                    ..quiet
                },
                InstanceVariation::new(seed, 0.0),
                8.0,
            );
            assert!(spikes > 0, "seed {seed} never crossed threshold");
        }
    }

    #[test]
    fn drift_wanders_the_threshold_and_zero_holds_it() {
        let dt = 0.0005;
        let mut ideal = Rc1::instantiate(Rc1Params::burst(), InstanceVariation::new(1, 0.0));
        let mut drifting = Rc1::instantiate(
            Rc1Params {
                drift: 0.2,
                ..Rc1Params::burst()
            },
            InstanceVariation::new(1, 0.0),
        );
        let mut spread: f32 = 0.0;
        for _ in 0..(20.0 / dt) as usize {
            ideal.step(&[0.0], dt);
            drifting.step(&[0.0], dt);
            spread = spread.max(drifting.state.drift.abs());
        }
        assert_eq!(ideal.state.drift, 0.0, "an ideal threshold must not move");
        assert_eq!(ideal.effective_threshold(), ideal.params.threshold);
        assert!(spread > 0.02, "threshold barely moved: {spread}");
        assert!(spread < 1.0, "threshold ran away: {spread}");
    }

    #[test]
    fn mismatch_spreads_a_bank_of_nominally_identical_cells() {
        // No noise, so any spread comes purely from component tolerance.
        let counts: Vec<usize> = (0..8)
            .map(|seed| spike_count(Rc1Params::tonic(), InstanceVariation::new(seed, 0.08), 3.0))
            .collect();
        let min = counts.iter().copied().min().unwrap();
        let max = counts.iter().copied().max().unwrap();
        assert!(min > 0, "some cell went silent: {counts:?}");
        assert!(max > min, "mismatch produced identical cells: {counts:?}");

        let ideal: Vec<usize> = (0..4)
            .map(|seed| spike_count(Rc1Params::tonic(), InstanceVariation::new(seed, 0.0), 3.0))
            .collect();
        assert!(
            ideal.iter().all(|n| *n == ideal[0]),
            "ideal modules should be identical: {ideal:?}"
        );
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let cell = Rc1::default();
        for spec in cell.params() {
            let got = cell.get_param(spec.name).expect(spec.name);
            assert!(
                (got - spec.default).abs() < 1e-9,
                "{} default {} but module has {got}",
                spec.name,
                spec.default
            );
        }
    }
}
