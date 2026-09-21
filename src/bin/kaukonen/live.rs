//! The `devices` and `play` subcommands, which need real audio hardware.
//!
//! Split out so the rest of the command line compiles without the `realtime`
//! feature, and so the code that talks to a device sits in one place.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use kaukonen::backend::Routing;
use kaukonen::backend::{self, Control, Direction, PlayOptions};
use kaukonen::module::AnalogModule;
use kaukonen::network::ModuleId;
use kaukonen::param::{self, trim_float};
use kaukonen::patch;
use kaukonen::wav::{Audio, SampleFormat};
use kaukonen::Stream;

/// How often the console prints a line of stream health while playing.
const REPORT_INTERVAL: Duration = Duration::from_secs(2);

/// How often the writer thread empties the audio thread's staging buffer.
const DRAIN_INTERVAL: Duration = Duration::from_millis(20);

pub const DEVICES_OPTIONS: &[&str] = &["verbose"];
pub const PLAY_OPTIONS: &[&str] = &[
    "patch",
    "device",
    "in-device",
    "input",
    "rate",
    "block",
    "out-left",
    "out-right",
    "in-channel",
    "channels",
    "gain",
    "set",
    "record",
    "seconds",
    "quiet",
];

/// `kaukonen devices`
pub fn devices(verbose: bool) -> Result<(), String> {
    for direction in [Direction::Output, Direction::Input] {
        let reports = backend::survey(direction)?;
        println!(
            "{} devices ({})",
            match direction {
                Direction::Output => "output",
                Direction::Input => "input",
            },
            reports.len()
        );
        if reports.is_empty() {
            println!("  none. On Linux, check the interface is visible to ALSA (`aplay -l`).");
            println!(
                "  On a Mac, System Settings → Sound should list the built-in speakers; \
                 an interface needs its driver and then a plug."
            );
        }
        for report in &reports {
            println!(
                "  [{}] {}{}",
                report.index,
                report.name,
                if report.is_default { "  (default)" } else { "" }
            );
            if verbose || reports.len() <= 8 {
                let channels: Vec<String> = report.channels.iter().map(|c| c.to_string()).collect();
                let rates: Vec<String> = report.rates.iter().map(|r| r.to_string()).collect();
                println!(
                    "        channels {}   rates {}   formats {}",
                    or_none(&channels.join("/")),
                    or_none(&rates.join(" ")),
                    or_none(&report.formats.join(" "))
                );
                if let Some((min, max)) = report.buffer_range {
                    println!("        buffer {min}..{max} frames");
                }
            }
        }
        println!();
    }
    println!("Select one with --device followed by an index or part of its name,");
    println!("for example `--device 2` or `--device scarlett`.");
    #[cfg(target_os = "macos")]
    {
        println!();
        println!("On a Mac the built-in speakers are enough to start. `--input` needs");
        println!("microphone permission under System Settings → Privacy & Security.");
    }
    Ok(())
}

/// `kaukonen play`
#[allow(clippy::too_many_arguments)]
pub struct PlayArgs<'a> {
    pub source: &'a str,
    pub device: Option<&'a str>,
    pub in_device: Option<&'a str>,
    pub want_input: bool,
    pub rate: Option<u32>,
    pub block: Option<u32>,
    pub channels: Option<u16>,
    pub routing: Routing,
    pub gain: f32,
    pub sets: Vec<&'a str>,
    pub record: Option<&'a str>,
    pub seconds: Option<f32>,
    pub quiet: bool,
}

