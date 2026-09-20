//! Envelope generator and slew limiter. A utility, not a neural module.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["gate"];
const OUT_JACKS: &[&str] = &["out", "inv"];
const STATE_FIELDS: &[&str] = &["out", "falling"];

/// How the gate is interpreted.
pub const MODES: &[&str] = &["ad", "ar", "slew"];

const PARAMS: &[ParamSpec] = params! {
    "attack", Unit::Seconds, 0.0002 ..= 20.0, 0.01, Log, "time to rise";
    "decay", Unit::Seconds, 0.0002 ..= 60.0, 0.3, Log, "time to fall";
    "mode", Unit::Enum(MODES), 0.0 ..= 2.0, 0.0, Linear, "ad fires on an edge, ar follows the gate, slew smooths it";
    "threshold", Unit::Volt, 0.01 ..= 10.0, 1.0, Linear, "gate voltage that counts as high";
    "level", Unit::Volt, 0.0 ..= 10.0, 5.0, Linear, "output amplitude";
};

/// Three shapes of the same circuit.
///
/// `ad` fires a full rise and fall from one edge, which is what turns an RC-1
/// burst into a percussive envelope. `ar` holds while the gate is high. `slew`
/// simply rate-limits whatever arrives, which also makes it the smoother for a
/// noisy control voltage.
#[derive(Clone, Debug)]
pub struct Env {
    pub attack: f32,
    pub decay: f32,
    pub mode: usize,
    pub threshold: Volt,
    pub level: Volt,
    /// Envelope in 0..1.
    value: f32,
    falling: bool,
    last_gate: Volt,
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    pub fn new() -> Self {
        Self {
            attack: 0.01,
            decay: 0.3,
            mode: 0,
            threshold: 1.0,
            level: 5.0,
            value: 0.0,
            falling: false,
            last_gate: 0.0,
        }
    }

    /// Fraction of the way from `from` to `to` one step of `time` covers.
    fn ramp(value: f32, target: f32, time: f32, dt: f32) -> f32 {
        // Straight-line segments, so the knob reads as a time rather than as a
        // time constant nobody can predict from the panel.
        let step = dt / time.max(1e-6);
        if target > value {
            (value + step).min(target)
        } else {
            (value - step).max(target)
        }
    }
}

