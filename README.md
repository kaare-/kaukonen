# Kaukonen

Continuous-time simulator for patchable analog Eurorack neural modules.

This is a phenomenological model of physical / analog hardware — voltage, leak, integration, excitability, local plasticity, and feedback. It is **not** a machine-learning framework. There is no backpropagation and no global loss.

## Modules

- **Neuron** — four weighted inputs, bias, leaky membrane, smooth saturation
- **RC-1** — three-state excitable cell (`v`, recovery `r`, slow adaptation `s`)
- **TC-3** — 3×3 leaky analog weights with local Hebbian plasticity and an `activity` output
- **Source** — test-only DC / sine / pulse stimulus

## Run

```bash
cargo test
cargo run --bin demo
```

The demo builds a small recurrent patch (source → TC-3 → neurons → RC-1s → TC-3, plus feedback), runs 5 simulated seconds at `dt = 0.5 ms`, and writes:

- `out/demo_trace.csv`
- `out/demo_trace.html` — open in a browser

## Architecture

Params (knobs) stay separate from State (capacitor voltages, weights, recovery). The network is a graph of patch cables. Multiple cables into one jack sum. Feedback uses the previous timestep’s outputs.
