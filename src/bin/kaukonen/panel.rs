//! The live control surface, served as a local web page.
//!
//! Audio still leaves through the interface. This is the knobs: the same
//! names the console's `set` takes, grouped the same way, reachable from a
//! browser on the same machine. Nothing is bound off loopback.

use kaukonen::diagram::Map;
use kaukonen::module::AnalogModule;
use kaukonen::network::ModuleId;
use kaukonen::param::{self, trim_float, ParamSpec};
use kaukonen::schedule::Schedule;
use kaukonen::BuiltPatch;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

/// What the page shows, and what a POST turns.
pub struct Surface {
    pub patch: String,
    pub device: String,
    pub gain: f32,
    pub muted: bool,
    pub quit: bool,
    pub input: InputView,
    pub map: Map,
    pub modules: Vec<Bank>,
    /// `(bank, knob)` pairs pinned at the top of the page.
    pub desk: Vec<(String, String)>,
}

/// Capture side of the page: enable, device, channel, and what is plugged in.
#[derive(Clone, Debug)]
pub struct InputView {
    /// The patch has an `audioin` the microphone can reach.
    pub available: bool,
    /// A capture stream is running.
    pub enabled: bool,
    /// Selected device name; empty means the host default.
    pub device: String,
    /// 1-based, as on the front of the box.
    pub channel: usize,
    /// How many channels the current device opened with.
    pub channels: usize,
    /// Devices `play` can open. Empty on `serve`.
    pub devices: Vec<InputDevice>,
    pub error: String,
}

#[derive(Clone, Debug)]
pub struct InputDevice {
    pub name: String,
    pub channels: u16,
    pub is_default: bool,
}

impl Default for InputView {
    fn default() -> Self {
        Self {
            available: false,
            enabled: false,
            device: String::new(),
            channel: 1,
            channels: 0,
            devices: Vec::new(),
            error: String::new(),
        }
    }
}

impl InputView {
    /// Apply a POST. Refuses a patch with no `audioin`, or a channel the
    /// current device does not have.
    pub fn merge(
        &self,
        enabled: Option<bool>,
        device: Option<String>,
        channel: Option<usize>,
    ) -> Result<Self, String> {
        if !self.available {
            return Err(
                "this patch has no audioin module, so there is nothing for a microphone to reach"
                    .to_string(),
            );
        }
        let mut next = self.clone();
        if let Some(on) = enabled {
            next.enabled = on;
        }
        if let Some(name) = device {
            next.device = name;
            if let Some(found) = next.devices.iter().find(|d| d.name == next.device) {
                next.channels = found.channels as usize;
            }
        }
        if let Some(ch) = channel {
            if ch < 1 {
                return Err("channel is numbered from 1".to_string());
            }
            if next.channels > 0 && ch > next.channels {
                return Err(format!(
                    "channel {ch} is past this device's {} channel{}",
                    next.channels,
                    if next.channels == 1 { "" } else { "s" }
                ));
            }
            next.channel = ch;
        } else if next.channels > 0 && next.channel > next.channels {
            next.channel = next.channels;
        }
        next.error.clear();
        Ok(next)
    }

    fn write_json(&self, out: &mut String) {
        out.push('{');
        let _ = std::fmt::Write::write_fmt(
            out,
            format_args!(
                "\"available\":{},\"enabled\":{},\"channel\":{},\"channels\":{}",
                self.available, self.enabled, self.channel, self.channels
            ),
        );
        out.push(',');
        push_str(out, "device", &self.device);
        out.push(',');
        push_str(out, "error", &self.error);
        out.push_str(",\"devices\":[");
        for (i, device) in self.devices.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            push_str(out, "name", &device.name);
            let _ = std::fmt::Write::write_fmt(
                out,
                format_args!(
                    ",\"channels\":{},\"is_default\":{}",
                    device.channels, device.is_default
                ),
            );
            out.push('}');
        }
        out.push_str("]}");
    }
}

/// Whether any module in the rack is an `audioin`.
pub fn patch_has_audioin(built: &BuiltPatch) -> bool {
    (0..built.network.len()).any(|i| built.network.get(ModuleId(i)).kind_name() == "audioin")
}

/// Knobs a weather-like rack should keep at the top even without a `desk` line.
fn fallback_desk(modules: &[Bank]) -> Vec<(String, String)> {
    const WANTED: &[(&str, &str)] = &[
        ("drone", "tune"),
        ("droneamp", "gain"),
        ("dronef", "cutoff"),
        ("osc", "tune"),
        ("call", "cutoff"),
        ("throat", "cutoff"),
        ("sky", "bias"),
        ("body", "cutoff"),
        ("verb", "mix"),
        ("verb", "decay"),
        ("pharynx", "rate"),
        ("hymn", "decay"),
        ("ear", "tilt"),
        ("trace", "natural_decay"),
        ("fbank", "tilt"),
        ("fbank", "mix"),
        ("band", "cutoff"),
        ("outcomp", "threshold"),
    ];
    WANTED
        .iter()
        .copied()
        .filter(|(bank, knob)| {
            modules
                .iter()
                .any(|m| m.name == *bank && m.knobs.iter().any(|k| k.spec.name == *knob))
        })
        .map(|(bank, knob)| (bank.to_string(), knob.to_string()))
        .collect()
}

/// One declared module, or a whole bank under a single set of sliders.
pub struct Bank {
    pub name: String,
    pub kind: String,
    pub count: usize,
    pub indices: Vec<usize>,
    pub knobs: Vec<KnobView>,
}

pub struct KnobView {
    pub spec: ParamSpec,
    pub value: f32,
}

impl Surface {
    /// Snapshot every knob before the rack moves to the audio thread.
    ///
    /// A bank is one card. Turning a slider sets every instance, which is
    /// what `set cells.rate` does at the console. Mismatch may have made
    /// the instances differ; the value shown is the first one's.
    pub fn from_built(patch: &str, device: &str, gain: f32, built: &BuiltPatch) -> Self {
        let mut modules: Vec<Bank> = Vec::new();
        for index in 0..built.network.len() {
            let label = built.network.label(ModuleId(index));
            let bank = label.split('[').next().unwrap_or(label);
            let module = built.network.get(ModuleId(index));
            if let Some(existing) = modules.iter_mut().find(|m| m.name == bank) {
                existing.indices.push(index);
                existing.count += 1;
                continue;
            }
            let knobs = module
                .params()
                .iter()
                .copied()
                .map(|spec| KnobView {
                    value: module.get_param(spec.name).unwrap_or(spec.default),
                    spec,
                })
                .collect();
            modules.push(Bank {
                name: bank.to_string(),
                kind: module.kind_name().to_string(),
                count: 1,
                indices: vec![index],
                knobs,
            });
        }
        Self {
            patch: patch.to_string(),
            device: device.to_string(),
            gain,
            muted: false,
            quit: false,
            input: InputView {
                available: patch_has_audioin(built),
                ..InputView::default()
            },
            map: Map::of(&built.network, &Schedule::build(&built.network)),
            desk: if built.desk.is_empty() {
                fallback_desk(&modules)
            } else {
                built.desk.clone()
            },
            modules,
        }
    }

    pub fn find_bank(&self, name: &str) -> Option<&Bank> {
        self.modules.iter().find(|m| m.name == name)
    }

    pub fn find_bank_mut(&mut self, name: &str) -> Option<&mut Bank> {
        self.modules.iter_mut().find(|m| m.name == name)
    }

