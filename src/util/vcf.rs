//! Four-pole ladder lowpass. A utility, not one of the neural modules.

use super::volts_to_ratio;
use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["in", "cutoff_cv", "res_cv"];
const OUT_JACKS: &[&str] = &["out", "pole2"];
const STATE_FIELDS: &[&str] = &["cutoff", "z0", "z1", "z2", "z3"];

/// Voltage at which the feedback stage starts to saturate.
const HEADROOM: f32 = 5.0;

const PARAMS: &[ParamSpec] = params! {
    "cutoff", Unit::Hertz, 5.0 ..= 18000.0, 800.0, Log, "corner frequency with no CV applied";
    "cv_depth", Unit::Ratio, -4.0 ..= 4.0, 1.0, Linear, "octaves of cutoff per volt on the CUTOFF CV jack";
    "resonance", Unit::Ratio, 0.0 ..= 1.3, 0.3, Linear, "feedback around the ladder; above about 1 it sings on its own";
    "res_depth", Unit::Ratio, -1.0 ..= 1.0, 0.0, Linear, "resonance change per volt on the RES CV jack";
    "drive", Unit::Ratio, 0.1 ..= 10.0, 1.0, Log, "gain into the saturating feedback stage";
    "level", Unit::Ratio, 0.0 ..= 4.0, 1.0, Linear, "output scale";
};

/// A cascade of four one-poles with saturating feedback around the loop.
///
/// The saturation is in the feedback path rather than every stage, which is
/// cheap and still gives the ladder its characteristic behaviour: the
/// resonance limits itself instead of exploding, and the filter can be pushed
/// into singing on its own with nothing patched in.
#[derive(Clone, Debug)]
pub struct Vcf {
    pub cutoff: f32,
    pub cv_depth: f32,
    pub resonance: f32,
    pub res_depth: f32,
    pub drive: f32,
    pub level: f32,
    stages: [f32; 4],
    corner: f32,
}

impl Default for Vcf {
    fn default() -> Self {
        Self::new()
    }
}

impl Vcf {
    pub fn new() -> Self {
        Self {
            cutoff: 800.0,
            cv_depth: 1.0,
            resonance: 0.3,
            res_depth: 0.0,
            drive: 1.0,
            level: 1.0,
            stages: [0.0; 4],
            corner: 800.0,
        }
    }

    /// Corner frequency actually in force, after CV.
    pub fn corner(&self) -> f32 {
        self.corner
    }
}

