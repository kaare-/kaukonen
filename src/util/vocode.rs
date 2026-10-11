//! Analog vocoder with a scale-locked carrier. A utility, not a neural module.

use super::scale::{self, SCALES};
use super::{poly_blep, volts_to_ratio};
use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

/// Formant-ish centres, a little tighter than the ADDAC bank.
const CENTRES: [f32; 8] = [100.0, 200.0, 350.0, 600.0, 1_000.0, 1_700.0, 3_000.0, 5_500.0];

const IN_JACKS: &[&str] = &["mod", "carrier", "shift_cv", "root_cv"];
const OUT_JACKS: &[&str] = &["out", "v_oct", "env", "lock"];
const STATE_FIELDS: &[&str] = &["pitch", "snapped", "sung", "lock", "env"];

const PARAMS: &[ParamSpec] = params! {
    "q", Unit::Ratio, 0.6 ..= 8.0, 2.4, Log, "selectivity of each vocoder band";
    "follow", Unit::Seconds, 0.004 ..= 0.4, 0.018, Log, "envelope time; faster is more syllables";
    "shift", Unit::Ratio, -2.0 ..= 2.0, 0.0, Linear, "formant shift in octaves; + is a smaller creature";
    "shift_depth", Unit::Ratio, -2.0 ..= 2.0, 0.25, Linear, "octaves per volt on SHIFT CV";
    "mix", Unit::Ratio, 0.0 ..= 1.0, 0.78, Linear, "dry modulator vs sung carrier";
    "tune", Unit::Ratio, 0.0 ..= 1.0, 0.62, Linear, "0 is the patched carrier, 1 is the autotuned saw";
    "scale", Unit::Enum(SCALES), 0.0 ..= 6.0, 6.0, Linear, "how the tracker snaps; phrygian is the flat 2";
    "root", Unit::Hertz, 20.0 ..= 2000.0, 55.0, Log, "scale root; 55 is A1";
    "glide", Unit::Seconds, 0.002 ..= 0.8, 0.06, Log, "how slowly the sung pitch walks";
    "level", Unit::Ratio, 0.0 ..= 4.0, 1.0, Linear, "output scale";
};

#[derive(Clone, Copy, Debug, Default)]
struct Cell {
    s1: f32,
    s2: f32,
}

/// Eight-band vocoder whose carrier can be a scale-locked saw.
///
/// The modulator is split into formant bands and those envelopes open the
/// same bands on a carrier. An external carrier is the usual analog path;
/// `tune` crossfades in an internal saw whose pitch is tracked from the
/// modulator and snapped to a scale. `v_oct` is that note relative to
/// `root`. Silence holds the last note rather than hunting.
#[derive(Clone, Debug)]
pub struct Vocode {
    pub q: f32,
    pub follow: f32,
    pub shift: f32,
    pub shift_depth: f32,
    pub mix: f32,
    pub tune: f32,
    pub scale: usize,
    pub root: f32,
    pub glide: f32,
    pub level: f32,
    mod_cells: [Cell; 8],
    car_cells: [Cell; 8],
    envs: [f32; 8],
    hp_x: f32,
    hp: f32,
    lp: f32,
    timer: f32,
    pitch: f32,
    snapped: f32,
    sung: f32,
    sung_log: f32,
    lock: f32,
    env: f32,
    phase: f32,
}

impl Default for Vocode {
    fn default() -> Self {
        Self::new()
    }
}

impl Vocode {
    pub fn new() -> Self {
        Self {
            q: 2.4,
            follow: 0.018,
            shift: 0.0,
            shift_depth: 0.25,
            mix: 0.78,
            tune: 0.62,
            scale: 6,
            root: 55.0,
            glide: 0.06,
            level: 1.0,
            mod_cells: [Cell::default(); 8],
            car_cells: [Cell::default(); 8],
            envs: [0.0; 8],
            hp_x: 0.0,
            hp: 0.0,
            lp: 0.0,
            timer: 0.0,
            pitch: 55.0,
            snapped: 55.0,
            sung: 55.0,
            sung_log: 55.0f32.ln(),
            lock: 0.0,
            env: 0.0,
            phase: 0.0,
        }
    }

    fn bandpass(cell: &mut Cell, x: f32, hz: f32, q: f32, dt: f32) -> f32 {
        let ceiling = 0.45 / dt.max(1e-9);
        let corner = hz.clamp(20.0, ceiling);
        let g = (std::f32::consts::PI * corner * dt).tan();
        let k = 1.0 / q.max(0.35);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let v3 = x - cell.s2;
        let bp = a1 * cell.s1 + a2 * v3;
        let lp = cell.s2 + a2 * cell.s1 + g * a2 * v3;
        cell.s1 = 2.0 * bp - cell.s1;
        cell.s2 = 2.0 * lp - cell.s2;
        bp
    }

    /// Pitch actually being sung, after glide.
    pub fn sung(&self) -> f32 {
        self.sung
    }

    /// Last snapped scale degree in hertz.
    pub fn snapped(&self) -> f32 {
        self.snapped
    }
}

