//! Shared scale tables for the vocoder and the quantizer.

/// How a pitch is snapped. `off` leaves the hertz alone.
pub const SCALES: &[&str] = &[
    "off",
    "chromatic",
    "major",
    "minor",
    "penta",
    "fifths",
    "phrygian",
];

/// Semitone degrees from the root. Phrygian is the minor with a flat 2.
const DEGREES: [&[i32]; 7] = [
    &[],
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    &[0, 2, 4, 5, 7, 9, 11],
    &[0, 2, 3, 5, 7, 8, 10],
    &[0, 3, 5, 7, 10],
    &[0, 7],
    &[0, 1, 3, 5, 7, 8, 10],
];

/// Snap `hz` onto `scale` built from `root`. `off` returns `hz`.
pub fn snap(hz: f32, root: f32, scale: usize) -> f32 {
    if !(hz.is_finite() && hz > 1.0) || !(root.is_finite() && root > 1.0) {
        return root.max(1.0);
    }
    if scale == 0 || scale >= DEGREES.len() || DEGREES[scale].is_empty() {
        return hz;
    }
    let n = 12.0 * (hz / root).log2();
    let octave = n.div_euclid(12.0);
    let semi = n - octave * 12.0;
    let mut best = 0.0f32;
    let mut best_d = 100.0f32;
    for wrap in [-1, 0, 1] {
        for &degree in DEGREES[scale] {
            let cand = degree as f32 + 12.0 * wrap as f32;
            let d = (semi - cand).abs();
            if d < best_d {
                best_d = d;
                best = cand;
            }
        }
    }
    (root * ((octave * 12.0 + best) / 12.0).exp2()).clamp(20.0, 4000.0)
}

/// Semitone position of `hz` above `root`, 0..12.
pub fn semi(hz: f32, root: f32) -> f32 {
    if !(hz.is_finite() && hz > 1.0) || !(root.is_finite() && root > 1.0) {
        return 0.0;
    }
    (12.0 * (hz / root).log2()).rem_euclid(12.0)
}

/// Whether `hz` sits on a degree of `scale` built from `root`.
pub fn in_scale(hz: f32, root: f32, scale: usize, slop: f32) -> bool {
    if scale == 0 || scale >= DEGREES.len() || DEGREES[scale].is_empty() {
        return true;
    }
    let s = semi(hz, root);
    DEGREES[scale].iter().any(|&d| {
        let deg = d as f32;
        (s - deg).abs() < slop || (s - deg - 12.0).abs() < slop
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrygian_is_the_flat_two() {
        let a1 = 55.0;
        let bb = snap(60.0, a1, 6);
        let expected = a1 * (1.0f32 / 12.0).exp2();
        assert!(
            (bb - expected).abs() < 0.4,
            "60 Hz snapped to {bb}, not Bb {expected}"
        );
        assert!(in_scale(bb, a1, 6, 0.2));
        assert!(!in_scale(61.74, a1, 6, 0.2), "B is not Phrygian from A");
    }

    #[test]
    fn off_does_not_move() {
        assert!((snap(61.7, 55.0, 0) - 61.7).abs() < 1e-4);
    }
}
