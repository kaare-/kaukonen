//! Command line for the simulator: render, inspect, sweep, bench and probe.

use kaukonen::module::AnalogModule;
use kaukonen::network::ModuleId;
use kaukonen::param::{trim_float, ParamSpec};
use kaukonen::patch::{self, Patch};
use kaukonen::render::{render, RenderOptions};
use kaukonen::schedule::Schedule;
use kaukonen::scope::Scope;
use kaukonen::stream::Stream;
use kaukonen::wav::{Audio, SampleFormat};
use kaukonen::{Module, Recorder};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Instant;

#[cfg(feature = "realtime")]
mod live;

const USAGE: &str = "\
kaukonen — patchable analog neural modules

usage:
  kaukonen render  --patch P [--in W] [--out W] [--seconds S] [--html H] [--csv C]
                            [--stride N] [--format int16|int24|float32]
  kaukonen inspect --patch P [--verbose]
  kaukonen modules [--kind K] [--markdown]
  kaukonen sweep   --patch P --knob T --from A --to B [--steps N] [--dir D] [--seconds S]
  kaukonen bench   --patch P [--seconds S] [--block N]
  kaukonen probe   --patch P --knob T [--epsilon E] [--seconds S]
  kaukonen devices [--verbose]
  kaukonen play    --patch P [--device D] [--rate HZ] [--block N] [--input]
                            [--in-device D] [--in-channel N] [--out-left N] [--out-right N]
                            [--gain G] [--set mod.knob=V] [--record W] [--seconds S]

  render   run a patch, writing audio, a trace and a scope page
  inspect  list a patch's modules, knobs, cables and feedback loops
  modules  the catalogue of module kinds and every knob they have
  sweep    render a bank of variations of one knob
  bench    how fast a patch runs, as a factor of real time
  probe    nudge one knob and report how far the two runs diverge
  devices  what audio hardware is available, and what it can do
  play     play a patch through an audio interface, knobs adjustable live

devices and play need the realtime feature:
  cargo run --release --features realtime -- devices
";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = run(&argv) {
        eprintln!("kaukonen: {message}");
        std::process::exit(1);
    }
}

fn run(argv: &[String]) -> Result<(), String> {
    let Some(command) = argv.first() else {
        print!("{USAGE}");
        return Ok(());
    };
    if command == "--help" || command == "-h" || command == "help" {
        print!("{USAGE}");
        return Ok(());
    }
    let args = Args::parse(&argv[1..])?;
    match command.as_str() {
        "render" => cmd_render(&args),
        "inspect" => cmd_inspect(&args),
        "modules" => cmd_modules(&args),
        "sweep" => cmd_sweep(&args),
        "bench" => cmd_bench(&args),
        "probe" => cmd_probe(&args),
        "devices" => cmd_devices(&args),
        "play" => cmd_play(&args),
        other => Err(format!(
            "unknown command '{other}'. Run `kaukonen --help` for the list."
        )),
    }
}

#[cfg(not(feature = "realtime"))]
fn needs_realtime(command: &str) -> Result<(), String> {
    Err(format!(
        "`{command}` talks to audio hardware, which is behind the realtime feature.\n\
         Rebuild with it:\n    cargo run --release --features realtime -- {command} ..."
    ))
}

#[cfg(not(feature = "realtime"))]
fn cmd_devices(_args: &Args) -> Result<(), String> {
    needs_realtime("devices")
}

#[cfg(not(feature = "realtime"))]
fn cmd_play(_args: &Args) -> Result<(), String> {
    needs_realtime("play")
}

#[cfg(feature = "realtime")]
fn cmd_devices(args: &Args) -> Result<(), String> {
    args.reject_unknown(live::DEVICES_OPTIONS)?;
    live::devices(args.flag("verbose"))
}

#[cfg(feature = "realtime")]
fn cmd_play(args: &Args) -> Result<(), String> {
    args.reject_unknown(live::PLAY_OPTIONS)?;
    let routing = live::Routing {
        left: args.get_usize("out-left")?.unwrap_or(1).saturating_sub(1),
        right: args.get_usize("out-right")?.unwrap_or(2).saturating_sub(1),
        input: args.get_usize("in-channel")?.unwrap_or(1).saturating_sub(1),
    };
    live::play(live::PlayArgs {
        source: args.require("patch")?,
        device: args.get("device"),
        in_device: args.get("in-device"),
        want_input: args.flag("input"),
        rate: args.get_usize("rate")?.map(|r| r as u32),
        block: args.get_usize("block")?.map(|b| b as u32),
        channels: args.get_usize("channels")?.map(|c| c as u16),
        routing,
        gain: args.get_f32("gain")?.unwrap_or(1.0),
        sets: args.all("set"),
        record: args.get("record"),
        seconds: args.get_f32("seconds")?,
        quiet: args.flag("quiet"),
    })
}

