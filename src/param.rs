//! Knob descriptions: names, units, ranges and taper.
//!
//! A module's params are its front-panel knobs. Describing them as data rather
//! than as bare struct fields is what lets a patch file, the CLI and a future
//! GUI all reach the same controls, and what lets mismatch and drift be applied
//! to a named slot instead of to an arbitrary float.

/// Physical dimension of a knob, used for display and for sensible stepping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Volts, e.g. a bias or offset.
    Volt,
    /// Hertz.
    Hertz,
    /// Seconds.
    Seconds,
    /// A rate constant in 1/seconds (a leak, a `1/τ`).
    Rate,
    /// Dimensionless gain, weight or fraction.
    Ratio,
    /// A discrete choice. The stored value is an index into the list.
    Enum(&'static [&'static str]),
}

impl Unit {
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Volt => " V",
            Self::Hertz => " Hz",
            Self::Seconds => " s",
            Self::Rate => " /s",
            Self::Ratio | Self::Enum(_) => "",
        }
    }

    pub fn choices(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Enum(names) => Some(names),
            _ => None,
        }
    }
}

/// How knob travel maps onto value. Frequencies and time constants want `Log`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taper {
    Linear,
    Log,
}

/// One knob on one module.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub name: &'static str,
    pub unit: Unit,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub taper: Taper,
    pub doc: &'static str,
}

impl ParamSpec {
    pub fn clamp(&self, value: f32) -> f32 {
        value.clamp(self.min, self.max)
    }

    pub fn contains(&self, value: f32) -> bool {
        (self.min..=self.max).contains(&value)
    }

    pub fn choices(&self) -> Option<&'static [&'static str]> {
        self.unit.choices()
    }

    /// Knob travel in `0..=1` to a value, honouring the taper.
    pub fn from_norm(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self.taper {
            Taper::Linear => self.min + (self.max - self.min) * t,
            Taper::Log => {
                // A log taper needs strictly positive endpoints; fall back to
                // linear for ranges that cross or touch zero.
                if self.min > 0.0 && self.max > 0.0 {
                    (self.min.ln() + (self.max.ln() - self.min.ln()) * t).exp()
                } else {
                    self.min + (self.max - self.min) * t
                }
            }
        }
    }

    /// Inverse of [`ParamSpec::from_norm`].
    pub fn to_norm(&self, value: f32) -> f32 {
        let value = self.clamp(value);
        match self.taper {
            Taper::Linear => {
                if self.max == self.min {
                    0.0
                } else {
                    (value - self.min) / (self.max - self.min)
                }
            }
            Taper::Log => {
                if self.min > 0.0 && self.max > 0.0 && self.max != self.min {
                    (value.ln() - self.min.ln()) / (self.max.ln() - self.min.ln())
                } else if self.max == self.min {
                    0.0
                } else {
                    (value - self.min) / (self.max - self.min)
                }
            }
        }
    }

    /// Parse a patch-file token: a number, or an enum choice by name.
    pub fn parse(&self, text: &str) -> Result<f32, ParamError> {
        let text = text.trim();
        if let Some(choices) = self.choices() {
            if let Some(i) = choices.iter().position(|c| c.eq_ignore_ascii_case(text)) {
                return Ok(i as f32);
            }
            // Numeric indices stay legal so serialized patches round-trip.
            if let Ok(v) = text.parse::<f32>() {
                if v >= 0.0 && (v as usize) < choices.len() {
                    return Ok(v.round());
                }
            }
            return Err(ParamError::BadChoice {
                name: self.name,
                got: text.to_string(),
                choices,
            });
        }
        text.parse::<f32>().map_err(|_| ParamError::BadNumber {
            name: self.name,
            got: text.to_string(),
        })
    }

    pub fn format(&self, value: f32) -> String {
        match self.choices() {
            Some(choices) => {
                let i = choice(value).min(choices.len().saturating_sub(1));
                choices[i].to_string()
            }
            None => format!("{}{}", trim_float(value), self.unit.suffix()),
        }
    }
}

/// Render a float without a trailing `.0` so serialized patches stay readable.
pub fn trim_float(v: f32) -> String {
    if v == v.trunc() && v.abs() < 1e7 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        let s = s.trim_end_matches('0');
        s.trim_end_matches('.').to_string()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamError {
    /// No such knob on this module kind.
    Unknown {
        kind: &'static str,
        name: String,
        known: Vec<&'static str>,
    },
    /// Value outside the knob's end stops.
    OutOfRange {
        name: &'static str,
        value: f32,
        min: f32,
        max: f32,
    },
    BadNumber {
        name: &'static str,
        got: String,
    },
    BadChoice {
        name: &'static str,
        got: String,
        choices: &'static [&'static str],
    },
}

