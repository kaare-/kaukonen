//! Local plasticity rules. No global loss, no backpropagation.
//!
//! Every rule sees only what is physically available at one route: the voltage
//! going in, the voltage coming out, its own stored weight, the global LEARN
//! and FORGET controls, and a few slow local averages the module keeps for it.
//! Nothing here can see another route's weight or any notion of error.

use crate::types::Volt;

/// Everything one route can see when deciding how to change.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// Voltage on this route's source channel.
    pub input: Volt,
    /// Voltage on this route's destination channel.
    pub output: Volt,
    /// This route's stored weight.
    pub weight: Volt,
    /// Global LEARN input. Negative is treated as zero.
    pub learn: Volt,
    /// Global FORGET input. Negative is treated as zero.
    pub forget: Volt,
    /// Sliding average of the destination channel's squared output, the
    /// sliding threshold a BCM-like rule compares against.
    pub theta: f32,
    /// Sum of |w| over every route feeding this destination, for scaling.
    pub column_norm: f32,
    /// The module's own ACTIVITY output from the previous step, so plasticity
    /// can gate plasticity.
    pub activity: Volt,
    /// High-passed input, positive while the input is rising.
    pub d_input: f32,
    /// High-passed output, positive while the output is rising.
    pub d_output: f32,
}

/// A local rule for one analog memory.
///
/// The built-in rules live in [`Rule`]; implement this trait to reason about a
/// rule on its own, and add a variant to wire a new one into TC-3.
pub trait PlasticityRule {
    fn dw(&self, ctx: &Context) -> f32;
}

/// The built-in local rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Coincident activity strengthens a route. Grows without limit unless
    /// something else holds it back.
    Hebbian,
    /// Hebbian with Oja's own normalizing term, so a route settles instead of
    /// running to its end stop.
    Oja,
    /// Sliding-threshold rule: a route weakens when its destination is below
    /// its own recent activity and strengthens when above.
    Bcm,
    /// Coincident activity weakens a route, which decorrelates channels.
    AntiHebbian,
    /// Hebbian accelerated by the module's own ACTIVITY output, so a
    /// connectome that is already changing changes faster. Gating strictly
    /// *on* activity could never start, since a still matrix reports none.
    ThreeFactor,
    /// Continuous stand-in for spike timing: potentiates when the output rises
    /// after the input, depresses when the input rises after the output. No
    /// spike times, so it stays continuous.
    Stdp,
}

impl Rule {
    /// Rule names addressable from a patch file, in index order.
    pub const NAMES: &'static [&'static str] = &[
        "hebbian",
        "oja",
        "bcm",
        "anti_hebbian",
        "three_factor",
        "stdp",
    ];

    pub fn index(self) -> usize {
        match self {
            Self::Hebbian => 0,
            Self::Oja => 1,
            Self::Bcm => 2,
            Self::AntiHebbian => 3,
            Self::ThreeFactor => 4,
            Self::Stdp => 5,
        }
    }

    /// Unknown indices fall back to Hebbian.
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Oja,
            2 => Self::Bcm,
            3 => Self::AntiHebbian,
            4 => Self::ThreeFactor,
            5 => Self::Stdp,
            _ => Self::Hebbian,
        }
    }

    pub fn name(self) -> &'static str {
        Self::NAMES[self.index()]
    }
}

/// The selected rule plus the knobs every rule draws from.
///
/// One flat set of knobs rather than per-rule structs, so switching `rule` in a
/// patch file never invalidates the rest of the panel.
#[derive(Clone, Copy, Debug)]
pub struct Plasticity {
    pub rule: Rule,
    /// Ignore input×output products below this magnitude, so residual noise
    /// does not integrate forever.
    pub deadzone: f32,
    /// Strength of Oja's normalizing term.
    pub gamma: f32,
    /// How fast the BCM sliding threshold follows the output.
    pub theta_rate: f32,
    /// Cutoff of the high-pass that feeds the STDP-like rule.
    pub diff_rate: f32,
    /// Rate of slow synaptic scaling toward `target_norm`. Zero is off.
    pub scaling: f32,
    /// Σ|w| each destination channel is scaled toward.
    pub target_norm: f32,
}

impl Default for Plasticity {
    fn default() -> Self {
        Self {
            rule: Rule::Hebbian,
            deadzone: 1e-4,
            gamma: 1.0,
            theta_rate: 1.0,
            diff_rate: 20.0,
            scaling: 0.0,
            target_norm: 1.0,
        }
    }
}

