//! Text to [`Patch`]. Line oriented, hand written, errors carry line numbers.

use super::{
    CableDecl, DeskDecl, JackRef, KnobDecl, ModuleDecl, Patch, PatchConfig, PatchError, RecordDecl,
    Selector,
};
use crate::param::trim_float;

/// Parse a `.kpatch` document.
pub fn parse(text: &str) -> Result<Patch, PatchError> {
    let mut patch = Patch {
        config: PatchConfig::default(),
        ..Patch::default()
    };
    // `dt` and `rate` are resolved after the whole file is read, so they do not
    // depend on whether `oversample` came before or after them.
    let mut dt_directive: Option<(f32, usize)> = None;
    let mut rate_directive: Option<(f32, usize)> = None;

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let body = strip_comment(raw);
        if body.trim().is_empty() {
            continue;
        }
        // Allow `a.out->b.in` as well as `a.out -> b.in`.
        let spaced = body.replace("->", " -> ");
        let mut tokens = spaced.split_whitespace();
        let keyword = tokens.next().unwrap_or_default();
        let rest: Vec<&str> = tokens.collect();

        match keyword {
            "dt" => dt_directive = Some((one_float(&rest, "dt", line)?, line)),
            "rate" => rate_directive = Some((one_float(&rest, "rate", line)?, line)),
            "oversample" => {
                let n = one_u64(&rest, "oversample", line)?;
                if n == 0 {
                    return Err(PatchError::at(line, "oversample must be at least 1"));
                }
                patch.config.oversample = n as u32;
            }
            "rail" => {
                patch.config.rail = bounded(&rest, "rail", line, 0.1, 1000.0)?;
            }
            "seed" => patch.config.seed = one_u64(&rest, "seed", line)?,
            "mismatch" => {
                // Beyond 100% a tolerance stops being a tolerance: it can
                // invert a component and turn a range inside out.
                patch.config.mismatch = bounded(&rest, "mismatch", line, 0.0, 1.0)?;
            }
            "seconds" => patch.config.seconds = bounded(&rest, "seconds", line, 0.0, 86_400.0)?,
            "module" => patch.modules.push(parse_module(&rest, line)?),
            "knob" => patch.knobs.push(parse_knob(&rest, line)?),
            "patch" => patch.cables.push(parse_cable(&rest, line)?),
            "record" => parse_record(&rest, line, &mut patch.records)?,
            "desk" => parse_desk(&rest, line, &mut patch.desk)?,
            other => {
                return Err(PatchError::at(
                    line,
                    format!(
                        "unknown directive '{other}' (expected dt, rate, oversample, rail, seed, \
                         mismatch, seconds, module, knob, patch, record or desk)"
                    ),
                ))
            }
        }
    }

    match (dt_directive, rate_directive) {
        (Some((_, dt_line)), Some(_)) => {
            return Err(PatchError::at(
                dt_line,
                "set either dt or rate, not both; rate is the frame rate and dt is 1/rate \
                 divided by oversample",
            ))
        }
        (Some((dt, line)), None) => {
            if dt <= 0.0 {
                return Err(PatchError::at(line, "dt must be positive"));
            }
            let rate = 1.0 / (dt * patch.config.oversample as f32);
            if !(MIN_RATE..=MAX_RATE).contains(&rate) {
                return Err(PatchError::at(
                    line,
                    format!(
                        "dt {} with oversample {} means {} Hz frames, outside {}..{} Hz",
                        trim_float(dt),
                        patch.config.oversample,
                        trim_float(rate),
                        trim_float(MIN_RATE),
                        trim_float(MAX_RATE)
                    ),
                ));
            }
            patch.config.sample_rate = rate;
        }
        (None, Some((hz, line))) => {
            if !(MIN_RATE..=MAX_RATE).contains(&hz) {
                return Err(PatchError::at(
                    line,
                    format!(
                        "rate is {}, which is outside {}..{} Hz",
                        trim_float(hz),
                        trim_float(MIN_RATE),
                        trim_float(MAX_RATE)
                    ),
                ));
            }
            patch.config.sample_rate = hz;
        }
        (None, None) => {}
    }
    Ok(patch)
}

