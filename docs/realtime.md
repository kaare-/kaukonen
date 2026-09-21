# Playing a patch through an audio interface

```bash
cargo build --release --features realtime

./target/release/kaukonen devices
./target/release/kaukonen play --patch patches/live.kpatch --device scarlett --block 256 --gain 0.4
```

The simulator itself has no dependencies. Talking to hardware needs one, so it
sits behind the `realtime` feature and nothing else in the crate touches it.
Without the feature, `devices` and `play` tell you what to rebuild with.

On Linux the build needs ALSA's headers, which are not installed by default:

```bash
sudo apt install libasound2-dev     # Debian, Ubuntu
sudo dnf install alsa-lib-devel     # Fedora
```

macOS and Windows need nothing extra; they use CoreAudio and WASAPI.

On a Mac laptop, this is a first session that should make sound:

```bash
xcode-select --install                          # if this Mac has never compiled
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

git clone -b cursor/data-driven-patchbay-knobs-audio-c53a https://github.com/kaare-/kaukonen.git
cd kaukonen
cargo build --release --features realtime
./target/release/kaukonen devices
./target/release/kaukonen play --patch patches/live.kpatch --block 256 --gain 0.3
```

The default output is the built-in speakers. `--device "MacBook"` (or whatever
`devices` printed) pins them; `--device scarlett` is enough for a Focusrite
once its driver is installed. Start with `--gain 0.3` — that is also the
default — and watch `peak` in the status line before turning it up.

Opening the microphone needs a permission: System Settings → Privacy &
Security → Microphone, and allow the terminal (or iTerm) that launched
`kaukonen`. That happens for `--input` at launch, or the first time
Capture is turned on from the page. Without either, nothing asks.

Bluetooth headphones add tens of milliseconds and often hide their buffer
size (`devices` then prints no range, and the status line cannot report
deadline use). Pass `--block 256` on wired or built-in output; raise it to
512 or 1024 if `late` starts climbing. An Aggregate Device in Audio MIDI
Setup is how to combine a USB interface with the built-in mic if they
are not the same box.

CoreAudio frequently reports `BufferSize::Unknown` until the stream is
running. That is why `play` without `--block` says it cannot report
deadline use — pin the block so the status line has a number to compare
against.

[`patches/live.kpatch`](../patches/live.kpatch) is written for this: the sound
comes entirely from three excitable cells running at an audible rate, and its
comments list the knobs worth turning first. Not every patch is playable —
`play` needs an `audioout` module, and says so if there isn't one.

## Finding the interface

```
$ kaukonen devices
output devices (3)
  [0] default  (default)
        channels 2   rates 44100 48000 96000   formats F32 I16
        buffer 16..8192 frames
  [1] Scarlett 4i4 USB
        channels 2/4   rates 44100 48000 88200 96000 176400 192000   formats F32 I16
        buffer 16..8192 frames
```

`--device` takes an index or any part of a name, so `--device scarlett` is
enough. If the fragment matches more than one device it says so rather than
guessing. `--in-device` selects the capture side separately, since the two are
not always the same device.

On Linux, ALSA prints complaints on stderr about every backend it cannot reach —
JACK not running, no `/dev/dsp`, PulseAudio refused. They are noise from
alsa-lib, not from this. Redirect with `2>/dev/null` if they get in the way.

## Rate, block size and latency

The device owns the clock, so the rack is rebuilt at the device's rate rather
than resampled. Everything in a patch is specified in seconds and hertz, so it
moves cleanly; only the frame rate changes. A patch saying `rate 48000` played on
a 44.1 kHz device runs at 44.1 kHz, and `play` says so.

```
device   Scarlett 4i4 USB
format   48000 Hz, 2 channels, F32
block    256 frames (5.3 ms of latency per buffer)
```

- `--rate 96000` asks for a rate. If the device refuses, the nearest supported
  rate is used and reported.
- `--block 256` pins the buffer size, clamped to what the device allows. Smaller
  is lower latency and less headroom. Without it the device picks.
- `--channels 4` asks for a channel count, for an interface with more than two
  outputs.

Only f32 and i16 are handled. Devices — especially behind ALSA's `plug` layer —
advertise far more; f32 is preferred, then i16.

## Routing on a multi-output interface

Channels are numbered from 1, as they are on the front of the box:

```bash
kaukonen play --patch p.kpatch --device scarlett --out-left 3 --out-right 4
kaukonen play --patch p.kpatch --device scarlett --input --in-channel 2
```

