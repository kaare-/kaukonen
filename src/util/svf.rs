//! State-variable filter. Lowpass, highpass and a wide bandpass from one cell.

use super::volts_to_ratio;
use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["in", "cutoff_cv", "q_cv"];
const OUT_JACKS: &[&str] = &["lp", "hp", "bp"];
const STATE_FIELDS: &[&str] = &["cutoff", "q", "s1", "s2"];

const PARAMS: &[ParamSpec] = params! {
    "cutoff", Unit::Hertz, 20.0 ..= 16000.0, 900.0, Log, "centre of the band with no CV applied";
    "cv_depth", Unit::Ratio, -4.0 ..= 4.0, 1.0, Linear, "octaves of cutoff per volt on the CUTOFF CV jack";
    "q", Unit::Ratio, 0.3 ..= 8.0, 0.65, Log, "selectivity; 0.5 is a large band, 4 is a peak";
    "q_depth", Unit::Ratio, -2.0 ..= 2.0, 0.0, Linear, "Q change per volt on the Q CV jack";
    "drive", Unit::Ratio, 0.1 ..= 10.0, 1.0, Log, "gain into the integrator pair";
    "level", Unit::Ratio, 0.0 ..= 4.0, 1.0, Linear, "output scale";
};

/// Trapezoidal state-variable filter.
///
/// One pair of integrators gives the three classical outputs at once. The
/// bandpass is the useful one at the end of a rack: a low Q is a large window
/// that still moves as a whole when cutoff CV walks.
#[derive(Clone, Debug)]
pub struct Svf {
    pub cutoff: f32,
    pub cv_depth: f32,
    pub q: f32,
    pub q_depth: f32,
    pub drive: f32,
    pub level: f32,
    s1: f32,
    s2: f32,
    corner: f32,
    quality: f32,
}

impl Default for Svf {
    fn default() -> Self {
        Self::new()
    }
}

impl Svf {
    pub fn new() -> Self {
        Self {
            cutoff: 900.0,
            cv_depth: 1.0,
            q: 0.65,
            q_depth: 0.0,
            drive: 1.0,
            level: 1.0,
            s1: 0.0,
            s2: 0.0,
            corner: 900.0,
            quality: 0.65,
        }
    }

    /// Centre frequency actually in force, after CV.
    pub fn corner(&self) -> f32 {
        self.corner
    }

    /// Q actually in force, after CV.
    pub fn quality(&self) -> f32 {
        self.quality
    }
}

impl AnalogModule for Svf {
    fn kind_name(&self) -> &'static str {
        "svf"
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
            "cutoff" => cutoff,
            "cv_depth" => cv_depth,
            "q" => q,
            "q_depth" => q_depth,
            "drive" => drive,
            "level" => level,
        }
        choices {}
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.corner, self.quality, self.s1, self.s2];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let x = inputs.first().copied().unwrap_or(0.0);
        let cutoff_cv = inputs.get(1).copied().unwrap_or(0.0);
        let q_cv = inputs.get(2).copied().unwrap_or(0.0);

        let ceiling = 0.45 / dt.max(1e-9);
        self.corner =
            (self.cutoff * volts_to_ratio(self.cv_depth * cutoff_cv)).clamp(20.0, ceiling);
        self.quality = (self.q + self.q_depth * q_cv).clamp(0.3, 8.0);

        // Linear trapezoidal SVF (Simper). Stable up to the same Nyquist
        // ceiling the ladder uses, and the three outputs stay complementary.
        let g = (std::f32::consts::PI * self.corner * dt).tan();
        let k = 1.0 / self.quality;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;

        let v0 = self.drive * x;
        let v3 = v0 - self.s2;
        let bp = a1 * self.s1 + a2 * v3;
        let lp = self.s2 + a2 * self.s1 + a3 * v3;
        self.s1 = 2.0 * bp - self.s1;
        self.s2 = 2.0 * lp - self.s2;
        let hp = v0 - k * bp - lp;

        let scale = self.level;
        ModuleOutputs::new(&[
            clamp_rail(scale * lp, DEFAULT_RAIL),
            clamp_rail(scale * hp, DEFAULT_RAIL),
            clamp_rail(scale * bp, DEFAULT_RAIL),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 96_000.0;

    fn response(svf: &mut Svf, hz: f32, amplitude: f32) -> f32 {
        let mut phase = 0.0f32;
        let settle = (0.25 / DT) as usize;
        let measure = (0.25 / DT) as usize;
        for _ in 0..settle {
            phase = (phase + hz * DT).fract();
            let x = amplitude * (phase * std::f32::consts::TAU).sin();
            svf.step(&[x, 0.0, 0.0], DT);
        }
        let mut peak = 0.0f32;
        for _ in 0..measure {
            phase = (phase + hz * DT).fract();
            let x = amplitude * (phase * std::f32::consts::TAU).sin();
            peak = peak.max(svf.step(&[x, 0.0, 0.0], DT)[2].abs());
        }
        peak / amplitude
    }

    fn filter(cutoff: f32, q: f32) -> Svf {
        let mut svf = Svf::new();
        svf.set_param("cutoff", cutoff).unwrap();
        svf.set_param("q", q).unwrap();
        svf
    }

    #[test]
    fn the_band_passes_the_centre_and_stops_the_edges() {
        let mid = response(&mut filter(1000.0, 0.65), 1000.0, 0.5);
        let low = response(&mut filter(1000.0, 0.65), 40.0, 0.5);
        let high = response(&mut filter(1000.0, 0.65), 12000.0, 0.5);
        assert!(mid > 0.35, "centre of the band was {mid}");
        assert!(low < 0.15 * mid, "40 Hz came through at {low} vs mid {mid}");
        assert!(
            high < 0.15 * mid,
            "12 kHz came through at {high} vs mid {mid}"
        );
    }

    #[test]
    fn a_large_band_is_wider_than_a_peak() {
        let wide_near = response(&mut filter(1000.0, 0.5), 400.0, 0.5);
        let wide_mid = response(&mut filter(1000.0, 0.5), 1000.0, 0.5);
        let thin_near = response(&mut filter(1000.0, 4.0), 400.0, 0.5);
        let thin_mid = response(&mut filter(1000.0, 4.0), 1000.0, 0.5);
        let wide_ratio = wide_near / wide_mid.max(1e-6);
        let thin_ratio = thin_near / thin_mid.max(1e-6);
        assert!(
            wide_ratio > 1.4 * thin_ratio,
            "Q did not change the width: wide {wide_ratio} thin {thin_ratio}"
        );
    }

    #[test]
    fn cutoff_cv_tracks_one_volt_per_octave() {
        let mut svf = filter(500.0, 0.65);
        svf.step(&[0.0, 1.0, 0.0], DT);
        assert!(
            (svf.corner() - 1000.0).abs() < 1.0,
            "corner {}",
            svf.corner()
        );
    }

    #[test]
    fn q_cv_narrows_the_band() {
        let mut svf = filter(800.0, 0.65);
        svf.set_param("q_depth", 0.4).unwrap();
        svf.step(&[0.0, 0.0, 5.0], DT);
        assert!((svf.quality() - 2.65).abs() < 0.05, "q {}", svf.quality());
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let svf = Svf::new();
        for spec in svf.params() {
            let got = svf.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