// ---------------------------------------------------------------- commands

fn cmd_render(args: &Args) -> Result<(), String> {
    args.reject_unknown(&[
        "patch", "in", "out", "seconds", "html", "csv", "stride", "format",
    ])?;
    let source = args.require("patch")?;
    let patch = load(source)?;
    let built = patch.build().map_err(|e| format!("{source}: {e}"))?;

    let input = match args.get("in") {
        Some(path) => {
            let audio = Audio::read(path).map_err(|e| format!("{path}: {e}"))?;
            println!(
                "in   {path}  {:.2} s  {} Hz  {} ch  peak {:.3}",
                audio.seconds(),
                audio.sample_rate,
                audio.channels,
                audio.peak()
            );
            Some(audio)
        }
        None => None,
    };

    let options = RenderOptions {
        seconds: args.get_f32("seconds")?,
        input,
        stride: args.get_usize("stride")?.unwrap_or(1),
    };
    let (engine, recorder, report) = render(built, &options);

    println!(
        "ran  {:.2} s of simulated time, {} samples recorded",
        report.seconds, report.frames
    );
    for note in summarize_floating(&report.floating) {
        println!("note {note}");
    }

    if let Some(path) = args.get("out") {
        let audio = report
            .audio
            .as_ref()
            .ok_or("this patch has no audioout module, so there is nothing to write")?;
        let format = match args.get("format") {
            Some(name) => SampleFormat::from_name(name)
                .ok_or_else(|| format!("unknown sample format '{name}'"))?,
            None => SampleFormat::Int24,
        };
        audio
            .write(path, format)
            .map_err(|e| format!("{path}: {e}"))?;
        println!(
            "out  {path}  {:.2} s  {} Hz  {} ch  peak {:.3}{}",
            audio.seconds(),
            audio.sample_rate,
            audio.channels,
            audio.peak(),
            if report.clipped() {
                format!("  (converter clipped, drove it to {:.2})", report.peak)
            } else {
                String::new()
            }
        );
    } else if report.audio.is_some() {
        println!("note the patch captured audio; pass --out to write it");
    }

    if let Some(path) = args.get("csv") {
        recorder
            .write_csv(path)
            .map_err(|e| format!("{path}: {e}"))?;
        println!("csv  {path}  {} channels", recorder.names.len());
    }
    if let Some(path) = args.get("html") {
        let mut scope = Scope::new(source, &recorder)
            .with_patch(&engine.network, engine.schedule())
            .with_note(format!(
                "{:.3} s at {} Hz frames, oversample {}",
                report.seconds,
                trim_float(engine.config.sample_rate),
                engine.config.oversample
            ));
        if let Some(audio) = &report.audio {
            scope = scope.with_audio(audio);
        }
        if report.clipped() {
            scope = scope.with_note(format!(
                "the converter clipped: the patch drove it to {:.2} of full scale",
                report.peak
            ));
        }
        scope.write(path).map_err(|e| format!("{path}: {e}"))?;
        println!("html {path}");
    }
    if recorder.names.is_empty() {
        println!("note the patch records no channels; add a `record` line");
    } else {
        print_channel_summary(&recorder);
    }
    Ok(())
}