Everything not routed is left silent. Input is taken from one channel, summed to
mono, and handed to every `audioin` in the patch.

## Turning knobs while it plays

`play` opens `http://127.0.0.1:7420` and the same console as before. The page
is the knobs — sliders honour each taper, a bank card sets every instance,
gain / mute / quit sit at the top, and the peak meter is the same number
the status line prints. Under that, Capture picks a microphone and
channel and shows the incoming level. The patch needs an `audioin`
(`listen.kpatch` has one; `live.kpatch` does not). Sound still leaves
through the interface; the browser never sees audio.

`--port` picks the port, `--no-browser` skips opening one, `--no-web` is
the console only. `kaukonen serve --patch patches/live.kpatch` is the same
page without a device, so the layout can be checked on a machine with no
sound card.

```
playing. Type `help` for the controls, `quit` to stop.
panel    http://127.0.0.1:7420
> modules
  cells[0]         rc1
  cells[1]         rc1
  ...
> knobs filt
  filt (vcf)
    cutoff           5..18000                 default 800 Hz
    resonance        0..1.3                   default 0.3
    ...
> set filt.cutoff 4000
  filt.cutoff = 4000 Hz
> set cells.rate 400
  cells.rate = 400  (3 instances)
> gain 0.3
> mute
> quit
```

| command | what it does |
| --- | --- |
| `set <module>.<knob> <value>` | Turn a knob. A bank name without an index sets every instance. |
| `knobs <module>` | That module's knobs, with ranges and defaults. |
| `modules` | What is in the rack. |
| `gain <value>` | Master output trim, on top of the patch's own levels. |
| `mute` / `unmute` | Fade out and back in. |
| `input on` / `input off` | Open or close the microphone. Optional device name. |
| `input device <name>` | Pick a capture device (index or part of the name). |
| `input channel <n>` | Pick a capture channel, numbered from 1. |
| `stats` | Stream health now. |
| `quit` | Fade out and stop. |

Values are validated against the knob's range before being sent, so a typo is
refused rather than doing something surprising mid-performance. Enum knobs take
their names: `set amp.response exponential`.

`--set module.knob=value` does the same thing at launch, repeatably, which is
how to try a variation without editing the patch.

## Is it keeping up?

While playing, a line every couple of seconds:

```
   6.0s  peak 0.412  blocks 1126  late 0  worst 476 us  of 5333 us (9% used)
```

`late` counts buffers that took longer than their own deadline; each one is a
dropout waiting to happen. `worst` against the deadline is the number to watch —
9% used is comfortable, 80% is not. If `late` starts climbing, raise `--block`,
lower the patch's `oversample`, or thin the rack.

`bench --block 256` gives the same figures without touching hardware, which is
worth doing before plugging anything in:

```
$ kaukonen bench --patch patches/voice.kpatch --block 256
  streaming 256-frame blocks: 5333.3 us deadline, 365.4 us mean, 460.4 us worst
  worst-case headroom x11.58
```

## Recording what came out

`--record session.wav` captures exactly what was sent to the device, after the
master gain, any fade and any clipping — so the file is what you heard, not the
rack's output before the monitoring chain.

Writing happens on its own thread. If it ever falls behind, audio is dropped
from the *recording* and counted, never from the output.

## Levels and speakers

The converter soft-clips at full scale, so nothing leaves the rack above it, and
each `audioout` has `level` and `headroom` knobs. Output fades up over 50 ms at
the start and down at the end, so starting and stopping does not put a step
through whatever is connected.

Even so: start with the default `--gain 0.3`, check `peak` in the status line,
and turn it up from there. A patch with feedback can get much louder than the
one you tested.

## JACK and PipeWire

ALSA is used by default, which also covers PipeWire through its ALSA
compatibility layer. For JACK proper:

```bash
cargo build --release --features jack
```

## What has and has not been tested

Verified here, against an ALSA device backed by a file rather than hardware:
device enumeration and selection, rate and format negotiation, the callback,
routing, the fade, live knob and gain changes taking effect mid-stream, and
recording. The audio captured off that device is **bit-identical** to the same
patch rendered offline, after the deliberate fade-in.

Not verified: any real converter. The machine this was built on has no audio
hardware, so no sound has come out of a speaker, and nothing here has met a
device that enforces a real clock. The parts most likely to need adjustment on
first contact are buffer sizes near a device's minimum, and duplex drift —
cpal's input and output are separate streams with separate clocks, so the ring
buffer between them will slowly slide even on an interface that is
hardware-locked. `input short` in the status line is that happening.
