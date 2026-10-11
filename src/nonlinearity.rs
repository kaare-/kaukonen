//! Swappable smooth saturations for analog outputs.

use crate::types::Volt;

/// Output nonlinearity applied after a module's linear/leaky state.
#[derive(Clone, Copy, Debug)]
pub enum Nonlinearity {
    /// Symmetric `tanh(v * response)`.
    Tanh { response: f32 },
    /// Cubic soft clip, then hard clamp at ±1 before gain.
    SoftClip { response: f32 },
    /// Separate positive / negative slopes into `tanh`.
    AsymTanh { pos: f32, neg: f32 },
}

impl Default for Nonlinearity {
    fn default() -> Self {
        Self::Tanh { response: 1.0 }
    }
}

impl Nonlinearity {
    /// Names used by the `shape` knob, in index order.
    pub const SHAPES: &'static [&'static str] = &["tanh", "soft_clip", "asym_tanh"];

    pub fn apply(self, v: Volt) -> Volt {
        match self {
            Self::Tanh { response } => (v * response).tanh(),
            Self::SoftClip { response } => {
                let x = (v * response).clamp(-1.5, 1.5);
                let y = x - (x * x * x) / 3.0;
                y.clamp(-1.0, 1.0)
            }
            Self::AsymTanh { pos, neg } => {
                if v >= 0.0 {
                    (v * pos).tanh()
                } else {
                    (v * neg).tanh()
                }
            }
        }
    }

    /// Index into [`Nonlinearity::SHAPES`].
    pub fn shape(self) -> usize {
        match self {
            Self::Tanh { .. } => 0,
            Self::SoftClip { .. } => 1,
            Self::AsymTanh { .. } => 2,
        }
    }

    /// Slope for positive inputs.
    pub fn response(self) -> f32 {
        match self {
            Self::Tanh { response } | Self::SoftClip { response } => response,
            Self::AsymTanh { pos, .. } => pos,
        }
    }

    /// Negative-slope ratio. 1.0 is symmetric; this is the knob that physical
    /// mismatch pushes away from 1.0 so no two modules clip identically.
    pub fn asymmetry(self) -> f32 {
        match self {
            Self::Tanh { .. } | Self::SoftClip { .. } => 1.0,
            Self::AsymTanh { pos, neg } => {
                if pos.abs() < 1e-9 {
                    1.0
                } else {
                    neg / pos
                }
            }
        }
    }

    /// Rebuild from flat knob values. Out-of-list shapes fall back to `tanh`.
    pub fn from_parts(shape: usize, response: f32, asymmetry: f32) -> Self {
        match shape {
            1 => Self::SoftClip { response },
            2 => Self::AsymTanh {
                pos: response,
                neg: response * asymmetry,
            },
            _ => Self::Tanh { response },
        }
    }

    /// Same shape and response, different negative slope.
    pub fn with_asymmetry(self, asymmetry: f32) -> Self {
        if (asymmetry - 1.0).abs() < 1e-9 {
            self
        } else {
            Self::AsymTanh {
                pos: self.response(),
                neg: self.response() * asymmetry,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tanh_is_odd_and_bounded() {
        let n = Nonlinearity::Tanh { response: 2.0 };
        assert!((n.apply(0.0)).abs() < 1e-6);
        assert!((n.apply(1.0) + n.apply(-1.0)).abs() < 1e-6);
        assert!(n.apply(50.0) < 1.0001);
    }

    #[test]
    fn asymmetric_tanh_tilts() {
        let n = Nonlinearity::AsymTanh { pos: 4.0, neg: 0.5 };
        assert!(n.apply(1.0) > n.apply(-1.0).abs());
    }

    #[test]
    fn flat_parts_round_trip() {
        for shape in 0..Nonlinearity::SHAPES.len() {
            let n = Nonlinearity::from_parts(shape, 0.7, 0.4);
            assert_eq!(n.shape(), shape);
            assert!((n.response() - 0.7).abs() < 1e-6);
            let expected_asym = if shape == 2 { 0.4 } else { 1.0 };
            assert!((n.asymmetry() - expected_asym).abs() < 1e-6);
        }
    }

    #[test]
    fn asymmetry_promotes_a_symmetric_curve() {
        let n = Nonlinearity::Tanh { response: 2.0 }.with_asymmetry(0.5);
        assert_eq!(n.shape(), 2);
        assert!((n.asymmetry() - 0.5).abs() < 1e-6);
        assert!((n.response() - 2.0).abs() < 1e-6);
    }
}