fn cmd_inspect(args: &Args) -> Result<(), String> {
    args.reject_unknown(&["patch", "verbose"])?;
    let source = args.require("patch")?;
    let patch = load(source)?;
    let built = patch.build().map_err(|e| format!("{source}: {e}"))?;
    let network = &built.network;
    let schedule = Schedule::build(network);
    let config = patch.config;

    println!("{source}");
    println!(
        "  {} Hz frames, oversample {}, dt {:.1} us, rail +/-{} V",
        trim_float(config.sample_rate),
        config.oversample,
        config.dt() * 1e6,
        trim_float(config.rail)
    );
    println!(
        "  seed {}, mismatch {:.1}%, seconds {}",
        config.seed,
        config.mismatch * 100.0,
        trim_float(config.seconds)
    );

    println!("\nmodules ({})", network.len());
    let verbose = args.flag("verbose");
    let banks = group_by_bank(network);
    for decl in &patch.modules {
        let members = banks.get(&decl.name).map(Vec::as_slice).unwrap_or_default();
        let Some(first) = members.first().map(|i| network.get(ModuleId(*i))) else {
            continue;
        };
        let count = if decl.count == 1 {
            String::new()
        } else {
            format!("[{}]", decl.count)
        };
        println!(
            "  {:<14} {:<9} in: {:<28} out: {}",
            format!("{}{count}", decl.name),
            first.kind_name(),
            or_dash(&first.in_jacks().join(" ")),
            or_dash(&first.out_jacks().join(" "))
        );
        // What the patch asked for, rather than every knob tolerance moved.
        let mut set: Vec<String> = Vec::new();
        if let Some(preset) = &decl.preset {
            set.push(format!("preset={preset}"));
        }
        for (key, value) in &decl.knobs {
            set.push(format!("{key}={value}"));
        }
        if !set.is_empty() {
            println!("      set: {}", set.join("  "));
        }
        if verbose {
            for index in members {
                let module = network.get(ModuleId(*index));
                println!(
                    "      {}: {}",
                    network.label(ModuleId(*index)),
                    changed_knobs(module).join("  ")
                );
            }
        } else if config.mismatch > 0.0 && decl.count > 1 {
            println!(
                "      {} instances, each individual at {:.1}% tolerance (--verbose to see them)",
                decl.count,
                config.mismatch * 100.0
            );
        }
    }

    println!("\ncables ({})", network.cables().len());
    for cable in network.cables() {
        let from = network.get(cable.from);
        let to = network.get(cable.to);
        println!(
            "  {}.{} -> {}.{}",
            network.label(cable.from),
            from.out_jacks()[cable.from_jack],
            network.label(cable.to),
            to.in_jacks()[cable.to_jack]
        );
    }

    let feedback = schedule.feedback_labels(network);
    if feedback.is_empty() {
        println!("\nno feedback loops: every cable resolves within one step");
    } else {
        println!(
            "\nfeedback ({}) — these carry one step of delay, which is what breaks the loop",
            feedback.len()
        );
        for label in &feedback {
            println!("  {label}");
        }
    }

    // Grouped by bank, so a rack of 2000 cells does not print 2000 lines.
    let mut warnings = Vec::new();
    for (bank, members) in &banks {
        let floating: Vec<usize> = members
            .iter()
            .copied()
            .filter(|index| {
                network.get(ModuleId(*index)).input_count() > 0
                    && schedule.incoming[*index].is_empty()
            })
            .collect();
        if floating.is_empty() {
            continue;
        }
        if members.len() == 1 {
            warnings.push(format!("nothing is patched into '{bank}'"));
        } else {
            warnings.push(format!(
                "nothing is patched into {} of the {} instances of '{bank}'",
                floating.len(),
                members.len()
            ));
        }
    }
    if built.channels.is_empty() {
        warnings.push("no `record` line, so a render would produce no trace".to_string());
    }
    if !warnings.is_empty() {
        println!("\nwarnings");
        for warning in warnings {
            println!("  {warning}");
        }
    }

    println!("\nrecords ({})", built.channels.len());
    if !built.channels.is_empty() {
        println!("  {}", built.channels.join(" "));
    }
    Ok(())
}