impl PlasticityRule for Plasticity {
    fn dw(&self, ctx: &Context) -> f32 {
        let learn = ctx.learn.max(0.0);
        let forget = ctx.forget.max(0.0);
        let product = ctx.input * ctx.output;
        let awake = product.abs() >= self.deadzone;
        let hebb = if awake { product } else { 0.0 };

        let core = match self.rule {
            Rule::Hebbian => learn * hebb,
            Rule::Oja => learn * (hebb - self.gamma * ctx.output * ctx.output * ctx.weight),
            Rule::Bcm => {
                if awake {
                    learn * ctx.input * (ctx.output * ctx.output - ctx.theta)
                } else {
                    0.0
                }
            }
            Rule::AntiHebbian => -learn * hebb,
            Rule::ThreeFactor => learn * hebb * (1.0 + ctx.activity),
            Rule::Stdp => learn * (ctx.input * ctx.d_output - ctx.output * ctx.d_input),
        };

        // Homeostasis: a slow pull on the whole destination channel toward a
        // target total conductance. This is what stops Hebbian growth from
        // simply pinning every route at its end stop.
        let homeostasis = if self.scaling > 0.0 {
            -self.scaling * (ctx.column_norm - self.target_norm) * ctx.weight
        } else {
            0.0
        };

        core + homeostasis - forget * ctx.weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            input: 1.0,
            output: 1.0,
            weight: 0.5,
            learn: 1.0,
            forget: 0.0,
            theta: 0.0,
            column_norm: 0.5,
            activity: 0.0,
            d_input: 0.0,
            d_output: 0.0,
        }
    }

    fn rule(rule: Rule) -> Plasticity {
        Plasticity {
            rule,
            ..Plasticity::default()
        }
    }

    #[test]
    fn rule_names_round_trip() {
        for (i, name) in Rule::NAMES.iter().enumerate() {
            let rule = Rule::from_index(i);
            assert_eq!(rule.index(), i);
            assert_eq!(rule.name(), *name);
        }
        assert_eq!(Rule::from_index(99), Rule::Hebbian);
    }

    #[test]
    fn hebbian_and_anti_hebbian_are_opposites() {
        let c = ctx();
        assert!(rule(Rule::Hebbian).dw(&c) > 0.0);
        assert_eq!(rule(Rule::Hebbian).dw(&c), -rule(Rule::AntiHebbian).dw(&c));
    }

    #[test]
    fn deadzone_ignores_tiny_products() {
        let c = Context {
            input: 1e-4,
            output: 1e-4,
            ..ctx()
        };
        assert_eq!(rule(Rule::Hebbian).dw(&c), 0.0);
    }

    #[test]
    fn oja_settles_where_hebbian_would_run_away() {
        // Oja's fixed point is w = x*y / (gamma*y^2) = 1 here.
        let at_one = Context {
            weight: 1.0,
            ..ctx()
        };
        let past_one = Context {
            weight: 1.5,
            ..ctx()
        };
        assert!(rule(Rule::Oja).dw(&at_one).abs() < 1e-6);
        assert!(rule(Rule::Oja).dw(&past_one) < 0.0);
        assert!(rule(Rule::Hebbian).dw(&past_one) > 0.0);
    }

    #[test]
    fn bcm_flips_sign_around_its_sliding_threshold() {
        let below = Context {
            theta: 4.0,
            ..ctx()
        };
        let above = Context {
            theta: 0.25,
            ..ctx()
        };
        assert!(rule(Rule::Bcm).dw(&below) < 0.0);
        assert!(rule(Rule::Bcm).dw(&above) > 0.0);
    }

    #[test]
    fn three_factor_speeds_up_with_its_own_activity() {
        let quiet = ctx();
        let busy = Context {
            activity: 2.0,
            ..ctx()
        };
        // A still matrix learns at the plain Hebbian rate, so it can start.
        assert_eq!(
            rule(Rule::ThreeFactor).dw(&quiet),
            rule(Rule::Hebbian).dw(&quiet)
        );
        assert_eq!(
            rule(Rule::ThreeFactor).dw(&busy),
            3.0 * rule(Rule::Hebbian).dw(&busy)
        );
    }

    #[test]
    fn stdp_is_antisymmetric_in_who_moved_first() {
        let post_after_pre = Context {
            d_output: 1.0,
            ..ctx()
        };
        let pre_after_post = Context {
            d_input: 1.0,
            ..ctx()
        };
        assert!(rule(Rule::Stdp).dw(&post_after_pre) > 0.0);
        assert!(rule(Rule::Stdp).dw(&pre_after_post) < 0.0);
    }

    #[test]
    fn scaling_pulls_an_overgrown_channel_back() {
        let mut p = rule(Rule::Hebbian);
        p.scaling = 2.0;
        p.target_norm = 1.0;
        let overgrown = Context {
            column_norm: 4.0,
            learn: 0.0,
            ..ctx()
        };
        let starved = Context {
            column_norm: 0.2,
            learn: 0.0,
            ..ctx()
        };
        assert!(p.dw(&overgrown) < 0.0);
        assert!(p.dw(&starved) > 0.0);
        // Off by default, so it changes nothing unless asked for.
        assert_eq!(rule(Rule::Hebbian).dw(&overgrown), 0.0);
    }

    #[test]
    fn forget_shrinks_every_rule() {
        for i in 0..Rule::NAMES.len() {
            let c = Context {
                learn: 0.0,
                forget: 1.0,
                weight: 0.8,
                ..ctx()
            };
            assert!(
                rule(Rule::from_index(i)).dw(&c) < 0.0,
                "{} ignored FORGET",
                Rule::from_index(i).name()
            );
        }
    }
}
