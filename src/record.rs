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

    /// Self-contained HTML with an inline canvas plot. No CDN.
    pub fn write_html(&self, path: impl AsRef<Path>, plotted: &[&str]) -> io::Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent)?;
        }
        let mut series = Vec::new();
        for name in plotted {
            if let Some(ch) = self.channel(name) {
                series.push((name.to_string(), ch.to_vec()));
            }
        }
        let mut f = File::create(path)?;
        writeln!(
            f,
            r###"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Kaukonen demo</title>
<style>
  body {{ font-family: ui-sans-serif, system-ui, sans-serif; background:#111; color:#eee; margin:24px; }}
  canvas {{ width:100%; max-width:1100px; height:420px; background:#1b1b1b; border:1px solid #333; }}
  .legend span {{ display:inline-block; margin-right:14px; }}
</style></head><body>
<h1>Kaukonen analog network</h1>
<p>{} samples, {:.3} s</p>
<canvas id="c" width="1100" height="420"></canvas>
<div class="legend" id="legend"></div>
<script>
const times = {};
const series = {};
const colors = ["#6ee7b7","#93c5fd","#f9a8d4","#fcd34d","#c4b5fd","#fda4af","#67e8f9"];
const canvas = document.getElementById("c");
const ctx = canvas.getContext("2d");
const legend = document.getElementById("legend");
series.forEach((s,i) => {{
  const el = document.createElement("span");
  el.style.color = colors[i % colors.length];
  el.textContent = "● " + s.name;
  legend.appendChild(el);
}});
function draw() {{
  const w = canvas.width, h = canvas.height, pad = 40;
  ctx.clearRect(0,0,w,h);
  let ymin = Infinity, ymax = -Infinity;
  series.forEach(s => s.y.forEach(v => {{ ymin = Math.min(ymin,v); ymax = Math.max(ymax,v); }}));
  if (!(isFinite(ymin) && isFinite(ymax))) return;
  if (ymin === ymax) {{ ymin -= 1; ymax += 1; }}
  const t0 = times[0] || 0, t1 = times[times.length-1] || 1;
  const x = t => pad + (t - t0) / (t1 - t0 || 1) * (w - 2*pad);
  const y = v => h - pad - (v - ymin) / (ymax - ymin) * (h - 2*pad);
  ctx.strokeStyle = "#333";
  ctx.beginPath(); ctx.moveTo(pad, pad); ctx.lineTo(pad, h-pad); ctx.lineTo(w-pad, h-pad); ctx.stroke();
  ctx.fillStyle = "#888";
  ctx.fillText(ymin.toFixed(2), 6, h-pad);
  ctx.fillText(ymax.toFixed(2), 6, pad+8);
  ctx.fillText(t0.toFixed(2)+"s", pad, h-12);
  ctx.fillText(t1.toFixed(2)+"s", w-pad-40, h-12);
  series.forEach((s,i) => {{
    ctx.strokeStyle = colors[i % colors.length];
    ctx.beginPath();
    s.y.forEach((v,k) => {{
      const xx = x(times[k]), yy = y(v);
      if (k===0) ctx.moveTo(xx,yy); else ctx.lineTo(xx,yy);
    }});
    ctx.stroke();
  }});
}}
draw();
</script></body></html>
"###,
            self.times.len(),
            self.times.last().copied().unwrap_or(0.0),
            json_f32(&self.times),
            json_series(&series)
        )?;
        Ok(())
    }
}

fn json_f32(xs: &[f32]) -> String {
    let mut s = String::from("[");
    for (i, v) in xs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{v:.5}"));
    }
    s.push(']');
    s
}

fn json_series(series: &[(String, Vec<f32>)]) -> String {
    let mut s = String::from("[");
    for (i, (name, y)) in series.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{{name:{},y:{}}}", js_string(name), json_f32(y)));
    }
    s.push(']');
    s
}

fn js_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
