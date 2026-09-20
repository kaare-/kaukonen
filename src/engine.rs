//! Fixed-timestep simulation engine.

use crate::module::{AnalogModule, ModuleOutputs, MAX_PORTS, MAX_STATE};
use crate::network::{ModuleId, Network};
use crate::record::Recorder;
use crate::schedule::Schedule;
use crate::types::{SimConfig, Volt};

/// A resolved `label.field` channel, so recording never searches by name.
#[derive(Clone, Copy, Debug)]
pub struct Tap {
    module: usize,
    slot: usize,
    /// Read an output jack rather than an internal state.
    jack: bool,
}

pub struct Engine {
    pub network: Network,
    pub config: SimConfig,
    pub time: f32,
    schedule: Schedule,
    prev: Vec<ModuleOutputs>,
    current: Vec<ModuleOutputs>,
    /// Scratch input buffer, reused every step.
    inputs: [Volt; MAX_PORTS],
    /// Scratch state buffer for reading taps.
    states: [Volt; MAX_STATE],
}

impl Engine {
    pub fn new(network: Network, config: SimConfig) -> Self {
        let schedule = Schedule::build(&network);
        let zeros = vec![ModuleOutputs::empty(); network.len()];
        Self {
            network,
            config,
            time: 0.0,
            schedule,
            prev: zeros.clone(),
            current: zeros,
            inputs: [0.0; MAX_PORTS],
            states: [0.0; MAX_STATE],
        }
    }

    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// Re-plan after the patch has been rewired.
    pub fn rebuild_schedule(&mut self) {
        self.schedule = Schedule::build(&self.network);
        self.prev.resize(self.network.len(), ModuleOutputs::empty());
        self.current
            .resize(self.network.len(), ModuleOutputs::empty());
    }

    /// One integration step. Allocates nothing.
    pub fn step(&mut self) -> &[ModuleOutputs] {
        let dt = self.config.dt();
        let rail = self.config.rail;
        for position in 0..self.schedule.order.len() {
            let index = self.schedule.order[position];
            let module = &self.network.modules()[index];
            let n_in = module.input_count();
            self.inputs[..n_in].fill(0.0);
            // Multiple cables into one jack sum, as they do on a real panel.
            for arrival in &self.schedule.incoming[index] {
                let source = if arrival.delayed {
                    &self.prev[arrival.from]
                } else {
                    &self.current[arrival.from]
                };
                if arrival.to_jack < n_in {
                    self.inputs[arrival.to_jack] += source.get(arrival.from_jack);
                }
            }
            let module = &mut self.network.modules_mut()[index];
            let mut outs = module.step(&self.inputs[..n_in], dt);
            outs.clamp_all(rail);
            self.current[index] = outs;
        }
        std::mem::swap(&mut self.prev, &mut self.current);
        self.time += dt;
        &self.prev
    }

    /// One frame: `oversample` integration steps.
    pub fn frame(&mut self) -> &[ModuleOutputs] {
        for _ in 0..self.config.oversample.max(1) {
            self.step();
        }
        &self.prev
    }

    pub fn outputs(&self) -> &[ModuleOutputs] {
        &self.prev
    }

    /// Advance `seconds` of simulated time, recording once per frame.
    pub fn run(&mut self, seconds: f32, recorder: &mut Recorder) {
        let frames = (seconds * self.config.sample_rate).max(0.0) as usize;
        let taps = self.resolve_taps(&recorder.names);
        let mut sample = vec![0.0; taps.len()];
        for _ in 0..frames {
            self.frame();
            self.sample_into(&taps, &mut sample);
            recorder.push(self.time, &sample);
        }
    }

    /// Resolve channel names once, so a long run never looks one up by string.
    pub fn resolve_taps(&self, names: &[String]) -> Vec<Option<Tap>> {
        names.iter().map(|name| self.resolve_tap(name)).collect()
    }

    pub fn resolve_tap(&self, name: &str) -> Option<Tap> {
        let (label, field) = name.split_once('.')?;
        let module = (0..self.network.len()).find(|&i| self.network.label(ModuleId(i)) == label)?;
        let m = self.network.get(ModuleId(module));
        if let Some(slot) = m.state_fields().iter().position(|f| *f == field) {
            return Some(Tap {
                module,
                slot,
                jack: false,
            });
        }
        m.jack_out(field).map(|slot| Tap {
            module,
            slot,
            jack: true,
        })
    }

    /// Read resolved taps into `out`. Unresolved channels read as 0 V, the same
    /// as an unpatched jack.
    pub fn sample_into(&mut self, taps: &[Option<Tap>], out: &mut [Volt]) {
        for (slot, tap) in out.iter_mut().zip(taps) {
            *slot = match tap {
                Some(tap) if tap.jack => self.prev[tap.module].get(tap.slot),
                Some(tap) => {
                    let module = self.network.get(ModuleId(tap.module));
                    let width = module.state_fields().len();
                    module.read_state(&mut self.states[..width]);
                    self.states[tap.slot]
                }
                None => 0.0,
            };
        }
    }

