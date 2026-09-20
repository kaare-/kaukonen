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
}