fn cmd_modules(args: &Args) -> Result<(), String> {
    args.reject_unknown(&["kind", "markdown"])?;
    let wanted = args.get("kind");
    if let Some(kind) = wanted {
        if Module::from_kind_name(kind).is_none() {
            return Err(format!(
                "unknown module kind '{kind}' (kinds: {})",
                Module::kind_names().join(", ")
            ));
        }
    }
    let markdown = args.flag("markdown");
    if markdown {
        println!("# Module reference\n");
        println!("Generated by `kaukonen modules --markdown`.\n");
    }

    for name in Module::kind_names() {
        if wanted.is_some_and(|k| k != name) {
            continue;
        }
        let module = Module::from_kind_name(name).expect("known kind");
        if markdown {
            println!("## `{name}`\n");
            println!(
                "- inputs: {}\n- outputs: {}\n- states: {}\n",
                jack_list(module.in_jacks()),
                jack_list(module.out_jacks()),
                jack_list(module.state_fields())
            );
            println!("| knob | range | default | doc |");
            println!("| --- | --- | --- | --- |");
            for spec in module.params() {
                println!(
                    "| `{}` | {} | `{}` | {} |",
                    spec.name,
                    // A choice list is pipe separated, which would otherwise
                    // end the table cell.
                    range_of(spec).replace('|', "\\|"),
                    spec.format(spec.default),
                    spec.doc
                );
            }
            if name == "rc1" {
                println!("\nPresets: {}.", rc1_presets().join(", "));
            }
            println!();
        } else {
            println!("{name}");
            println!("  in     {}", or_dash(&module.in_jacks().join(" ")));
            println!("  out    {}", or_dash(&module.out_jacks().join(" ")));
            println!("  state  {}", or_dash(&module.state_fields().join(" ")));
            for spec in module.params() {
                println!(
                    "  {:<16} {:<22} default {:<10} {}",
                    spec.name,
                    range_of(spec),
                    spec.format(spec.default),
                    spec.doc
                );
            }
            println!();
        }
    }
    if wanted.is_none() && !markdown {
        println!("rc1 presets: {}", rc1_presets().join(", "));
    }
    Ok(())
}

fn cmd_sweep(args: &Args) -> Result<(), String> {
    args.reject_unknown(&["patch", "knob", "from", "to", "steps", "dir", "seconds"])?;
    let source = args.require("patch")?;
    let target = args.require("knob")?;
    let patch = load(source)?;
    let (module, knob) = split_target(target)?;
    let spec = spec_for(&patch, &module, &knob)?;

    let from = args.get_f32("from")?.unwrap_or(spec.min);
    let to = args.get_f32("to")?.unwrap_or(spec.max);
    let steps = args.get_usize("steps")?.unwrap_or(5).max(2);
    let dir = args.get("dir").unwrap_or("out/sweep");

    println!(
        "sweeping {target} from {} to {} in {steps} steps ({:?} taper)",
        trim_float(from),
        trim_float(to),
        spec.taper
    );
    println!(
        "  {:<12} {:<10} {:<10} channels (min..max)",
        "value", "peak", "rms"
    );

    for step in 0..steps {
        // Step along the knob's own taper, so a frequency sweep sounds even.
        let t = step as f32 / (steps - 1) as f32;
        let a = spec.to_norm(from);
        let b = spec.to_norm(to);
        // Quantize, or the taper's round trip leaves 120 looking like
        // 120.000008 in the table and in the filename.
        let value = quantize(spec.from_norm(a + (b - a) * t));

        let mut variant = patch.clone();
        variant.knobs.push(patch::KnobDecl {
            module: module.clone(),
            selector: patch::Selector::All,
            param: knob.clone(),
            value: trim_float(value),
            line: 0,
        });
        let built = variant.build().map_err(|e| format!("{source}: {e}"))?;
        let options = RenderOptions {
            seconds: args.get_f32("seconds")?,
            input: None,
            stride: 1,
        };
        let (_engine, recorder, report) = render(built, &options);

        let mut summary = String::new();
        for name in recorder.names.iter().take(3) {
            if let Some((min, max)) = recorder.min_max(name) {
                let _ = write!(summary, "{name} {:.3}..{:.3}  ", min, max);
            }
        }
        let (peak, rms) = match &report.audio {
            Some(audio) => (
                audio.peak(),
                (audio.samples.iter().map(|s| s * s).sum::<f32>()
                    / audio.samples.len().max(1) as f32)
                    .sqrt(),
            ),
            None => (0.0, 0.0),
        };
        println!(
            "  {:<12} {:<10.4} {:<10.4} {}",
            spec.format(value),
            peak,
            rms,
            summary.trim_end()
        );

        if let Some(audio) = &report.audio {
            let path = format!("{dir}/{}_{}.wav", knob, trim_float(value).replace('.', "_"));
            audio
                .write(&path, SampleFormat::Int24)
                .map_err(|e| format!("{path}: {e}"))?;
        }
    }
    if patch.records.is_empty() {
        println!("note the patch records no channels, so only audio is summarised");
    }
    Ok(())
}