    /// Apply a knob turn. Returns the instances, the `'static` knob name,
    /// the checked value, and a formatted readout.
    pub fn set_knob(
        &mut self,
        module: &str,
        knob: &str,
        raw: &str,
    ) -> Result<(Vec<usize>, &'static str, f32, String), String> {
        let bank = self
            .find_bank_mut(module)
            .ok_or_else(|| format!("no module called '{module}'"))?;
        let slot = bank
            .knobs
            .iter_mut()
            .find(|k| k.spec.name == knob)
            .ok_or_else(|| format!("a {} has no knob '{knob}'", bank.kind))?;
        let value = slot
            .spec
            .parse(raw)
            .map_err(|e| e.to_string())
            .and_then(|v| param::checked(&slot.spec, v).map_err(|e| e.to_string()))?;
        slot.value = value;
        let formatted = slot.spec.format(value);
        Ok((bank.indices.clone(), slot.spec.name, value, formatted))
    }

    pub fn to_json(&self, extra: &str) -> String {
        let mut out = String::new();
        out.push('{');
        push_str(&mut out, "patch", &self.patch);
        out.push(',');
        push_str(&mut out, "device", &self.device);
        out.push(',');
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "\"gain\":{},\"muted\":{},\"quit\":{}",
                json_num(self.gain),
                self.muted,
                self.quit
            ),
        );
        out.push_str(",\"input\":");
        self.input.write_json(&mut out);
        out.push_str(",\"map\":");
        out.push_str(&self.map.to_json());
        if !extra.is_empty() {
            out.push(',');
            out.push_str(extra);
        }
        out.push_str(",\"desk\":[");
        for (i, (module, knob)) in self.desk.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            push_str(&mut out, "module", module);
            out.push(',');
            push_str(&mut out, "knob", knob);
            out.push('}');
        }
        out.push(']');
        out.push_str(",\"modules\":[");
        for (i, bank) in self.modules.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            push_str(&mut out, "name", &bank.name);
            out.push(',');
            push_str(&mut out, "kind", &bank.kind);
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!(",\"count\":{},\"knobs\":[", bank.count),
            );
            for (j, knob) in bank.knobs.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                out.push('{');
                push_str(&mut out, "name", knob.spec.name);
                out.push(',');
                push_str(&mut out, "doc", knob.spec.doc);
                out.push(',');
                push_str(
                    &mut out,
                    "taper",
                    match knob.spec.taper {
                        kaukonen::param::Taper::Log => "log",
                        kaukonen::param::Taper::Linear => "linear",
                    },
                );
                out.push(',');
                push_str(&mut out, "unit", knob.spec.unit.suffix().trim());
                out.push(',');
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!(
                        "\"min\":{},\"max\":{},\"default\":{},\"value\":{}",
                        json_num(knob.spec.min),
                        json_num(knob.spec.max),
                        json_num(knob.spec.default),
                        json_num(knob.value)
                    ),
                );
                if let Some(choices) = knob.spec.choices() {
                    out.push_str(",\"choices\":[");
                    for (c, name) in choices.iter().enumerate() {
                        if c > 0 {
                            out.push(',');
                        }
                        out.push('"');
                        out.push_str(&escape_json(name));
                        out.push('"');
                    }
                    out.push(']');
                }
                out.push('}');
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }
}

/// A running panel. Dropping it does not stop the accept thread; `stop` does.
pub struct Panel {
    pub addr: SocketAddr,
    /// Read by `play` (realtime) and by the panel tests. Quiet otherwise.
    #[allow(dead_code)]
    pub surface: Arc<Mutex<Surface>>,
    pub stop: Arc<AtomicBool>,
}

/// How a host applies a change that came from the page.
pub trait Host: Send + Sync + 'static {
    fn set_knob(&self, modules: &[usize], knob: &'static str, value: f32) -> Result<(), String>;
    fn set_gain(&self, value: f32) -> Result<(), String>;
    fn set_mute(&self, muted: bool) -> Result<(), String>;
    /// Open, close, or retarget capture. `serve` refuses an enable.
    fn set_input(&self, wanted: &InputView) -> Result<InputView, String> {
        if wanted.enabled {
            Err("open with play to use a microphone. serve has no audio.".to_string())
        } else {
            Ok(wanted.clone())
        }
    }
    fn extra_json(&self) -> String {
        String::new()
    }
}

/// A host that only updates the surface. Used by `serve` (no audio).
pub struct Silent;

impl Host for Silent {
    fn set_knob(&self, _modules: &[usize], _knob: &'static str, _value: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_gain(&self, _value: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_mute(&self, _muted: bool) -> Result<(), String> {
        Ok(())
    }
}

/// Bind loopback and serve the page on a background thread.
pub fn start(surface: Surface, host: Arc<dyn Host>, port: u16) -> Result<Panel, String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        if port == 0 {
            format!("cannot bind a local port: {e}")
        } else {
            format!(
                "cannot bind 127.0.0.1:{port}: {e}. Pass --port 0 to pick one, \
                 or --no-web to skip the page."
            )
        }
    })?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("cannot listen: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("cannot read bound address: {e}"))?;
    let surface = Arc::new(Mutex::new(surface));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let surface = Arc::clone(&surface);
        let stop = Arc::clone(&stop);
        thread::spawn(move || accept_loop(listener, surface, host, stop));
    }
    Ok(Panel {
        addr,
        surface,
        stop,
    })
}

pub fn url(addr: SocketAddr) -> String {
    format!("http://127.0.0.1:{}", addr.port())
}

pub fn open_browser(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = result {
        eprintln!("note     could not open a browser ({e}). Open {url} yourself.");
    }
}