impl std::fmt::Display for ParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { kind, name, known } => {
                write!(f, "module kind '{kind}' has no knob '{name}'")?;
                if !known.is_empty() {
                    write!(f, " (knobs: {})", known.join(", "))?;
                }
                Ok(())
            }
            Self::OutOfRange {
                name,
                value,
                min,
                max,
            } => write!(
                f,
                "knob '{name}' = {} is outside {}..{}",
                trim_float(*value),
                trim_float(*min),
                trim_float(*max)
            ),
            Self::BadNumber { name, got } => {
                write!(f, "knob '{name}': '{got}' is not a number")
            }
            Self::BadChoice { name, got, choices } => write!(
                f,
                "knob '{name}': '{got}' is not one of {}",
                choices.join(", ")
            ),
        }
    }
}

impl std::error::Error for ParamError {}

/// Look up a spec by name in a module's knob list.
pub fn find_spec(specs: &'static [ParamSpec], name: &str) -> Option<&'static ParamSpec> {
    specs.iter().find(|s| s.name == name)
}

/// Turn an enum knob's stored float into a choice index.
///
/// Enum values travel as floats like every other knob, so a position halfway
/// between two choices has to land on the nearer one rather than truncate.
pub fn choice(value: f32) -> usize {
    if value.is_finite() && value > 0.0 {
        value.round() as usize
    } else {
        0
    }
}

/// Validate a value against a knob's end stops. NaN is rejected as out of range.
pub fn checked(spec: &ParamSpec, value: f32) -> Result<f32, ParamError> {
    if spec.contains(value) {
        Ok(value)
    } else {
        Err(ParamError::OutOfRange {
            name: spec.name,
            value,
            min: spec.min,
            max: spec.max,
        })
    }
}

/// Build the `&'static [ParamSpec]` table for a module.
///
/// ```text
/// params! {
///     "leak", Rate, 0.0 ..= 200.0, 8.0, Log, "membrane leak (1/tau)";
/// }
/// ```
macro_rules! params {
    ( $( $name:literal, $unit:expr, $min:literal ..= $max:literal, $default:literal, $taper:ident, $doc:literal );* $(;)? ) => {
        &[ $(
            $crate::param::ParamSpec {
                name: $name,
                unit: $unit,
                min: $min,
                max: $max,
                default: $default,
                taper: $crate::param::Taper::$taper,
                doc: $doc,
            }
        ),* ]
    };
}

pub(crate) use params;

#[cfg(test)]
mod tests {
    use super::*;

    const SPECS: &[ParamSpec] = params! {
        "leak", Unit::Rate, 0.0 ..= 200.0, 8.0, Linear, "leak";
        "freq", Unit::Hertz, 1.0 ..= 1000.0, 100.0, Log, "frequency";
        "shape", Unit::Enum(&["tanh", "soft_clip"]), 0.0 ..= 1.0, 0.0, Linear, "curve";
    };

    #[test]
    fn linear_norm_round_trips() {
        let s = &SPECS[0];
        for v in [0.0, 8.0, 100.0, 200.0] {
            assert!((s.from_norm(s.to_norm(v)) - v).abs() < 1e-3, "v={v}");
        }
    }

    #[test]
    fn log_taper_puts_geometric_mean_at_centre() {
        let s = &SPECS[1];
        let mid = s.from_norm(0.5);
        assert!((mid - 31.62).abs() < 0.1, "mid={mid}");
        assert!((s.from_norm(s.to_norm(250.0)) - 250.0).abs() < 1e-2);
    }

    #[test]
    fn log_taper_falls_back_when_range_touches_zero() {
        let s = &SPECS[0];
        assert!((s.from_norm(0.5) - 100.0).abs() < 1e-3);
    }

    #[test]
    fn enums_parse_by_name_and_index() {
        let s = &SPECS[2];
        assert_eq!(s.parse("soft_clip"), Ok(1.0));
        assert_eq!(s.parse("TANH"), Ok(0.0));
        assert_eq!(s.parse("1"), Ok(1.0));
        assert!(s.parse("wobble").is_err());
        assert_eq!(s.format(1.0), "soft_clip");
    }

    #[test]
    fn numbers_reject_garbage() {
        assert!(SPECS[0].parse("12.5").is_ok());
        assert!(SPECS[0].parse("twelve").is_err());
    }

    #[test]
    fn floats_serialize_without_noise() {
        assert_eq!(trim_float(8.0), "8");
        assert_eq!(trim_float(0.015), "0.015");
        assert_eq!(trim_float(-1.6), "-1.6");
    }
}