/// Slower than this and a patch is not really running; faster and a short run
/// produces a trace large enough to exhaust the machine.
const MIN_RATE: f32 = 0.001;
const MAX_RATE: f32 = 1_000_000.0;

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

fn one_float(tokens: &[&str], what: &str, line: usize) -> Result<f32, PatchError> {
    if tokens.len() != 1 {
        return Err(PatchError::at(
            line,
            format!("{what} takes exactly one value"),
        ));
    }
    tokens[0]
        .parse()
        .map_err(|_| PatchError::at(line, format!("{what}: '{}' is not a number", tokens[0])))
}

/// One value that has to fall inside a range, so a typo cannot produce a rack
/// that crashes or a run that eats the machine.
fn bounded(
    tokens: &[&str],
    what: &str,
    line: usize,
    min: f32,
    max: f32,
) -> Result<f32, PatchError> {
    let value = one_float(tokens, what, line)?;
    if !(min..=max).contains(&value) {
        return Err(PatchError::at(
            line,
            format!(
                "{what} is {}, which is outside {}..{}",
                trim_float(value),
                trim_float(min),
                trim_float(max)
            ),
        ));
    }
    Ok(value)
}

fn one_u64(tokens: &[&str], what: &str, line: usize) -> Result<u64, PatchError> {
    if tokens.len() != 1 {
        return Err(PatchError::at(
            line,
            format!("{what} takes exactly one value"),
        ));
    }
    tokens[0]
        .parse()
        .map_err(|_| PatchError::at(line, format!("{what}: '{}' is not an integer", tokens[0])))
}

/// `module <name> <kind>[<count>] [key=value ...]`
fn parse_module(tokens: &[&str], line: usize) -> Result<ModuleDecl, PatchError> {
    if tokens.len() < 2 {
        return Err(PatchError::at(
            line,
            "module takes a name and a kind, e.g. `module cells rc1[4] rate=90`",
        ));
    }
    let name = tokens[0];
    check_ident(name, line)?;
    let (kind, count) = split_count(tokens[1], line)?;
    check_ident(kind, line)?;

    let mut preset = None;
    let mut knobs = Vec::new();
    for token in &tokens[2..] {
        let (key, value) = token.split_once('=').ok_or_else(|| {
            PatchError::at(
                line,
                format!("expected key=value on a module line, found '{token}'"),
            )
        })?;
        if value.is_empty() {
            return Err(PatchError::at(line, format!("'{key}=' has no value")));
        }
        if key == "preset" {
            preset = Some(value.to_string());
        } else {
            knobs.push((key.to_string(), value.to_string()));
        }
    }

    Ok(ModuleDecl {
        name: name.to_string(),
        kind: kind.to_string(),
        count,
        preset,
        knobs,
        line,
    })
}

/// `knob <name>[sel].<param> <value>`
fn parse_knob(tokens: &[&str], line: usize) -> Result<KnobDecl, PatchError> {
    if tokens.len() != 2 {
        return Err(PatchError::at(
            line,
            "knob takes a target and a value, e.g. `knob cells[2].bias 2.4`",
        ));
    }
    let (module, selector, param) = split_dotted(tokens[0], line)?;
    Ok(KnobDecl {
        module,
        selector,
        param,
        value: tokens[1].to_string(),
        line,
    })
}

/// `patch <ref> -> <ref>`
fn parse_cable(tokens: &[&str], line: usize) -> Result<CableDecl, PatchError> {
    if tokens.len() != 3 || tokens[1] != "->" {
        return Err(PatchError::at(
            line,
            "patch takes `<module>.<out> -> <module>.<in>`",
        ));
    }
    Ok(CableDecl {
        from: parse_jack(tokens[0], line)?,
        to: parse_jack(tokens[2], line)?,
        line,
    })
}

fn parse_jack(token: &str, line: usize) -> Result<JackRef, PatchError> {
    let (module, selector, jack) = split_dotted(token, line)?;
    Ok(JackRef {
        module,
        selector,
        jack,
    })
}