fn accept_loop(
    listener: TcpListener,
    surface: Arc<Mutex<Surface>>,
    host: Arc<dyn Host>,
    stop: Arc<AtomicBool>,
) {
    listener.set_nonblocking(true).expect("nonblocking listen");
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let surface = Arc::clone(&surface);
                let host = Arc::clone(&host);
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    let _ = handle_client(stream, &surface, host.as_ref(), &stop);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

fn handle_client(
    mut stream: TcpStream,
    surface: &Mutex<Surface>,
    host: &dyn Host,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let mut buf = vec![0u8; 16_384];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let (method, path, body) = match parse_request(&req) {
        Some(parts) => parts,
        None => {
            write_response(&mut stream, 400, "text/plain", b"bad request")?;
            return Ok(());
        }
    };

    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            write_response(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                PAGE.as_bytes(),
            )?;
        }
        ("GET", "/api/state") => {
            let extra = host.extra_json();
            let json = surface
                .lock()
                .map(|s| s.to_json(&extra))
                .unwrap_or_else(|_| "{\"error\":\"locked\"}".to_string());
            write_response(&mut stream, 200, "application/json", json.as_bytes())?;
        }
        ("POST", "/api/set") => {
            let module = json_string(body, "module").unwrap_or_default();
            let knob = json_string(body, "knob").unwrap_or_default();
            let raw = json_raw(body, "value").unwrap_or_default();
            let applied = match surface.lock() {
                Ok(mut s) => s.set_knob(&module, &knob, &raw).map(|t| {
                    let count = s.find_bank(&module).map(|m| m.count).unwrap_or(1);
                    (t, count)
                }),
                Err(_) => Err("the panel is locked".to_string()),
            };
            match applied {
                Ok(((indices, name, value, formatted), count)) => {
                    if let Err(e) = host.set_knob(&indices, name, value) {
                        write_response(
                            &mut stream,
                            500,
                            "application/json",
                            error_json(&e).as_bytes(),
                        )?;
                    } else {
                        let msg = if count > 1 {
                            format!("{module}.{knob} = {formatted} ({count} instances)")
                        } else {
                            format!("{module}.{knob} = {formatted}")
                        };
                        println!("  {msg}");
                        write_response(
                            &mut stream,
                            200,
                            "application/json",
                            format!(
                                "{{\"ok\":true,\"readout\":\"{}\"}}",
                                escape_json(&formatted)
                            )
                            .as_bytes(),
                        )?;
                    }
                }
                Err(err) => {
                    write_response(
                        &mut stream,
                        400,
                        "application/json",
                        error_json(&err).as_bytes(),
                    )?;
                }
            }
        }
        ("POST", "/api/gain") => {
            let value = json_f32(body, "value").unwrap_or(0.3).clamp(0.0, 8.0);
            if let Ok(mut s) = surface.lock() {
                s.gain = value;
            }
            if let Err(e) = host.set_gain(value) {
                write_response(
                    &mut stream,
                    500,
                    "application/json",
                    error_json(&e).as_bytes(),
                )?;
            } else {
                println!("  gain = {}", trim_float(value));
                write_response(&mut stream, 200, "application/json", b"{\"ok\":true}")?;
            }
        }
        ("POST", "/api/mute") => {
            let muted = json_bool(body, "muted").unwrap_or(true);
            if let Ok(mut s) = surface.lock() {
                s.muted = muted;
            }
            if let Err(e) = host.set_mute(muted) {
                write_response(
                    &mut stream,
                    500,
                    "application/json",
                    error_json(&e).as_bytes(),
                )?;
            } else {
                println!("{}", if muted { "  muted" } else { "  unmuted" });
                write_response(&mut stream, 200, "application/json", b"{\"ok\":true}")?;
            }
        }
        ("POST", "/api/input") => {
            let enabled = json_bool(body, "enabled");
            let device = json_string(body, "device");
            let channel = json_f32(body, "channel").map(|v| v.round() as usize);
            let merged = match surface.lock() {
                Ok(s) => s.input.merge(enabled, device, channel),
                Err(_) => Err("the panel is locked".to_string()),
            };
            match merged {
                Ok(wanted) => match host.set_input(&wanted) {
                    Ok(state) => {
                        if let Ok(mut s) = surface.lock() {
                            let devices = std::mem::take(&mut s.input.devices);
                            s.input = state;
                            if s.input.devices.is_empty() {
                                s.input.devices = devices;
                            }
                            s.input.error.clear();
                        }
                        let label = if wanted.enabled {
                            if wanted.device.is_empty() {
                                "input on".to_string()
                            } else {
                                format!("input {} ch {}", wanted.device, wanted.channel)
                            }
                        } else {
                            "input off".to_string()
                        };
                        println!("  {label}");
                        write_response(&mut stream, 200, "application/json", b"{\"ok\":true}")?;
                    }
                    Err(e) => {
                        if let Ok(mut s) = surface.lock() {
                            s.input.error = e.clone();
                            if enabled == Some(true) {
                                s.input.enabled = false;
                            }
                        }
                        write_response(
                            &mut stream,
                            400,
                            "application/json",
                            error_json(&e).as_bytes(),
                        )?;
                    }
                },
                Err(err) => {
                    write_response(
                        &mut stream,
                        400,
                        "application/json",
                        error_json(&err).as_bytes(),
                    )?;
                }
            }
        }
        ("POST", "/api/quit") => {
            if let Ok(mut s) = surface.lock() {
                s.quit = true;
            }
            stop.store(true, Ordering::Relaxed);
            write_response(&mut stream, 200, "application/json", b"{\"ok\":true}")?;
        }
        _ => {
            write_response(&mut stream, 404, "text/plain", b"not found")?;
        }
    }
    Ok(())
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         Access-Control-Allow-Origin: *\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

fn parse_request(req: &str) -> Option<(&str, &str, &str)> {
    let (head, body) = req
        .split_once("\r\n\r\n")
        .or_else(|| req.split_once("\n\n"))?;
    let mut lines = head.split('\n');
    let start = lines.next()?.trim_end_matches('\r');
    let mut parts = start.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?.split('?').next()?;
    Some((method, path, body))
}