impl AnalogModule for Env {
    fn kind_name(&self) -> &'static str {
        "env"
    }

    fn in_jacks(&self) -> &'static [&'static str] {
        IN_JACKS
    }

    fn out_jacks(&self) -> &'static [&'static str] {
        OUT_JACKS
    }

    knob_accessors! {
        PARAMS;
        floats {
            "attack" => attack,
            "decay" => decay,
            "threshold" => threshold,
            "level" => level,
        }
        choices {
            "mode" => mode,
        }
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        // Report the jack voltage, not the internal 0..1 envelope, so
        // `record env.out` means the same thing as the cable carries.
        let values = [
            self.level * self.value,
            if self.falling { 1.0 } else { 0.0 },
        ];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let gate = inputs.first().copied().unwrap_or(0.0);
        let high = gate >= self.threshold;
        let rising_edge = high && self.last_gate < self.threshold;
        self.last_gate = gate;

        match self.mode {
            // Follow the gate up, release when it drops.
            1 => {
                self.falling = !high;
                let target = if high { 1.0 } else { 0.0 };
                let time = if high { self.attack } else { self.decay };
                self.value = Self::ramp(self.value, target, time, dt);
            }
            // Rate-limited follower of the input itself.
            2 => {
                let target = (gate / self.level.max(1e-6)).clamp(-1.0, 1.0);
                self.falling = target < self.value;
                let time = if target > self.value {
                    self.attack
                } else {
                    self.decay
                };
                self.value = Self::ramp(self.value, target, time, dt);
            }
            // One edge fires a whole rise and fall.
            _ => {
                if rising_edge {
                    self.falling = false;
                }
                if !self.falling {
                    self.value = Self::ramp(self.value, 1.0, self.attack, dt);
                    if self.value >= 1.0 {
                        self.falling = true;
                    }
                } else {
                    self.value = Self::ramp(self.value, 0.0, self.decay, dt);
                }
            }
        }

        let out = clamp_rail(self.level * self.value, DEFAULT_RAIL);
        ModuleOutputs::new(&[out, -out])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 20_000.0;

    fn env(mode: f32, attack: f32, decay: f32) -> Env {
        let mut env = Env::new();
        env.set_param("mode", mode).unwrap();
        env.set_param("attack", attack).unwrap();
        env.set_param("decay", decay).unwrap();
        env
    }

    /// Run with a fixed gate and return the output trace.
    fn trace(env: &mut Env, gate: Volt, seconds: f32) -> Vec<Volt> {
        (0..(seconds / DT) as usize)
            .map(|_| env.step(&[gate], DT)[0])
            .collect()
    }

    #[test]
    fn ad_fires_a_whole_shape_from_one_edge() {
        let mut env = env(0.0, 0.05, 0.1);
        // A gate lasting a single step is enough.
        env.step(&[5.0], DT);
        let after = trace(&mut env, 0.0, 0.5);
        let peak = after.iter().fold(0.0f32, |m, v| m.max(*v));
        assert!(
            (peak - 5.0).abs() < 0.05,
            "never reached full level: {peak}"
        );
        assert!(
            after.last().unwrap().abs() < 0.05,
            "never returned to rest: {}",
            after.last().unwrap()
        );
    }

    #[test]
    fn the_attack_knob_is_a_time_in_seconds() {
        for attack in [0.02, 0.1] {
            let mut env = env(0.0, attack, 10.0);
            let mut elapsed = 0.0;
            loop {
                let out = env.step(&[5.0], DT)[0];
                elapsed += DT;
                if out >= 4.99 || elapsed > 1.0 {
                    break;
                }
            }
            assert!(
                (elapsed - attack).abs() < 0.01,
                "attack {attack} took {elapsed}"
            );
        }
    }

    #[test]
    fn ar_holds_while_the_gate_is_high() {
        let mut env = env(1.0, 0.01, 0.2);
        let held = trace(&mut env, 5.0, 0.3);
        assert!(
            (held.last().unwrap() - 5.0).abs() < 0.01,
            "did not hold: {}",
            held.last().unwrap()
        );
        let released = trace(&mut env, 0.0, 0.3);
        assert!(released.last().unwrap().abs() < 0.01, "did not release");
    }

    #[test]
    fn slew_mode_rate_limits_a_jumping_input() {
        let mut env = env(2.0, 0.1, 0.1);
        // A step input should take the attack time to arrive.
        let rising = trace(&mut env, 5.0, 0.02);
        assert!(
            *rising.last().unwrap() < 2.0,
            "slew let a step straight through: {}",
            rising.last().unwrap()
        );
        let settled = trace(&mut env, 5.0, 0.2);
        assert!((settled.last().unwrap() - 5.0).abs() < 0.05);
    }

    #[test]
    fn the_threshold_knob_decides_what_counts_as_a_gate() {
        let mut env = env(1.0, 0.01, 0.01);
        env.set_param("threshold", 3.0).unwrap();
        let weak = trace(&mut env, 2.0, 0.1);
        assert_eq!(*weak.last().unwrap(), 0.0, "2 V opened a 3 V gate");
        let strong = trace(&mut env, 4.0, 0.1);
        assert!(*strong.last().unwrap() > 4.9, "4 V did not open it");
    }

    #[test]
    fn the_recorded_state_matches_the_jack() {
        let mut env = env(1.0, 0.001, 10.0);
        let out = trace(&mut env, 5.0, 0.05);
        let jack = *out.last().unwrap();
        assert!(jack > 4.9, "jack {jack}");
        assert!(
            (env.snapshot()[0].1 - jack).abs() < 1e-6,
            "state says {} but the jack carries {jack}",
            env.snapshot()[0].1
        );
    }

    #[test]
    fn the_inverted_output_mirrors_the_envelope() {
        let mut env = env(1.0, 0.001, 0.1);
        let out = env.step(&[5.0], DT);
        assert!((out[0] + out[1]).abs() < 1e-6);
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let env = Env::new();
        for spec in env.params() {
            let got = env.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
