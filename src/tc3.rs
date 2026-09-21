//! TC-3: three-channel plastic analog connectome.

use crate::integrate::leaky_integrate;
use crate::module::{AnalogModule, ModuleOutputs};
use crate::noise::{ou_increment, Rng};
use crate::param::{self, params, ParamError, ParamSpec, Unit};
use crate::plasticity::{Context, Plasticity, PlasticityRule, Rule};
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["a", "b", "c", "learn", "forget"];
const OUT_JACKS: &[&str] = &["a", "b", "c", "activity"];
const STATE_FIELDS: &[&str] = &[
    "w00", "w01", "w02", "w10", "w11", "w12", "w20", "w21", "w22", "activity", "norm", "theta",
];

const PARAMS: &[ParamSpec] = params! {
    "natural_decay", Unit::Rate, 0.0 ..= 10.0, 0.02, Linear, "passive leak of every stored weight toward zero";
    "w_min", Unit::Ratio, -10.0 ..= 0.0, -2.0, Linear, "lower end stop of the analog weight memory";
    "w_max", Unit::Ratio, 0.0 ..= 10.0, 2.0, Linear, "upper end stop of the analog weight memory";
    "activity_gain", Unit::Ratio, 0.0 ..= 10.0, 0.05, Linear, "scales the sum of |dw/dt| into the ACTIVITY jack";
    "activity_rate", Unit::Rate, 0.1 ..= 2000.0, 50.0, Log, "ballistics of the ACTIVITY meter; lower is a slower needle";
    "drift", Unit::Ratio, 0.0 ..= 1.0, 0.0, Linear, "RMS wander of each stored weight, held in check by natural_decay";
    "rule", Unit::Enum(Rule::NAMES), 0.0 ..= 5.0, 0.0, Linear, "local plasticity rule";
    "deadzone", Unit::Ratio, 0.0 ..= 1.0, 0.0001, Linear, "ignore input*output products below this magnitude";
    "gamma", Unit::Ratio, 0.0 ..= 10.0, 1.0, Linear, "strength of Oja's normalizing term";
    "theta_rate", Unit::Rate, 0.001 ..= 100.0, 1.0, Log, "how fast the BCM sliding threshold follows the output";
    "diff_rate", Unit::Rate, 0.1 ..= 2000.0, 20.0, Log, "cutoff of the high-pass feeding the STDP-like rule";
    "scaling", Unit::Rate, 0.0 ..= 10.0, 0.0, Linear, "rate of slow synaptic scaling toward target_norm; zero is off";
    "target_norm", Unit::Ratio, 0.0 ..= 20.0, 1.0, Linear, "total conductance each destination channel is scaled toward";
};

#[derive(Clone, Copy, Debug)]
pub struct Tc3Params {
    pub natural_decay: f32,
    pub w_min: Volt,
    pub w_max: Volt,
    /// Scales Σ|dw/dt| into a patchable voltage.
    pub activity_gain: f32,
    /// Ballistics of the ACTIVITY meter. The raw sum of |dw/dt| spikes hard
    /// enough to sit on the rail, so it is smoothed before it leaves the jack.
    pub activity_rate: f32,
    pub plasticity: Plasticity,
    /// RMS wander of each stored weight. Zero means a mathematically ideal
    /// memory; anything else makes the connectome slowly forget where it was.
    pub drift: f32,
}

impl Default for Tc3Params {
    fn default() -> Self {
        Self {
            natural_decay: 0.02,
            w_min: -2.0,
            w_max: 2.0,
            activity_gain: 0.05,
            activity_rate: 50.0,
            plasticity: Plasticity::default(),
            drift: 0.0,
        }
    }
}