fn json_string(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let rest = body.split_once(&needle)?.1;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else if c == '"' {
            break;
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// A JSON value as the page sent it: a quoted string or a bare number.
fn json_raw(body: &str, key: &str) -> Option<String> {
    if let Some(s) = json_string(body, key) {
        return Some(s);
    }
    json_f32(body, key).map(trim_float)
}

fn json_f32(body: &str, key: &str) -> Option<f32> {
    let needle = format!("\"{key}\"");
    let rest = body.split_once(&needle)?.1;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let token = rest.split([',', '}', ']', ' ', '\n', '\r', '\t']).next()?;
    token.parse().ok()
}

fn json_bool(body: &str, key: &str) -> Option<bool> {
    let needle = format!("\"{key}\"");
    let rest = body.split_once(&needle)?.1;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn error_json(message: &str) -> String {
    format!("{{\"ok\":false,\"error\":\"{}\"}}", escape_json(message))
}

fn push_str(out: &mut String, key: &str, value: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":\"");
    out.push_str(&escape_json(value));
    out.push('"');
}

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn json_num(v: f32) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    trim_float(v)
}

/// Block until the page asks to quit, the process is interrupted, or `until`.
pub fn wait_until_quit(stop: &AtomicBool, until: Option<Instant>) {
    while !stop.load(Ordering::Relaxed) {
        if until.is_some_and(|t| Instant::now() >= t) {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(50));
    }
}

const PAGE: &str = r##"<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>kaukonen</title>
<style>
:root { color-scheme: dark; }
* { box-sizing: border-box; }
body { font-family: ui-sans-serif, system-ui, sans-serif; background: #0d0f12;
       color: #e5e7eb; margin: 0; padding: 12px 16px 40px; }
h1 { font-size: 16px; font-weight: 600; margin: 0 0 2px; }
h2 { font-size: 11px; font-weight: 600; text-transform: uppercase;
     letter-spacing: .08em; color: #9ca3af; margin: 0 0 6px; }
.sub { color: #9ca3af; font-size: 12px; margin: 0; }
header { display: flex; justify-content: space-between; gap: 16px;
         align-items: center; flex-wrap: wrap; margin-bottom: 8px; }
.meter { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }
.peak { width: 120px; height: 7px; background: #1b2028; border: 1px solid #2d333b;
        border-radius: 99px; overflow: hidden; }
.peak > i { display: block; height: 100%; width: 0; background: #34d399; }
.peak.hot > i { background: #f59e0b; }
.peak.clip > i { background: #f87171; }
.stat { font-variant-numeric: tabular-nums; font-size: 11px; color: #9ca3af; }
.stat b { color: #e5e7eb; font-weight: 600; }
.transport { display: flex; gap: 8px 10px; align-items: center; flex-wrap: wrap;
             background: #12151a; border: 1px solid #23282f; border-radius: 6px;
             padding: 5px 10px; margin-bottom: 6px; }
.transport label { font-size: 11px; color: #9ca3af; }
.transport .val { font-variant-numeric: tabular-nums; min-width: 2.8em; font-size: 11px; }
.transport select { background: #1b1f26; color: #e5e7eb; border: 1px solid #2d333b;
                    border-radius: 4px; padding: 2px 6px; max-width: 220px; font-size: 12px; }
.transport .gain { width: 140px; }
.transport .sep { width: 1px; height: 18px; background: #2d333b; margin: 0 4px; }
button { background: #1b1f26; color: #e5e7eb; border: 1px solid #2d333b;
         border-radius: 5px; padding: 4px 10px; font-size: 12px; cursor: pointer; }
button:hover { border-color: #4b5563; }
button:disabled, select:disabled { opacity: .45; cursor: not-allowed; }
button.on { border-color: #065f46; color: #6ee7b7; }
button.danger { border-color: #7f1d1d; color: #fecaca; }
.note { color: #9ca3af; font-size: 11px; margin: 0 0 6px; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
        gap: 14px; }
.card { background: #12151a; border: 1px solid #23282f; border-radius: 8px;
        padding: 12px 14px 8px; }
.card header { margin: 0 0 8px; display: block; }
.card .kind { font-family: ui-monospace, monospace; font-size: 11px; color: #7c8899; }
.row { display: grid; grid-template-columns: 92px 1fr 72px; gap: 8px;
       align-items: center; margin: 6px 0; }
.row label { font-size: 12px; color: #d1d5db; overflow: hidden; text-overflow: ellipsis; }
.row input[type=range] { width: 100%; accent-color: #60a5fa; }
.row select { width: 100%; background: #1b1f26; color: #e5e7eb;
              border: 1px solid #2d333b; border-radius: 4px; padding: 3px 6px; }
.row .val { font-variant-numeric: tabular-nums; font-size: 12px; color: #93c5fd;
            text-align: right; }
.row .doc { grid-column: 1 / -1; font-size: 11px; color: #6b7280; margin: -4px 0 4px; }
.hint { color: #6b7280; font-size: 12px; margin-top: 18px; }
.topsticky { position: sticky; top: 0; z-index: 8; background: #0d0f12;
             padding-bottom: 2px; }
.deskwrap { margin-bottom: 6px; }
#desk { display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
        gap: 4px; }
.deskrow { display: flex; flex-wrap: wrap; gap: 4px 8px; align-items: center;
           background: #12151a; border: 1px solid #2a3a32; border-radius: 5px;
           padding: 3px 8px; min-width: 0; }
.deskrow.wide { grid-column: 1 / -1; }
.deskrow .who { font-size: 10px; font-weight: 600; color: #6ee7b7; min-width: 5.4em;
                text-transform: uppercase; }
.deskrow .cell { display: flex; align-items: center; gap: 5px; }
.deskrow label { font-size: 11px; color: #d1d5db; }
.deskrow input[type=range] { width: 88px; accent-color: #34d399; }
.deskrow .val { font-variant-numeric: tabular-nums; font-size: 11px; color: #93c5fd;
                min-width: 3.2em; }
.mapwrap { background: #12151a; border: 1px solid #23282f; border-radius: 8px;
           padding: 8px 10px 6px; margin-bottom: 12px; }
.mapwrap header { display: flex; justify-content: space-between; align-items: baseline;
                  margin-bottom: 8px; }
.mapscroll { overflow: auto; max-height: 560px; }
svg.map { display: block; width: max-content; height: auto; max-width: none; }
svg.map .module { stroke: #39414d; stroke-width: 1.2; cursor: pointer; }
svg.map .module:hover, svg.map .module.on { stroke: #60a5fa; stroke-width: 1.8; }
svg.map .label { fill: #e5e7eb; font: 600 11px ui-sans-serif, sans-serif;
                 pointer-events: none; }
svg.map .kind { fill: #7c8899; font: 10px ui-monospace, monospace;
                pointer-events: none; }
svg.map .cable { fill: none; stroke: #4b5563; stroke-width: 1.3; }
svg.map .cable.feedback { stroke: #f59e0b; stroke-dasharray: 5 4; }
.card.focus { border-color: #60a5fa; }
code { font-family: ui-monospace, monospace; color: #93c5fd; }
</style>
</head>
<body>
<header>
  <div>
    <h1 id="title">kaukonen</h1>
    <p class="sub" id="sub">connecting…</p>
  </div>
  <div class="meter">
    <div class="peak" id="peakbar"><i id="peakfill"></i></div>
    <div class="stat">peak <b id="peak">—</b></div>
    <div class="stat"><b id="elapsed">0.0</b>s</div>
    <div class="stat">late <b id="late">0</b></div>
  </div>
</header>
<div class="topsticky">
<div class="transport" id="inputrow">
  <label for="gain">gain</label>
  <input id="gain" class="gain" type="range" min="0" max="1" step="0.001" value="0.3">
  <span class="val" id="gainval">0.3</span>
  <button id="mute">mute</button>
  <button id="quit" class="danger">quit</button>
  <span class="sep"></span>
  <label>capture</label>
  <button id="capture">off</button>
  <label for="indevice">device</label>
  <select id="indevice"></select>
  <label for="inchannel">ch</label>
  <select id="inchannel"></select>
  <div class="peak" id="inpeakbar"><i id="inpeakfill"></i></div>
  <div class="stat">in <b id="inpeak">—</b></div>
</div>
<p class="note" id="inputhint" hidden></p>
<div class="deskwrap" id="deskwrap" hidden>
  <div id="desk"></div>
</div>
</div>
<div class="mapwrap" id="mapwrap" hidden>
  <header>
    <h2>patch</h2>
    <span class="stat" id="mapstat"></span>
  </header>
  <div class="mapscroll" id="mapscroll"></div>
  <p class="note">Click a module to open its knobs. Dashed cables close a
  feedback loop.</p>
</div>
<div class="grid" id="grid"></div>
<p class="hint">Sound comes out of the audio interface, not this page. A
microphone goes in the same way — the browser never sees audio. These are
the same knobs as <code>set cells.rate 400</code> at the console.
A bank slider sets every instance.</p>
<script>
const $ = (id) => document.getElementById(id);
let dragging = new Set();
let started = performance.now();
let muted = false;
let last = null;

function fromNorm(k, t) {
  t = Math.min(1, Math.max(0, t));
  if (k.taper === 'log' && k.min > 0 && k.max > 0)
    return Math.exp(Math.log(k.min) + (Math.log(k.max) - Math.log(k.min)) * t);
  return k.min + (k.max - k.min) * t;
}
function toNorm(k, v) {
  if (k.taper === 'log' && k.min > 0 && k.max > 0)
    return (Math.log(v) - Math.log(k.min)) / (Math.log(k.max) - Math.log(k.min));
  if (k.max === k.min) return 0;
  return (v - k.min) / (k.max - k.min);
}
function fmt(k, v) {
  if (k.choices) return k.choices[Math.min(k.choices.length - 1, Math.max(0, Math.round(v)))];
  const a = Math.abs(v);
  const n = a >= 100 ? v.toFixed(1) : a >= 10 ? v.toFixed(2) : v.toFixed(3);
  return n.replace(/\.?0+$/, '') + (k.unit ? ' ' + k.unit : '');
}

function render(state) {
  $('title').textContent = state.patch.split('/').pop();
  const bits = [state.device || 'no device'];
  if (last && last.rate) bits.push(last.rate + ' Hz');
  const banks = state.modules.length;
  const cells = state.modules.reduce((n, m) => n + (m.count || 1), 0);
  $('sub').textContent = bits.join(' · ') + ' · ' + cells + ' modules · ' + banks + ' banks';
  if (!dragging.has('gain')) {
    $('gain').value = state.gain;
    $('gainval').textContent = Number(state.gain).toFixed(2);
  }
  muted = !!state.muted;
  $('mute').textContent = muted ? 'unmute' : 'mute';
  if (state.peak != null) {
    const p = Math.min(1, state.peak);
    $('peak').textContent = p.toFixed(3);
    $('peakfill').style.width = (p * 100) + '%';
    const bar = $('peakbar');
    bar.className = 'peak' + (p > 0.95 ? ' clip' : p > 0.7 ? ' hot' : '');
  }
  if (state.elapsed != null) $('elapsed').textContent = Number(state.elapsed).toFixed(1);
  if (state.late != null) $('late').textContent = state.late;
  renderInput(state);
  if (state.map && !$('mapscroll').dataset.drawn) drawMap(state.map);
  if (!$('desk').dataset.drawn) renderDesk(state);
  const grid = $('grid');
  if (grid.childElementCount === 0) {
    state.modules.forEach(mod => grid.appendChild(card(mod)));
  } else {
    state.modules.forEach(mod => {
      mod.knobs.forEach(k => {
        const id = mod.name + '.' + k.name;
        if (dragging.has(id)) return;
        document.querySelectorAll('[data-id="' + CSS.escape(id) + '"]').forEach(input => {
          if (k.choices) input.value = String(Math.round(k.value));
          else input.value = toNorm(k, k.value);
        });
        document.querySelectorAll('[data-read="' + CSS.escape(id) + '"]').forEach(readout => {
          readout.textContent = fmt(k, k.value);
        });
      });
    });
  }
}

const DESK_LABEL = {
  drone: 'bed', dronef: 'bed', droneamp: 'bed',
  osc: 'tone', call: 'call', throat: 'throat',
  sky: 'sky', body: 'body',
  verb: 'room', pharynx: 'pharynx', hymn: 'hymn',
  ear: 'ear', trace: 'trace', fade: 'trace',
  fbank: 'bank', band: 'band', outcomp: 'comp',
  split: 'mic split', miclim: 'limiter',
  ava: 'retreat', avb: 'advance'
};
const DESK_FALLBACK = [
  ['drone', 'tune'], ['droneamp', 'gain'], ['dronef', 'cutoff'],
  ['osc', 'tune'], ['call', 'cutoff'], ['throat', 'cutoff'],
  ['sky', 'bias'], ['body', 'cutoff'],
  ['verb', 'mix'], ['verb', 'decay'],
  ['pharynx', 'rate'], ['hymn', 'decay'],
  ['ear', 'tilt'],
  ['trace', 'natural_decay'],
  ['fbank', 'tilt'], ['fbank', 'mix'],
  ['band', 'cutoff'],
  ['outcomp', 'threshold']
];

function deskItems(state) {
  if (state.desk && state.desk.length) return state.desk;
  return DESK_FALLBACK
    .filter(([mod, knob]) => {
      const m = (state.modules || []).find(x => x.name === mod);
      return m && m.knobs.some(k => k.name === knob);
    })
    .map(([module, knob]) => ({ module, knob }));
}

function renderDesk(state) {
  const wrap = $('deskwrap');
  const host = $('desk');
  const items = deskItems(state);
  if (!items.length) {
    wrap.hidden = true;
    return;
  }
  wrap.hidden = false;
  host.innerHTML = '';
  const groups = [];
  items.forEach(item => {
    const mod = state.modules.find(m => m.name === item.module);
    if (!mod) return;
    const label = DESK_LABEL[item.module] || item.module;
    let g = groups.find(x => x.label === label);
    if (!g) {
      g = { label, knobs: [] };
      groups.push(g);
    }
    const knob = mod.knobs.find(k => k.name === item.knob);
    const key = item.module + '.' + item.knob;
    if (knob && !g.knobs.some(k => k.key === key)) {
      g.knobs.push({ spec: knob, module: item.module, key });
    }
  });
  groups.forEach(g => {
    const row = document.createElement('div');
    row.className = 'deskrow' + (g.knobs.length >= 3 ? ' wide' : '');
    row.id = 'desk-' + g.label;
    const who = document.createElement('div');
    who.className = 'who';
    who.textContent = g.label;
    row.appendChild(who);
    g.knobs.forEach(entry => {
      const k = entry.spec;
      const id = entry.module + '.' + k.name;
      const cell = document.createElement('div');
      cell.className = 'cell';
      const lab = document.createElement('label');
      lab.textContent = k.name;
      lab.title = k.doc;
      const input = document.createElement('input');
      input.type = 'range';
      input.min = '0';
      input.max = '1';
      input.step = '0.001';
      input.value = toNorm(k, k.value);
      input.dataset.id = id;
      input.title = k.doc;
      const val = document.createElement('span');
      val.className = 'val';
      val.dataset.read = id;
      val.textContent = fmt(k, k.value);
      const send = () => {
        const raw = fromNorm(k, Number(input.value));
        val.textContent = fmt(k, raw);
        post('/api/set', { module: entry.module, knob: k.name, value: raw });
      };
      input.addEventListener('pointerdown', () => dragging.add(id));
      input.addEventListener('pointerup', () => dragging.delete(id));
      input.addEventListener('input', send);
      cell.appendChild(lab);
      cell.appendChild(input);
      cell.appendChild(val);
      row.appendChild(cell);
    });
    host.appendChild(row);
  });
  host.dataset.drawn = '1';
}

function renderInput(state) {
  const inp = state.input || {};
  const hint = $('inputhint');
  const can = !!inp.available;
  const listed = inp.devices && inp.devices.length;
  $('capture').disabled = !can || !listed;
  $('indevice').disabled = !can || !listed;
  $('inchannel').disabled = !can || !listed;
  $('capture').textContent = inp.enabled ? 'on' : 'off';
  $('capture').className = inp.enabled ? 'on' : '';
  if (inp.error) {
    hint.textContent = inp.error;
  } else if (!can) {
    hint.textContent = 'this patch has no audioin, so a microphone has nowhere to go. listen.kpatch, duet.kpatch and weather.kpatch have one.';
  } else if (!listed) {
    hint.textContent = 'open with play to pick a microphone. serve has no audio.';
  } else {
    hint.textContent = '';
  }
  hint.hidden = !hint.textContent;
  fillDevices(inp);
  fillChannels(inp);
  if (state.input_peak != null) {
    const p = Math.min(1, Number(state.input_peak) || 0);
    $('inpeak').textContent = p.toFixed(3);
    $('inpeakfill').style.width = (p * 100) + '%';
    const bar = $('inpeakbar');
    bar.className = 'peak' + (p > 0.95 ? ' clip' : p > 0.7 ? ' hot' : '');
  }
}

function fillDevices(inp) {
  const sel = $('indevice');
  const devices = inp.devices || [];
  const names = devices.map(d => d.name).join('\0');
  if (sel.dataset.names !== names) {
    sel.dataset.names = names;
    sel.innerHTML = '';
    if (!devices.length) {
      const o = document.createElement('option');
      o.value = '';
      o.textContent = 'no input device';
      sel.appendChild(o);
    } else {
      devices.forEach(d => {
        const o = document.createElement('option');
        o.value = d.name;
        o.textContent = d.name + (d.is_default ? '  (default)' : '');
        sel.appendChild(o);
      });
    }
  }
  if (!dragging.has('indevice') && inp.device) sel.value = inp.device;
}

function fillChannels(inp) {
  const sel = $('inchannel');
  const n = Math.max(inp.channels || 0, inp.channel || 1, 1);
  if (sel.dataset.n !== String(n)) {
    sel.innerHTML = '';
    for (let i = 1; i <= n; i++) {
      const o = document.createElement('option');
      o.value = String(i);
      o.textContent = String(i);
      sel.appendChild(o);
    }
    sel.dataset.n = String(n);
  }
  if (!dragging.has('inchannel')) sel.value = String(inp.channel || 1);
}

function sendInput(extra) {
  const body = {
    enabled: $('capture').textContent === 'on',
    device: $('indevice').value,
    channel: Number($('inchannel').value) || 1
  };
  Object.assign(body, extra || {});
  post('/api/input', body);
}

const KIND_FILL = {
  audioin: '#14332a', audioout: '#1a2838', rc1: '#3b2f14', tc3: '#2a1f38',
  neuron: '#331e2a', vcf: '#1a2e33', svf: '#1a2e33', fbank: '#1a2e33',
  cochlea: '#1a2e33',
  verb: '#242018', comp: '#1a2420', vca: '#1a2a22', vco: '#2a2418',
  mix: '#1b2028', env: '#2a2218', noise: '#222', source: '#1f2430'
};

function drawMap(map) {
  const wrap = $('mapwrap');
  const host = $('mapscroll');
  if (!map || !map.nodes || !map.nodes.length) {
    wrap.hidden = true;
    return;
  }
  wrap.hidden = false;
  $('mapstat').textContent = map.nodes.length + ' modules · ' +
    (map.cables || []).length + ' cables';
  const BOX_W = 120, BOX_H = 34, COL = 168, ROW = 48, PAD = 14;
  const cols = map.nodes.reduce((m, n) => Math.max(m, n.col), 0) + 1;
  const rows = map.nodes.reduce((m, n) => Math.max(m, n.row), 0) + 1;
  const width = PAD * 2 + cols * COL;
  const height = PAD * 2 + rows * ROW;
  const at = (i) => {
    const n = map.nodes.find(x => x.index === i) || { col: 0, row: 0 };
    return [PAD + n.col * COL, PAD + n.row * ROW];
  };
  let svg = '<svg class="map" width="' + width + '" height="' + height +
    '" viewBox="0 0 ' + width + ' ' + height +
    '" xmlns="http://www.w3.org/2000/svg">';
  svg += '<defs><marker id="tip" viewBox="0 0 8 8" refX="7" refY="4" \
markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L8,4 L0,8 z" \
fill="#4b5563"/></marker></defs>';
  (map.cables || []).forEach(c => {
    const [x0, y0] = at(c.from);
    const [x1, y1] = at(c.to);
    const sx = x0 + BOX_W, sy = y0 + BOX_H / 2;
    const ex = x1, ey = y1 + BOX_H / 2;
    const bow = c.feedback ? -26 : 0;
    const curve = Math.max(28, Math.abs(ex - sx) * 0.4);
    const title = (map.nodes[c.from] ? map.nodes[c.from].name : c.from) + '.' +
      c.from_jack + ' → ' + (map.nodes[c.to] ? map.nodes[c.to].name : c.to) +
      '.' + c.to_jack;
    svg += '<path d="M' + sx + ',' + sy + ' C' + (sx + curve) + ',' + (sy + bow) +
      ' ' + (ex - curve) + ',' + (ey + bow) + ' ' + ex + ',' + ey +
      '" class="cable' + (c.feedback ? ' feedback' : '') +
      '" marker-end="url(#tip)"><title>' + title + '</title></path>';
  });
  map.nodes.forEach(n => {
    const [x, y] = at(n.index);
    const fill = KIND_FILL[n.kind] || '#1b2028';
    svg += '<g data-bank="' + n.bank + '" data-index="' + n.index + '">';
    svg += '<rect class="module" x="' + x + '" y="' + y + '" width="' + BOX_W +
      '" height="' + BOX_H + '" rx="6" fill="' + fill + '"/>';
    svg += '<text class="label" x="' + (x + 8) + '" y="' + (y + 14) + '">' +
      n.name + '</text>';
    svg += '<text class="kind" x="' + (x + 8) + '" y="' + (y + 27) + '">' +
      n.kind + '</text></g>';
  });
  svg += '</svg>';
  host.innerHTML = svg;
  host.dataset.drawn = '1';
  host.querySelectorAll('g[data-bank]').forEach(g => {
    g.addEventListener('click', () => focusBank(g.dataset.bank));
  });
}

function focusBank(bank) {
  document.querySelectorAll('svg.map .module').forEach(el => el.classList.remove('on'));
  document.querySelectorAll('svg.map g[data-bank="' + CSS.escape(bank) + '"] .module')
    .forEach(el => el.classList.add('on'));
  document.querySelectorAll('.card').forEach(el => el.classList.remove('focus'));
  const card = document.getElementById('desk-' + bank) ||
    document.getElementById('card-' + bank);
  if (card) {
    card.classList.add('focus');
    card.scrollIntoView({ behavior: 'smooth', block: 'center' });
  }
}

function card(mod, idPrefix) {
  const el = document.createElement('section');
  el.className = 'card';
  el.id = (idPrefix || 'card-') + mod.name;
  const title = document.createElement('h2');
  title.textContent = mod.count > 1 ? mod.name + '[' + mod.count + ']' : mod.name;
  const kind = document.createElement('div');
  kind.className = 'kind';
  kind.textContent = mod.kind + (mod.count > 1 ? ' · slider sets every instance' : '');
  el.appendChild(title);
  el.appendChild(kind);
  mod.knobs.forEach(k => {
    const id = mod.name + '.' + k.name;
    const row = document.createElement('div');
    row.className = 'row';
    const lab = document.createElement('label');
    lab.textContent = k.name;
    lab.title = k.doc;
    row.appendChild(lab);
    let input;
    if (k.choices) {
      input = document.createElement('select');
      k.choices.forEach((name, i) => {
        const o = document.createElement('option');
        o.value = String(i);
        o.textContent = name;
        input.appendChild(o);
      });
      input.value = String(Math.round(k.value));
    } else {
      input = document.createElement('input');
      input.type = 'range';
      input.min = '0';
      input.max = '1';
      input.step = '0.001';
      input.value = toNorm(k, k.value);
    }
    input.dataset.id = id;
    input.title = k.doc;
    const val = document.createElement('span');
    val.className = 'val';
    val.dataset.read = id;
    val.textContent = fmt(k, k.value);
    const send = () => {
      const raw = k.choices ? input.value : fromNorm(k, Number(input.value));
      val.textContent = fmt(k, k.choices ? Number(input.value) : raw);
      post('/api/set', { module: mod.name, knob: k.name, value: raw });
    };
    input.addEventListener('pointerdown', () => dragging.add(id));
    input.addEventListener('pointerup', () => dragging.delete(id));
    input.addEventListener('input', send);
    row.appendChild(input);
    row.appendChild(val);
    el.appendChild(row);
  });
  return el;
}

async function post(path, body) {
  try {
    await fetch(path, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    });
  } catch (e) { /* stream gone */ }
}

async function poll() {
  try {
    const r = await fetch('/api/state');
    last = await r.json();
    render(last);
    if (last.quit) {
      document.body.insertAdjacentHTML('afterbegin',
        '<p class="sub">session ended.</p>');
      return;
    }
  } catch (e) {
    $('sub').textContent = 'lost the session. is play still running?';
  }
  setTimeout(poll, 250);
}

$('gain').addEventListener('pointerdown', () => dragging.add('gain'));
$('gain').addEventListener('pointerup', () => dragging.delete('gain'));
$('gain').addEventListener('input', () => {
  const v = Number($('gain').value);
  $('gainval').textContent = v.toFixed(2);
  post('/api/gain', { value: v });
});
$('mute').addEventListener('click', () => post('/api/mute', { muted: !muted }));
$('quit').addEventListener('click', () => post('/api/quit', {}));
$('capture').addEventListener('click', () => {
  sendInput({ enabled: $('capture').textContent !== 'on' });
});
$('indevice').addEventListener('pointerdown', () => dragging.add('indevice'));
$('indevice').addEventListener('change', () => {
  dragging.delete('indevice');
  sendInput({ device: $('indevice').value, channel: 1 });
});
$('inchannel').addEventListener('pointerdown', () => dragging.add('inchannel'));
$('inchannel').addEventListener('change', () => {
  dragging.delete('inchannel');
  sendInput({ channel: Number($('inchannel').value) || 1 });
});
poll();
</script>
</body></html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use kaukonen::patch;

    fn live_surface() -> Surface {
        let text = std::fs::read_to_string("patches/live.kpatch").unwrap();
        let built = patch::parse(&text).unwrap().build().unwrap();
        Surface::from_built("patches/live.kpatch", "test device", 0.3, &built)
    }

    fn listen_surface() -> Surface {
        let text = std::fs::read_to_string("patches/listen.kpatch").unwrap();
        let built = patch::parse(&text).unwrap().build().unwrap();
        Surface::from_built("patches/listen.kpatch", "test device", 0.3, &built)
    }

    #[test]
    fn a_patch_becomes_one_card_per_bank() {
        let s = live_surface();
        let names: Vec<&str> = s.modules.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"cells"));
        assert!(names.contains(&"filt"));
        let cells = s.find_bank("cells").unwrap();
        assert_eq!(cells.count, 3);
        assert_eq!(cells.indices.len(), 3);
        assert!(cells.knobs.iter().any(|k| k.spec.name == "rate"));
    }

    #[test]
    fn turning_a_bank_knob_names_every_instance() {
        let mut s = live_surface();
        let (indices, name, value, readout) = s.set_knob("cells", "rate", "400").unwrap();
        assert_eq!(name, "rate");
        assert!((value - 400.0).abs() < 1e-3);
        assert_eq!(indices.len(), 3);
        assert!(readout.contains("400"));
        let cells = s.find_bank("cells").unwrap();
        let rate = cells.knobs.iter().find(|k| k.spec.name == "rate").unwrap();
        assert!((rate.value - 400.0).abs() < 1e-3);
    }

    #[test]
    fn an_unknown_knob_is_refused() {
        let mut s = live_surface();
        let err = s.set_knob("filt", "flux", "1").unwrap_err();
        assert!(err.contains("flux"), "{err}");
        let err = s.set_knob("ghost", "cutoff", "1").unwrap_err();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn enum_knobs_take_their_names() {
        let mut s = live_surface();
        let (_, _, value, readout) = s.set_knob("brain", "rule", "bcm").unwrap();
        assert!((value - 2.0).abs() < 1e-6 || readout == "bcm");
        assert_eq!(readout, "bcm");
    }

    #[test]
    fn json_round_trips_the_names_the_page_needs() {
        let s = live_surface();
        let json = s.to_json("\"elapsed\":1.5,\"peak\":0.2,\"late\":0");
        assert!(json.contains("\"name\":\"cells\""));
        assert!(json.contains("\"name\":\"rate\""));
        assert!(json.contains("\"taper\":\"log\""));
        assert!(json.contains("\"elapsed\":1.5"));
        assert!(json.contains("\"available\":false"));
        assert!(json.contains("\"map\":"));
        assert!(json.contains("\"from_jack\""));
        assert!(!s.map.nodes.is_empty());
        assert!(!s.map.cables.is_empty());
        assert!(!json.contains("NaN"));
    }

    #[test]
    fn duet_carries_a_map_of_every_module() {
        let text = std::fs::read_to_string("patches/duet.kpatch").unwrap();
        let built = patch::parse(&text).unwrap().build().unwrap();
        let s = Surface::from_built("patches/duet.kpatch", "test", 0.3, &built);
        assert!(s.input.available);
        assert!(s.map.nodes.len() >= 70, "nodes={}", s.map.nodes.len());
        assert!(s.map.cables.len() >= 80, "cables={}", s.map.cables.len());
        assert!(s.map.nodes.iter().any(|n| n.kind == "audioin"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "ring"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "hops"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "circ"));
        assert!(s.map.cables.iter().any(|c| c.feedback));
        let ring_wrap = s.map.cables.iter().any(|c| {
            c.feedback
                && s.map.nodes.get(c.from).is_some_and(|n| n.bank == "ring")
                && s.map.nodes.get(c.to).is_some_and(|n| n.bank == "hops")
        });
        assert!(ring_wrap, "the ring should close with a feedback hop");
    }

    #[test]
    fn weather_carries_a_map_and_a_microphone() {
        let text = std::fs::read_to_string("patches/weather.kpatch").unwrap();
        let built = patch::parse(&text).unwrap().build().unwrap();
        let s = Surface::from_built("patches/weather.kpatch", "test", 0.4, &built);
        assert!(s.input.available);
        assert!(s.map.nodes.len() >= 40, "nodes={}", s.map.nodes.len());
        assert!(s.map.nodes.iter().any(|n| n.bank == "sky"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "rain"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "grit"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "sift"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "sand"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "poke"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "echo"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "write"));
        assert!(s.map.nodes.iter().any(|n| n.name == "recoil"));
        assert!(s.map.nodes.iter().any(|n| n.name == "cloud"));
        assert!(s.map.nodes.iter().any(|n| n.name == "roomcv"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "band"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "fbank"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "verb"));
        assert!(s.map.nodes.iter().any(|n| n.name == "ava"));
        assert!(s.map.nodes.iter().any(|n| n.name == "avb"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "pharynx"));
        assert!(s.map.nodes.iter().any(|n| n.name == "hymn"));
        assert!(s.map.nodes.iter().any(|n| n.name == "throat"));
        assert!(s.map.nodes.iter().any(|n| n.name == "call"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "body"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "stretch"));
        assert!(s.map.nodes.iter().any(|n| n.name == "miclim"));
        assert!(s.map.nodes.iter().any(|n| n.name == "ear"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "tide"));
        assert!(s.map.nodes.iter().any(|n| n.name == "trace"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "replay"));
        assert!(s.map.nodes.iter().any(|n| n.bank == "outcomp"));
        assert!(s.map.nodes.iter().any(|n| n.name == "a"));
        assert!(s.map.cables.iter().any(|c| c.feedback));
        assert_eq!(s.desk[0], ("drone".into(), "tune".into()));
        assert_eq!(s.desk[1], ("droneamp".into(), "gain".into()));
        assert_eq!(s.desk[2], ("dronef".into(), "cutoff".into()));
        assert!(s.desk.iter().any(|(m, k)| m == "osc" && k == "tune"));
        assert!(s.desk.iter().any(|(m, k)| m == "sky" && k == "bias"));
        assert!(s.desk.iter().any(|(m, k)| m == "ear" && k == "tilt"));
        assert!(s
            .desk
            .iter()
            .any(|(m, k)| m == "trace" && k == "natural_decay"));
        assert!(s.desk.iter().any(|(m, k)| m == "band" && k == "cutoff"));
        assert!(s.desk.iter().any(|(m, k)| m == "pharynx" && k == "rate"));
        assert!(s.desk.iter().any(|(m, k)| m == "hymn" && k == "decay"));
        assert!(s.desk.iter().any(|(m, k)| m == "throat" && k == "cutoff"));
        assert!(s.desk.iter().any(|(m, k)| m == "call" && k == "cutoff"));
        assert!(s.desk.iter().any(|(m, k)| m == "body" && k == "cutoff"));
        assert!(s.desk.iter().any(|(m, k)| m == "fbank" && k == "tilt"));
        assert!(s.desk.iter().any(|(m, k)| m == "verb" && k == "mix"));
        assert!(s.desk.iter().any(|(m, k)| m == "outcomp" && k == "threshold"));
        assert!(!s.desk.iter().any(|(m, _)| m == "miclim" || m == "ava"));
        let json = s.to_json("");
        assert!(json.contains("\"desk\":["));
        assert!(json.contains("\"module\":\"drone\""));
    }

    #[test]
    fn weather_keeps_the_tone_strip_without_a_desk_line() {
        let text = std::fs::read_to_string("patches/weather.kpatch")
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with("desk "))
            .collect::<Vec<_>>()
            .join("\n");
        let built = patch::parse(&text).unwrap().build().unwrap();
        assert!(built.desk.is_empty());
        let s = Surface::from_built("patches/weather.kpatch", "test", 0.4, &built);
        assert_eq!(s.desk[0], ("drone".into(), "tune".into()));
        assert!(s.desk.iter().any(|(m, k)| m == "osc" && k == "tune"));
        assert!(s.desk.iter().any(|(m, k)| m == "sky" && k == "bias"));
        assert!(s
            .desk
            .iter()
            .any(|(m, k)| m == "trace" && k == "natural_decay"));
        assert!(s.desk.iter().any(|(m, k)| m == "band" && k == "cutoff"));
        assert!(s.desk.iter().any(|(m, k)| m == "pharynx" && k == "rate"));
        assert!(s.desk.iter().any(|(m, k)| m == "throat" && k == "cutoff"));
        assert!(s.desk.iter().any(|(m, k)| m == "call" && k == "cutoff"));
        assert!(s.desk.iter().any(|(m, k)| m == "body" && k == "cutoff"));
        assert!(s.desk.iter().any(|(m, k)| m == "fbank" && k == "tilt"));
        assert!(s.desk.iter().any(|(m, k)| m == "verb" && k == "mix"));
        assert!(s.desk.iter().any(|(m, k)| m == "outcomp" && k == "threshold"));
    }

    #[test]
    fn a_loopback_weather_page_serves_the_tone_strip() {
        let text = std::fs::read_to_string("patches/weather.kpatch").unwrap();
        let built = patch::parse(&text).unwrap().build().unwrap();
        let surface = Surface::from_built("patches/weather.kpatch", "test", 0.4, &built);
        let panel = start(surface, Arc::new(Silent), 0).expect("bind");
        let base = url(panel.addr);
        let page = get(&format!("{base}/"));
        assert!(page.contains("id=\"deskwrap\""), "{page}");
        assert!(page.contains("bed"), "{page}");
        assert!(page.contains("tone"), "{page}");
        assert!(page.contains("throat"), "{page}");
        let state = get(&format!("{base}/api/state"));
        assert!(state.contains("\"module\":\"drone\""), "{state}");
        assert!(state.contains("\"module\":\"osc\""), "{state}");
        assert!(state.contains("\"module\":\"sky\""), "{state}");
        assert!(state.contains("\"module\":\"trace\""), "{state}");
        assert!(state.contains("\"module\":\"throat\""), "{state}");
        assert!(state.contains("\"module\":\"call\""), "{state}");
        assert!(state.contains("\"module\":\"body\""), "{state}");
        assert!(state.contains("\"module\":\"fbank\""), "{state}");
        assert!(state.contains("\"module\":\"verb\""), "{state}");
        assert!(state.contains("\"module\":\"outcomp\""), "{state}");
        let response = post(
            &format!("{base}/api/set"),
            r#"{"module":"drone","knob":"tune","value":55}"#,
        );
        assert!(response.contains("\"ok\":true"), "{response}");
        panel.stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn live_has_no_audioin_and_listen_does() {
        assert!(!live_surface().input.available);
        assert!(listen_surface().input.available);
        let err = live_surface()
            .input
            .merge(Some(true), None, None)
            .unwrap_err();
        assert!(err.contains("audioin"), "{err}");
    }

    #[test]
    fn input_merge_checks_the_channel() {
        let mut s = listen_surface();
        s.input.devices.push(InputDevice {
            name: "Mic".into(),
            channels: 2,
            is_default: true,
        });
        s.input.channels = 2;
        let next = s
            .input
            .merge(Some(true), Some("Mic".into()), Some(2))
            .unwrap();
        assert!(next.enabled);
        assert_eq!(next.channel, 2);
        let err = s.input.merge(None, None, Some(3)).unwrap_err();
        assert!(err.contains("channel 3"), "{err}");
        let err = s.input.merge(None, None, Some(0)).unwrap_err();
        assert!(err.contains("numbered from 1"), "{err}");
    }

    #[test]
    fn the_tiny_json_reader_gets_strings_and_numbers() {
        let body = r#"{"module":"cells","knob":"rate","value":400.5,"muted":true}"#;
        assert_eq!(json_string(body, "module").unwrap(), "cells");
        assert_eq!(json_string(body, "knob").unwrap(), "rate");
        assert!((json_f32(body, "value").unwrap() - 400.5).abs() < 1e-5);
        assert_eq!(json_bool(body, "muted"), Some(true));
        assert_eq!(json_raw(body, "value").unwrap(), "400.5");
    }

    #[test]
    fn parse_request_splits_a_post() {
        let req = "POST /api/set HTTP/1.1\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        let (method, path, body) = parse_request(req).unwrap();
        assert_eq!(method, "POST");
        assert_eq!(path, "/api/set");
        assert_eq!(body, "{\"a\":1}");
    }

    #[test]
    fn the_page_has_somewhere_to_put_the_knobs() {
        assert!(PAGE.contains("id=\"grid\""));
        assert!(PAGE.contains("/api/set"));
        assert!(PAGE.contains("fromNorm"));
        assert!(PAGE.contains("/api/input"));
        assert!(PAGE.contains("id=\"capture\""));
        assert!(PAGE.contains("id=\"indevice\""));
        assert!(PAGE.contains("id=\"inchannel\""));
        assert!(PAGE.contains("id=\"inpeak\""));
        assert!(PAGE.contains("id=\"mapwrap\""));
        assert!(PAGE.contains("id=\"deskwrap\""));
        assert!(PAGE.contains("topsticky"));
        assert!(PAGE.contains("#desk { display: grid;"));
        assert!(PAGE.contains("mic split"));
        assert!(PAGE.contains("renderDesk"));
        assert!(PAGE.contains("drawMap"));
        assert!(PAGE.contains("focusBank"));
    }

    #[test]
    fn a_loopback_server_answers_and_turns_a_knob() {
        let surface = live_surface();
        let panel = start(surface, Arc::new(Silent), 0).expect("bind");
        let base = url(panel.addr);
        let page = get(&format!("{base}/"));
        assert!(page.contains("kaukonen"));
        assert!(page.contains("id=\"grid\""));
        let state = get(&format!("{base}/api/state"));
        assert!(state.contains("\"name\":\"cells\""));
        let response = post(
            &format!("{base}/api/set"),
            r#"{"module":"cells","knob":"rate","value":512}"#,
        );
        assert!(response.contains("\"ok\":true"), "{response}");
        let locked = panel.surface.lock().unwrap();
        let rate = locked
            .find_bank("cells")
            .unwrap()
            .knobs
            .iter()
            .find(|k| k.spec.name == "rate")
            .unwrap();
        assert!((rate.value - 512.0).abs() < 1e-3);
        panel.stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn serve_refuses_to_open_a_microphone() {
        let surface = listen_surface();
        let panel = start(surface, Arc::new(Silent), 0).expect("bind");
        let base = url(panel.addr);
        let state = get(&format!("{base}/api/state"));
        assert!(state.contains("\"available\":true"), "{state}");
        assert!(state.contains("\"devices\":[]"), "{state}");
        let refused = post(
            &format!("{base}/api/input"),
            r#"{"enabled":true,"channel":1}"#,
        );
        assert!(refused.contains("\"ok\":false"), "{refused}");
        assert!(refused.contains("play"), "{refused}");
        let channel = post(&format!("{base}/api/input"), r#"{"channel":1}"#);
        assert!(channel.contains("\"ok\":true"), "{channel}");
        let live = live_surface();
        let live_panel = start(live, Arc::new(Silent), 0).expect("bind");
        let no_adc = post(
            &format!("{}/api/input", url(live_panel.addr)),
            r#"{"enabled":true}"#,
        );
        assert!(no_adc.contains("audioin"), "{no_adc}");
        panel.stop.store(true, Ordering::Relaxed);
        live_panel.stop.store(true, Ordering::Relaxed);
    }

    fn get(url: &str) -> String {
        fetch("GET", url, None)
    }

    fn post(url: &str, body: &str) -> String {
        fetch("POST", url, Some(body))
    }

    fn fetch(method: &str, url: &str, body: Option<&str>) -> String {
        let url = url.strip_prefix("http://").unwrap();
        let (host, path) = url.split_once('/').unwrap();
        let mut stream = TcpStream::connect(host).unwrap();
        let extra = match body {
            Some(b) => format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}",
                b.len()
            ),
            None => "\r\n".to_string(),
        };
        stream
            .write_all(format!("{method} /{path} HTTP/1.1\r\nHost: {host}\r\n{extra}").as_bytes())
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf);
        text.split("\r\n\r\n").nth(1).unwrap_or(&text).to_string()
    }
}
