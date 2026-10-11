//! Anti-aliased oscillator. A utility, not one of the neural modules.

use super::{poly_blep, volts_to_ratio};
use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{knob_accessors, params, ParamSpec, Unit};
use crate::types::{clamp_rail, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &["v_oct", "fm", "pwm", "sync"];
const OUT_JACKS: &[&str] = &["saw", "tri", "sine", "pulse"];
const STATE_FIELDS: &[&str] = &["phase", "freq"];

const PARAMS: &[ParamSpec] = params! {
    "tune", Unit::Hertz, 0.01 ..= 12000.0, 110.0, Log, "frequency with no CV applied";
    "fm_depth", Unit::Hertz, 0.0 ..= 5000.0, 0.0, Linear, "hertz per volt of linear FM on the FM jack";
    "width", Unit::Ratio, 0.01 ..= 0.99, 0.5, Linear, "pulse duty cycle";
    "pwm_depth", Unit::Ratio, 0.0 ..= 1.0, 0.0, Linear, "duty cycle change per volt on the PWM jack";
    "level", Unit::Volt, 0.0 ..= 10.0, 5.0, Linear, "output amplitude";
};

/// Four simultaneous waveforms from one phase, with 1 V/oct and linear FM.
///
/// The saw and pulse outputs are corrected with polyBLEP, so they stay usable
/// at audio rate rather than folding their harmonics back down the spectrum.
/// The triangle is left plain: its harmonics already fall away as 1/n², so it
/// aliases far less than a jump does.
#[derive(Clone, Debug)]
pub struct Vco {
    pub tune: f32,
    pub fm_depth: f32,
    pub width: f32,
    pub pwm_depth: f32,
    pub level: f32,
    phase: f32,
    freq: f32,
    last_sync: Volt,
}

impl Default for Vco {
    fn default() -> Self {
        Self::new()
    }
}

impl Vco {
    pub fn new() -> Self {
        Self {
            tune: 110.0,
            fm_depth: 0.0,
            width: 0.5,
            pwm_depth: 0.0,
            level: 5.0,
            phase: 0.0,
            freq: 110.0,
            last_sync: 0.0,
        }
    }

    pub fn frequency(&self) -> f32 {
        self.freq
    }
}

impl AnalogModule for Vco {
    fn kind_name(&self) -> &'static str {
        "vco"
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
            "tune" => tune,
            "fm_depth" => fm_depth,
            "width" => width,
            "pwm_depth" => pwm_depth,
            "level" => level,
        }
        choices {}
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.phase, self.freq];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let v_oct = inputs.first().copied().unwrap_or(0.0);
        let fm = inputs.get(1).copied().unwrap_or(0.0);
        let pwm = inputs.get(2).copied().unwrap_or(0.0);
        let sync = inputs.get(3).copied().unwrap_or(0.0);

        // Pitch is exponential; the FM jack is linear on top of it, which is
        // what makes it useful for anything other than vibrato.
        let nyquist = 0.5 / dt.max(1e-9);
        self.freq = (self.tune * volts_to_ratio(v_oct) + self.fm_depth * fm).clamp(0.0, nyquist);

        // Reset on a rising edge, so one oscillator can be slaved to another.
        if self.last_sync <= 0.0 && sync > 0.0 {
            self.phase = 0.0;
        }
        self.last_sync = sync;

        let increment = (self.freq * dt).clamp(0.0, 0.5);
        let width = (self.width + self.pwm_depth * pwm).clamp(0.01, 0.99);
        let phase = self.phase;

        let saw = 2.0 * phase - 1.0 - poly_blep(phase, increment);
        let tri = 4.0 * (phase - 0.5).abs() - 1.0;
        let sine = (phase * std::f32::consts::TAU).sin();
        let square = if phase < width { 1.0 } else { -1.0 };
        let pulse = square + poly_blep(phase, increment)
            - poly_blep((phase + 1.0 - width).fract(), increment);

        self.phase = (self.phase + increment).fract();

        let scale = |v: f32| clamp_rail(self.level * v, DEFAULT_RAIL);
        ModuleOutputs::new(&[scale(saw), scale(tri), scale(sine), scale(pulse)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect one output jack for a while.
    fn record(vco: &mut Vco, jack: usize, dt: f32, seconds: f32) -> Vec<f32> {
        (0..(seconds / dt) as usize)
            .map(|_| vco.step(&[0.0; 4], dt)[jack])
            .collect()
    }

    /// Frequency measured from upward zero crossings.
    fn measured_hz(trace: &[f32], dt: f32) -> f32 {
        let times: Vec<f32> = trace
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
            .map(|(i, _)| i as f32 * dt)
            .collect();
        if times.len() < 2 {
            return 0.0;
        }
        (times.len() - 1) as f32 / (times[times.len() - 1] - times[0])
    }

    #[test]
    fn every_waveform_runs_at_the_tuned_frequency() {
        let dt = 1.0 / 48_000.0;
        for jack in 0..4 {
            let mut vco = Vco::new();
            vco.set_param("tune", 220.0).unwrap();
            let trace = record(&mut vco, jack, dt, 0.5);
            let hz = measured_hz(&trace, dt);
            assert!(
                (hz - 220.0).abs() < 2.0,
                "jack {jack} ran at {hz} Hz instead of 220"
            );
        }
    }

    #[test]
    fn one_volt_of_pitch_cv_doubles_the_frequency() {
        let dt = 1.0 / 48_000.0;
        let mut vco = Vco::new();
        vco.set_param("tune", 110.0).unwrap();
        let trace: Vec<f32> = (0..24_000)
            .map(|_| vco.step(&[1.0, 0.0, 0.0, 0.0], dt)[2])
            .collect();
        let hz = measured_hz(&trace, dt);
        assert!((hz - 220.0).abs() < 2.0, "1 V gave {hz} Hz");
    }

    #[test]
    fn linear_fm_adds_hertz_per_volt() {
        let dt = 1.0 / 48_000.0;
        let mut vco = Vco::new();
        vco.set_param("tune", 200.0).unwrap();
        vco.set_param("fm_depth", 100.0).unwrap();
        let trace: Vec<f32> = (0..24_000)
            .map(|_| vco.step(&[0.0, 2.0, 0.0, 0.0], dt)[2])
            .collect();
        let hz = measured_hz(&trace, dt);
        assert!(
            (hz - 400.0).abs() < 3.0,
            "200 Hz + 2 V x 100 Hz/V gave {hz}"
        );
    }

    #[test]
    fn polyblep_keeps_a_high_saw_from_folding_down() {
        // A 6 kHz saw at 48 kHz has only a few harmonics below Nyquist, so
        // everything else folds back. Compare the energy below the fundamental
        // against a naive saw, where folded partials land.
        let dt = 1.0 / 48_000.0;
        let freq = 6000.0;
        let mut vco = Vco::new();
        vco.set_param("tune", freq).unwrap();
        vco.set_param("level", 1.0).unwrap();
        let corrected = record(&mut vco, 0, dt, 0.25);

        let mut phase = 0.0f32;
        let naive: Vec<f32> = (0..corrected.len())
            .map(|_| {
                phase = (phase + freq * dt).fract();
                2.0 * phase - 1.0
            })
            .collect();

        let low_energy = |trace: &[f32]| {
            // Any content below the fundamental can only be folded aliasing.
            (1..40)
                .map(|bin| {
                    let hz = bin as f32 * 100.0;
                    goertzel(trace, hz, 1.0 / dt)
                })
                .sum::<f32>()
        };
        let clean = low_energy(&corrected);
        let dirty = low_energy(&naive);
        assert!(
            clean < 0.5 * dirty,
            "polyBLEP did not reduce folded energy: {clean} vs {dirty}"
        );
    }

    /// Magnitude of one frequency, via the Goertzel algorithm.
    fn goertzel(trace: &[f32], hz: f32, sample_rate: f32) -> f32 {
        let w = std::f32::consts::TAU * hz / sample_rate;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0, 0.0);
        for x in trace {
            let s0 = x + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt() / trace.len() as f32
    }

    #[test]
    fn pulse_width_sets_the_duty_cycle() {
        let dt = 1.0 / 48_000.0;
        for width in [0.25, 0.5, 0.75] {
            let mut vco = Vco::new();
            vco.set_param("tune", 100.0).unwrap();
            vco.set_param("width", width).unwrap();
            let trace = record(&mut vco, 3, dt, 0.5);
            let high = trace.iter().filter(|v| **v > 0.0).count() as f32 / trace.len() as f32;
            assert!((high - width).abs() < 0.02, "width {width} measured {high}");
        }
    }

    #[test]
    fn sync_restarts_the_cycle() {
        let dt = 1.0 / 48_000.0;
        let mut vco = Vco::new();
        vco.set_param("tune", 100.0).unwrap();
        for _ in 0..100 {
            vco.step(&[0.0; 4], dt);
        }
        assert!(vco.phase > 0.1);
        vco.step(&[0.0, 0.0, 0.0, 5.0], dt);
        // Reset to zero, then advanced by one increment.
        assert!(vco.phase < 2.0 * 100.0 * dt, "phase {}", vco.phase);
    }

    #[test]
    fn pitch_is_capped_below_nyquist() {
        let dt = 1.0 / 48_000.0;
        let mut vco = Vco::new();
        vco.set_param("tune", 12000.0).unwrap();
        // 10 V of pitch CV asks for 12 MHz.
        vco.step(&[10.0, 0.0, 0.0, 0.0], dt);
        assert!(vco.frequency() <= 24_000.0, "freq {}", vco.frequency());
        assert!(vco.frequency().is_finite());
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let vco = Vco::new();
        for spec in vco.params() {
            let got = vco.get_param(spec.name).expect(spec.name);
            assert!((got - spec.default).abs() < 1e-9, "{}", spec.name);
        }
    }
}
