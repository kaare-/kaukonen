//! The `.kpatch` patch language: instances, knobs, cables and recording.
//!
//! A patch file is the interface to the simulator. It is line oriented and
//! hand-parsed so the crate stays dependency free, and it round-trips: parsing
//! then serializing a patch reproduces it.

mod parse;
mod write;

pub use parse::parse;

use crate::module::{AnalogModule, Module};
use crate::network::Network;
use crate::param;
use crate::rc1::{Rc1, Rc1Params};
use crate::types::{InstanceVariation, SimConfig, Volt, DEFAULT_RAIL, DEFAULT_SAMPLE_RATE};

/// Which instances of a module a reference points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector {
    /// `name` — legal only for a single-instance module.
    Single,
    /// `name[3]` — one instance.
    Index(usize),
    /// `name[*]` — every instance.
    All,
    /// `name[i]` — pair instances index for index with the other side.
    Matched,
}

impl Selector {
    pub fn suffix(self) -> String {
        match self {
            Self::Single => String::new(),
            Self::Index(k) => format!("[{k}]"),
            Self::All => "[*]".to_string(),
            Self::Matched => "[i]".to_string(),
        }
    }
}

/// `name[sel].jack`
#[derive(Clone, Debug, PartialEq)]
pub struct JackRef {
    pub module: String,
    pub selector: Selector,
    pub jack: String,
}

impl std::fmt::Display for JackRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}.{}", self.module, self.selector.suffix(), self.jack)
    }
}

/// One `module` line: a kind, an instance count, and the knobs shared by all
/// of those instances.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleDecl {
    pub name: String,
    pub kind: String,
    pub count: usize,
    pub preset: Option<String>,
    pub knobs: Vec<(String, String)>,
    pub line: usize,
}

/// One `knob` line, overriding a single instance after the `module` line.
#[derive(Clone, Debug, PartialEq)]
pub struct KnobDecl {
    pub module: String,
    pub selector: Selector,
    pub param: String,
    pub value: String,
    pub line: usize,
}

/// One `patch` line, which may expand into many cables.
#[derive(Clone, Debug, PartialEq)]
pub struct CableDecl {
    pub from: JackRef,
    pub to: JackRef,
    pub line: usize,
}

/// One entry on a `record` line.
#[derive(Clone, Debug, PartialEq)]
pub enum RecordDecl {
    /// `record *` — every internal state of every instance.
    Everything,
    Channel {
        module: String,
        selector: Selector,
        field: String,
        line: usize,
    },
}

/// Simulation settings a patch can set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatchConfig {
    /// Frames per second of simulated time.
    pub sample_rate: f32,
    /// Integration steps per frame.
    pub oversample: u32,
    pub rail: Volt,
    /// Seed for per-instance component mismatch.
    pub seed: u64,
    /// Fractional spread between nominally identical modules. Zero is ideal.
    pub mismatch: f32,
    /// Default run length for `render`.
    pub seconds: f32,
}

impl Default for PatchConfig {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            oversample: 1,
            rail: DEFAULT_RAIL,
            seed: 0,
            mismatch: 0.0,
            seconds: 5.0,
        }
    }
}

impl PatchConfig {
    /// The integration step handed to modules.
    pub fn dt(&self) -> f32 {
        self.sim().dt()
    }

    pub fn sim(&self) -> SimConfig {
        SimConfig {
            sample_rate: self.sample_rate,
            oversample: self.oversample,
            rail: self.rail,
        }
    }
}

/// A parsed patch file, before instantiation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patch {
    pub config: PatchConfig,
    pub modules: Vec<ModuleDecl>,
    pub knobs: Vec<KnobDecl>,
    pub cables: Vec<CableDecl>,
    pub records: Vec<RecordDecl>,
}

/// A patch turned into a runnable network.
#[derive(Clone, Debug)]
pub struct BuiltPatch {
    pub network: Network,
    pub config: SimConfig,
    pub seconds: f32,
    /// Resolved recorder channel names, in `label.field` form.
    pub channels: Vec<String>,
}