pub fn play(args: PlayArgs<'_>) -> Result<(), String> {
    let text = std::fs::read_to_string(args.source).map_err(|e| format!("{}: {e}", args.source))?;
    let mut parsed = patch::parse(&text).map_err(|e| format!("{}: {e}", args.source))?;

    let device = backend::find_device(Direction::Output, args.device)?;
    let output = backend::negotiate(
        &device,
        Direction::Output,
        args.rate.or(Some(parsed.config.sample_rate as u32)),
        args.block,
        args.channels,
    )?;

    // The device owns the clock, so the rack is rebuilt at its rate rather than
    // resampled. Everything in a patch is in seconds and hertz, so it moves
    // cleanly; only the frame rate changes.
    let device_rate = output.sample_rate() as f32;
    let patch_rate = parsed.config.sample_rate;
    parsed.config.sample_rate = device_rate;

    println!("device   {}", output.device_name);
    println!(
        "format   {} Hz, {} channels, {:?}",
        output.sample_rate(),
        output.channels(),
        output.format
    );
    match output.block() {
        Some(frames) => println!(
            "block    {frames} frames ({:.1} ms of latency per buffer)",
            output.latency_ms().unwrap_or(0.0)
        ),
        None => {
            println!("block    device default (pass --block to pin it)");
            println!(
                "note     without a fixed block the status line cannot report deadline use. \
                 CoreAudio often hides the size until the stream is running."
            );
        }
    }
    if let Some(asked) = output.rate_changed_from {
        println!(
            "note     asked for {asked} Hz; the device runs at {}",
            output.sample_rate()
        );
    }
    if let Some(asked) = args.channels {
        if asked as usize != output.channels() {
            println!(
                "note     asked for {asked} channels; the device opened {}",
                output.channels()
            );
        }
    }
    println!("gain     {}", trim_float(args.gain));
    if (patch_rate - device_rate).abs() > 0.5 {
        println!(
            "note     the patch says rate {}; running it at {} to match the device",
            trim_float(patch_rate),
            trim_float(device_rate)
        );
    }
    if output.channels() > 2 {
        println!(
            "routing  left to channel {}, right to channel {} (--out-left/--out-right)",
            args.routing.left + 1,
            args.routing.right + 1
        );
    }

    // Knob overrides from the command line, applied before anything runs.
    for set in &args.sets {
        let (target, value) = set
            .split_once('=')
            .ok_or_else(|| format!("--set wants module.knob=value, got '{set}'"))?;
        let (module, knob) = target
            .rsplit_once('.')
            .ok_or_else(|| format!("--set wants module.knob=value, got '{set}'"))?;
        parsed.knobs.push(patch::KnobDecl {
            module: module.split('[').next().unwrap_or(module).to_string(),
            selector: patch::Selector::All,
            param: knob.to_string(),
            value: value.to_string(),
            line: 0,
        });
    }

    let built = parsed
        .build()
        .map_err(|e| format!("{}: {e}", args.source))?;
    let module_count = built.network.len();
    let cable_count = built.network.cables().len();
    // A directory of every knob, so the console can resolve names without
    // touching the rack once the audio thread owns it.
    let directory = Directory::of(&built);
    let rack = Stream::new(built);
    if !rack.has_output() {
        return Err(format!(
            "{} has no audioout module, so there is nothing to play. Add one and patch \
             something to its left and right jacks.",
            args.source
        ));
    }

    let input = if args.want_input || args.in_device.is_some() {
        if !rack.has_input() {
            return Err("this patch has no audioin module for the input to reach".to_string());
        }
        let in_device = backend::find_device(Direction::Input, args.in_device)?;
        let config = backend::negotiate(
            &in_device,
            Direction::Input,
            Some(output.sample_rate()),
            args.block,
            None,
        )?;
        println!(
            "input    {} at {} Hz, {} channels, taking channel {}",
            config.device_name,
            config.sample_rate(),
            config.channels(),
            args.routing.input + 1
        );
        if config.sample_rate() != output.sample_rate() {
            println!(
                "note     input runs at {} Hz and output at {} Hz; they will drift",
                config.sample_rate(),
                output.sample_rate()
            );
        }
        Some((in_device, config))
    } else {
        None
    };

    args.routing
        .check(output.channels(), input.as_ref().map(|(_, c)| c.channels()))?;

    println!("rack     {module_count} modules, {cable_count} cables");
    // The names are what the console's `set` takes, so say them up front rather
    // than making someone type `modules` to find out what is in the rack.
    println!("knobs on {}", directory.summary().join("  "));
    println!();

    let playback = backend::play(
        rack,
        &device,
        PlayOptions {
            output,
            routing: args.routing,
            gain: args.gain,
            input,
            record: args.record.is_some(),
        },
    )?;
    let _ = playback.send(Control::Fade {
        to: 1.0,
        then_stop: false,
    });

    println!("playing. Type `help` for the controls, `quit` to stop.");
    let started = Instant::now();

    // Draining has to be independent of the console, or an interactive session
    // that sits waiting at the prompt would let the staging buffer fill and
    // lose everything after the first couple of seconds.
    let collected = Arc::new(Mutex::new(Vec::<f32>::new()));
    let drain_stop = Arc::new(AtomicBool::new(false));
    let drain = playback.record.as_ref().map(|stage| {
        let stage = Arc::clone(stage);
        let collected = Arc::clone(&collected);
        let stop = Arc::clone(&drain_stop);
        // A spare buffer with the same capacity, so taking the staged audio is
        // a pointer swap. Copying it out under the lock would make the audio
        // thread's try_lock fail and drop blocks from the recording.
        let mut spare: Vec<f32> = Vec::with_capacity(stage.capacity);
        std::thread::spawn(move || loop {
            let finished = stop.load(Ordering::Relaxed);
            if let Ok(mut staged) = stage.samples.lock() {
                std::mem::swap(&mut *staged, &mut spare);
            }
            if let Ok(mut out) = collected.lock() {
                out.extend_from_slice(&spare);
            }
            spare.clear();
            if finished {
                break;
            }
            std::thread::sleep(DRAIN_INTERVAL);
        })
    });

    let mut last_report = Instant::now();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        if let Some(limit) = args.seconds {
            if started.elapsed().as_secs_f32() >= limit {
                break;
            }
        }
        if !args.quiet && last_report.elapsed() >= REPORT_INTERVAL {
            last_report = Instant::now();
            report(&playback, started);
        }

        // Only block on the console when there is no deadline to keep.
        if args.seconds.is_none() {
            match lines.next() {
                None => break,
                Some(Err(e)) => return Err(format!("cannot read from the console: {e}")),
                Some(Ok(line)) => {
                    if !handle(&line, &playback, &directory, started)? {
                        break;
                    }
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(20));
        }
        if playback.finished.load(Ordering::Relaxed) {
            break;
        }
    }

    println!("stopping.");
    playback.fade_out();
    drain_stop.store(true, Ordering::Relaxed);
    if let Some(handle) = drain {
        let _ = handle.join();
    }
    report(&playback, started);

    if let Some(path) = args.record {
        let samples = collected.lock().map(|s| s.clone()).unwrap_or_default();
        let audio = Audio {
            sample_rate: playback.negotiated.sample_rate(),
            channels: 2,
            samples,
        };
        audio
            .write(path, SampleFormat::Int24)
            .map_err(|e| format!("{path}: {e}"))?;
        println!(
            "recorded {path}  {:.2} s  peak {:.3}",
            audio.seconds(),
            audio.peak()
        );
        let dropped = playback.stats.record_dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            println!(
                "note     {dropped} frames never reached the recording. The output was \
                 unaffected; the writer could not keep up."
            );
        }
    }
    Ok(())
}