/// `desk <module[.knob]> [<module[.knob]> ...]`
fn parse_desk(tokens: &[&str], line: usize, out: &mut Vec<DeskDecl>) -> Result<(), PatchError> {
    if tokens.is_empty() {
        return Err(PatchError::at(
            line,
            "desk takes knobs, e.g. `desk split.cutoff band.q` or a bank name",
        ));
    }
    for token in tokens {
        if token.contains('.') {
            let (module, selector, knob) = split_dotted(token, line)?;
            if selector != Selector::Single {
                return Err(PatchError::at(
                    line,
                    "desk names a bank, not an instance; use `split.cutoff`",
                ));
            }
            out.push(DeskDecl {
                module,
                knob: Some(knob),
                line,
            });
        } else {
            check_ident(token, line)?;
            out.push(DeskDecl {
                module: (*token).to_string(),
                knob: None,
                line,
            });
        }
    }
    Ok(())
}

/// `record <chan> [<chan> ...]` or `record *`
fn parse_record(tokens: &[&str], line: usize, out: &mut Vec<RecordDecl>) -> Result<(), PatchError> {
    if tokens.is_empty() {
        return Err(PatchError::at(
            line,
            "record takes channels, e.g. `record cells[*].v` or `record *`",
        ));
    }
    for token in tokens {
        if *token == "*" {
            out.push(RecordDecl::Everything);
            continue;
        }
        let (module, selector, field) = split_dotted(token, line)?;
        out.push(RecordDecl::Channel {
            module,
            selector,
            field,
            line,
        });
    }
    Ok(())
}

/// Split `name[sel].field` into its three parts.
fn split_dotted(token: &str, line: usize) -> Result<(String, Selector, String), PatchError> {
    let (head, field) = token.rsplit_once('.').ok_or_else(|| {
        PatchError::at(
            line,
            format!("'{token}' needs a dot, as in `module.jack` or `module[2].knob`"),
        )
    })?;
    if field.is_empty() {
        return Err(PatchError::at(
            line,
            format!("'{token}' has an empty name after the dot"),
        ));
    }
    let (name, selector) = split_selector(head, line)?;
    check_ident(&name, line)?;
    Ok((name, selector, field.to_string()))
}

/// Split `name[sel]` into a name and a selector.
fn split_selector(head: &str, line: usize) -> Result<(String, Selector), PatchError> {
    let Some(open) = head.find('[') else {
        return Ok((head.to_string(), Selector::Single));
    };
    if !head.ends_with(']') {
        return Err(PatchError::at(line, format!("'{head}' is missing a ']'")));
    }
    let name = &head[..open];
    let inner = &head[open + 1..head.len() - 1];
    let selector = match inner {
        "*" => Selector::All,
        "i" => Selector::Matched,
        other => Selector::Index(other.parse().map_err(|_| {
            PatchError::at(
                line,
                format!("'{other}' is not an instance index; use a number, * or i"),
            )
        })?),
    };
    Ok((name.to_string(), selector))
}

/// `kind[count]` on a module line.
fn split_count(token: &str, line: usize) -> Result<(&str, usize), PatchError> {
    let Some(open) = token.find('[') else {
        return Ok((token, 1));
    };
    if !token.ends_with(']') {
        return Err(PatchError::at(line, format!("'{token}' is missing a ']'")));
    }
    let inner = &token[open + 1..token.len() - 1];
    let count: usize = inner
        .parse()
        .map_err(|_| PatchError::at(line, format!("'{inner}' is not an instance count")))?;
    if count == 0 {
        return Err(PatchError::at(line, "an instance count must be at least 1"));
    }
    Ok((&token[..open], count))
}