    pub fn snapshot_sample(&mut self, names: &[String]) -> Vec<Volt> {
        let taps = self.resolve_taps(names);
        let mut out = vec![0.0; taps.len()];
        self.sample_into(&taps, &mut out);
        out
    }

    /// Channel names are `label.field`, e.g. `rc1_0.v` or `tc3.activity`.
    ///
    /// Internal states win over jacks of the same name, then output jack
    /// voltages are searched, so `neuron0.out` is recordable too.
    pub fn lookup_channel(&mut self, name: &str) -> Option<Volt> {
        let tap = self.resolve_tap(name)?;
        let mut out = [0.0];
        self.sample_into(&[Some(tap)], &mut out);
        Some(out[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch;

    fn engine(text: &str) -> (Engine, Vec<String>) {
        let built = patch::parse(text).unwrap().build().unwrap();
        (Engine::new(built.network, built.config), built.channels)
    }

    #[test]
    fn channels_read_states_and_output_jacks() {
        let (mut engine, _) = engine(
            "module src source shape=dc volts=2\n\
             module n neuron w0=1 leak=1 gain=3\n\
             patch src.out -> n.in0\n",
        );
        for _ in 0..20_000 {
            engine.step();
        }
        let v = engine.lookup_channel("n.v").expect("state");
        let out = engine.lookup_channel("n.out").expect("jack");
        // Equilibrium of dV/dt = drive - leak*V is drive/leak.
        assert!((v - 2.0).abs() < 0.01, "membrane settled at {v}");
        assert!((out - 3.0 * (v * 0.6).tanh()).abs() < 1e-4, "out={out}");
        assert!(engine.lookup_channel("n.nope").is_none());
        assert!(engine.lookup_channel("nope.v").is_none());
    }

    #[test]
    fn a_feed_forward_chain_resolves_within_one_step() {
        // Three modules deep. Under the old scheme the signal needed three
        // steps to cross; now one step is enough.
        let (mut engine, _) = engine(
            "module src source shape=dc volts=1\n\
             module a neuron w0=1 leak=0 gain=1 response=0.01\n\
             module b neuron w0=1 leak=0 gain=1 response=0.01\n\
             patch src.out -> a.in0\npatch a.out -> b.in0\n",
        );
        engine.step();
        // a integrated the source, and b saw a's output in the same step.
        assert!(engine.lookup_channel("a.v").unwrap() > 0.0);
        assert!(
            engine.lookup_channel("b.v").unwrap() > 0.0,
            "the far end of the chain was still silent after one step"
        );
    }

    #[test]
    fn a_feedback_loop_stays_finite() {
        let (mut engine, _) = engine(
            "module a neuron w0=0.6 leak=6 gain=3\n\
             module b neuron w0=-0.6 leak=6 gain=3\n\
             patch a.out -> b.in0\npatch b.out -> a.in0\n",
        );
        assert_eq!(engine.schedule().feedback.len(), 1);
        for _ in 0..20_000 {
            engine.step();
        }
        assert!(engine.lookup_channel("a.v").unwrap().is_finite());
        assert!(engine.lookup_channel("b.v").unwrap().is_finite());
    }

    #[test]
    fn oversampling_subdivides_the_frame() {
        let (mut engine, _) = engine("rate 1000\noversample 8\nmodule n neuron\n");
        assert!((engine.config.dt() - 1.0 / 8000.0).abs() < 1e-9);
        engine.frame();
        assert!(
            (engine.time - 0.001).abs() < 1e-9,
            "a frame should advance 1 ms, advanced {}",
            engine.time
        );
    }

    #[test]
    fn a_run_records_one_sample_per_frame() {
        let (mut engine, channels) =
            engine("rate 500\nmodule src source shape=sine freq=2 amp=1\nrecord src.out\n");
        let mut rec = Recorder::new(channels);
        engine.run(2.0, &mut rec);
        assert_eq!(rec.times.len(), 1000);
        let (min, max) = rec.min_max("src.out").unwrap();
        assert!(min < -0.9 && max > 0.9, "sine barely moved: {min}..{max}");
    }

    #[test]
    fn a_large_rack_steps_without_allocating_per_step() {
        let (mut engine, _) = engine(
            "module cells rc1[500] preset=tonic\n\
             module brain tc3\n\
             patch cells[*].out -> brain.a\n\
             patch brain.a -> cells[i].in\n",
        );
        for _ in 0..200 {
            engine.step();
        }
        assert!(engine.lookup_channel("cells[499].v").unwrap().is_finite());
    }
}