fn report(playback: &backend::Playback, started: Instant) {
    let stats = &playback.stats;
    let blocks = stats.blocks.load(Ordering::Relaxed);
    let late = stats.late.load(Ordering::Relaxed);
    let worst = stats.worst_micros.load(Ordering::Relaxed);
    let peak = stats.take_peak();
    let deadline = playback
        .negotiated
        .block()
        .map(|frames| frames as f32 / playback.negotiated.sample_rate() as f32 * 1e6);

    let mut line = format!(
        "{:>6.1}s  peak {:.3}  blocks {blocks}  late {late}  worst {worst} us",
        started.elapsed().as_secs_f32(),
        peak
    );
    if let Some(deadline) = deadline {
        line.push_str(&format!(
            "  of {:.0} us ({:.0}% used)",
            deadline,
            worst as f32 / deadline * 100.0
        ));
    }
    let starved = stats.input_starved.load(Ordering::Relaxed);
    if starved > 0 {
        line.push_str(&format!("  input short {starved}"));
    }
    println!("{line}");
    if late > 0 {
        println!(
            "         {late} blocks missed their deadline. Raise --block, lower oversample, \
             or thin the patch."
        );
    }
}

/// One module of the running rack, as the console knows it.
///
/// Taken before the rack moves to the audio thread. A module's kind and knob
/// table are `'static`, so the console can keep describing it afterwards — and
/// a knob name taken from here can be sent to the audio thread without
/// allocating anything.
struct Entry {
    label: String,
    index: usize,
    kind: &'static str,
    params: &'static [param::ParamSpec],
}

/// Every knob in the running rack, resolvable by the name the patch used.
struct Directory {
    entries: Vec<Entry>,
}

impl Directory {
    fn of(built: &kaukonen::BuiltPatch) -> Self {
        Self {
            entries: (0..built.network.len())
                .map(|index| {
                    let module = built.network.get(ModuleId(index));
                    Entry {
                        label: built.network.label(ModuleId(index)).to_string(),
                        index,
                        kind: module.kind_name(),
                        params: module.params(),
                    }
                })
                .collect(),
        }
    }

    /// One name per declared module, with a bank shown once rather than 64 times.
    fn summary(&self) -> Vec<String> {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for entry in &self.entries {
            let bank = entry
                .label
                .split('[')
                .next()
                .unwrap_or(&entry.label)
                .to_string();
            match seen.iter_mut().find(|(name, _)| *name == bank) {
                Some((_, count)) => *count += 1,
                None => seen.push((bank, 1)),
            }
        }
        seen.into_iter()
            .map(|(name, count)| {
                if count == 1 {
                    name
                } else {
                    format!("{name}[{count}]")
                }
            })
            .collect()
    }