fn check_ident(name: &str, line: usize) -> Result<(), PatchError> {
    let mut chars = name.chars();
    let ok = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(PatchError::at(
            line,
            format!("'{name}' is not a valid name (letters, digits and _ only)"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let patch = parse("# just a comment\n\n   \nmodule a rc1  # trailing\n").unwrap();
        assert_eq!(patch.modules.len(), 1);
        assert_eq!(patch.modules[0].kind, "rc1");
    }

    #[test]
    fn rate_sets_the_frame_rate() {
        let patch = parse("rate 48000\n").unwrap();
        assert_eq!(patch.config.sample_rate, 48000.0);
        assert_eq!(patch.config.oversample, 1);
    }

    #[test]
    fn dt_and_oversample_resolve_in_either_order() {
        // dt is the integration step, so oversampling raises the frame rate to
        // keep it, whichever line came first.
        for text in ["dt 0.0005\noversample 4\n", "oversample 4\ndt 0.0005\n"] {
            let config = parse(text).unwrap().config;
            assert_eq!(config.oversample, 4);
            assert!((config.dt() - 0.0005).abs() < 1e-9, "{text:?}");
            assert!((config.sample_rate - 500.0).abs() < 1e-3, "{text:?}");
        }
    }

    #[test]
    fn dt_and_rate_together_are_rejected() {
        let err = parse("dt 0.0005\nrate 48000\n").unwrap_err();
        assert!(err.message.contains("not both"), "{err}");
    }

    #[test]
    fn selectors_parse() {
        let patch = parse(
            "module a rc1[4]\npatch a[*].out -> a[2].in\npatch a[i].out -> a[i].in\npatch a[0].out -> a[1].in\n",
        )
        .unwrap();
        assert_eq!(patch.cables[0].from.selector, Selector::All);
        assert_eq!(patch.cables[0].to.selector, Selector::Index(2));
        assert_eq!(patch.cables[1].from.selector, Selector::Matched);
        assert_eq!(patch.cables[2].from.selector, Selector::Index(0));
    }

    #[test]
    fn glued_arrows_are_accepted() {
        let patch = parse("module a rc1\nmodule b neuron\npatch a.out->b.in0\n").unwrap();
        assert_eq!(patch.cables.len(), 1);
        assert_eq!(patch.cables[0].to.jack, "in0");
    }

    #[test]
    fn desk_accepts_a_bank_or_a_knob() {
        let patch = parse("module a rc1\nmodule b svf\ndesk a.rate b\n").unwrap();
        assert_eq!(patch.desk.len(), 2);
        assert_eq!(patch.desk[0].module, "a");
        assert_eq!(patch.desk[0].knob.as_deref(), Some("rate"));
        assert_eq!(patch.desk[1].module, "b");
        assert_eq!(patch.desk[1].knob, None);
    }

    #[test]
    fn syntax_errors_report_their_line() {
        let cases = [
            ("module a rc1\nwobble 3\n", 2, "unknown directive"),
            ("dt\n", 1, "exactly one value"),
            ("dt banana\n", 1, "not a number"),
            ("rate 0\n", 1, "outside"),
            ("module\n", 1, "takes a name and a kind"),
            ("module a rc1 bias\n", 1, "expected key=value"),
            ("module a rc1 bias=\n", 1, "has no value"),
            ("module a rc1[0]\n", 1, "at least 1"),
            ("module a rc1[x]\n", 1, "not an instance count"),
            ("module 9a rc1\n", 1, "not a valid name"),
            ("module a rc1\npatch a.out b.in\n", 2, "patch takes"),
            ("module a rc1\npatch aout -> b.in\n", 2, "needs a dot"),
            (
                "module a rc1\nknob a.bias\n",
                2,
                "takes a target and a value",
            ),
            ("module a rc1\nrecord\n", 2, "record takes channels"),
            ("module a rc1\ndesk\n", 2, "desk takes knobs"),
            (
                "module a rc1\npatch a[q].out -> a.in\n",
                2,
                "not an instance index",
            ),
            ("module a rc1\npatch a[0.out -> a.in\n", 2, "missing a ']'"),
            // Values that used to be accepted here and crash much later.
            ("rail -1\n", 1, "outside 0.1..1000"),
            ("rail 0\n", 1, "outside"),
            ("mismatch 3\n", 1, "outside 0..1"),
            ("mismatch -1\n", 1, "outside"),
            ("rate 1000000000\n", 1, "outside 0.001..1000000"),
            ("rate 0.0000001\n", 1, "outside"),
            ("dt 0.0000000001\n", 1, "outside"),
            ("seconds -5\n", 1, "outside"),
        ];
        for (text, line, needle) in cases {
            let err = parse(text).expect_err(&format!("expected failure for {text:?}"));
            assert_eq!(err.line, line, "{text:?} -> {err}");
            assert!(
                err.message.contains(needle),
                "{text:?} -> {err} (wanted {needle:?})"
            );
        }
    }
}
