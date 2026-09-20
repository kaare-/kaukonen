//! Cheap, stable integrators for leaky analog states.

/// Exact solution of `dV/dt = drive - leak * V` over one step.
///
/// When `leak` is near zero this falls back to forward Euler so a disabled
/// leak still charges.
pub fn leaky_integrate(v: f32, drive: f32, leak: f32, dt: f32) -> f32 {
    if leak.abs() < 1e-8 {
        return v + drive * dt;
    }
    let v_inf = drive / leak;
    v_inf + (v - v_inf) * (-leak * dt).exp()
}

/// One-pole lowpass with its corner at `hz`, as an exact step.
///
/// The leak *is* the filter: `dy/dt = k(x - y)` with `k = 2*pi*hz`.
pub fn lowpass(y: f32, x: f32, hz: f32, dt: f32) -> f32 {
    let k = std::f32::consts::TAU * hz.max(0.0);
    leaky_integrate(y, k * x, k, dt)
}

/// Forward Euler for leftover nonlinear terms: `x + dt * dx`.
pub fn euler(x: f32, dx: f32, dt: f32) -> f32 {
    x + dt * dx
}

/// Two-stage RK2 (Heun) for a scalar derivative `f(x)`.
pub fn rk2(x: f32, dt: f32, f: impl Fn(f32) -> f32) -> f32 {
    let k1 = f(x);
    let k2 = f(x + dt * k1);
    x + 0.5 * dt * (k1 + k2)
}

/// Softplus, used as a smooth rectifier around a threshold.
pub fn softplus(x: f32) -> f32 {
    if x > 20.0 {
        x
    } else if x < -20.0 {
        0.0
    } else {
        x.exp().ln_1p()
    }
}

/// Logistic sigmoid.
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leak_relaxes_toward_equilibrium() {
        let v = leaky_integrate(1.0, 0.0, 10.0, 1.0);
        assert!(v.abs() < 0.01);
    }

    #[test]
    fn zero_leak_is_pure_integration() {
        let v = leaky_integrate(0.0, 2.0, 0.0, 0.5);
        assert!((v - 1.0).abs() < 1e-6);
    }
}
