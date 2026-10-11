//! Phenomenological cochlea: log-spaced hair cells. A utility, not a neural module.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

/// Six place codes, roughly an octave and a bit apart.
/// Rumble / chest / formant / speech / hiss / air.
const CENTRES: [f32; 6] = [80.0, 200.0, 500.0, 1_200.0, 3_000.0, 7_000.0];

const IN_JACKS: &[&str] = &["in", "tilt_cv"];
const OUT_JACKS: &[&str] = &[
    "rumble", "chest", "formant", "speech", "hiss", "air", "place", "loud",
];
const STATE_FIELDS: &[&str] = &[
    "rumble", "chest", "formant", "speech", "hiss", "air", "place", "loud",
];

const PARAMS: &[ParamSpec] = params! {
    "q", Unit::Ratio, 0.6 ..= 6.0, 1.55, Log, "selectivity of each hair cell";
    "tilt", Unit::Ratio, -2.0 ..= 2.0, 0.0, Linear, "lows vs highs; + lifts the air";
    "tilt_depth", Unit::Ratio, -2.0 ..= 2.0, 0.2, Linear, "how hard TILT CV leans the ear";
    "inhibit", Unit::Ratio, 0.0 ..= 0.6, 0.14, Linear, "neighbor subtraction; a little is a sharper place code";
    "follow", Unit::Seconds, 0.008 ..= 0.8, 0.07, Log, "base follow at 500 Hz; lower bands are slower";
    "level", Unit::Volt, 0.0 ..= 8.0, 5.0, Linear, "envelope scale";
};

#[derive(Clone, Copy, Debug, Default)]
struct Cell {
    s1: f32,
    s2: f32,
    env: f32,
}

/// Six bandpasses with hair-cell followers, a centroid, and a loudness.
///
/// The bands never leave as audio. Each jack is a place-coded envelope so a
/// microphone can current-drive different nerves without sitting on the
/// speakers. Low hairs follow more slowly, the way a real basilar membrane
/// does, and a little lateral inhibition keeps a tone from lighting every
/// neighbor.
#[derive(Clone, Debug)]
pub struct Cochlea {
    pub q: f32,
    pub tilt: f32,
    pub tilt_depth: f32,
    pub inhibit: f32,
    pub follow: f32,
    pub level: f32,
    cells: [Cell; 6],
    envs: [f32; 6],
    place: f32,
    loud: f32,
}

impl Default for Cochlea {
    fn default() -> Self {
        Self::new()
    }
}

