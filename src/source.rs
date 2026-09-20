//! Test-only stimulus sources. Not a Eurorack hardware module.

use crate::module::{AnalogModule, ModuleOutputs};
use crate::param::{self, params, ParamError, ParamSpec, Unit};
use crate::types::{clamp_rail, InstanceVariation, Volt, DEFAULT_RAIL};

const IN_JACKS: &[&str] = &[];
const OUT_JACKS: &[&str] = &["out"];
const STATE_FIELDS: &[&str] = &["phase", "out"];

/// Waveform names for the `shape` knob, in index order.
pub const SHAPES: &[&str] = &["dc", "sine", "pulse"];

const PARAMS: &[ParamSpec] = params! {
    "shape", Unit::Enum(SHAPES), 0.0 ..= 2.0, 0.0, Linear, "stimulus waveform";
    "volts", Unit::Volt, -10.0 ..= 10.0, 0.0, Linear, "level of the dc shape";
    "freq", Unit::Hertz, 0.001 ..= 20000.0, 1.0, Log, "cycles per second for sine and pulse";
    "amp", Unit::Volt, 0.0 ..= 10.0, 1.0, Linear, "peak amplitude above the offset";
    "offset", Unit::Volt, -10.0 ..= 10.0, 0.0, Linear, "dc offset added to sine and pulse";
    "width", Unit::Ratio, 0.0 ..= 1.0, 0.5, Linear, "pulse duty cycle as a fraction of the period";
};

/// Convenience constructor-facing description of a stimulus.
#[derive(Clone, Copy, Debug)]
pub enum SourceKind {
    Dc {
        volts: Volt,
    },
    Sine {
        freq_hz: f32,
        amp: Volt,
        offset: Volt,
    },
    /// `period` and `width` are in seconds; they are stored as frequency and
    /// duty cycle so the knobs read the same way for sine and pulse.
    Pulse {
        period: f32,
        width: f32,
        amp: Volt,
        offset: Volt,
    },
}

/// Flat knob values behind whichever shape is selected.
#[derive(Clone, Copy, Debug)]
pub struct SourceParams {
    pub shape: usize,
    pub volts: Volt,
    pub freq: f32,
    pub amp: Volt,
    pub offset: Volt,
    /// Duty cycle in `0..=1`.
    pub width: f32,
}

impl Default for SourceParams {
    fn default() -> Self {
        Self {
            shape: 0,
            volts: 0.0,
            freq: 1.0,
            amp: 1.0,
            offset: 0.0,
            width: 0.5,
        }
    }
}