/// A problem with a patch file, located by line.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchError {
    pub line: usize,
    pub message: String,
}

impl PatchError {
    pub fn at(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line == 0 {
            f.write_str(&self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for PatchError {}

/// Where one module's instances live in the network.
struct Bank {
    first: usize,
    count: usize,
}

impl Patch {
    /// Instance label as it appears in channel names: `cells[3]`, or just
    /// `brain` when the module has a single instance.
    pub fn label(name: &str, count: usize, instance: usize) -> String {
        if count == 1 {
            name.to_string()
        } else {
            format!("{name}[{instance}]")
        }
    }

    /// Instantiate the network, apply knobs, and resolve cables and channels.
    pub fn build(&self) -> Result<BuiltPatch, PatchError> {
        let mut network = Network::new();
        let mut banks: Vec<(String, Bank)> = Vec::new();

        for (module_index, decl) in self.modules.iter().enumerate() {
            if banks.iter().any(|(name, _)| *name == decl.name) {
                return Err(PatchError::at(
                    decl.line,
                    format!("module '{}' is declared twice", decl.name),
                ));
            }
            let first = network.len();
            for instance in 0..decl.count {
                let module = self.instantiate(decl, module_index, instance)?;
                let label = Self::label(&decl.name, decl.count, instance);
                network.add(label, module);
            }
            banks.push((
                decl.name.clone(),
                Bank {
                    first,
                    count: decl.count,
                },
            ));
        }

        let find_bank = |name: &str, line: usize| -> Result<&Bank, PatchError> {
            banks
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, bank)| bank)
                .ok_or_else(|| PatchError::at(line, format!("no module named '{name}'")))
        };

        for knob in &self.knobs {
            let bank = find_bank(&knob.module, knob.line)?;
            let instances = resolve(&knob.module, bank.count, knob.selector, knob.line)?;
            if knob.selector == Selector::Matched {
                return Err(PatchError::at(
                    knob.line,
                    "[i] only makes sense on a patch line",
                ));
            }
            for instance in instances {
                let id = crate::network::ModuleId(bank.first + instance);
                let module = network.get_mut(id);
                set_knob(module, &knob.param, &knob.value, knob.line)?;
            }
        }

        for cable in &self.cables {
            let from_bank = find_bank(&cable.from.module, cable.line)?;
            let to_bank = find_bank(&cable.to.module, cable.line)?;
            let from_list = resolve(
                &cable.from.module,
                from_bank.count,
                cable.from.selector,
                cable.line,
            )?;
            let to_list = resolve(
                &cable.to.module,
                to_bank.count,
                cable.to.selector,
                cable.line,
            )?;
            let pairs = pair_up(&from_list, &to_list, cable)?;
            for (a, b) in pairs {
                let from_id = crate::network::ModuleId(from_bank.first + a);
                let to_id = crate::network::ModuleId(to_bank.first + b);
                network
                    .patch(from_id, &cable.from.jack, to_id, &cable.to.jack)
                    .map_err(|e| PatchError::at(cable.line, e.message))?;
            }
        }

        let mut channels = Vec::new();
        for record in &self.records {
            match record {
                RecordDecl::Everything => {
                    for i in 0..network.len() {
                        let id = crate::network::ModuleId(i);
                        let label = network.label(id).to_string();
                        for field in network.get(id).state_fields() {
                            channels.push(format!("{label}.{field}"));
                        }
                    }
                }
                RecordDecl::Channel {
                    module,
                    selector,
                    field,
                    line,
                } => {
                    let bank = find_bank(module, *line)?;
                    if *selector == Selector::Matched {
                        return Err(PatchError::at(
                            *line,
                            "[i] only makes sense on a patch line",
                        ));
                    }
                    let instances = resolve(module, bank.count, *selector, *line)?;
                    for instance in instances {
                        let id = crate::network::ModuleId(bank.first + instance);
                        let m = network.get(id);
                        let known = m.state_fields().contains(&field.as_str())
                            || m.out_jacks().contains(&field.as_str());
                        if !known {
                            return Err(PatchError::at(
                                *line,
                                format!(
                                    "module '{module}' ({}) has no state or output jack '{field}' \
                                     (states: {}; outputs: {})",
                                    m.kind_name(),
                                    m.state_fields().join(", "),
                                    m.out_jacks().join(", ")
                                ),
                            ));
                        }
                        channels.push(format!("{}.{field}", network.label(id)));
                    }
                }
            }
        }
        channels.dedup();

        Ok(BuiltPatch {
            network,
            config: self.config.sim(),
            seconds: self.config.seconds,
            channels,
        })
    }

    fn instantiate(
        &self,
        decl: &ModuleDecl,
        module_index: usize,
        instance: usize,
    ) -> Result<Module, PatchError> {
        let mut module = match (&decl.preset, decl.kind.as_str()) {
            (Some(preset), "rc1") => {
                let params = Rc1Params::from_preset(preset).ok_or_else(|| {
                    PatchError::at(
                        decl.line,
                        format!(
                            "rc1 has no preset '{preset}' (presets: {})",
                            Rc1Params::preset_names().join(", ")
                        ),
                    )
                })?;
                Module::Rc1(Rc1::new(params))
            }
            (Some(_), kind) => {
                return Err(PatchError::at(
                    decl.line,
                    format!("module kind '{kind}' has no presets"),
                ))
            }
            (None, kind) => Module::from_kind_name(kind).ok_or_else(|| {
                PatchError::at(
                    decl.line,
                    format!(
                        "unknown module kind '{kind}' (kinds: {})",
                        Module::kind_names().join(", ")
                    ),
                )
            })?,
        };

        for (name, value) in &decl.knobs {
            set_knob(&mut module, name, value, decl.line)?;
        }

        // Component tolerance perturbs the value that was dialled in, so this
        // runs after the knobs.
        let seed = instance_seed(self.config.seed, module_index, instance);
        module.apply_variation(InstanceVariation::new(seed, self.config.mismatch));
        Ok(module)
    }
}

/// Parse and apply one knob value, using the spec to interpret enum names.
fn set_knob(module: &mut Module, name: &str, value: &str, line: usize) -> Result<(), PatchError> {
    let spec = param::find_spec(module.params(), name).ok_or_else(|| {
        PatchError::at(
            line,
            format!(
                "module kind '{}' has no knob '{name}' (knobs: {})",
                module.kind_name(),
                module
                    .params()
                    .iter()
                    .map(|s| s.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })?;
    let parsed = spec
        .parse(value)
        .map_err(|e| PatchError::at(line, e.to_string()))?;
    module
        .set_param(name, parsed)
        .map_err(|e| PatchError::at(line, e.to_string()))
}

/// Which instance indices a selector names.
fn resolve(
    name: &str,
    count: usize,
    selector: Selector,
    line: usize,
) -> Result<Vec<usize>, PatchError> {
    match selector {
        Selector::Single if count == 1 => Ok(vec![0]),
        Selector::Single => Err(PatchError::at(
            line,
            format!("module '{name}' has {count} instances; use {name}[k], {name}[*] or {name}[i]"),
        )),
        Selector::Index(k) if k < count => Ok(vec![k]),
        Selector::Index(k) => Err(PatchError::at(
            line,
            format!("{name}[{k}] is out of range; {name} has {count} instances"),
        )),
        Selector::All | Selector::Matched => Ok((0..count).collect()),
    }
}

/// Expand a patch line into concrete instance pairs.
///
/// `[i]` on either side pairs instances index for index; anything else takes
/// the cross product, which is how a bank fans into one jack and sums.
fn pair_up(
    from: &[usize],
    to: &[usize],
    cable: &CableDecl,
) -> Result<Vec<(usize, usize)>, PatchError> {
    let matched =
        cable.from.selector == Selector::Matched || cable.to.selector == Selector::Matched;
    if !matched {
        let mut pairs = Vec::with_capacity(from.len() * to.len());
        for a in from {
            for b in to {
                pairs.push((*a, *b));
            }
        }
        return Ok(pairs);
    }
    let (a_len, b_len) = (from.len(), to.len());
    if a_len == b_len {
        Ok(from.iter().copied().zip(to.iter().copied()).collect())
    } else if a_len == 1 {
        Ok(to.iter().map(|b| (from[0], *b)).collect())
    } else if b_len == 1 {
        Ok(from.iter().map(|a| (*a, to[0])).collect())
    } else {
        Err(PatchError::at(
            cable.line,
            format!(
                "cannot index-match {} instances of '{}' against {} of '{}'",
                a_len, cable.from.module, b_len, cable.to.module
            ),
        ))
    }
}

/// Give every instance its own deterministic tolerance draw.
fn instance_seed(base: u64, module_index: usize, instance: usize) -> u64 {
    base.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((module_index as u64).wrapping_mul(0xD1B5_4A32_D192_ED03))
        .wrapping_add(
            (instance as u64)
                .wrapping_add(1)
                .wrapping_mul(0xA0761D6478BD642F),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# a small recurrent patch
dt 0.0005
seed 7
mismatch 0.05
seconds 2

module src   source  shape=pulse freq=1.4 width=0.11 amp=2.2 offset=0.15
module brain tc3     natural_decay=0.015
module cells rc1[4]  preset=burst rate=90
module layer neuron[4] w0=0.9 leak=7

knob cells[2].bias 2.4

patch src.out       -> brain.a
patch src.out       -> brain.learn
patch brain.a       -> layer[*].in0
patch layer[i].out  -> cells[i].in
patch cells[*].out  -> brain.b

record cells[*].v brain.w00 brain.activity
";

    #[test]
    fn sample_patch_builds() {
        let patch = parse(SAMPLE).expect("parse");
        assert_eq!(patch.modules.len(), 4);
        let built = patch.build().expect("build");
        // 1 source + 1 tc3 + 4 cells + 4 neurons
        assert_eq!(built.network.len(), 10);
        // 2 + 4 (fan-out) + 4 (matched) + 4 (fan-in)
        assert_eq!(built.network.cables().len(), 14);
        assert_eq!(built.channels.len(), 6);
        assert_eq!(built.channels[0], "cells[0].v");
        assert!((built.seconds - 2.0).abs() < 1e-6);
    }

    #[test]
    fn knob_line_overrides_one_instance() {
        let built = parse(SAMPLE).unwrap().build().unwrap();
        let mut biases = Vec::new();
        for i in 0..built.network.len() {
            let id = crate::network::ModuleId(i);
            if built.network.label(id).starts_with("cells") {
                biases.push(built.network.get(id).get_param("bias").unwrap());
            }
        }
        assert_eq!(biases.len(), 4);
        // cells[2] was dialled up; mismatch keeps the others near the preset.
        assert!(biases[2] > 2.3, "biases={biases:?}");
        assert!(biases[0] < 2.2, "biases={biases:?}");
    }

    #[test]
    fn mismatch_makes_instances_individual() {
        let ideal = parse(&SAMPLE.replace("mismatch 0.05", "mismatch 0"))
            .unwrap()
            .build()
            .unwrap();
        let varied = parse(SAMPLE).unwrap().build().unwrap();
        let read = |built: &BuiltPatch, label: &str| {
            (0..built.network.len())
                .map(crate::network::ModuleId)
                .find(|id| built.network.label(*id) == label)
                .map(|id| built.network.get(id).get_param("sharpness").unwrap())
                .unwrap()
        };
        assert_eq!(read(&ideal, "cells[0]"), read(&ideal, "cells[1]"));
        assert_ne!(read(&varied, "cells[0]"), read(&varied, "cells[1]"));
    }

    /// Membrane trace of every cell in a noisy bank.
    fn noisy_run(text: &str) -> Vec<f32> {
        use crate::engine::Engine;
        let built = parse(text).unwrap().build().unwrap();
        let mut engine = Engine::new(built.network, built.config);
        let mut trace = Vec::new();
        for _ in 0..500 {
            engine.step();
            for channel in &built.channels {
                trace.push(engine.lookup_channel(channel).unwrap());
            }
        }
        trace
    }

    #[test]
    fn noisy_patches_are_reproducible_and_seed_sensitive() {
        let text = "\
            seed 11\n\
            mismatch 0.05\n\
            module cells rc1[4] preset=tonic noise=0.2 drift=0.05\n\
            record cells[*].v\n";
        assert_eq!(noisy_run(text), noisy_run(text), "same seed must repeat");
        let other = text.replace("seed 11", "seed 12");
        assert_ne!(noisy_run(text), noisy_run(&other), "seed had no effect");
    }

    #[test]
    fn instances_in_a_bank_do_not_share_a_noise_stream() {
        let built = parse("module cells rc1[2] preset=tonic noise=0.3\nrecord cells[*].v\n")
            .unwrap()
            .build()
            .unwrap();
        let mut engine = crate::engine::Engine::new(built.network, built.config);
        let mut differed = false;
        for _ in 0..500 {
            engine.step();
            let a = engine.lookup_channel("cells[0].v").unwrap();
            let b = engine.lookup_channel("cells[1].v").unwrap();
            differed |= (a - b).abs() > 1e-9;
        }
        assert!(differed, "both cells drew the same noise");
    }

    #[test]
    fn record_everything_covers_every_state() {
        let patch = parse("module cells rc1[3]\nrecord *\n").unwrap();
        let built = patch.build().unwrap();
        // Three cells, each reporting v, r, s and drift.
        assert_eq!(built.channels.len(), 12);
        assert!(built.channels.contains(&"cells[2].s".to_string()));
    }

    #[test]
    fn output_jacks_are_recordable() {
        let built = parse("module n neuron\nrecord n.out n.v\n")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(built.channels, vec!["n.out", "n.v"]);
    }

    #[test]
    fn errors_name_the_line() {
        let cases = [
            ("module a flux\n", 1, "unknown module kind"),
            ("module a rc1\nknob a.wobble 1\n", 2, "has no knob"),
            ("module a rc1[2]\npatch a.out -> a.in\n", 2, "2 instances"),
            ("module a rc1\npatch a.out -> a.gate\n", 2, "input jack"),
            ("module a rc1\nrecord a.q\n", 2, "no state or output jack"),
            ("module a rc1[2]\nmodule a neuron\n", 2, "declared twice"),
            (
                "module a rc1\nmodule b rc1[3]\npatch a[i].out -> b.in\n",
                3,
                "3 instances",
            ),
            ("module a rc1 preset=wobble\n", 1, "no preset"),
            ("module a neuron preset=burst\n", 1, "has no presets"),
            ("module a rc1\nknob a.bias 99\n", 2, "outside"),
        ];
        for (text, line, needle) in cases {
            let err = parse(text)
                .and_then(|p| p.build())
                .expect_err(&format!("expected failure for {text:?}"));
            assert_eq!(err.line, line, "{text:?} -> {err}");
            assert!(
                err.message.contains(needle),
                "{text:?} -> {err} (wanted {needle:?})"
            );
        }
    }

    #[test]
    fn index_matched_banks_of_unequal_size_fan_out_from_one() {
        let built = parse("module a rc1\nmodule b neuron[5]\npatch a[i].out -> b[i].in0\n")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(built.network.cables().len(), 5);
    }

    #[test]
    fn shipped_patches_build() {
        for path in [
            "patches/live.kpatch",
            "patches/voice.kpatch",
            "patches/listen.kpatch",
            "patches/duet.kpatch",
            "patches/weather.kpatch",
            "patches/demo.kpatch",
            "patches/chaos.kpatch",
            "patches/bank64.kpatch",
        ] {
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
            parse(&text)
                .and_then(|p| p.build())
                .unwrap_or_else(|e| panic!("{path}: {e}"));
        }
    }

    #[test]
    fn cross_product_connects_every_pair() {
        let built = parse("module a rc1[3]\nmodule b neuron[4]\npatch a[*].out -> b[*].in0\n")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(built.network.cables().len(), 12);
    }
}
