# The `.kpatch` format

A patch file is the interface to the simulator. It is line oriented, parsed by
hand, and round-trips: loading a patch and serializing it back produces an
equivalent file.

Every line is a directive followed by whitespace-separated arguments. Anything
after `#` is a comment. Blank lines are ignored. Errors carry the line number
and list what would have been legal instead.

```
# kaukonen patch v1
rate 48000
oversample 2
seconds 6
seed 12
mismatch 0.04

module cells rc1[3] preset=burst rate=150 noise=0.01
module brain tc3    rule=oja scaling=0.4 target_norm=1.2

knob cells[2].bias 2.4

patch cells[*].out   -> brain.a
patch brain.activity -> brain.learn

record cells[*].v brain.norm brain.activity
```

## Simulation directives

| directive | meaning |
| --- | --- |
| `rate <hz>` | Frames per second of simulated time. A frame is one sample as the outside world sees it. |
| `dt <seconds>` | The integration step, as an alternative to `rate`. Mutually exclusive with it. |
| `oversample <n>` | Integration steps per frame. Raises accuracy without raising the frame rate. |
| `rail <volts>` | Symmetric clamp on every jack, `±rail`. Defaults to 10 V. |
| `seed <integer>` | Seeds component tolerance and every noise stream, so a run repeats exactly. |
| `mismatch <fraction>` | Spread between nominally identical modules. `0` is a mathematically ideal rack. |
| `seconds <seconds>` | Default run length, which `--seconds` overrides. |

`dt` and `rate` are resolved after the whole file is read, so neither depends on
where `oversample` appears. Setting both is an error, because the two would
disagree about what a frame is.

The default is 2 kHz frames with no oversampling, which is the timescale the
neural modules were designed around. Audio rate is something a patch opts into.

## Modules

```
module <name> <kind>[<count>] [preset=<name>] [knob=value ...]
```

`name` is how the rest of the file refers to it. `kind` is one of the module
kinds — see [`modules.md`](modules.md), or run `kaukonen modules`. The optional
`[count]` declares a bank of that many instances, which is how a rack of
hundreds stays writable:

```
module cells rc1[64] preset=burst noise=0.02
```

Each instance gets its own tolerance draw and its own noise stream, derived from
the patch `seed` and its position in the rack. Sixty-four cells with identical
knobs are therefore sixty-four different cells.

`preset=` selects a factory starting point before any knobs are applied. Only
`rc1` has presets: `subthreshold`, `pulse`, `tonic` and `burst`.

Knob values are numbers, or the name of a choice for knobs that have them
(`rule=oja`, `shape=asym_tanh`, `response=exponential`). A value outside a
knob's range is an error rather than being silently clamped, so a typo is
caught rather than half-obeyed.

## Knob overrides

```
knob <name>[<selector>].<knob> <value>
```

Sets one knob after the `module` line, which is how a single instance of a bank
is made different on purpose rather than by tolerance:

```
knob cells[2].bias 2.4
knob cells[*].rate 90
```

## Cables

```
patch <name>[<selector>].<out jack> -> <name>[<selector>].<in jack>
```

A cable carries a voltage from one output jack to one input jack. Several cables
into the same input sum, exactly as they do on a panel. Cycles are allowed and
expected.

Selectors decide which instances a reference means:

| selector | meaning |
| --- | --- |
| *(none)* | The only instance. An error on a bank, which is what you want — it makes an ambiguous patch impossible. |
| `[3]` | Instance 3. |
| `[*]` | Every instance. |
| `[i]` | Pair the two sides index for index. |

Anything other than `[i]` takes the cross product, so `cells[*].out -> brain.a`
fans a whole bank into one jack and sums it, and `cells[*].out -> layer[*].in0`
connects every cell to every neuron. `[i]` pairs them instead:

```
patch layer[i].out -> cells[i].in
```

Index-matching requires equal counts, or one side having a single instance, in
which case it fans out.

## Desk

```
desk <module[.knob]> [<module[.knob]> ...]
```

Pins knobs at the top of the live page so a large rack still has a playable
surface. A bank name without a knob pins every knob on that bank. A slider
on the desk is the same control as `set split.cutoff` at the console.

```
desk split.cutoff split.q band.cutoff band.q
desk fbank.tilt verb.mix
desk miclim outcomp.threshold outcomp.ratio
```

Unknown modules or knobs are an error. Several `desk` lines append.

## Recording

```
record <channel> [<channel> ...]
record *
```

A channel is `label.field`, where `field` is an internal state or an output
jack. Internal states win a name collision. `record *` records every internal
state of every instance, which is the "record everything" option.

```
record cells[*].v brain.w00 brain.norm
```

Instance labels are `cells[0]`, `cells[1]` and so on for a bank, and just
`brain` for a single instance.

## Feedback and delay

Modules are evaluated in topological order, so a chain of four modules resolves
within a single step. Only cables that close a cycle read the previous step's
value, because a loop has to be broken somewhere. `kaukonen inspect` lists
exactly which cables those are.