    /// Instances named by `cells[2]`, or by `cells` meaning the whole bank.
    fn instances(&self, label: &str) -> Vec<&Entry> {
        let exact: Vec<&Entry> = self.entries.iter().filter(|e| e.label == label).collect();
        if !exact.is_empty() {
            return exact;
        }
        let prefix = format!("{label}[");
        self.entries
            .iter()
            .filter(|e| e.label.starts_with(&prefix))
            .collect()
    }

    /// Resolve `cells[2].bias` to the instances it names and the knob's spec.
    fn resolve(&self, target: &str) -> Result<(Vec<usize>, &'static param::ParamSpec), String> {
        let (label, knob) = target
            .rsplit_once('.')
            .ok_or_else(|| format!("'{target}' should look like module.knob"))?;
        let matched = self.instances(label);
        let first = *matched
            .first()
            .ok_or_else(|| format!("no module called '{label}'"))?;
        let spec = first
            .params
            .iter()
            .find(|s| s.name == knob)
            .ok_or_else(|| {
                format!(
                    "a {} has no knob '{knob}'. Knobs: {}",
                    first.kind,
                    first
                        .params
                        .iter()
                        .map(|s| s.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        Ok((matched.iter().map(|e| e.index).collect(), spec))
    }
}

/// Handle one console line. Returns false to stop playing.
fn handle(
    line: &str,
    playback: &backend::Playback,
    directory: &Directory,
    started: Instant,
) -> Result<bool, String> {
    let mut words = line.split_whitespace();
    let Some(command) = words.next() else {
        return Ok(true);
    };
    let rest: Vec<&str> = words.collect();

    match command {
        "quit" | "q" | "exit" | "stop" => return Ok(false),
        "help" | "?" => {
            println!("  set <module>.<knob> <value>   turn a knob while it plays");
            println!("  knobs <module>                list a module's knobs and ranges");
            println!("  modules                       list what is in the rack");
            println!("  gain <value>                  master output trim");
            println!("  mute / unmute                 fade out and back in");
            println!("  stats                         print stream health now");
            println!("  quit                          fade out and stop");
        }
        "modules" => {
            for entry in &directory.entries {
                println!("  {:<16} {}", entry.label, entry.kind);
            }
        }
        "knobs" => {
            let label = rest.first().ok_or("knobs needs a module name")?;
            let instances = directory.instances(label);
            let entry = instances
                .first()
                .ok_or_else(|| format!("no module called '{label}'"))?;
            println!("  {} ({})", entry.label, entry.kind);
            for spec in entry.params {
                println!(
                    "    {:<16} {:<24} default {}",
                    spec.name,
                    match spec.choices() {
                        Some(choices) => choices.join("|"),
                        None => format!("{}..{}", trim_float(spec.min), trim_float(spec.max)),
                    },
                    spec.format(spec.default)
                );
            }
        }
        "set" => {
            if rest.len() != 2 {
                return Err("set wants <module>.<knob> <value>".to_string());
            }
            let (modules, spec) = directory.resolve(rest[0])?;
            let value = spec
                .parse(rest[1])
                .map_err(|e| e.to_string())
                .and_then(|v| param::checked(spec, v).map_err(|e| e.to_string()))?;
            for module in &modules {
                playback.send(Control::Knob {
                    module: *module,
                    knob: spec.name,
                    value,
                })?;
            }
            println!(
                "  {} = {}{}",
                rest[0],
                spec.format(value),
                if modules.len() == 1 {
                    String::new()
                } else {
                    format!("  ({} instances)", modules.len())
                }
            );
        }
        "gain" => {
            let value: f32 = rest
                .first()
                .ok_or("gain needs a value")?
                .parse()
                .map_err(|_| "gain wants a number".to_string())?;
            playback.send(Control::Gain(value.clamp(0.0, 8.0)))?;
            println!("  gain = {}", trim_float(value));
        }
        "mute" => {
            playback.send(Control::Fade {
                to: 0.0,
                then_stop: false,
            })?;
            println!("  muted");
        }
        "unmute" => {
            playback.send(Control::Fade {
                to: 1.0,
                then_stop: false,
            })?;
            println!("  unmuted");
        }
        "stats" => report(playback, started),
        other => println!("  unknown command '{other}'. Type `help`."),
    }
    let _ = std::io::stdout().flush();
    Ok(true)
}

fn or_none(text: &str) -> String {
    if text.is_empty() {
        "?".to_string()
    } else {
        text.to_string()
    }
}
