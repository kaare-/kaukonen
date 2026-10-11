//! Fixed filter bank in the ADDAC 601 tradition. A utility, not a neural module.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

/// ADDAC 601 centres: 50 / 110 / 220 / 500 / 1.1k / 2.2k / 5.2k / 11k.
const CENTRES: [f32; 8] = [
    50.0, 110.0, 220.0, 500.0, 1_100.0, 2_200.0, 5_200.0, 11_000.0,
];

const IN_JACKS: &[&str] = &["in", "tilt_cv", "res_cv"];
const OUT_JACKS: &[&str] = &["out", "odd", "even", "env"];
const STATE_FIELDS: &[&str] = &[
    "sub", "bass", "low", "lowmid", "mid", "highmid", "high", "air", "env",
];

const PARAMS: &[ParamSpec] = params! {
    "sub", Unit::Ratio, 0.0 ..= 2.0, 1.0, Linear, "50 Hz band";
    "bass", Unit::Ratio, 0.0 ..= 2.0, 1.0, Linear, "110 Hz band";
    "low", Unit::Ratio, 0.0 ..= 2.0, 1.0, Linear, "220 Hz band";
    "lowmid", Unit::Ratio, 0.0 ..= 2.0, 1.0, Linear, "500 Hz band";
    "mid", Unit::Ratio, 0.0 ..= 2.0, 1.0, Linear, "1.1 kHz band";
    "highmid", Unit::Ratio, 0.0 ..= 2.0, 1.0, Linear, "2.2 kHz band";
    "high", Unit::Ratio, 0.0 ..= 2.0, 1.0, Linear, "5.2 kHz band";
    "air", Unit::Ratio, 0.0 ..= 2.0, 1.0, Linear, "11 kHz band";
    "q", Unit::Ratio, 0.5 ..= 8.0, 2.1, Log, "selectivity shared by every band";
    "tilt", Unit::Ratio, -2.0 ..= 2.0, 0.0, Linear, "lows vs highs; + lifts the air";
    "tilt_depth", Unit::Ratio, -2.0 ..= 2.0, 0.25, Linear, "how hard TILT CV leans the bank";
    "q_depth", Unit::Ratio, -2.0 ..= 2.0, 0.0, Linear, "Q change per volt on RES CV";
    "mix", Unit::Ratio, 0.0 ..= 1.0, 1.0, Linear, "dry/wet; 1 is the bank alone";
    "level", Unit::Ratio, 0.0 ..= 4.0, 1.0, Linear, "output scale";
};

#[derive(Clone, Copy, Debug, Default)]
struct Cell {
    s1: f32,
    s2: f32,
}

/// Eight fixed bandpasses, each with a VCA, plus odd/even and a follow.
///
/// The centres are the ADDAC 601's. Levels are knobs, not eight CV jacks —
/// one TILT CV leans the whole bank so it still fits a small panel.
#[derive(Clone, Debug)]
pub struct Fbank {
    pub sub: f32,
    pub bass: f32,
    pub low: f32,
    pub lowmid: f32,
    pub mid: f32,
    pub highmid: f32,
    pub high: f32,
    pub air: f32,
    pub q: f32,
    pub tilt: f32,
    pub tilt_depth: f32,
    pub q_depth: f32,
    pub mix: f32,
    pub level: f32,
    cells: [Cell; 8],
    bands: [f32; 8],
    env: f32,
}

impl Default for Fbank {
    fn default() -> Self {
        Self::new()
    }
}

impl Fbank {
    pub fn new() -> Self {
        Self {
            sub: 1.0,
            bass: 1.0,
            low: 1.0,
            lowmid: 1.0,
            mid: 1.0,
            highmid: 1.0,
            high: 1.0,
            air: 1.0,
            q: 2.1,
            tilt: 0.0,
            tilt_depth: 0.25,
            q_depth: 0.0,
            mix: 1.0,
            level: 1.0,
            cells: [Cell::default(); 8],
            bands: [0.0; 8],
            env: 0.0,
        }
    }

