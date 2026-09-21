//! A self-contained HTML page for looking at what a patch did.
//!
//! A voltage-against-time line is a weak instrument for the things worth
//! looking for here — isolated pulses, limit cycles, bursting, near-threshold
//! instability, chaos — so the page also draws phase portraits, a spike raster
//! across a bank, an interval histogram and a spectrum, and embeds the rendered
//! audio so a headless run can still be listened to. One file, no CDN.

use crate::network::Network;
use crate::record::Recorder;
use crate::schedule::Schedule;
use crate::wav::{Audio, SampleFormat};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

/// Above this many samples per channel, the browser gets min/max buckets
/// instead of every point. Peaks survive; the page stays openable.
const MAX_POINTS: usize = 16_000;

/// A rack this size is worth drawing. Beyond it the diagram is unreadable.
const MAX_DIAGRAM_MODULES: usize = 96;

/// Everything the page can show.
pub struct Scope<'a> {
    pub title: &'a str,
    pub recorder: &'a Recorder,
    pub network: Option<&'a Network>,
    pub schedule: Option<&'a Schedule>,
    pub audio: Option<&'a Audio>,
    /// Extra lines shown above the plots.
    pub notes: Vec<String>,
}

impl<'a> Scope<'a> {
    pub fn new(title: &'a str, recorder: &'a Recorder) -> Self {
        Self {
            title,
            recorder,
            network: None,
            schedule: None,
            audio: None,
            notes: Vec::new(),
        }
    }

    pub fn with_patch(mut self, network: &'a Network, schedule: &'a Schedule) -> Self {
        self.network = Some(network);
        self.schedule = Some(schedule);
        self
    }

    pub fn with_audio(mut self, audio: &'a Audio) -> Self {
        self.audio = Some(audio);
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn write(&self, path: impl AsRef<Path>) -> io::Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(path, self.render())
    }

    pub fn render(&self) -> String {
        let mut page = String::new();
        page.push_str("<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n");
        let _ = writeln!(page, "<title>{}</title>", escape(self.title));
        page.push_str("<style>\n");
        page.push_str(STYLE);
        page.push_str("</style></head><body>\n");
        let _ = writeln!(page, "<h1>{}</h1>", escape(self.title));

        page.push_str("<p class=\"notes\">");
        let _ = write!(
            page,
            "{} channels, {} samples, {:.3} s",
            self.recorder.names.len(),
            self.recorder.times.len(),
            self.recorder.times.last().copied().unwrap_or(0.0)
        );
        for note in &self.notes {
            let _ = write!(page, "<br>{}", escape(note));
        }
        page.push_str("</p>\n");

        if let Some(audio) = self.audio {
            page.push_str("<section><h2>Rendered audio</h2>\n");
            let _ = writeln!(
                page,
                "<audio controls src=\"data:audio/wav;base64,{}\"></audio>",
                base64(&audio.encode(SampleFormat::Int16))
            );
            let _ = writeln!(
                page,
                "<p class=\"notes\">{:.2} s, {} Hz, {} ch, peak {:.3}</p></section>",
                audio.seconds(),
                audio.sample_rate,
                audio.channels,
                audio.peak()
            );
        }

        if let (Some(network), Some(schedule)) = (self.network, self.schedule) {
            page.push_str("<section><h2>Patch</h2>\n");
            if network.len() <= MAX_DIAGRAM_MODULES {
                page.push_str(&crate::diagram::svg(network, schedule));
                page.push_str(
                    "<p class=\"notes\">Dashed cables close a feedback loop and \
                     carry one step of delay.</p>",
                );
            } else {
                let _ = writeln!(
                    page,
                    "<p class=\"notes\">{} modules is too many to draw legibly; \
                     use <code>kaukonen inspect</code> for the wiring.</p>",
                    network.len()
                );
            }
            page.push_str("</section>\n");
        }

        // With nothing recorded there is nothing to plot, and emitting the
        // canvases anyway leaves a page full of empty boxes and NaN axes.
        if self.recorder.times.is_empty() || self.recorder.names.is_empty() {
            page.push_str(
                "<section><h2>Traces</h2><p class=\"notes\">This run recorded no samples. \
                 Add a <code>record</code> line to the patch, and check it ran for longer \
                 than zero seconds.</p></section>\n</body></html>\n",
            );
            return page;
        }

        page.push_str(BODY);
        page.push_str("<script>\nconst DATA = ");
        page.push_str(&self.data_json());
        page.push_str(";\n</script>\n<script>\n");
        page.push_str(SCRIPT);
        page.push_str("</script>\n</body></html>\n");
        page
    }

