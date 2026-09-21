# Kaukonen

Continuous-time simulator for patchable analog Eurorack neural modules.

This is a phenomenological model of physical hardware — voltage, leak,
integration, excitability, local plasticity, component tolerance and feedback.
It is **not** a machine-learning framework: there is no backpropagation, no
global loss, and nothing in a plasticity rule can see anything but its own
route. It is also not a synthesizer, though it can be heard.

No third-party dependencies, apart from cpal behind the optional `realtime`
feature, which is the only thing that talks to audio hardware.

## The neural modules

Three module kinds do the interesting work.

- **Neuron** — four weighted inputs, bias, leaky membrane, smooth saturation.
  A continuous-time recurrent neuron rather than an artificial-neural-network
  one: the state persists, leaks, and accepts feedback.
- **RC-1** — a three-state excitable cell (membrane `v`, fast recovery `r`, slow
  adaptation `s`). Not an integrate-and-fire comparator: the threshold region
  stays continuous, so one set of knobs gives isolated pulses, another a tonic
  train, another bursts, and another genuine chaos.
- **TC-3** — three inputs to three outputs through nine routes, each holding its
  own leaky analog memory. Six local plasticity rules, global LEARN and FORGET
  inputs, and an ACTIVITY output reporting how fast its own memory is moving,
  so plasticity becomes a signal you can patch.

Around them is a **utility rack** — `vco`, `vcf`, `vca`, `mix`, `env`, `noise`
and a test `source` — plus `audioin` and `audioout`. These are ordinary analog
building blocks with nothing to discover; they exist so the neural modules can
be heard, scaled, gated and mixed.

Full knob reference: [`docs/modules.md`](docs/modules.md), or run
`kaukonen modules`.

## Patching

A patch is a text file. Cables carry voltages, several into one input sum, and
cycles are expected.

```
rate 48000
oversample 2
seconds 6
seed 12
mismatch 0.04

module cells rc1[3] preset=burst rate=150 noise=0.01
module brain tc3    rule=oja scaling=0.4 target_norm=1.2
module gate  env    mode=ad attack=0.004 decay=0.22
module osc   vco    tune=110 fm_depth=30
module filt  vcf    cutoff=260 resonance=1.05
module amp   vca    response=exponential
module dac   audioout

patch cells[*].out   -> brain.a
patch brain.activity -> brain.learn
patch cells[0].out   -> gate.gate
patch osc.saw        -> filt.in
patch filt.out       -> amp.in
patch gate.out       -> amp.cv
patch amp.out        -> dac.left

record cells[*].v brain.norm brain.activity
```

`module cells rc1[64]` declares sixty-four instances; `[*]`, `[3]` and `[i]`
select them in a cable. The full grammar is in
[`docs/patch-format.md`](docs/patch-format.md).

## Run

```bash
cargo test
cargo run --bin demo                      # the shipped demonstration rack

cargo run --bin kaukonen -- inspect --patch patches/voice.kpatch
cargo run --bin kaukonen -- render  --patch patches/voice.kpatch \
    --out out/voice.wav --html out/voice.html --stride 12
```

`render` writes a WAV, a CSV trace and a self-contained HTML page. Open the page
in a browser: it has stacked traces with shared zoom and pan, phase portraits, a
spike raster, an interval histogram, a spectrum, a diagram of the patch, and the
rendered audio embedded so you can play it.

| command | what it is for |
| --- | --- |
| `render` | Run a patch and write audio, a trace and a scope page. |
| `inspect` | Modules, knobs, cables, feedback loops, unpatched inputs. |
| `modules` | The catalogue of kinds and every knob. `--markdown` for docs. |
| `sweep` | Render a bank of variations of one knob along its own taper. |
| `bench` | Wall time as a factor of real time, and worst-case streaming headroom. |
| `probe` | Nudge a knob and report whether the two runs diverge. |
| `devices` | What audio hardware is available, and what it can do. |
| `play` | Play through an interface, with every knob adjustable live. |

## Patches

