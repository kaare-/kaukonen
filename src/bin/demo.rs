//! The demonstration rack, run through the same path as the command line.
//!
//! The patch itself lives in `patches/demo.kpatch` and is compiled in, so this
//! binary is a shortcut rather than a second copy of the wiring. For anything
//! beyond the default run, use `kaukonen render --patch patches/demo.kpatch`.

use kaukonen::patch;
use kaukonen::rc1::{count_bursts, count_crossings};
use kaukonen::render::{render, RenderOptions};
use kaukonen::scope::Scope;

const PATCH: &str = include_str!("../../patches/demo.kpatch");

/// Keep the HTML light: one sample every 4 frames is 2 ms at the default rate.
const STRIDE: usize = 4;

fn main() {
    let parsed = patch::parse(PATCH).expect("the compiled-in patch should parse");
    let dt = parsed.config.dt();
    let built = parsed.build().expect("the compiled-in patch should build");
    let (engine, recorder, report) = render(
        built,
        &RenderOptions {
            stride: STRIDE,
            ..RenderOptions::default()
        },
    );

    println!("Kaukonen demo — {:.1} s @ dt = {dt} s", report.seconds);
    recorder.write_csv("out/demo_trace.csv").expect("write csv");
    Scope::new("Kaukonen demo", &recorder)
        .with_patch(&engine.network, engine.schedule())
        .write("out/demo_trace.html")
        .expect("write html");
    println!("wrote out/demo_trace.csv and out/demo_trace.html");

    for name in &recorder.names {
        if let Some((min, max)) = recorder.min_max(name) {
            println!("  {name:16}  min={min:8.3}  max={max:8.3}");
        }
    }

    let sample_period = dt * STRIDE as f32;
    if let Some(trace) = recorder.channel("rc1_0.v") {
        let spikes = count_crossings(trace, 0.0);
        let times: Vec<f32> = trace
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
            .map(|(i, _)| i as f32 * sample_period)
            .collect();
        println!(
            "  rc1_0 spikes={spikes} bursts≈{}",
            count_bursts(&times, 0.25)
        );
    }
    if let Some(trace) = recorder.channel("rc1_1.v") {
        println!("  rc1_1 spikes={}", count_crossings(trace, 0.0));
    }
    if let Some(weights) = recorder.channel("tc3.w00") {
        if let (Some(first), Some(last)) = (weights.first(), weights.last()) {
            println!(
                "  tc3.w00 {first:.3} → {last:.3}  (moved {})",
                (last - first).abs() > 0.01
            );
        }
    }
}
