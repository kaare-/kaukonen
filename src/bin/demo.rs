//! Tiny recurrent patch: source → TC-3 → neurons → RC-1s → TC-3, plus feedback.

use kaukonen::neuron::NeuronParams;
use kaukonen::nonlinearity::Nonlinearity;
use kaukonen::plasticity::{Plasticity, Rule};
use kaukonen::rc1::{count_bursts, count_crossings, Rc1Params};
use kaukonen::{
    Engine, Network, Neuron, Rc1, Recorder, SimConfig, Source, Tc3, Tc3Params, DEFAULT_DT,
};

fn main() {
    let mut net = Network::new();

    let src = net.add("src", Source::pulse(0.7, 0.08, 2.2, 0.15));
    let tc3 = net.add(
        "tc3",
        Tc3::new(Tc3Params {
            natural_decay: 0.015,
            activity_gain: 0.004,
            // Without homeostasis, Hebbian growth pins every route at w_max
            // and ACTIVITY sits on the rail, which tells you nothing.
            plasticity: Plasticity {
                rule: Rule::Oja,
                gamma: 0.6,
                scaling: 0.4,
                target_norm: 1.2,
                ..Plasticity::default()
            },
            ..Tc3Params::default()
        }),
    );
    let n0 = net.add(
        "neuron0",
        Neuron::new(NeuronParams {
            weights: [0.9, 0.35, 0.0, 0.0],
            bias: 0.15,
            leak: 7.0,
            gain: 4.0,
            nonlinearity: Nonlinearity::Tanh { response: 0.7 },
            noise: 0.0,
        }),
    );
    let n1 = net.add(
        "neuron1",
        Neuron::new(NeuronParams {
            weights: [0.8, 0.25, 0.0, 0.0],
            bias: -0.05,
            leak: 6.0,
            gain: 4.5,
            nonlinearity: Nonlinearity::AsymTanh { pos: 0.9, neg: 0.5 },
            noise: 0.0,
        }),
    );
    let rc0 = net.add("rc1_0", Rc1::new(Rc1Params::burst()));
    let rc1 = net.add(
        "rc1_1",
        Rc1::new(Rc1Params {
            drive_gain: 0.35,
            ..Rc1Params::tonic()
        }),
    );

    net.patch(src, "out", tc3, "a").unwrap();
    net.patch(src, "out", tc3, "learn").unwrap();
    net.patch(tc3, "a", n0, "in0").unwrap();
    net.patch(tc3, "b", n1, "in0").unwrap();
    net.patch(n0, "out", rc0, "in").unwrap();
    net.patch(n1, "out", rc1, "in").unwrap();
    net.patch(rc0, "out", tc3, "b").unwrap();
    net.patch(rc1, "out", tc3, "c").unwrap();
    net.patch(rc0, "out", n0, "in1").unwrap();
    net.patch(tc3, "activity", n1, "in1").unwrap();
    net.patch(rc1, "out", tc3, "forget").unwrap();

    let channels = [
        "src.out",
        "neuron0.v",
        "neuron1.v",
        "rc1_0.v",
        "rc1_0.s",
        "rc1_1.v",
        "rc1_1.s",
        "tc3.w00",
        "tc3.w11",
        "tc3.activity",
    ];
    // 0.5 ms * 4 = 2 ms samples keep the HTML light.
    let mut rec = Recorder::with_stride(channels, 4);
    let mut engine = Engine::new(
        net,
        SimConfig {
            dt: DEFAULT_DT,
            rail: 10.0,
        },
    );
    engine.run(5.0, &mut rec);

    rec.write_csv("out/demo_trace.csv").expect("write csv");
    rec.write_html(
        "out/demo_trace.html",
        &[
            "neuron0.v",
            "rc1_0.v",
            "rc1_0.s",
            "rc1_1.v",
            "tc3.w00",
            "tc3.activity",
        ],
    )
    .expect("write html");

    println!("Kaukonen demo — 5.0 s @ dt = {DEFAULT_DT} s");
    println!("wrote out/demo_trace.csv and out/demo_trace.html");
    for name in [
        "neuron0.v",
        "neuron1.v",
        "rc1_0.v",
        "rc1_0.s",
        "rc1_1.v",
        "tc3.w00",
        "tc3.activity",
    ] {
        if let Some((min, max)) = rec.min_max(name) {
            println!("  {name:16}  min={min:8.3}  max={max:8.3}");
        }
    }

    if let Some(v) = rec.channel("rc1_0.v") {
        let spikes = count_crossings(v, 0.0);
        let mut times = Vec::new();
        let mut t = 0.0;
        let dt_rec = DEFAULT_DT * 4.0;
        let mut prev = v[0];
        for &x in v {
            if prev < 0.0 && x >= 0.0 {
                times.push(t);
            }
            prev = x;
            t += dt_rec;
        }
        let bursts = count_bursts(&times, 0.25);
        println!("  rc1_0 spikes={spikes} bursts≈{bursts}");
    }
    if let Some(v) = rec.channel("rc1_1.v") {
        println!("  rc1_1 spikes={}", count_crossings(v, 0.0));
    }
    if let (Some(w0), Some(w1)) = (
        rec.channel("tc3.w00").and_then(|c| c.first().copied()),
        rec.channel("tc3.w00").and_then(|c| c.last().copied()),
    ) {
        println!(
            "  tc3.w00 {w0:.3} → {w1:.3}  (moved {})",
            (w1 - w0).abs() > 0.01
        );
    }
}