| patch | what it shows |
| --- | --- |
| [`demo.kpatch`](patches/demo.kpatch) | The original small recurrent rack: stimulus, TC-3, two neurons, two cells, feedback. |
| [`voice.kpatch`](patches/voice.kpatch) | A percussive voice whose rhythm is RC-1 bursting. |
| [`chaos.kpatch`](patches/chaos.kpatch) | Two cells cross-coupled in their chaotic window. `probe` this one. |
| [`bank64.kpatch`](patches/bank64.kpatch) | Sixty-four mismatched cells weakly coupled through a changing connectome. |
| [`listen.kpatch`](patches/listen.kpatch) | Recorded sound teaching a connectome, which colours it back. Needs `--in`. |

## Sound

`audioin` plays a WAV and offers an envelope follower alongside it, so recorded
sound can drive the neural modules and their plasticity. `audioout` is the
converter: it emits at its own rate, filters just below that rate's Nyquist so
an oversampled rack decimates cleanly, and soft-clips only the peaks.

```bash
cargo run --bin kaukonen -- render --patch patches/listen.kpatch \
    --in recording.wav --out out/listen.wav --html out/listen.html
```

The default timestep stays at 0.5 ms, the timescale the neural modules were
designed around. Audio rate is something a patch opts into with `rate 48000`,
and each module has a `rate` knob spanning biological to audible, so one RC-1
can burst over seconds while another oscillates at 300 Hz in the same rack.

## Playing it live

```bash
cargo build --release --features realtime

kaukonen devices                                   # find the interface
kaukonen play --patch patches/voice.kpatch \
    --device scarlett --block 256 --gain 0.3
```

`play` is a console: every knob in the rack is reachable by the name the patch
gave it while the sound is running.

```
> set filt.cutoff 4000
> set cells.rate 400          # a bank name sets every instance
> gain 0.3
> quit
```

The device owns the clock, so the rack is rebuilt at the device's rate rather
than resampled. `--block` pins the buffer size and reports the resulting latency,
`--out-left`/`--out-right` route onto a multi-output interface, `--input` opens
the capture side, and `--record` captures exactly what was sent. A status line
reports peak level and how many buffers missed their deadline, which is the
number to watch when tuning block size.

The simulator itself has no dependencies; hardware access is the one thing that
needs one, so it sits behind the `realtime` feature. Details, including what has
and has not been tested against real hardware, in
[`docs/realtime.md`](docs/realtime.md).

## Physical imperfection

Off by default, so an ideal rack is exactly reproducible.

- **Component mismatch** — `mismatch 0.04` gives every instance its own draw on
  capacitance, recovery rate, threshold, saturation slope, even the end stops of
  a TC-3 memory. Nominally identical modules are not identical.
- **Thermal noise** — a `noise` knob defined as the RMS of the fluctuation it
  produces, not as a raw diffusion coefficient, so it means the same thing at
  any timestep.
- **Slow drift** — an RC-1's threshold wanders on its own adaptation timescale;
  a TC-3's weights wander against their own decay.
- **Imperfect symmetry** — mismatch reaches the saturation curve, so no two
  neurons clip identically.

Everything is seeded from the patch `seed` and the module's position in the
rack, so a noisy run repeats exactly and no two instances share a stream. That
matters: comparing two nearly-identical runs is how `probe` detects chaos.

## Architecture

Params (knobs) stay separate from state (capacitor voltages, weights, recovery),
and both are described as data — `ParamSpec` carries a knob's range, unit,
default and taper, which is what lets a patch file, the CLI and component
tolerance all reach the same controls.

The network is a graph of patch cables. Modules are evaluated in topological
order, so a feed-forward chain resolves within one step; only cables that close
a cycle read the previous step's value, because a loop has to be broken
somewhere. `inspect` names them.

Roughly, 2000 cells with mismatch, noise and drift run at about twice real time
at 2 kHz. Audio rate with oversampling is slower than real time for anything
large; `bench` tells you which side of the line a patch is on.
