//! [`Patch`] back to text, so a patch loaded and edited in memory can be saved.

use super::{Patch, PatchConfig, RecordDecl};
use crate::param::trim_float;

impl std::fmt::Display for Patch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let d = PatchConfig::default();
        writeln!(f, "# kaukonen patch v1")?;
        writeln!(f, "rate {}", trim_float(self.config.sample_rate))?;
        if self.config.oversample != d.oversample {
            writeln!(f, "oversample {}", self.config.oversample)?;
        }
        if self.config.rail != d.rail {
            writeln!(f, "rail {}", trim_float(self.config.rail))?;
        }
        if self.config.seed != d.seed {
            writeln!(f, "seed {}", self.config.seed)?;
        }
        if self.config.mismatch != d.mismatch {
            writeln!(f, "mismatch {}", trim_float(self.config.mismatch))?;
        }
        if self.config.seconds != d.seconds {
            writeln!(f, "seconds {}", trim_float(self.config.seconds))?;
        }
        if !self.desk.is_empty() {
            write!(f, "desk")?;
            for item in &self.desk {
                match &item.knob {
                    Some(knob) => write!(f, " {}.{knob}", item.module)?,
                    None => write!(f, " {}", item.module)?,
                }
            }
            writeln!(f)?;
        }

        if !self.modules.is_empty() {
            writeln!(f)?;
        }
        for decl in &self.modules {
            let kind = if decl.count == 1 {
                decl.kind.clone()
            } else {
                format!("{}[{}]", decl.kind, decl.count)
            };
            write!(f, "module {} {}", decl.name, kind)?;
            if let Some(preset) = &decl.preset {
                write!(f, " preset={preset}")?;
            }
            for (key, value) in &decl.knobs {
                write!(f, " {key}={value}")?;
            }
            writeln!(f)?;
        }

        if !self.knobs.is_empty() {
            writeln!(f)?;
        }
        for knob in &self.knobs {
            writeln!(
                f,
                "knob {}{}.{} {}",
                knob.module,
                knob.selector.suffix(),
                knob.param,
                knob.value
            )?;
        }

        if !self.cables.is_empty() {
            writeln!(f)?;
        }
        for cable in &self.cables {
            writeln!(f, "patch {} -> {}", cable.from, cable.to)?;
        }

        if !self.records.is_empty() {
            writeln!(f)?;
            write!(f, "record")?;
            for record in &self.records {
                match record {
                    RecordDecl::Everything => write!(f, " *")?,
                    RecordDecl::Channel {
                        module,
                        selector,
                        field,
                        ..
                    } => write!(f, " {}{}.{}", module, selector.suffix(), field)?,
                }
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;

    const SAMPLE: &str = "\
dt 0.0005
rail 8
seed 7
mismatch 0.05
seconds 2
desk cells.rate
module src source shape=pulse freq=1.4 amp=2.2
module brain tc3 natural_decay=0.015
module cells rc1[4] preset=burst rate=90
knob cells[2].bias 2.4
patch src.out -> brain.a
patch cells[*].out -> brain.b
patch brain.a -> cells[i].in
record cells[*].v brain.activity *
";

    #[test]
    fn serializing_round_trips() {
        let first = parse(SAMPLE).expect("parse sample");
        let text = first.to_string();
        let second = parse(&text).expect("parse serialized");
        // Line numbers differ because the canonical form regroups directives,
        // so compare everything that describes the patch itself.
        assert_eq!(first.config, second.config);
        assert_eq!(first.modules.len(), second.modules.len());
        for (a, b) in first.modules.iter().zip(&second.modules) {
            assert_eq!(
                (&a.name, &a.kind, a.count, &a.preset, &a.knobs),
                (&b.name, &b.kind, b.count, &b.preset, &b.knobs)
            );
        }
        for (a, b) in first.cables.iter().zip(&second.cables) {
            assert_eq!((&a.from, &a.to), (&b.from, &b.to));
        }
        for (a, b) in first.knobs.iter().zip(&second.knobs) {
            assert_eq!(
                (&a.module, a.selector, &a.param, &a.value),
                (&b.module, b.selector, &b.param, &b.value)
            );
        }
        assert_eq!(second.records.len(), 3);
        assert_eq!(first.desk.len(), second.desk.len());
        for (a, b) in first.desk.iter().zip(&second.desk) {
            assert_eq!((&a.module, &a.knob), (&b.module, &b.knob));
        }
        // A second pass is textually identical, so the form is canonical.
        assert_eq!(text, second.to_string());
    }

    #[test]
    fn defaults_are_omitted() {
        let text = parse("module a rc1\n").unwrap().to_string();
        assert!(text.contains("rate "));
        assert!(!text.contains("oversample"));
        assert!(!text.contains("mismatch"));
        assert!(!text.contains("seconds"));
    }
}