impl Tc3Params {
    pub fn vary(mut self, variation: InstanceVariation) -> Self {
        self.natural_decay = variation.scale_param(1, self.natural_decay).max(0.0);
        self.activity_gain = variation.scale_param(2, self.activity_gain);
        // Even the end stops of an analog memory are a component tolerance —
        // but an upper stop below the lower one is not a tolerance, it is a
        // contradiction, and clamping between them would panic.
        self.w_max = variation.scale_param(3, self.w_max).max(0.0);
        self.w_min = variation.scale_param(4, self.w_min).min(0.0);
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Tc3State {
    pub weights: [[Volt; 3]; 3],
    pub last_activity: Volt,
    /// Sliding average of each output channel's squared voltage.
    pub theta: [f32; 3],
    /// Low-passed inputs and outputs, so subtracting them high-passes the
    /// signal and tells a rule which way it is moving.
    pub slow_in: [Volt; 3],
    pub slow_out: [Volt; 3],
}

impl Default for Tc3State {
    fn default() -> Self {
        Self {
            weights: [[0.35, 0.08, -0.05], [0.10, 0.30, 0.06], [-0.04, 0.12, 0.28]],
            last_activity: 0.0,
            theta: [0.0; 3],
            slow_in: [0.0; 3],
            slow_out: [0.0; 3],
        }
    }
}

impl Tc3State {
    /// Σ|w| over every route feeding output channel `j`.
    pub fn column_norm(&self, j: usize) -> f32 {
        self.weights.iter().map(|row| row[j].abs()).sum()
    }

    /// Σ|w| over the whole matrix.
    pub fn total_norm(&self) -> f32 {
        self.weights
            .iter()
            .flat_map(|row| row.iter())
            .map(|w| w.abs())
            .sum()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Tc3 {
    pub params: Tc3Params,
    pub state: Tc3State,
    rng: Rng,
}

impl Tc3 {
    pub fn new(params: Tc3Params) -> Self {
        Self::instantiate(params, InstanceVariation::IDENTITY)
    }

    pub fn instantiate(params: Tc3Params, variation: InstanceVariation) -> Self {
        let mut state = Tc3State::default();
        if !variation.is_identity() {
            for i in 0..3 {
                for j in 0..3 {
                    state.weights[i][j] += variation.offset((i * 3 + j) as u32) * 0.05;
                }
            }
        }
        Self {
            params: params.vary(variation),
            state,
            rng: Rng::new(variation.seed),
        }
    }

    pub fn mix(&self, inputs: [Volt; 3]) -> [Volt; 3] {
        let mut out = [0.0; 3];
        for (x, row) in inputs.iter().zip(&self.state.weights) {
            for (slot, w) in out.iter_mut().zip(row) {
                *slot += x * w;
            }
        }
        out
    }
}

impl AnalogModule for Tc3 {
    fn kind_name(&self) -> &'static str {
        "tc3"
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
        let learn = &p.plasticity;
        Some(match name {
            "natural_decay" => p.natural_decay,
            "w_min" => p.w_min,
            "w_max" => p.w_max,
            "activity_gain" => p.activity_gain,
            "activity_rate" => p.activity_rate,
            "drift" => p.drift,
            "rule" => learn.rule.index() as f32,
            "deadzone" => learn.deadzone,
            "gamma" => learn.gamma,
            "theta_rate" => learn.theta_rate,
            "diff_rate" => learn.diff_rate,
            "scaling" => learn.scaling,
            "target_norm" => learn.target_norm,
            _ => return None,
        })
    }

    fn set_param(&mut self, name: &str, value: f32) -> Result<(), ParamError> {
        let spec = param::find_spec(PARAMS, name).ok_or_else(|| self.unknown_param(name))?;
        let v = param::checked(spec, value)?;
        let p = &mut self.params;
        match name {
            "natural_decay" => p.natural_decay = v,
            "w_min" => p.w_min = v,
            "w_max" => p.w_max = v,
            "activity_gain" => p.activity_gain = v,
            "activity_rate" => p.activity_rate = v,
            "drift" => p.drift = v,
            "rule" => p.plasticity.rule = Rule::from_index(param::choice(v)),
            "deadzone" => p.plasticity.deadzone = v,
            "gamma" => p.plasticity.gamma = v,
            "theta_rate" => p.plasticity.theta_rate = v,
            "diff_rate" => p.plasticity.diff_rate = v,
            "scaling" => p.plasticity.scaling = v,
            "target_norm" => p.plasticity.target_norm = v,
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
        let w = &self.state.weights;
        let values = [
            w[0][0],
            w[0][1],
            w[0][2],
            w[1][0],
            w[1][1],
            w[1][2],
            w[2][0],
            w[2][1],
            w[2][2],
            self.state.last_activity,
            self.state.total_norm(),
            self.state.theta.iter().sum::<f32>() / 3.0,
        ];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let a = inputs.first().copied().unwrap_or(0.0);
        let b = inputs.get(1).copied().unwrap_or(0.0);
        let c = inputs.get(2).copied().unwrap_or(0.0);
        let learn = inputs.get(3).copied().unwrap_or(0.0);
        let forget = inputs.get(4).copied().unwrap_or(0.0);
        let ins = [a, b, c];
        let outs = self.mix(ins);

        let p = self.params;
        let rule = &p.plasticity;
        let activity_in = self.state.last_activity;
        let norms = [
            self.state.column_norm(0),
            self.state.column_norm(1),
            self.state.column_norm(2),
        ];

        // Slow local averages the rules read: a sliding activity threshold and
        // low-passed signals whose difference says which way each is moving.
        let mut d_in = [0.0; 3];
        let mut d_out = [0.0; 3];
        for j in 0..3 {
            self.state.theta[j] = leaky_integrate(
                self.state.theta[j],
                rule.theta_rate * outs[j] * outs[j],
                rule.theta_rate,
                dt,
            );
            self.state.slow_in[j] = leaky_integrate(
                self.state.slow_in[j],
                rule.diff_rate * ins[j],
                rule.diff_rate,
                dt,
            );
            self.state.slow_out[j] = leaky_integrate(
                self.state.slow_out[j],
                rule.diff_rate * outs[j],
                rule.diff_rate,
                dt,
            );
            d_in[j] = ins[j] - self.state.slow_in[j];
            d_out[j] = outs[j] - self.state.slow_out[j];
        }
        let theta = self.state.theta;

        let rng = &mut self.rng;
        let mut activity = 0.0;
        for (i, row) in self.state.weights.iter_mut().enumerate() {
            for (j, w) in row.iter_mut().enumerate() {
                let ctx = Context {
                    input: ins[i],
                    output: outs[j],
                    weight: *w,
                    learn,
                    forget,
                    theta: theta[j],
                    column_norm: norms[j],
                    activity: activity_in,
                    d_input: d_in[i],
                    d_output: d_out[j],
                };
                let plastic = rule.dw(&ctx);
                let dw = plastic - p.natural_decay * *w;
                activity += dw.abs();
                // Exact leak toward the instantaneous plastic drive, then the
                // memory's own imperfection on top. The decay is already the
                // relaxation that keeps the wander bounded.
                let next = leaky_integrate(*w, plastic, p.natural_decay, dt);
                let wander = ou_increment(rng, p.drift, p.natural_decay, dt);
                *w = (next + wander).clamp(p.w_min, p.w_max);
            }
        }
        let target = p.activity_gain * activity;
        let smoothed = leaky_integrate(
            self.state.last_activity,
            p.activity_rate * target,
            p.activity_rate,
            dt,
        );
        let activity_v = clamp_rail(smoothed, DEFAULT_RAIL);
        self.state.last_activity = activity_v;

        ModuleOutputs::new(&[
            clamp_rail(outs[0], DEFAULT_RAIL),
            clamp_rail(outs[1], DEFAULT_RAIL),
            clamp_rail(outs[2], DEFAULT_RAIL),
            activity_v,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_are_linear_mix() {
        let mut m = Tc3::new(Tc3Params::default());
        m.state.weights = [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, -1.0]];
        let y = m.step(&[1.0, 0.5, 0.25, 0.0, 0.0], 0.0005);
        assert!((y[0] - 1.0).abs() < 1e-5);
        assert!((y[1] - 1.0).abs() < 1e-5);
        assert!((y[2] + 0.25).abs() < 1e-5);
    }

    #[test]
    fn learn_grows_coincident_weight() {
        let mut m = Tc3::new(Tc3Params {
            natural_decay: 0.0,
            ..Tc3Params::default()
        });
        m.state.weights = [[0.2, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]];
        let before = m.state.weights[0][0];
        for _ in 0..2000 {
            m.step(&[2.0, 0.0, 0.0, 1.0, 0.0], 0.0005);
        }
        assert!(
            m.state.weights[0][0] > before + 0.05,
            "w00 {} did not grow from {before}",
            m.state.weights[0][0]
        );
    }

    #[test]
    fn forget_and_decay_shrink_weight() {
        let mut m = Tc3::new(Tc3Params {
            natural_decay: 2.0,
            ..Tc3Params::default()
        });
        m.state.weights = [[1.2, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]];
        for _ in 0..4000 {
            m.step(&[0.0, 0.0, 0.0, 0.0, 3.0], 0.0005);
        }
        assert!(
            m.state.weights[0][0].abs() < 0.2,
            "w00 still {}",
            m.state.weights[0][0]
        );
    }

    #[test]
    fn state_fields_track_the_weight_matrix() {
        let mut m = Tc3::default();
        m.state.weights[1][2] = 0.77;
        let snapshot = m.snapshot();
        let w12 = snapshot.iter().find(|(k, _)| *k == "w12").unwrap().1;
        assert!((w12 - 0.77).abs() < 1e-6);
    }

    /// Drive one route hard with LEARN high and report where its weight ends up.
    fn learn_to_rest(plasticity: Plasticity, seconds: f32) -> Tc3 {
        let mut m = Tc3::new(Tc3Params {
            natural_decay: 0.0,
            plasticity,
            ..Tc3Params::default()
        });
        m.state.weights = [[0.2, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]];
        let dt = 0.0005;
        for _ in 0..(seconds / dt) as usize {
            m.step(&[1.0, 0.0, 0.0, 1.0, 0.0], dt);
        }
        m
    }

    #[test]
    fn oja_settles_a_weight_where_hebbian_pins_it() {
        let hebbian = learn_to_rest(Plasticity::default(), 20.0);
        assert!(
            (hebbian.state.weights[0][0] - hebbian.params.w_max).abs() < 1e-4,
            "hebbian should have pinned at the end stop, got {}",
            hebbian.state.weights[0][0]
        );

        // Oja's fixed point for a single active route is w = 1/sqrt(gamma).
        let oja = learn_to_rest(
            Plasticity {
                rule: Rule::Oja,
                gamma: 4.0,
                ..Plasticity::default()
            },
            20.0,
        );
        assert!(
            (oja.state.weights[0][0] - 0.5).abs() < 0.02,
            "oja settled at {} instead of 0.5",
            oja.state.weights[0][0]
        );
    }

    #[test]
    fn synaptic_scaling_keeps_a_channel_off_its_end_stop() {
        // Scaling competes with learning rather than dictating the answer, so
        // the channel lands between the target and the end stop, and pulling
        // harder lands closer to the target.
        let norm_at = |scaling| {
            learn_to_rest(
                Plasticity {
                    scaling,
                    target_norm: 0.8,
                    ..Plasticity::default()
                },
                30.0,
            )
            .state
            .column_norm(0)
        };
        let gentle = norm_at(5.0);
        let firm = norm_at(50.0);
        assert!(gentle < 2.0, "gentle scaling still pinned: {gentle}");
        assert!(
            0.8 < firm && firm < gentle,
            "firmer scaling should land nearer 0.8: gentle={gentle} firm={firm}"
        );
        assert!((firm - 0.8).abs() < 0.1, "firm scaling settled at {firm}");
    }

    #[test]
    fn anti_hebbian_dissolves_a_route_that_hebbian_would_grow() {
        let anti = learn_to_rest(
            Plasticity {
                rule: Rule::AntiHebbian,
                ..Plasticity::default()
            },
            5.0,
        );
        // A lone route's own output vanishes with it, so it decays toward zero
        // rather than reversing sign.
        assert!(
            anti.state.weights[0][0] < 0.01,
            "anti-hebbian left w00 at {}",
            anti.state.weights[0][0]
        );
        let hebbian = learn_to_rest(Plasticity::default(), 5.0);
        assert!(hebbian.state.weights[0][0] > 0.2);
    }

    #[test]
    fn three_factor_learns_faster_when_the_matrix_is_already_moving() {
        let plain = learn_to_rest(Plasticity::default(), 1.0);
        let accelerated = learn_to_rest(
            Plasticity {
                rule: Rule::ThreeFactor,
                ..Plasticity::default()
            },
            1.0,
        );
        assert!(
            accelerated.state.weights[0][0] > plain.state.weights[0][0],
            "three-factor {} did not outrun hebbian {}",
            accelerated.state.weights[0][0],
            plain.state.weights[0][0]
        );
    }

    #[test]
    fn theta_tracks_the_output_it_is_asked_to_judge() {
        let mut m = Tc3::new(Tc3Params {
            plasticity: Plasticity {
                rule: Rule::Bcm,
                theta_rate: 20.0,
                ..Plasticity::default()
            },
            ..Tc3Params::default()
        });
        m.state.weights = [[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]];
        let mut out = 0.0;
        for _ in 0..8000 {
            out = m.step(&[1.5, 0.0, 0.0, 0.0, 0.0], 0.0005)[0];
        }
        assert!(
            (m.state.theta[0] - out * out).abs() < 0.05 * out * out,
            "theta {} did not settle at out^2 {}",
            m.state.theta[0],
            out * out
        );
    }

    #[test]
    fn drift_makes_the_memory_wander_and_zero_holds_it() {
        let quiet = |drift| Tc3Params {
            natural_decay: 0.5,
            drift,
            ..Tc3Params::default()
        };
        let mut ideal = Tc3::instantiate(quiet(0.0), InstanceVariation::new(2, 0.0));
        let mut drifting = Tc3::instantiate(quiet(0.3), InstanceVariation::new(2, 0.0));
        let start = ideal.state.weights[0][0];
        let mut wander: f32 = 0.0;
        for _ in 0..40_000 {
            // No inputs, so nothing but decay and imperfection moves a weight.
            ideal.step(&[0.0; 5], 0.0005);
            drifting.step(&[0.0; 5], 0.0005);
            wander = wander.max((drifting.state.weights[0][0] - ideal.state.weights[0][0]).abs());
        }
        assert!(
            ideal.state.weights[0][0].abs() < 1e-3,
            "ideal weight should have decayed to zero from {start}, got {}",
            ideal.state.weights[0][0]
        );
        assert!(wander > 0.05, "memory barely drifted: {wander}");
        assert!(
            drifting.state.weights[0][0].abs() < drifting.params.w_max,
            "drift pinned the weight at its end stop"
        );
    }

    #[test]
    fn heavy_tolerance_cannot_turn_the_end_stops_inside_out() {
        // Tolerance wide enough to flip a sign used to put w_max below w_min,
        // and clamping between them panics.
        for seed in 0..64 {
            let m = Tc3::instantiate(Tc3Params::default(), InstanceVariation::new(seed, 1.0));
            assert!(
                m.params.w_min <= m.params.w_max,
                "seed {seed} gave w_min {} above w_max {}",
                m.params.w_min,
                m.params.w_max
            );
        }
        let mut wild = Tc3::instantiate(Tc3Params::default(), InstanceVariation::new(7, 1.0));
        for _ in 0..200 {
            wild.step(&[1.0, 1.0, 1.0, 1.0, 0.0], 0.0005);
        }
        assert!(wild.state.weights[0][0].is_finite());
    }

    #[test]
    fn mismatch_individualizes_the_end_stops() {
        let a = Tc3::instantiate(Tc3Params::default(), InstanceVariation::new(1, 0.1));
        let b = Tc3::instantiate(Tc3Params::default(), InstanceVariation::new(2, 0.1));
        assert_ne!(a.params.w_max, b.params.w_max);
        assert_ne!(a.state.weights[0][0], b.state.weights[0][0]);
        let ideal = Tc3::instantiate(Tc3Params::default(), InstanceVariation::new(1, 0.0));
        assert_eq!(ideal.params.w_max, Tc3Params::default().w_max);
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let m = Tc3::default();
        for spec in m.params() {
            let got = m.get_param(spec.name).expect(spec.name);
            assert!(
                (got - spec.default).abs() < 1e-9,
                "{} default {} but module has {got}",
                spec.name,
                spec.default
            );
        }
    }
}
