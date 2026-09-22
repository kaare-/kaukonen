use kaukonen::module::AnalogModule;
use kaukonen::neuron::{Neuron, NeuronParams};
use kaukonen::rc1::{Rc1, Rc1Params};
use kaukonen::tc3::{Tc3, Tc3Params};
use kaukonen::util::{Cochlea, Comp, Fbank, Svf, Vca, Vcf, Vco, Verb};
use kaukonen::{patch, Engine};
use std::time::Instant;

fn time_module(name: &str, module: &mut impl AnalogModule, inputs: &[f32], steps: usize) {
    let dt = 1.0 / 48_000.0;
    // Warm up, then measure.
    for _ in 0..1000 {
        module.step(inputs, dt);
    }
    let start = Instant::now();
    let mut sink = 0.0f32;
    for _ in 0..steps {
        sink += module.step(inputs, dt)[0];
    }
    let ns = start.elapsed().as_secs_f64() * 1e9 / steps as f64;
    println!("  {name:<10} {ns:7.1} ns/step   (sink {sink:.3})");
}

fn main() {
    let steps = 2_000_000;
    println!("per-module cost, one step each:");
    time_module(
        "neuron",
        &mut Neuron::new(NeuronParams::default()),
        &[1.0; 4],
        steps,
    );
    time_module("rc1", &mut Rc1::new(Rc1Params::burst()), &[1.0], steps);
    time_module("tc3", &mut Tc3::new(Tc3Params::default()), &[1.0; 5], steps);
    time_module("vco", &mut Vco::new(), &[0.0; 4], steps);
    time_module("vcf", &mut Vcf::new(), &[1.0, 0.0, 0.0], steps);
    time_module("svf", &mut Svf::new(), &[1.0, 0.0, 0.0], steps);
    time_module("comp", &mut Comp::new(), &[1.0], steps);
    time_module("fbank", &mut Fbank::new(), &[1.0, 0.0, 0.0], steps);
    time_module("verb", &mut Verb::new(), &[1.0, 0.0, 0.0], steps);
    time_module("cochlea", &mut Cochlea::new(), &[1.0, 0.0], steps);
    time_module("vca", &mut Vca::new(), &[1.0, 2.0], steps);

    println!("\nwhole-rack cost:");
    for (name, text) in [
        ("64 rc1", "rate 48000\nmodule c rc1[64] preset=tonic\n"),
        ("64 neuron", "rate 48000\nmodule c neuron[64]\n"),
        (
            "64 rc1 + fan-in",
            "rate 48000\nmodule c rc1[64] preset=tonic\nmodule b tc3\npatch c[*].out -> b.a\n",
        ),
    ] {
        let built = patch::parse(text).unwrap().build().unwrap();
        let modules = built.network.len();
        let mut engine = Engine::new(built.network, built.config);
        for _ in 0..2000 {
            engine.step();
        }
        let n = 100_000;
        let start = Instant::now();
        for _ in 0..n {
            engine.step();
        }
        let total = start.elapsed().as_secs_f64() * 1e9 / n as f64;
        println!(
            "  {name:<16} {total:8.0} ns/step  {:6.1} ns/module-step",
            total / modules as f64
        );
    }
}