fn cmd_bench(args: &Args) -> Result<(), String> {
    args.reject_unknown(&["patch", "seconds", "block"])?;
    let source = args.require("patch")?;
    let patch = load(source)?;
    let built = patch.build().map_err(|e| format!("{source}: {e}"))?;
    let modules = built.network.len();
    let cables = built.network.cables().len();
    let seconds = args.get_f32("seconds")?.unwrap_or(built.seconds);
    let config = built.config;

    let mut engine = kaukonen::Engine::new(built.network, config);
    let frames = (seconds * config.sample_rate) as usize;
    let start = Instant::now();
    for _ in 0..frames {
        engine.frame();
    }
    let elapsed = start.elapsed().as_secs_f64();

    let steps = frames as f64 * config.oversample.max(1) as f64;
    let module_steps = steps * modules as f64;
    println!("{source}");
    println!("  {modules} modules, {cables} cables");
    println!(
        "  {} Hz frames x{} oversample = {:.0} steps/s of simulated time",
        trim_float(config.sample_rate),
        config.oversample,
        config.sample_rate as f64 * config.oversample.max(1) as f64
    );
    println!("  {seconds:.2} s simulated in {elapsed:.3} s wall");
    println!(
        "  realtime factor x{:.2}{}",
        seconds as f64 / elapsed.max(1e-9),
        if seconds as f64 / elapsed.max(1e-9) < 1.0 {
            "  (slower than real time)"
        } else {
            ""
        }
    );
    if module_steps > 0.0 {
        println!(
            "  {:.0} module-steps/s, {:.0} ns per module-step",
            module_steps / elapsed.max(1e-9),
            elapsed * 1e9 / module_steps
        );
    }

    // An average faster than real time is not enough to stream without
    // glitching: one block that misses its deadline is an audible dropout.
    let block = args.get_usize("block")?.unwrap_or(256).max(1);
    let rebuilt = load(source)?
        .build()
        .map_err(|e| format!("{source}: {e}"))?;
    let mut stream = Stream::new(rebuilt);
    if !stream.has_output() {
        println!("  (no audioout, so there is nothing to stream)");
        return Ok(());
    }
    let mut buffer = vec![0.0f32; block * 2];
    let deadline = block as f64 / config.sample_rate as f64;
    let blocks = ((seconds as f64 / deadline) as usize).max(1);
    // One block first, so a cold cache is not counted as jitter.
    stream.process(&mut buffer, 2);
    let mut total = 0.0;
    let mut worst = 0.0f64;
    for _ in 0..blocks {
        let start = Instant::now();
        stream.process(&mut buffer, 2);
        let taken = start.elapsed().as_secs_f64();
        total += taken;
        worst = worst.max(taken);
    }
    println!(
        "  streaming {block}-frame blocks: {:.1} us deadline, {:.1} us mean, {:.1} us worst",
        deadline * 1e6,
        total / blocks as f64 * 1e6,
        worst * 1e6
    );
    let headroom = deadline / worst.max(1e-12);
    println!(
        "  worst-case headroom x{headroom:.2}{}",
        if headroom < 1.0 {
            "  (would drop out)"
        } else if headroom < 2.0 {
            "  (tight)"
        } else {
            ""
        }
    );
    Ok(())
}