impl SourceParams {
    pub fn from_kind(kind: SourceKind) -> Self {
        match kind {
            SourceKind::Dc { volts } => Self {
                shape: 0,
                volts,
                ..Self::default()
            },
            SourceKind::Sine {
                freq_hz,
                amp,
                offset,
            } => Self {
                shape: 1,
                freq: freq_hz,
                amp,
                offset,
                ..Self::default()
            },
            SourceKind::Pulse {
                period,
                width,
                amp,
                offset,
            } => {
                let freq = if period > 0.0 { 1.0 / period } else { 0.0 };
                let duty = if period > 0.0 {
                    (width / period).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                Self {
                    shape: 2,
                    freq,
                    amp,
                    offset,
                    width: duty,
                    ..Self::default()
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SourceState {
    /// Phase in cycles, wrapped to `0..1`.
    pub phase: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Source {
    pub params: SourceParams,
    pub state: SourceState,
}

impl Source {
    pub fn dc(volts: Volt) -> Self {
        Self::instantiate(SourceKind::Dc { volts }, InstanceVariation::IDENTITY)
    }

    pub fn sine(freq_hz: f32, amp: Volt, offset: Volt) -> Self {
        Self::instantiate(
            SourceKind::Sine {
                freq_hz,
                amp,
                offset,
            },
            InstanceVariation::IDENTITY,
        )
    }

    /// `period` and `width` are in seconds.
    pub fn pulse(period: f32, width: f32, amp: Volt, offset: Volt) -> Self {
        Self::instantiate(
            SourceKind::Pulse {
                period,
                width,
                amp,
                offset,
            },
            InstanceVariation::IDENTITY,
        )
    }

    pub fn instantiate(kind: SourceKind, _variation: InstanceVariation) -> Self {
        Self {
            params: SourceParams::from_kind(kind),
            state: SourceState::default(),
        }
    }

    fn sample(&self) -> Volt {
        let p = &self.params;
        match p.shape {
            1 => p.offset + p.amp * (self.state.phase * std::f32::consts::TAU).sin(),
            2 => {
                if self.state.phase < p.width {
                    p.offset + p.amp
                } else {
                    p.offset
                }
            }
            _ => p.volts,
        }
    }
}

impl AnalogModule for Source {
    fn kind_name(&self) -> &'static str {
        "source"
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
            "shape" => p.shape as f32,
            "volts" => p.volts,
            "freq" => p.freq,
            "amp" => p.amp,
            "offset" => p.offset,
            "width" => p.width,
            _ => return None,
        })
    }

    fn set_param(&mut self, name: &str, value: f32) -> Result<(), ParamError> {
        let spec = param::find_spec(PARAMS, name).ok_or_else(|| self.unknown_param(name))?;
        let v = param::checked(spec, value)?;
        let p = &mut self.params;
        match name {
            "shape" => p.shape = param::choice(v),
            "volts" => p.volts = v,
            "freq" => p.freq = v,
            "amp" => p.amp = v,
            "offset" => p.offset = v,
            "width" => p.width = v,
            _ => return Err(self.unknown_param(name)),
        }
        Ok(())
    }

    fn state_fields(&self) -> &'static [&'static str] {
        STATE_FIELDS
    }

    fn read_state(&self, out: &mut [Volt]) {
        let values = [self.state.phase, self.sample()];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
    }

    fn step(&mut self, _inputs: &[Volt], dt: f32) -> ModuleOutputs {
        let out = clamp_rail(self.sample(), DEFAULT_RAIL);
        if self.params.shape != 0 {
            self.state.phase = (self.state.phase + self.params.freq * dt).fract();
        }
        ModuleOutputs::one(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mut src: Source, dt: f32, steps: usize) -> Vec<Volt> {
        (0..steps).map(|_| src.step(&[], dt)[0]).collect()
    }

    #[test]
    fn dc_holds_its_level() {
        let trace = run(Source::dc(2.5), 0.0005, 100);
        assert!(trace.iter().all(|v| (*v - 2.5).abs() < 1e-6));
    }

    #[test]
    fn sine_runs_at_the_knob_frequency() {
        let dt = 0.0005;
        let trace = run(Source::sine(4.0, 1.0, 0.0), dt, 4000);
        let times: Vec<f32> = trace
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
            .map(|(i, _)| i as f32 * dt)
            .collect();
        assert!(times.len() >= 3, "too few cycles: {times:?}");
        let measured = (times.len() - 1) as f32 / (times[times.len() - 1] - times[0]);
        assert!((measured - 4.0).abs() < 0.05, "measured {measured} Hz");
    }

    #[test]
    fn pulse_keeps_its_duty_in_seconds() {
        let dt = 0.0005;
        let trace = run(Source::pulse(0.7, 0.08, 2.2, 0.15), dt, 2800);
        let high = trace.iter().filter(|v| **v > 1.0).count();
        let high_seconds = high as f32 * dt;
        // Two 0.08 s pulses inside 1.4 s of simulated time.
        assert!(
            (high_seconds - 0.16).abs() < 0.01,
            "high for {high_seconds} s"
        );
    }

    #[test]
    fn param_defaults_match_the_default_module() {
        let src = Source::default();
        for spec in src.params() {
            let got = src.get_param(spec.name).expect(spec.name);
            assert!(
                (got - spec.default).abs() < 1e-9,
                "{} default {} but module has {got}",
                spec.name,
                spec.default
            );
        }
    }
}