    /// Traces as JSON, bucketed to min/max pairs when there are too many.
    fn data_json(&self) -> String {
        let samples = self.recorder.times.len();
        let bucket = samples.div_ceil(MAX_POINTS).max(1);
        let times = bucketed_times(&self.recorder.times, bucket);

        let mut json = String::from("{\"times\":");
        push_numbers(&mut json, &times, 5);
        let _ = write!(json, ",\"bucket\":{bucket},\"series\":[");
        for (index, name) in self.recorder.names.iter().enumerate() {
            let Some(channel) = self.recorder.channel(name) else {
                continue;
            };
            if index > 0 {
                json.push(',');
            }
            let (min, max) = bucketed_extremes(channel, bucket);
            let _ = write!(json, "{{\"name\":\"{}\",\"min\":", escape(name));
            push_numbers(&mut json, &min, 5);
            json.push_str(",\"max\":");
            push_numbers(&mut json, &max, 5);
            json.push('}');
        }
        json.push_str("],\"rate\":");
        // Effective sample rate of the recorded trace, for the spectrum axis.
        let rate = if self.recorder.times.len() > 1 {
            1.0 / (self.recorder.times[1] - self.recorder.times[0]).max(1e-12)
        } else {
            0.0
        };
        let _ = write!(json, "{rate:.4}}}");
        json
    }
}

/// Time of the first sample in each bucket.
fn bucketed_times(times: &[f32], bucket: usize) -> Vec<f32> {
    times.iter().step_by(bucket).copied().collect()
}

/// Smallest and largest value in each bucket, so spikes are never skipped.
fn bucketed_extremes(values: &[f32], bucket: usize) -> (Vec<f32>, Vec<f32>) {
    if bucket <= 1 {
        return (values.to_vec(), values.to_vec());
    }
    let mut mins = Vec::with_capacity(values.len() / bucket + 1);
    let mut maxes = Vec::with_capacity(values.len() / bucket + 1);
    for chunk in values.chunks(bucket) {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for v in chunk {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        mins.push(if lo.is_finite() { lo } else { 0.0 });
        maxes.push(if hi.is_finite() { hi } else { 0.0 });
    }
    (mins, maxes)
}

fn push_numbers(out: &mut String, values: &[f32], places: usize) {
    out.push('[');
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if v.is_finite() {
            let _ = write!(out, "{v:.places$}");
        } else {
            out.push('0');
        }
    }
    out.push(']');
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Standard base64, so the audio can ride along inside the page.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for shift in [18, 12, 6, 0] {
            out.push(ALPHABET[((triple >> shift) & 0x3F) as usize] as char);
        }
        // Pad out the bytes the last chunk did not supply.
        let pad = 3 - chunk.len();
        for _ in 0..pad {
            out.pop();
        }
        for _ in 0..pad {
            out.push('=');
        }
    }
    out
}