fn cmd_probe(args: &Args) -> Result<(), String> {
    args.reject_unknown(&["patch", "knob", "epsilon", "seconds"])?;
    let source = args.require("patch")?;
    let target = args.require("knob")?;
    let patch = load(source)?;
    let (module, knob) = split_target(target)?;
    let spec = spec_for(&patch, &module, &knob)?;
    let epsilon = args.get_f32("epsilon")?.unwrap_or(1e-4);

    if patch.records.is_empty() {
        return Err("probe needs a `record` line to compare trajectories".to_string());
    }

    let base_value = {
        let built = patch.build().map_err(|e| format!("{source}: {e}"))?;
        let index = (0..built.network.len())
            .find(|i| {
                let label = built.network.label(ModuleId(*i));
                label == module || label.starts_with(&format!("{module}["))
            })
            .ok_or_else(|| format!("no module named '{module}'"))?;
        built
            .network
            .get(ModuleId(index))
            .get_param(&knob)
            .ok_or_else(|| format!("'{knob}' is not readable"))?
    };
    let nudged = spec.clamp(base_value + epsilon.abs().max(1e-9));
    if nudged == base_value {
        return Err(format!(
            "{target} is at its end stop, so it cannot be nudged upward"
        ));
    }

    let options = RenderOptions {
        seconds: args.get_f32("seconds")?,
        input: None,
        stride: 1,
    };
    let run = |value: Option<f32>| -> Result<Recorder, String> {
        let mut variant = patch.clone();
        if let Some(value) = value {
            variant.knobs.push(patch::KnobDecl {
                module: module.clone(),
                selector: patch::Selector::All,
                param: knob.clone(),
                value: format!("{value}"),
                line: 0,
            });
        }
        let built = variant.build().map_err(|e| format!("{source}: {e}"))?;
        Ok(render(built, &options).1)
    };
    let a = run(None)?;
    let b = run(Some(nudged))?;

    let distances = trajectory_distance(&a, &b);
    if distances.is_empty() {
        return Err("the run produced no samples to compare".to_string());
    }
    let start_value = spec.format(base_value);
    println!("{source}");
    println!(
        "  nudged {target} from {start_value} by {}",
        trim_float(nudged - base_value)
    );
    println!(
        "  comparing {} channels over {} samples",
        a.names.len(),
        distances.len()
    );

    let at = |fraction: f32| {
        let index = ((distances.len() - 1) as f32 * fraction) as usize;
        (a.times[index], distances[index])
    };
    for fraction in [0.0, 0.1, 0.5, 1.0] {
        let (time, distance) = at(fraction);
        println!("  t={time:8.3} s   separation {distance:.3e} V");
    }

    let (t0, d0) = at(0.05);
    let (t1, d1) = at(1.0);
    let grew = d1 > d0 * 10.0;
    if d0 > 0.0 && d1 > 0.0 && t1 > t0 {
        let rate = (d1 / d0).ln() / (t1 - t0);
        println!("  mean divergence rate {rate:+.3} /s");
    }
    println!(
        "  {}",
        if grew {
            "sensitive: a tiny change grows into a different trajectory"
        } else {
            "stable: the two runs stay close together"
        }
    );
    Ok(())
}

// ---------------------------------------------------------------- helpers

fn load(path: &str) -> Result<Patch, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    patch::parse(&text).map_err(|e| format!("{path}: {e}"))
}

fn split_target(target: &str) -> Result<(String, String), String> {
    let (module, knob) = target
        .rsplit_once('.')
        .ok_or_else(|| format!("'{target}' should look like module.knob"))?;
    let module = module.split('[').next().unwrap_or(module);
    Ok((module.to_string(), knob.to_string()))
}