impl AnalogModule for Vcf {
    fn kind_name(&self) -> &'static str {
        "vcf"
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
            "resonance" => resonance,
            "res_depth" => res_depth,
            "drive" => drive,
            "level" => level,
        }
        choices {}
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [
            self.corner,
            self.stages[0],
            self.stages[1],
            self.stages[2],
            self.stages[3],
        ];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let x = inputs.first().copied().unwrap_or(0.0);
        let cutoff_cv = inputs.get(1).copied().unwrap_or(0.0);
        let res_cv = inputs.get(2).copied().unwrap_or(0.0);

        // Cutoff tracks 1 V/oct like pitch does, capped below Nyquist so the
        // one-pole coefficient stays in range.
        let ceiling = 0.45 / dt.max(1e-9);
        self.corner = (self.cutoff * volts_to_ratio(self.cv_depth * cutoff_cv)).clamp(1.0, ceiling);
        let resonance = (self.resonance + self.res_depth * res_cv).clamp(0.0, 1.5);

        let g = 1.0 - (-std::f32::consts::TAU * self.corner * dt).exp();
        // Half of the input is fed back in with the resonance, the usual
        // compensation that stops the passband collapsing as resonance rises.
        let feedback = 4.0 * resonance * (self.stages[3] - 0.5 * x);
        // Saturate around the rack's own voltage scale. Scaling to 1.0 instead
        // would compress an ordinary 5 V signal by a third.
        let mut v = HEADROOM * (self.drive * (x - feedback) / HEADROOM).tanh();
        for stage in &mut self.stages {
            *stage += g * (v - *stage);
            v = *stage;
        }

        let out = clamp_rail(self.level * self.stages[3], DEFAULT_RAIL);
        let pole2 = clamp_rail(self.level * self.stages[1], DEFAULT_RAIL);
        ModuleOutputs::new(&[out, pole2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 96_000.0;

    /// Steady-state amplitude of the filter's response to a sine at `hz`.
    fn response(vcf: &mut Vcf, hz: f32, amplitude: f32) -> f32 {
        let mut phase = 0.0f32;
        // Settle first, then measure.
        let settle = (0.2 / DT) as usize;
        let measure = (0.2 / DT) as usize;
        for _ in 0..settle {
            phase = (phase + hz * DT).fract();
            let x = amplitude * (phase * std::f32::consts::TAU).sin();
            vcf.step(&[x, 0.0, 0.0], DT);
        }
        let mut peak = 0.0f32;
        for _ in 0..measure {
            phase = (phase + hz * DT).fract();
            let x = amplitude * (phase * std::f32::consts::TAU).sin();
            peak = peak.max(vcf.step(&[x, 0.0, 0.0], DT)[0].abs());
        }
        peak / amplitude
    }

    fn filter(cutoff: f32, resonance: f32) -> Vcf {
        let mut vcf = Vcf::new();
        vcf.set_param("cutoff", cutoff).unwrap();
        vcf.set_param("resonance", resonance).unwrap();
        vcf
    }

    #[test]
    fn it_passes_low_and_stops_high() {
        let low = response(&mut filter(1000.0, 0.0), 50.0, 1.0);
        let high = response(&mut filter(1000.0, 0.0), 12000.0, 1.0);
        // Unity in the passband, allowing for the mild feedback-stage curve.
        assert!((low - 1.0).abs() < 0.05, "passband gain {low}");
        assert!(high < 0.02, "12 kHz came through at {high}");
    }

    #[test]
    fn the_corner_lands_where_four_poles_put_it() {
        // Each pole is 3 dB down at the knob, so four in series reach -3 dB
        // together well below it: (1 + r^2)^-2 = 1/sqrt(2) gives r = 0.435.
        let cutoff = 1000.0;
        let expected = 0.435 * cutoff;
        let reference = response(&mut filter(cutoff, 0.0), 20.0, 0.5);
        let target = reference * std::f32::consts::FRAC_1_SQRT_2;
        let mut found = 0.0;
        let mut hz = 100.0;
        while hz < 4000.0 {
            if response(&mut filter(cutoff, 0.0), hz, 0.5) < target {
                found = hz;
                break;
            }
            hz *= 1.02;
        }
        assert!(found > 0.0, "never reached -3 dB");
        assert!(
            (found - expected).abs() < 0.15 * expected,
            "-3 dB at {found} Hz, expected near {expected} Hz"
        );
    }

    #[test]
    fn resonance_lifts_the_corner() {
        let flat = response(&mut filter(1000.0, 0.0), 1000.0, 0.5);
        let peaky = response(&mut filter(1000.0, 0.9), 1000.0, 0.5);
        assert!(
            peaky > 1.5 * flat,
            "resonance did nothing: {flat} -> {peaky}"
        );
    }

    #[test]
    fn it_sings_on_its_own_when_pushed() {
        let mut vcf = filter(500.0, 1.3);
        // A single click is all it takes to start a self-oscillating filter.
        vcf.step(&[5.0, 0.0, 0.0], DT);
        let trace: Vec<f32> = (0..(0.5 / DT) as usize)
            .map(|_| vcf.step(&[0.0, 0.0, 0.0], DT)[0])
            .collect();
        let tail = &trace[trace.len() / 2..];
        let peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.05, "filter fell silent: peak {peak}");
        assert!(
            peak.is_finite() && peak < DEFAULT_RAIL,
            "it blew up: {peak}"
        );

        let crossings = tail
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        let hz = crossings as f32 / (tail.len() as f32 * DT);
        assert!(
            (hz - 500.0).abs() < 150.0,
            "sang at {hz} Hz instead of near the 500 Hz cutoff"
        );
    }

    #[test]
    fn hard_drive_saturates_instead_of_exploding() {
        let mut vcf = filter(2000.0, 1.2);
        vcf.set_param("drive", 8.0).unwrap();
        let mut phase = 0.0f32;
        let mut peak = 0.0f32;
        for _ in 0..(0.3 / DT) as usize {
            phase = (phase + 300.0 * DT).fract();
            let x = 9.0 * (phase * std::f32::consts::TAU).sin();
            peak = peak.max(vcf.step(&[x, 0.0, 0.0], DT)[0].abs());
        }
        assert!(peak.is_finite(), "filter diverged");
        assert!(peak <= DEFAULT_RAIL);
    }

    #[test]
    fn cutoff_cv_tracks_one_volt_per_octave() {
        let mut vcf = filter(500.0, 0.0);
        vcf.step(&[0.0, 2.0, 0.0], DT);
        assert!(
            (vcf.corner() - 2000.0).abs() < 1.0,
            "corner {}",
            vcf.corner()
        );
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let vcf = Vcf::new();
        for spec in vcf.params() {
            let got = vcf.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