const STYLE: &str = r##"
:root { color-scheme: dark; }
body { font-family: ui-sans-serif, system-ui, sans-serif; background: #0d0f12;
       color: #e5e7eb; margin: 0 auto; padding: 24px; max-width: 1200px; }
h1 { font-size: 20px; font-weight: 600; margin: 0 0 4px; }
h2 { font-size: 14px; font-weight: 600; text-transform: uppercase;
     letter-spacing: .08em; color: #9ca3af; margin: 28px 0 10px; }
section { margin-bottom: 8px; }
.notes { color: #9ca3af; font-size: 13px; line-height: 1.6; margin: 4px 0 12px; }
canvas { display: block; width: 100%; background: #12151a;
         border: 1px solid #23282f; border-radius: 6px; }
.row { display: flex; gap: 16px; flex-wrap: wrap; }
.row > div { flex: 1 1 340px; min-width: 300px; }
.trace { position: relative; margin-bottom: 6px; }
.trace .name { position: absolute; left: 10px; top: 6px; font-size: 12px;
               color: #d1d5db; pointer-events: none; }
.trace .range { position: absolute; right: 10px; top: 6px; font-size: 11px;
                color: #6b7280; pointer-events: none; }
.trace .value { position: absolute; right: 10px; bottom: 6px; font-size: 12px;
                color: #fcd34d; pointer-events: none; font-variant-numeric: tabular-nums; }
select, button { background: #1b1f26; color: #e5e7eb; border: 1px solid #2d333b;
                 border-radius: 5px; padding: 5px 9px; font-size: 13px; }
label { font-size: 12px; color: #9ca3af; margin-right: 6px; }
.controls { display: flex; gap: 10px; align-items: center; flex-wrap: wrap;
            margin-bottom: 10px; }
audio { width: 100%; max-width: 560px; }
.patch { width: 100%; height: auto; background: #12151a; border: 1px solid #23282f;
         border-radius: 6px; }
.patch .module { fill: #1b2028; stroke: #39414d; }
.patch .label { fill: #e5e7eb; font: 600 12px ui-sans-serif, sans-serif; }
.patch .kind { fill: #7c8899; font: 10px ui-monospace, monospace; }
.patch .cable { fill: none; stroke: #6b7280; stroke-width: 1.4; }
.patch .feedback { stroke: #f59e0b; stroke-dasharray: 5 4; }
code { font-family: ui-monospace, monospace; color: #93c5fd; }
"##;

const BODY: &str = r##"
<section>
  <h2>Traces</h2>
  <div class="controls">
    <button id="reset">Reset zoom</button>
    <span class="notes">Scroll to zoom, drag to pan. Every pane shares the time axis.</span>
  </div>
  <div id="traces"></div>
</section>

<section>
  <h2>Phase portrait</h2>
  <div class="controls">
    <label for="px">x</label><select id="px"></select>
    <label for="py">y</label><select id="py"></select>
    <span class="notes">A closed loop is a limit cycle; a point is rest; a filled
      band that never repeats is chaos.</span>
  </div>
  <div class="row">
    <div><canvas id="phase" height="380"></canvas></div>
    <div>
      <canvas id="spectrum" height="380"></canvas>
      <div class="controls" style="margin-top:8px">
        <label for="sc">channel</label><select id="sc"></select>
      </div>
    </div>
  </div>
</section>

<section>
  <h2>Spikes</h2>
  <div class="controls">
    <span class="notes">Upward crossings of 0 V. The raster covers every channel;
      the histogram shows one channel's intervals.</span>
  </div>
  <div class="row">
    <div><canvas id="raster" height="260"></canvas></div>
    <div>
      <canvas id="isi" height="260"></canvas>
      <div class="controls" style="margin-top:8px">
        <label for="ic">channel</label><select id="ic"></select>
      </div>
    </div>
  </div>
</section>
"##;

const SCRIPT: &str = r##"
const COLORS = ["#6ee7b7","#93c5fd","#fca5a5","#fcd34d","#c4b5fd","#f9a8d4",
                "#67e8f9","#a3e635","#fdba74","#d8b4fe"];
const times = DATA.times;
const series = DATA.series;
const tFirst = times.length ? times[0] : 0;
const tLast = times.length ? times[times.length - 1] : 1;
let view = { a: tFirst, b: tLast };

function fill(select, selected) {
  series.forEach((s, i) => {
    const option = document.createElement("option");
    option.value = i;
    option.textContent = s.name;
    select.appendChild(option);
  });
  select.value = Math.min(selected, series.length - 1);
}

// Index range of the samples currently in view.
function span() {
  const lo = Math.max(0, indexAt(view.a) - 1);
  const hi = Math.min(times.length - 1, indexAt(view.b) + 1);
  return [lo, Math.max(lo + 1, hi)];
}
function indexAt(t) {
  if (times.length < 2) return 0;
  const step = (tLast - tFirst) / (times.length - 1);
  return Math.round((t - tFirst) / (step || 1));
}

function sizeFor(canvas, cssHeight) {
  const ratio = window.devicePixelRatio || 1;
  const width = canvas.clientWidth || 800;
  canvas.width = Math.floor(width * ratio);
  canvas.height = Math.floor(cssHeight * ratio);
  const ctx = canvas.getContext("2d");
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  ctx.clearRect(0, 0, width, cssHeight);
  return [ctx, width, cssHeight];
}

// ---- traces, one pane per channel, sharing the time axis
const panes = [];
const container = document.getElementById("traces");
series.forEach((s, i) => {
  const wrap = document.createElement("div");
  wrap.className = "trace";
  const canvas = document.createElement("canvas");
  canvas.height = 96;
  const name = document.createElement("span");
  name.className = "name";
  name.style.color = COLORS[i % COLORS.length];
  name.textContent = s.name;
  const range = document.createElement("span");
  range.className = "range";
  const value = document.createElement("span");
  value.className = "value";
  wrap.append(canvas, name, range, value);
  container.appendChild(wrap);
  panes.push({ canvas, range, value, series: s, color: COLORS[i % COLORS.length] });
});

let cursor = null;

function drawTrace(pane) {
  const [ctx, w, h] = sizeFor(pane.canvas, 96);
  const [lo, hi] = span();
  let ymin = Infinity, ymax = -Infinity;
  for (let i = lo; i <= hi; i++) {
    ymin = Math.min(ymin, pane.series.min[i]);
    ymax = Math.max(ymax, pane.series.max[i]);
  }
  if (!isFinite(ymin) || !isFinite(ymax)) { ymin = -1; ymax = 1; }
  if (ymax - ymin < 1e-9) { ymin -= 0.5; ymax += 0.5; }
  const pad = (ymax - ymin) * 0.08;
  ymin -= pad; ymax += pad;
  pane.range.textContent = ymin.toFixed(3) + " .. " + ymax.toFixed(3);

  const x = t => (t - view.a) / (view.b - view.a || 1) * w;
  const y = v => h - (v - ymin) / (ymax - ymin) * h;

  if (ymin < 0 && ymax > 0) {
    ctx.strokeStyle = "#2a3039";
    ctx.beginPath(); ctx.moveTo(0, y(0)); ctx.lineTo(w, y(0)); ctx.stroke();
  }

  // One column per pixel, drawn as the band the samples covered, so a spike
  // narrower than a pixel is still visible.
  ctx.strokeStyle = pane.color;
  ctx.lineWidth = 1;
  ctx.beginPath();
  let px = -1, lowest = Infinity, highest = -Infinity;
  for (let i = lo; i <= hi; i++) {
    const column = Math.round(x(times[i]));
    if (column !== px) {
      if (px >= 0) {
        ctx.moveTo(px + 0.5, y(highest));
        ctx.lineTo(px + 0.5, y(lowest));
      }
      px = column; lowest = Infinity; highest = -Infinity;
    }
    lowest = Math.min(lowest, pane.series.min[i]);
    highest = Math.max(highest, pane.series.max[i]);
  }
  if (px >= 0) { ctx.moveTo(px + 0.5, y(highest)); ctx.lineTo(px + 0.5, y(lowest)); }
  ctx.stroke();

  if (cursor !== null && cursor >= view.a && cursor <= view.b) {
    ctx.strokeStyle = "#fcd34d88";
    ctx.beginPath(); ctx.moveTo(x(cursor), 0); ctx.lineTo(x(cursor), h); ctx.stroke();
    const i = Math.max(0, Math.min(times.length - 1, indexAt(cursor)));
    pane.value.textContent = pane.series.max[i].toFixed(3) + " V";
  } else {
    pane.value.textContent = "";
  }
}

function drawPhase() {
  const [ctx, w, h] = sizeFor(document.getElementById("phase"), 380);
  const xs = series[+document.getElementById("px").value];
  const ys = series[+document.getElementById("py").value];
  if (!xs || !ys) return;
  const [lo, hi] = span();
  let x0 = Infinity, x1 = -Infinity, y0 = Infinity, y1 = -Infinity;
  for (let i = lo; i <= hi; i++) {
    x0 = Math.min(x0, xs.min[i]); x1 = Math.max(x1, xs.max[i]);
    y0 = Math.min(y0, ys.min[i]); y1 = Math.max(y1, ys.max[i]);
  }
  if (x1 - x0 < 1e-9) { x0 -= 0.5; x1 += 0.5; }
  if (y1 - y0 < 1e-9) { y0 -= 0.5; y1 += 0.5; }
  const px = v => 34 + (v - x0) / (x1 - x0) * (w - 48);
  const py = v => h - 28 - (v - y0) / (y1 - y0) * (h - 44);
  ctx.strokeStyle = "#2a3039";
  ctx.beginPath(); ctx.moveTo(34, 12); ctx.lineTo(34, h - 28); ctx.lineTo(w - 14, h - 28); ctx.stroke();
  ctx.fillStyle = "#6b7280"; ctx.font = "11px sans-serif";
  ctx.fillText(xs.name, w / 2 - 20, h - 8);
  ctx.save(); ctx.translate(12, h / 2); ctx.rotate(-Math.PI / 2);
  ctx.fillText(ys.name, 0, 0); ctx.restore();
  // Later points brighter, so the direction of travel is visible.
  for (let i = lo; i < hi; i++) {
    const age = (i - lo) / Math.max(1, hi - lo);
    ctx.strokeStyle = "rgba(147,197,253," + (0.15 + 0.75 * age).toFixed(3) + ")";
    ctx.beginPath();
    ctx.moveTo(px(xs.max[i]), py(ys.max[i]));
    ctx.lineTo(px(xs.max[i + 1]), py(ys.max[i + 1]));
    ctx.stroke();
  }
}

// Radix-2 FFT, in place, on the magnitudes we need for the spectrum pane.
function fft(re, im) {
  const n = re.length;
  for (let i = 1, j = 0; i < n; i++) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) { [re[i], re[j]] = [re[j], re[i]]; [im[i], im[j]] = [im[j], im[i]]; }
  }
  for (let len = 2; len <= n; len <<= 1) {
    const step = -2 * Math.PI / len;
    for (let i = 0; i < n; i += len) {
      for (let k = 0; k < len / 2; k++) {
        const angle = step * k;
        const wr = Math.cos(angle), wi = Math.sin(angle);
        const ar = re[i + k], ai = im[i + k];
        const br = re[i + k + len / 2], bi = im[i + k + len / 2];
        const cr = br * wr - bi * wi, ci = br * wi + bi * wr;
        re[i + k] = ar + cr; im[i + k] = ai + ci;
        re[i + k + len / 2] = ar - cr; im[i + k + len / 2] = ai - ci;
      }
    }
  }
}

function drawSpectrum() {
  const [ctx, w, h] = sizeFor(document.getElementById("spectrum"), 380);
  const s = series[+document.getElementById("sc").value];
  if (!s || !DATA.rate) return;
  const [lo, hi] = span();
  let size = 1;
  while (size * 2 <= hi - lo) size *= 2;
  if (size < 64) {
    ctx.fillStyle = "#6b7280"; ctx.font = "12px sans-serif";
    ctx.fillText("zoom out for enough samples to transform", 14, 24);
    return;
  }
  const re = new Float64Array(size), im = new Float64Array(size);
  let mean = 0;
  for (let i = 0; i < size; i++) mean += s.max[lo + i];
  mean /= size;
  for (let i = 0; i < size; i++) {
    // Hann window, and the mean removed so DC does not swamp the plot.
    const window = 0.5 - 0.5 * Math.cos(2 * Math.PI * i / (size - 1));
    re[i] = (s.max[lo + i] - mean) * window;
  }
  fft(re, im);
  const bins = size / 2;
  const nyquist = DATA.rate / 2;
  let peak = 1e-12;
  const mag = new Float64Array(bins);
  for (let i = 0; i < bins; i++) {
    mag[i] = Math.hypot(re[i], im[i]);
    if (i > 0) peak = Math.max(peak, mag[i]);
  }
  const fmin = Math.max(DATA.rate / size, 0.1);
  const px = f => 34 + (Math.log10(Math.max(f, fmin)) - Math.log10(fmin)) /
                      (Math.log10(nyquist) - Math.log10(fmin)) * (w - 48);
  const py = db => h - 28 - (db + 80) / 80 * (h - 44);
  ctx.strokeStyle = "#2a3039";
  ctx.beginPath(); ctx.moveTo(34, 12); ctx.lineTo(34, h - 28); ctx.lineTo(w - 14, h - 28); ctx.stroke();
  ctx.fillStyle = "#6b7280"; ctx.font = "11px sans-serif";
  ctx.fillText(fmin.toFixed(1) + " Hz", 30, h - 10);
  ctx.fillText(nyquist.toFixed(0) + " Hz", w - 66, h - 10);
  ctx.fillText("0 dB", 4, py(0) + 4);
  ctx.fillText("-80", 6, h - 30);
  ctx.strokeStyle = "#a3e635";
  ctx.beginPath();
  for (let i = 1; i < bins; i++) {
    const db = 20 * Math.log10(mag[i] / peak);
    const x = px(i * DATA.rate / size);
    const y = py(Math.max(db, -80));
    if (i === 1) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  }
  ctx.stroke();
}

// Upward crossings of zero, in seconds.
function crossings(s) {
  const out = [];
  for (let i = 1; i < times.length; i++) {
    if (s.max[i - 1] < 0 && s.max[i] >= 0) out.push(times[i]);
  }
  return out;
}

function drawRaster() {
  const [ctx, w, h] = sizeFor(document.getElementById("raster"), 260);
  const rows = series.length;
  const lane = Math.max(4, Math.min(18, (h - 24) / Math.max(1, rows)));
  const x = t => 110 + (t - view.a) / (view.b - view.a || 1) * (w - 124);
  ctx.font = "10px ui-monospace, monospace";
  let any = false;
  series.forEach((s, i) => {
    const y = 14 + i * lane;
    ctx.fillStyle = "#6b7280";
    ctx.fillText(s.name.slice(0, 16), 6, y + lane * 0.7);
    ctx.strokeStyle = COLORS[i % COLORS.length];
    ctx.beginPath();
    for (const t of crossings(s)) {
      if (t < view.a || t > view.b) continue;
      any = true;
      ctx.moveTo(x(t), y + 1);
      ctx.lineTo(x(t), y + lane - 2);
    }
    ctx.stroke();
  });
  if (!any) {
    ctx.fillStyle = "#6b7280";
    ctx.fillText("no channel crosses 0 V in this window", 120, h / 2);
  }
}

function drawIsi() {
  const [ctx, w, h] = sizeFor(document.getElementById("isi"), 260);
  const s = series[+document.getElementById("ic").value];
  if (!s) return;
  const spikes = crossings(s).filter(t => t >= view.a && t <= view.b);
  const gaps = [];
  for (let i = 1; i < spikes.length; i++) gaps.push(spikes[i] - spikes[i - 1]);
  ctx.fillStyle = "#6b7280"; ctx.font = "11px sans-serif";
  if (gaps.length < 2) {
    ctx.fillText("not enough intervals in this window", 14, h / 2);
    return;
  }
  const lo = Math.min(...gaps), hi = Math.max(...gaps);
  const bins = 28;
  const counts = new Array(bins).fill(0);
  for (const g of gaps) {
    const k = Math.min(bins - 1, Math.floor((g - lo) / ((hi - lo) || 1) * bins));
    counts[k]++;
  }
  const tallest = Math.max(...counts);
  const bw = (w - 48) / bins;
  ctx.fillStyle = "#c4b5fd";
  counts.forEach((c, i) => {
    const bh = (c / tallest) * (h - 46);
    ctx.fillRect(34 + i * bw, h - 28 - bh, Math.max(1, bw - 2), bh);
  });
  ctx.fillStyle = "#6b7280";
  ctx.fillText((lo * 1000).toFixed(1) + " ms", 30, h - 10);
  ctx.fillText((hi * 1000).toFixed(1) + " ms", w - 70, h - 10);
  ctx.fillText(gaps.length + " intervals, " + tallest + " in the tallest bin", 34, 18);
}

function drawAll() {
  panes.forEach(drawTrace);
  drawPhase();
  drawSpectrum();
  drawRaster();
  drawIsi();
}

// ---- interaction
function timeAt(event, canvas) {
  const rect = canvas.getBoundingClientRect();
  const fraction = (event.clientX - rect.left) / rect.width;
  return view.a + fraction * (view.b - view.a);
}

panes.forEach(pane => {
  pane.canvas.addEventListener("wheel", event => {
    event.preventDefault();
    const focus = timeAt(event, pane.canvas);
    const scale = event.deltaY > 0 ? 1.25 : 0.8;
    const width = (view.b - view.a) * scale;
    const limit = Math.min(width, tLast - tFirst);
    view.a = Math.max(tFirst, focus - (focus - view.a) * scale);
    view.b = Math.min(tLast, view.a + limit);
    if (view.b - view.a < 1e-6) view.b = view.a + 1e-6;
    drawAll();
  }, { passive: false });

  let dragging = null;
  pane.canvas.addEventListener("mousedown", event => { dragging = timeAt(event, pane.canvas); });
  window.addEventListener("mouseup", () => { dragging = null; });
  pane.canvas.addEventListener("mousemove", event => {
    cursor = timeAt(event, pane.canvas);
    if (dragging !== null) {
      const shift = dragging - cursor;
      const width = view.b - view.a;
      view.a = Math.max(tFirst, Math.min(tLast - width, view.a + shift));
      view.b = view.a + width;
    }
    drawAll();
  });
  pane.canvas.addEventListener("mouseleave", () => { cursor = null; drawAll(); });
});

document.getElementById("reset").addEventListener("click", () => {
  view = { a: tFirst, b: tLast };
  drawAll();
});

fill(document.getElementById("px"), 0);
fill(document.getElementById("py"), 1);
fill(document.getElementById("sc"), series.length - 1);
fill(document.getElementById("ic"), 0);
for (const id of ["px", "py", "sc", "ic"]) {
  document.getElementById(id).addEventListener("change", drawAll);
}
window.addEventListener("resize", drawAll);
drawAll();
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch;
    use crate::render::{render, RenderOptions};

    fn recorded(text: &str) -> (Recorder, Network, Schedule) {
        let built = patch::parse(text).unwrap().build().unwrap();
        let (engine, recorder, _) = render(built, &RenderOptions::default());
        let schedule = engine.schedule().clone();
        (recorder, engine.network, schedule)
    }

    #[test]
    fn a_page_is_self_contained() {
        let (recorder, network, schedule) =
            recorded("rate 500\nseconds 1\nmodule cell rc1 preset=tonic\nrecord cell.v cell.r\n");
        let page = Scope::new("test", &recorder)
            .with_patch(&network, &schedule)
            .render();
        assert!(page.starts_with("<!DOCTYPE html>"));
        assert!(page.contains("</html>"));
        // Nothing is fetched: the whole point is one openable file. The only
        // URL allowed is the SVG namespace, which is a declaration.
        assert!(!page.contains("<script src"), "page loads outside script");
        assert!(!page.contains("<link"), "page loads outside stylesheet");
        assert_eq!(
            page.matches("http").count(),
            page.matches("http://www.w3.org/2000/svg").count(),
            "page contains a URL that is not the SVG namespace"
        );
        assert!(page.contains("cell.v"));
        assert!(page.contains("<svg"));
    }

    #[test]
    fn audio_is_embedded_as_a_playable_source() {
        let (recorder, _, _) = recorded("rate 500\nseconds 1\nmodule n neuron\nrecord n.v\n");
        let audio = Audio::mono(8000, vec![0.25; 800]);
        let page = Scope::new("test", &recorder).with_audio(&audio).render();
        assert!(page.contains("<audio controls src=\"data:audio/wav;base64,"));
        let start = page.find("base64,").unwrap() + 7;
        let end = page[start..].find('"').unwrap() + start;
        assert!(end - start > 100, "base64 payload looks empty");
    }

    #[test]
    fn base64_matches_the_known_answers() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn long_traces_are_bucketed_without_losing_peaks() {
        let mut values = vec![0.0; 100_000];
        values[54_321] = 9.0;
        values[54_322] = -9.0;
        let bucket = values.len().div_ceil(MAX_POINTS);
        let (min, max) = bucketed_extremes(&values, bucket);
        assert!(min.len() <= MAX_POINTS + 1);
        assert_eq!(max.iter().copied().fold(f32::MIN, f32::max), 9.0);
        assert_eq!(min.iter().copied().fold(f32::MAX, f32::min), -9.0);
    }

    #[test]
    fn an_empty_run_says_so_instead_of_drawing_nothing() {
        let (recorder, _, _) = recorded("rate 1000\nseconds 0\nmodule n neuron\nrecord n.v\n");
        assert!(recorder.times.is_empty());
        let page = Scope::new("empty", &recorder).render();
        assert!(page.contains("recorded no samples"));
        assert!(!page.contains("<canvas"), "drew canvases with no data");
        assert!(page.ends_with("</html>\n"));
    }

    #[test]
    fn short_traces_are_left_alone() {
        let values = vec![1.0, 2.0, 3.0];
        let (min, max) = bucketed_extremes(&values, 1);
        assert_eq!(min, values);
        assert_eq!(max, values);
    }

    #[test]
    fn a_huge_rack_skips_the_diagram_instead_of_drawing_mush() {
        let (recorder, network, schedule) =
            recorded("rate 200\nseconds 0.1\nmodule cells rc1[120]\nrecord cells[0].v\n");
        let page = Scope::new("big", &recorder)
            .with_patch(&network, &schedule)
            .render();
        assert!(!page.contains("<svg"));
        assert!(page.contains("too many to draw"));
    }

    #[test]
    fn feedback_cables_are_marked_in_the_diagram() {
        let (_recorder, network, schedule) = recorded(
            "rate 200\nseconds 0.1\nmodule a neuron\nmodule b neuron\n\
             patch a.out -> b.in0\npatch b.out -> a.in0\nrecord a.v\n",
        );
        let svg = crate::diagram::svg(&network, &schedule);
        assert_eq!(svg.matches("class=\"cable\"").count(), 1);
        assert_eq!(svg.matches("cable feedback").count(), 1);
    }
}
