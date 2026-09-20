//! Run a patch for a while and collect what came out.
//!
//! This is the whole offline pipeline in one place: hand any `audioin` modules
//! the sound to play, step the rack, record the requested channels, and take
//! the captured audio off any `audioout`.

use crate::engine::Engine;
use crate::module::AnalogModule;
use crate::network::ModuleId;
use crate::patch::BuiltPatch;
use crate::record::Recorder;
use crate::wav::Audio;

/// What a render produced besides the recorder's traces.
#[derive(Clone, Debug)]
pub struct RenderReport {
    pub seconds: f32,
    pub frames: usize,
    /// Captured sound, if the patch has an `audioout`.
    pub audio: Option<Audio>,
    /// Largest level the converter saw, before soft clipping. Above 1.0 means
    /// the patch drove it into the ceiling.
    pub peak: f32,
    /// Labels of modules whose inputs are entirely unpatched.
    pub floating: Vec<String>,
}

impl RenderReport {
    pub fn clipped(&self) -> bool {
        self.peak > 1.0
    }
}

/// Settings for one render.
#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    /// Run length. Defaults to the patch's `seconds`.
    pub seconds: Option<f32>,
    /// Sound to feed every `audioin` module.
    pub input: Option<Audio>,
    /// Record one sample every `stride` frames. Zero or one records all.
    pub stride: usize,
}

/// Run a built patch to completion.
pub fn render(built: BuiltPatch, options: &RenderOptions) -> (Engine, Recorder, RenderReport) {
    let seconds = options.seconds.unwrap_or(built.seconds).max(0.0);
    let mut recorder = Recorder::with_stride(built.channels.clone(), options.stride.max(1));
    let mut engine = Engine::new(built.network, built.config);

    if let Some(audio) = &options.input {
        for index in 0..engine.network.len() {
            if let Some(input) = engine.network.get_mut(ModuleId(index)).as_audio_in_mut() {
                input.load(audio.clone());
            }
        }
    }

    let floating = floating_modules(&engine);
    engine.run(seconds, &mut recorder);

    let mut audio = None;
    let mut peak = 0.0f32;
    for index in 0..engine.network.len() {
        if let Some(out) = engine.network.get(ModuleId(index)).as_audio_out() {
            peak = peak.max(out.peak());
            if audio.is_none() {
                audio = Some(out.captured());
            }
        }
    }

    let report = RenderReport {
        seconds,
        frames: recorder.times.len(),
        audio,
        peak,
        floating,
    };
    (engine, recorder, report)
}

/// Modules that have inputs but nothing patched into any of them. Usually a
/// typo in a patch file rather than a deliberate choice.
fn floating_modules(engine: &Engine) -> Vec<String> {
    let schedule = engine.schedule();
    (0..engine.network.len())
        .filter(|index| {
            let module = engine.network.get(ModuleId(*index));
            module.input_count() > 0 && schedule.incoming[*index].is_empty()
        })
        .map(|index| engine.network.label(ModuleId(index)).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch;

    fn run(text: &str, options: &RenderOptions) -> (Engine, Recorder, RenderReport) {
        let built = patch::parse(text).unwrap().build().unwrap();
        render(built, options)
    }

    #[test]
    fn a_patch_with_no_audioout_captures_nothing() {
        let (_, rec, report) = run(
            "rate 1000\nseconds 1\nmodule n neuron\nrecord n.v\n",
            &RenderOptions::default(),
        );
        assert!(report.audio.is_none());
        assert_eq!(rec.times.len(), 1000);
        assert_eq!(report.frames, 1000);
    }

    #[test]
    fn sound_goes_in_and_comes_back_out() {
        // A tone in, straight through to the converter, and out again.
        let rate = 16_000;
        let samples: Vec<f32> = (0..rate)
            .map(|i| (i as f32 / rate as f32 * 220.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        let (_, _, report) = run(
            "rate 16000\nseconds 1\n\
             module adc audioin gain=5\n\
             module dac audioout rate=16000 level=0.2\n\
             patch adc.out -> dac.left\npatch adc.out -> dac.right\n",
            &RenderOptions {
                input: Some(Audio::mono(rate as u32, samples)),
                ..RenderOptions::default()
            },
        );
        let audio = report.audio.clone().expect("captured audio");
        assert_eq!(audio.sample_rate, 16_000);
        assert_eq!(audio.channels, 2);
        assert!(
            (audio.frames() as i64 - 16_000).abs() < 50,
            "captured {} frames",
            audio.frames()
        );
        // Half scale in, half scale out, allowing for the decimation filter.
        assert!(
            (0.3..0.55).contains(&audio.peak()),
            "level changed on the way through: peak {}",
            audio.peak()
        );
        assert!(!report.clipped());
    }

    #[test]
    fn an_overdriven_converter_is_reported() {
        let (_, _, report) = run(
            "rate 8000\nseconds 0.2\n\
             module src source shape=dc volts=9\n\
             module dac audioout rate=8000 level=1\n\
             patch src.out -> dac.left\n",
            &RenderOptions::default(),
        );
        assert!(report.clipped(), "peak was {}", report.peak);
        assert!(report.audio.unwrap().peak() <= 1.0);
    }

    #[test]
    fn unpatched_modules_are_pointed_out() {
        let (_, _, report) = run(
            "rate 1000\nseconds 0.1\n\
             module src source shape=dc volts=1\n\
             module wired neuron\nmodule lonely neuron\n\
             patch src.out -> wired.in0\n",
            &RenderOptions::default(),
        );
        assert_eq!(report.floating, vec!["lonely".to_string()]);
    }

    #[test]
    fn stride_thins_the_recording_without_shortening_the_run() {
        let (engine, rec, report) = run(
            "rate 1000\nseconds 1\nmodule n neuron\nrecord n.v\n",
            &RenderOptions {
                stride: 10,
                ..RenderOptions::default()
            },
        );
        assert_eq!(rec.times.len(), 100);
        assert_eq!(report.frames, 100);
        assert!((engine.time - 1.0).abs() < 1e-3, "time {}", engine.time);
    }

    #[test]
    fn the_seconds_option_overrides_the_patch() {
        let (engine, _, _) = run(
            "rate 1000\nseconds 10\nmodule n neuron\n",
            &RenderOptions {
                seconds: Some(0.25),
                ..RenderOptions::default()
            },
        );
        assert!((engine.time - 0.25).abs() < 1e-3, "time {}", engine.time);
    }
}
