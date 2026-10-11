//! Time-series recording of named analog channels.

use crate::types::Volt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Recorder {
    pub times: Vec<f32>,
    pub names: Vec<String>,
    values: Vec<Vec<Volt>>,
    stride: usize,
    tick: usize,
}

impl Recorder {
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::with_stride(names, 1)
    }

    /// Record every `stride` engine steps.
    pub fn with_stride(names: impl IntoIterator<Item = impl Into<String>>, stride: usize) -> Self {
        let names: Vec<String> = names.into_iter().map(Into::into).collect();
        let n = names.len();
        Self {
            times: Vec::new(),
            names,
            values: vec![Vec::new(); n],
            stride: stride.max(1),
            tick: 0,
        }
    }

    pub fn push(&mut self, time: f32, sample: &[Volt]) {
        if self.tick % self.stride != 0 {
            self.tick += 1;
            return;
        }
        self.tick += 1;
        self.times.push(time);
        for (i, v) in sample.iter().enumerate().take(self.values.len()) {
            self.values[i].push(*v);
        }
    }

    pub fn channel(&self, name: &str) -> Option<&[Volt]> {
        let i = self.names.iter().position(|n| n == name)?;
        Some(&self.values[i])
    }

    pub fn min_max(&self, name: &str) -> Option<(Volt, Volt)> {
        let ch = self.channel(name)?;
        if ch.is_empty() {
            return None;
        }
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &v in ch {
            min = min.min(v);
            max = max.max(v);
        }
        Some((min, max))
    }

    pub fn write_csv(&self, path: impl AsRef<Path>) -> io::Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = File::create(path)?;
        write!(f, "time")?;
        for name in &self.names {
            write!(f, ",{name}")?;
        }
        writeln!(f)?;
        for i in 0..self.times.len() {
            write!(f, "{:.6}", self.times[i])?;
            for ch in &self.values {
                let v = ch.get(i).copied().unwrap_or(0.0);
                write!(f, ",{v:.6}")?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}