    fn levels(&self) -> [f32; 8] {
        [
            self.sub,
            self.bass,
            self.low,
            self.lowmid,
            self.mid,
            self.highmid,
            self.high,
            self.air,
        ]
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

impl AnalogModule for Fbank {
    fn kind_name(&self) -> &'static str {
        "fbank"
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
            "sub" => sub,
            "bass" => bass,
            "low" => low,
            "lowmid" => lowmid,
            "mid" => mid,
            "highmid" => highmid,
            "high" => high,
            "air" => air,
            "q" => q,
            "tilt" => tilt,
            "tilt_depth" => tilt_depth,
            "q_depth" => q_depth,
            "mix" => mix,
            "level" => level,
        }
        choices {}
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let mut values = [0.0; 9];
        values[..8].copy_from_slice(&self.bands);
        values[8] = self.env;
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let x = inputs.first().copied().unwrap_or(0.0);
        let tilt_cv = inputs.get(1).copied().unwrap_or(0.0);
        let res_cv = inputs.get(2).copied().unwrap_or(0.0);
        let q = (self.q + self.q_depth * res_cv).clamp(0.5, 8.0);
        let tilt = (self.tilt + self.tilt_depth * tilt_cv).clamp(-2.0, 2.0);
        let levels = self.levels();

        let mut wet = 0.0;
        let mut odd = 0.0;
        let mut even = 0.0;
        let mut peak = 0.0f32;
        for i in 0..8 {
            let lean = 1.0 + tilt * (i as f32 - 3.5) / 4.0;
            let gain = (levels[i] * lean.clamp(0.0, 3.0)).clamp(0.0, 4.0);
            let y = Self::bandpass(&mut self.cells[i], x, CENTRES[i], q, dt) * gain;
            self.bands[i] = y;
            wet += y;
            // ADDAC numbers the 50 Hz band as 1, so even indices are odd bands.
            if i % 2 == 0 {
                odd += y;
            } else {
                even += y;
            }
            peak = peak.max(y.abs());
        }
        // Eight overlapping bands sum hot; this keeps a flat bank near unity.
        let scale = 0.38 * self.level;
        wet *= scale;
        odd *= scale;
        even *= scale;
        let mix = self.mix.clamp(0.0, 1.0);
        let out = (1.0 - mix) * x + mix * wet;
        self.env += 80.0 * dt * (peak - self.env);

        ModuleOutputs::new(&[
            clamp_rail(out, DEFAULT_RAIL),
            clamp_rail(odd, DEFAULT_RAIL),
            clamp_rail(even, DEFAULT_RAIL),
            clamp_rail(self.env, DEFAULT_RAIL),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 96_000.0;

    fn drive(bank: &mut Fbank, hz: f32, amplitude: f32) -> [f32; 4] {
        let mut phase = 0.0f32;
        let settle = (0.2 / DT) as usize;
        let measure = (0.15 / DT) as usize;
        for _ in 0..settle {
            phase = (phase + hz * DT).fract();
            let x = amplitude * (phase * std::f32::consts::TAU).sin();
            bank.step(&[x, 0.0, 0.0], DT);
        }
        let mut peak = [0.0f32; 4];
        for _ in 0..measure {
            phase = (phase + hz * DT).fract();
            let x = amplitude * (phase * std::f32::consts::TAU).sin();
            let y = bank.step(&[x, 0.0, 0.0], DT);
            for i in 0..4 {
                peak[i] = peak[i].max(y[i].abs());
            }
        }
        peak
    }

    #[test]
    fn a_tone_prefers_its_own_band() {
        let mut bank = Fbank::new();
        bank.mix = 1.0;
        let low = drive(&mut bank, 110.0, 0.6);
        let mut bank = Fbank::new();
        bank.mix = 1.0;
        let high = drive(&mut bank, 5200.0, 0.6);
        assert!(
            low[2] > 1.4 * low[1],
            "110 Hz should prefer even: odd {} even {}",
            low[1],
            low[2]
        );
        assert!(
            high[1] > 1.4 * high[2],
            "5.2 kHz should prefer odd: odd {} even {}",
            high[1],
            high[2]
        );
    }

    #[test]
    fn pulling_air_down_kills_the_top() {
        let mut bright = Fbank::new();
        bright.mix = 1.0;
        let open = drive(&mut bright, 11000.0, 0.5)[0];
        let mut dark = Fbank::new();
        dark.mix = 1.0;
        dark.air = 0.0;
        dark.high = 0.0;
        let shut = drive(&mut dark, 11000.0, 0.5)[0];
        assert!(open > 2.0 * shut, "air still open: {open} vs {shut}");
    }

    #[test]
    fn mix_of_zero_is_dry() {
        let mut bank = Fbank::new();
        bank.mix = 0.0;
        bank.level = 0.0;
        let mut phase = 0.0f32;
        let mut peak = 0.0f32;
        for _ in 0..2000 {
            phase = (phase + 440.0 * DT).fract();
            let x = 0.7 * (phase * std::f32::consts::TAU).sin();
            peak = peak.max((bank.step(&[x, 0.0, 0.0], DT)[0] - x).abs());
        }
        assert!(peak < 0.02, "dry leaked {peak}");
    }

    #[test]
    fn tilt_lifts_the_high_bands() {
        let mut flat = Fbank::new();
        let mut tilted = Fbank::new();
        tilted.tilt = 1.2;
        let a = drive(&mut flat, 5200.0, 0.5)[0];
        let b = drive(&mut tilted, 5200.0, 0.5)[0];
        assert!(b > 1.25 * a, "tilt did nothing: flat {a} tilted {b}");
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let bank = Fbank::new();
        for spec in bank.params() {
            let got = bank.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
