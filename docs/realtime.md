# Playing a patch live

The offline renderer owns the clock: it runs a patch as fast as it can and
writes a file. A real-time host owns the clock instead — it hands over a buffer
and expects it filled before a deadline. [`Stream`](../src/stream.rs) is that
side of the arrangement.

There is deliberately no audio backend in this crate. Keeping it dependency free
is worth more than saving a host a dozen lines, and the part that is easy to get
wrong is the simulation, not the glue.

## The shape of it

```rust
use kaukonen::{patch, Stream};

let text = std::fs::read_to_string("patches/voice.kpatch")?;
let built = patch::parse(&text)?.build()?;
let mut stream = Stream::new(built);

// The host must run at this rate. One engine frame becomes one host frame, so
// nothing resamples on either side.
let rate = stream.sample_rate();

// In the audio callback, for an interleaved buffer:
stream.process(&mut buffer, channels);

// Or, with live input:
stream.process_duplex(&input, &mut buffer, channels);
```

`process` allocates nothing and takes no locks. A patch with no `audioout` fills
the buffer with silence rather than leaving whatever the host had there.

`Stream::new` switches capture off on every converter, because a stream runs
indefinitely and a growing file buffer would be a leak with a long fuse. Read
frames as they go; do not expect to collect a file at the end.

The streaming path and the file path are the same simulation. There is a test
asserting a streamed patch matches its offline render to within 1e-6, so a patch
cannot sound different live from how it rendered.

## Matching rates

Set the patch's `rate` to the rate the device will run at:

```
rate 48000
oversample 2
```

`oversample` costs CPU without changing the frame rate, which is the knob to
reach for if a patch aliases. If the host insists on a rate the patch was not
written for, change the patch rather than resampling — everything in the rack is
specified in seconds and hertz, so it transposes cleanly.

## Will it keep up?

An average faster than real time is not enough. One block that misses its
deadline is an audible dropout, so what matters is the worst case:

```
$ kaukonen bench --patch patches/voice.kpatch --block 256
  ...
  streaming 256-frame blocks: 5333.3 us deadline, 365.4 us mean, 460.4 us worst
  worst-case headroom x11.58
```

Headroom below 1 means it would drop out; below 2 is tight, since a real audio
thread shares a machine with everything else. A 48-cell rack at 48 kHz with 4x
oversampling reports x0.81 and says so plainly rather than pretending.

## Wiring it to cpal

Roughly this, in your own binary, with cpal as your dependency rather than this
crate's:

```rust
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

let device = cpal::default_host()
    .default_output_device()
    .expect("no output device");
let channels = 2;
let config = cpal::StreamConfig {
    channels: channels as u16,
    sample_rate: cpal::SampleRate(stream.sample_rate() as u32),
    buffer_size: cpal::BufferSize::Fixed(256),
};

let output = device.build_output_stream(
    &config,
    move |buffer: &mut [f32], _| stream.process(buffer, channels),
    |err| eprintln!("stream error: {err}"),
    None,
)?;
output.play()?;
```

Two things to get right, neither of which this crate can do for you:

- **Do not allocate, lock or log in the callback.** `process` does not; make
  sure your closure does not either.
- **Duplex needs a ring buffer.** cpal's input and output are separate streams
  with separate clocks, so feeding `process_duplex` directly from an input
  callback will eventually drift. Put a ring buffer between them and accept the
  latency, or run output-only and play files in through `audioin`.

## What has not been tested

`Stream` is covered by unit tests, including the match against the offline
render, the independence of block size, and the absence of buffer growth. The
cpal glue above is not: the machine this was developed on has no audio hardware,
so no sound has come out of a speaker. Treat the snippet as a sketch of the
shape rather than as something known to work.