/// The spec for a knob named in a patch, so a sweep knows its range and taper.
fn spec_for(patch: &Patch, module: &str, knob: &str) -> Result<ParamSpec, String> {
    let decl = patch
        .modules
        .iter()
        .find(|m| m.name == module)
        .ok_or_else(|| format!("this patch has no module named '{module}'"))?;
    let instance = Module::from_kind_name(&decl.kind)
        .ok_or_else(|| format!("unknown module kind '{}'", decl.kind))?;
    instance
        .params()
        .iter()
        .find(|s| s.name == knob)
        .copied()
        .ok_or_else(|| {
            format!(
                "module kind '{}' has no knob '{knob}' (knobs: {})",
                decl.kind,
                instance
                    .params()
                    .iter()
                    .map(|s| s.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// Round to four decimals, enough for any knob and free of float residue.
fn quantize(value: f32) -> f32 {
    (value * 1e4).round() / 1e4
}

/// Collapse per-instance labels into one line per bank.
fn summarize_floating(labels: &[String]) -> Vec<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for label in labels {
        let bank = label.split('[').next().unwrap_or(label);
        *counts.entry(bank).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(bank, count)| {
            if count == 1 {
                format!("nothing is patched into '{bank}'")
            } else {
                format!("nothing is patched into {count} instances of '{bank}'")
            }
        })
        .collect()
}

/// Instance indices of each declared module, keyed by the name in the patch.
fn group_by_bank(network: &kaukonen::Network) -> BTreeMap<String, Vec<usize>> {
    let mut banks: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for index in 0..network.len() {
        let label = network.label(ModuleId(index));
        let bank = label.split('[').next().unwrap_or(label).to_string();
        banks.entry(bank).or_default().push(index);
    }
    banks
}

/// Knobs whose value differs from the kind's factory default.
fn changed_knobs(module: &Module) -> Vec<String> {
    module
        .params()
        .iter()
        .filter_map(|spec| {
            let value = module.get_param(spec.name)?;
            if (value - spec.default).abs() <= 1e-6 * spec.default.abs().max(1.0) {
                return None;
            }
            Some(format!("{}={}", spec.name, spec.format(value)))
        })
        .collect()
}

fn print_channel_summary(recorder: &Recorder) {
    println!("channels");
    for name in &recorder.names {
        if let Some((min, max)) = recorder.min_max(name) {
            println!("  {name:<24} min {min:9.3}  max {max:9.3}");
        }
    }
}

/// Per-sample RMS distance between two runs' recorded channels.
fn trajectory_distance(a: &Recorder, b: &Recorder) -> Vec<f32> {
    let samples = a.times.len().min(b.times.len());
    (0..samples)
        .map(|i| {
            let mut sum = 0.0;
            let mut count = 0;
            for name in &a.names {
                if let (Some(x), Some(y)) = (a.channel(name), b.channel(name)) {
                    if let (Some(x), Some(y)) = (x.get(i), y.get(i)) {
                        sum += (x - y) * (x - y);
                        count += 1;
                    }
                }
            }
            if count == 0 {
                0.0
            } else {
                (sum / count as f32).sqrt()
            }
        })
        .collect()
}

fn range_of(spec: &ParamSpec) -> String {
    match spec.choices() {
        Some(choices) => choices.join("|"),
        None => format!(
            "{}..{}{}",
            trim_float(spec.min),
            trim_float(spec.max),
            spec.unit.suffix()
        ),
    }
}

fn jack_list(names: &[&str]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn or_dash(text: &str) -> String {
    if text.is_empty() {
        "-".to_string()
    } else {
        text.to_string()
    }
}

fn rc1_presets() -> Vec<&'static str> {
    kaukonen::rc1::Rc1Params::preset_names()
}

// ---------------------------------------------------------------- arguments

/// `--key value` and `--key=value`, plus valueless flags.
struct Args {
    entries: Vec<(String, String)>,
}

impl Args {
    fn parse(tokens: &[String]) -> Result<Self, String> {
        let mut entries = Vec::new();
        let mut index = 0;
        while index < tokens.len() {
            let token = &tokens[index];
            let Some(name) = token.strip_prefix("--") else {
                return Err(format!(
                    "expected an option starting with --, found '{token}'"
                ));
            };
            if name.is_empty() {
                return Err("'--' is not an option".to_string());
            }
            if let Some((key, value)) = name.split_once('=') {
                entries.push((key.to_string(), value.to_string()));
                index += 1;
                continue;
            }
            let takes_value = tokens
                .get(index + 1)
                .is_some_and(|next| !next.starts_with("--"));
            if takes_value {
                entries.push((name.to_string(), tokens[index + 1].clone()));
                index += 2;
            } else {
                entries.push((name.to_string(), String::new()));
                index += 1;
            }
        }
        Ok(Self { entries })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
    }

    /// Every value given for a repeatable option, such as `--set`.
    #[cfg_attr(not(feature = "realtime"), allow(dead_code))]
    fn all(&self, name: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(key, value)| key == name && !value.is_empty())
            .map(|(_, value)| value.as_str())
            .collect()
    }

    fn flag(&self, name: &str) -> bool {
        self.entries.iter().any(|(key, _)| key == name)
    }

    fn require(&self, name: &str) -> Result<&str, String> {
        self.get(name)
            .ok_or_else(|| format!("--{name} is required"))
    }

    fn get_f32(&self, name: &str) -> Result<Option<f32>, String> {
        match self.get(name) {
            None => Ok(None),
            Some(text) => text
                .parse()
                .map(Some)
                .map_err(|_| format!("--{name}: '{text}' is not a number")),
        }
    }

    fn get_usize(&self, name: &str) -> Result<Option<usize>, String> {
        match self.get(name) {
            None => Ok(None),
            Some(text) => text
                .parse()
                .map(Some)
                .map_err(|_| format!("--{name}: '{text}' is not a whole number")),
        }
    }

    /// Catch a mistyped option rather than silently ignoring it.
    fn reject_unknown(&self, allowed: &[&str]) -> Result<(), String> {
        for (key, _) in &self.entries {
            if !allowed.contains(&key.as_str()) {
                return Err(format!(
                    "unknown option --{key} (this command takes: {})",
                    allowed
                        .iter()
                        .map(|a| format!("--{a}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
        }
        Ok(())
    }
}