impl AnalogModule for Vocode {
    fn kind_name(&self) -> &'static str {
        "vocode"
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
            "q" => q,
            "follow" => follow,
            "shift" => shift,
            "shift_depth" => shift_depth,
            "mix" => mix,
            "tune" => tune,
            "root" => root,
            "glide" => glide,
            "level" => level,
        }
        choices {
            "scale" => scale,
        }
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.pitch, self.snapped, self.sung, self.lock, self.env];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let modulator = inputs.first().copied().unwrap_or(0.0);
        let carrier_in = inputs.get(1).copied().unwrap_or(0.0);
        let shift_cv = inputs.get(2).copied().unwrap_or(0.0);
        let root_cv = inputs.get(3).copied().unwrap_or(0.0);

        let q = self.q.clamp(0.6, 8.0);
        let follow = self.follow.clamp(0.004, 0.4);
        let shift = (self.shift + self.shift_depth * shift_cv).clamp(-2.0, 2.0);
        let mix = self.mix.clamp(0.0, 1.0);
        let tune = self.tune.clamp(0.0, 1.0);
        let root = (self.root * volts_to_ratio(root_cv)).clamp(20.0, 2000.0);
        let glide = self.glide.clamp(0.002, 0.8);
        let level = self.level.clamp(0.0, 4.0);

        // Pitch track: a DC block and a 700 Hz one-pole, then zero crossings.
        self.hp = (modulator - self.hp_x) + 0.995 * self.hp;
        self.hp_x = modulator;
        let lp_g = 1.0 - (-std::f32::consts::TAU * 700.0 * dt).exp();
        let prev_lp = self.lp;
        self.lp += lp_g * (self.hp - self.lp);
        self.timer += dt;
        let voiced = self.env > 0.08;
        if prev_lp <= 0.0 && self.lp > 0.0 {
            let period = self.timer;
            self.timer = 0.0;
            if voiced && period > 1.0 / 900.0 && period < 1.0 / 35.0 {
                let hz = 1.0 / period;
                let track = 0.35;
                self.pitch += track * (hz - self.pitch);
                self.snapped = scale::snap(self.pitch, root, self.scale);
                self.lock = (self.lock + 0.08).min(5.0);
            }
        }
        if !voiced {
            self.lock *= 1.0 - 12.0 * dt;
            self.timer = 0.0;
        } else {
            self.lock *= 1.0 - 0.4 * dt;
        }

        let target = if self.lock > 0.15 {
            self.snapped
        } else {
            self.sung
        }
        .max(20.0);
        let glide_c = 1.0 - (-dt / glide).exp();
        self.sung_log += glide_c * (target.ln() - self.sung_log);
        self.sung = self.sung_log.exp().clamp(20.0, 4000.0);

        let nyquist = 0.45 / dt.max(1e-9);
        let freq = self.sung.min(nyquist);
        let increment = (freq * dt).clamp(0.0, 0.5);
        let saw = 2.0 * self.phase - 1.0 - poly_blep(self.phase, increment);
        self.phase = (self.phase + increment).fract();
        let internal = 2.4 * saw;
        let carrier = (1.0 - tune) * carrier_in + tune * internal;

        let ratio = 2.0f32.powf(shift);
        let env_c = 1.0 - (-dt / follow).exp();
        let mut wet = 0.0f32;
        let mut env_sum = 0.0f32;
        for i in 0..8 {
            let hz = (CENTRES[i] * ratio).clamp(30.0, nyquist);
            let m = Self::bandpass(&mut self.mod_cells[i], modulator, hz, q, dt);
            self.envs[i] += env_c * (m.abs() - self.envs[i]);
            let e = self.envs[i].max(0.0);
            env_sum += e;
            let c = Self::bandpass(&mut self.car_cells[i], carrier, hz, q, dt);
            wet += c * e;
        }
        self.env = env_sum / 8.0;
        // The door is the mean envelope, squared. A vocoder that keeps
        // leaking its saw through silence is a test tone, not a mouth.
        let gate = (self.env / 0.06).clamp(0.0, 1.0);
        wet *= 0.55 * gate * gate;

        let mixed = (1.0 - mix) * modulator + mix * wet;
        let out = clamp_rail(level * mixed, DEFAULT_RAIL);
        let v_oct = clamp_rail((self.sung / root.max(1.0)).log2(), DEFAULT_RAIL);
        let env_v = clamp_rail(self.env * 4.0, DEFAULT_RAIL);
        let lock_v = clamp_rail(self.lock, DEFAULT_RAIL);
        ModuleOutputs::new(&[out, v_oct, env_v, lock_v])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 96_000.0;

    fn drive(v: &mut Vocode, hz: f32, amp: f32, seconds: f32, carrier: f32) {
        let mut phase = 0.0f32;
        for _ in 0..(seconds / DT) as usize {
            phase = (phase + hz * DT).fract();
            let x = amp * (phase * std::f32::consts::TAU).sin();
            v.step(&[x, carrier, 0.0, 0.0], DT);
        }
    }

    fn saw_drive(v: &mut Vocode, hz: f32, amp: f32, seconds: f32) {
        let mut phase = 0.0f32;
        for _ in 0..(seconds / DT) as usize {
            phase = (phase + hz * DT).fract();
            let carrier = amp * (2.0 * phase - 1.0);
            let x = amp * (phase * std::f32::consts::TAU).sin();
            v.step(&[x, carrier, 0.0, 0.0], DT);
        }
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let v = Vocode::new();
        for spec in v.params() {
            let got = v.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }

    #[test]
    fn mix_of_zero_is_the_modulator() {
        let mut v = Vocode::new();
        v.set_param("mix", 0.0).unwrap();
        v.set_param("tune", 0.0).unwrap();
        v.set_param("level", 1.0).unwrap();
        let mut phase = 0.0f32;
        let mut peak_in = 0.0f32;
        let mut peak_out = 0.0f32;
        for _ in 0..(0.05 / DT) as usize {
            phase = (phase + 220.0 * DT).fract();
            let x = 1.2 * (phase * std::f32::consts::TAU).sin();
            peak_in = peak_in.max(x.abs());
            peak_out = peak_out.max(v.step(&[x, 0.0, 0.0, 0.0], DT)[0].abs());
        }
        assert!(
            (peak_out - peak_in).abs() < 0.05,
            "dry mix changed the mouth: in {peak_in} out {peak_out}"
        );
    }

    #[test]
    fn a_tone_on_a_saw_makes_sound() {
        let mut v = Vocode::new();
        v.set_param("mix", 1.0).unwrap();
        v.set_param("tune", 0.0).unwrap();
        saw_drive(&mut v, 220.0, 1.5, 0.25);
        let mut peak = 0.0f32;
        let mut phase = 0.0f32;
        for _ in 0..(0.05 / DT) as usize {
            phase = (phase + 220.0 * DT).fract();
            let x = 1.5 * (phase * std::f32::consts::TAU).sin();
            let c = 1.5 * (2.0 * phase - 1.0);
            peak = peak.max(v.step(&[x, c, 0.0, 0.0], DT)[0].abs());
        }
        assert!(peak > 0.08, "vocoder stayed silent: {peak}");
        assert!(peak.is_finite() && peak < DEFAULT_RAIL);
    }

    #[test]
    fn chromatic_snaps_near_the_root() {
        let mut v = Vocode::new();
        v.set_param("scale", 1.0).unwrap();
        v.set_param("root", 220.0).unwrap();
        v.set_param("tune", 1.0).unwrap();
        v.set_param("glide", 0.01).unwrap();
        drive(&mut v, 215.0, 1.6, 0.35, 0.0);
        let snapped = v.snapped();
        assert!(
            (snapped - 220.0).abs() < 6.0,
            "215 Hz snapped to {snapped}, not A3"
        );
    }

    #[test]
    fn fifths_keeps_the_weather_key() {
        let mut v = Vocode::new();
        v.set_param("scale", 5.0).unwrap();
        v.set_param("root", 55.0).unwrap();
        v.set_param("tune", 1.0).unwrap();
        v.set_param("glide", 0.01).unwrap();
        drive(&mut v, 70.0, 1.6, 0.4, 0.0);
        let snapped = v.snapped();
        let e2 = 82.41;
        assert!(
            (snapped - e2).abs() < 4.0 || (snapped - 55.0).abs() < 4.0,
            "70 Hz left the A/E bed: {snapped}"
        );
    }

    #[test]
    fn silence_drops_the_lock() {
        let mut v = Vocode::new();
        drive(&mut v, 220.0, 1.6, 0.25, 0.0);
        assert!(v.lock > 0.4, "a tone never locked: {}", v.lock);
        for _ in 0..(0.4 / DT) as usize {
            v.step(&[0.0, 0.0, 0.0, 0.0], DT);
        }
        assert!(v.lock < 0.2, "lock hung after silence: {}", v.lock);
    }

    #[test]
    fn silence_closes_the_mouth() {
        let mut v = Vocode::new();
        v.set_param("mix", 1.0).unwrap();
        v.set_param("tune", 1.0).unwrap();
        drive(&mut v, 220.0, 1.6, 0.2, 0.0);
        for _ in 0..(0.25 / DT) as usize {
            v.step(&[0.0, 0.0, 0.0, 0.0], DT);
        }
        let mut peak = 0.0f32;
        for _ in 0..(0.05 / DT) as usize {
            peak = peak.max(v.step(&[0.0, 0.0, 0.0, 0.0], DT)[0].abs());
        }
        assert!(peak < 0.02, "the saw leaked through silence: {peak}");
    }

    #[test]
    fn v_oct_is_zero_at_the_root() {
        let mut v = Vocode::new();
        v.set_param("root", 220.0).unwrap();
        v.set_param("scale", 1.0).unwrap();
        v.set_param("tune", 1.0).unwrap();
        v.set_param("glide", 0.008).unwrap();
        drive(&mut v, 220.0, 1.6, 0.3, 0.0);
        let v_oct = v.step(&[1.0, 0.0, 0.0, 0.0], DT)[1];
        assert!(v_oct.abs() < 0.08, "root was {v_oct} V, not 0");
    }
}