impl Cochlea {
    pub fn new() -> Self {
        Self {
            q: 1.55,
            tilt: 0.0,
            tilt_depth: 0.2,
            inhibit: 0.14,
            follow: 0.07,
            level: 5.0,
            cells: [Cell::default(); 6],
            envs: [0.0; 6],
            place: 0.0,
            loud: 0.0,
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
}

impl AnalogModule for Cochlea {
    fn kind_name(&self) -> &'static str {
        "cochlea"
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
            "tilt" => tilt,
            "tilt_depth" => tilt_depth,
            "inhibit" => inhibit,
            "follow" => follow,
            "level" => level,
        }
        choices {}
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let mut values = [0.0; 8];
        values[..6].copy_from_slice(&self.envs);
        values[6] = self.place;
        values[7] = self.loud;
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let x = inputs.first().copied().unwrap_or(0.0);
        let tilt_cv = inputs.get(1).copied().unwrap_or(0.0);
        let q = self.q.clamp(0.6, 6.0);
        let tilt = (self.tilt + self.tilt_depth * tilt_cv).clamp(-2.0, 2.0);
        let inhibit = self.inhibit.clamp(0.0, 0.6);
        let follow = self.follow.clamp(0.008, 0.8);
        let level = self.level.clamp(0.0, 8.0);

        let mut raw = [0.0f32; 6];
        for i in 0..6 {
            let lean = 1.0 + tilt * (i as f32 - 2.5) / 3.0;
            let gain = lean.clamp(0.15, 3.0);
            let y = Self::bandpass(&mut self.cells[i], x, CENTRES[i], q, dt) * gain;
            // Hair-cell follow: half-wave, then a one-pole whose tau scales
            // as 1/sqrt(f) so rumble is slow and air is quick.
            let tau = follow * (500.0 / CENTRES[i]).sqrt();
            let coeff = 1.0 - (-dt / tau.max(1e-4)).exp();
            self.cells[i].env += coeff * (y.abs() - self.cells[i].env);
            raw[i] = self.cells[i].env.max(0.0);
        }

        let mut sharp = [0.0f32; 6];
        for i in 0..6 {
            let mut v = raw[i];
            if i > 0 {
                v -= inhibit * raw[i - 1];
            }
            if i + 1 < 6 {
                v -= inhibit * raw[i + 1];
            }
            sharp[i] = (v * level).max(0.0);
            self.envs[i] = sharp[i];
        }

        let mut energy = 0.0f32;
        let mut moment = 0.0f32;
        for (i, &v) in sharp.iter().enumerate() {
            energy += v;
            moment += v * i as f32;
        }
        self.loud = energy / 6.0;
        // Band index 0..5 maps onto 0..5 V, a place code the climate can read.
        self.place = if energy > 1e-4 {
            moment / energy
        } else {
            self.place * 0.999
        };

        ModuleOutputs::new(&[
            clamp_rail(sharp[0], DEFAULT_RAIL),
            clamp_rail(sharp[1], DEFAULT_RAIL),
            clamp_rail(sharp[2], DEFAULT_RAIL),
            clamp_rail(sharp[3], DEFAULT_RAIL),
            clamp_rail(sharp[4], DEFAULT_RAIL),
            clamp_rail(sharp[5], DEFAULT_RAIL),
            clamp_rail(self.place, DEFAULT_RAIL),
            clamp_rail(self.loud, DEFAULT_RAIL),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 48_000.0;

    fn drive(ear: &mut Cochlea, hz: f32, amplitude: f32) -> [f32; 8] {
        let mut phase = 0.0f32;
        let settle = (0.35 / DT) as usize;
        let measure = (0.2 / DT) as usize;
        for _ in 0..settle {
            phase = (phase + hz * DT).fract();
            let x = amplitude * (phase * std::f32::consts::TAU).sin();
            ear.step(&[x, 0.0], DT);
        }
        let mut peak = [0.0f32; 8];
        for _ in 0..measure {
            phase = (phase + hz * DT).fract();
            let x = amplitude * (phase * std::f32::consts::TAU).sin();
            let y = ear.step(&[x, 0.0], DT);
            for i in 0..8 {
                peak[i] = peak[i].max(y[i].abs());
            }
        }
        peak
    }

    #[test]
    fn a_low_tone_prefers_rumble() {
        let mut ear = Cochlea::new();
        let y = drive(&mut ear, 90.0, 0.6);
        assert!(
            y[0] > y[5] + 0.4,
            "90 Hz should prefer rumble over air: {y:?}"
        );
        assert!(y[6] < 2.0, "bass place should sit low: {y:?}");
    }

    #[test]
    fn a_high_tone_prefers_air() {
        let mut ear = Cochlea::new();
        let y = drive(&mut ear, 6500.0, 0.6);
        assert!(
            y[5] > y[0] + 0.4,
            "6.5 kHz should prefer air over rumble: {y:?}"
        );
        assert!(y[6] > 2.5, "treble place should sit high: {y:?}");
    }

    #[test]
    fn formant_lights_the_middle_hair() {
        let mut ear = Cochlea::new();
        let y = drive(&mut ear, 500.0, 0.6);
        assert!(
            y[2] > y[0] && y[2] > y[5],
            "500 Hz should prefer formant: {y:?}"
        );
        assert!(y[7] > 0.15, "loudness stayed shut: {y:?}");
    }

    #[test]
    fn silence_decays() {
        let mut ear = Cochlea::new();
        drive(&mut ear, 400.0, 0.5);
        for _ in 0..(0.6 / DT) as usize {
            ear.step(&[0.0, 0.0], DT);
        }
        let now = ear.step(&[0.0, 0.0], DT);
        assert!(now[7] < 0.08, "loudness hung after silence: {}", now[7]);
    }
}
