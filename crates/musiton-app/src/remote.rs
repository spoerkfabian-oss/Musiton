//! Remote control for automated tests on a real machine.
//!
//! `musiton --remote-control` opens a control port on 127.0.0.1 (never on the network)
//! and writes its port and a random token to `remote.json` in the data folder. Each
//! connection sends one JSON line (`{"token": …, "cmd": …, …}`) and gets one JSON line
//! back. `musiton --remote '<json>'` is a client that adds the token, sends the command and
//! prints the answer, so a shell on the same machine can drive Musiton.
//!
//! Mouse, keyboard and text commands are fed into the window's input as if a person had
//! used it, one step per frame; positions are in screenshot pixels.

use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Switch that turns the control port on.
pub const SERVER_ARG: &str = "--remote-control";
/// Switch for the client: `--remote '<json command>'`.
pub const CLIENT_ARG: &str = "--remote";
/// How long the client and the port wait for Musiton to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest request line accepted.
const MAX_REQUEST: u64 = 1 << 20;

/// Where the running app can be reached.
#[derive(Debug, Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
}

fn endpoint_file() -> Option<PathBuf> {
    crate::settings::Settings::data_dir().map(|d| d.join("remote.json"))
}

/// One command of the protocol.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Song, tracks, transport, levels, dropouts, status and plugin problems.
    State,
    /// Open a song file.
    Open {
        /// The file.
        path: PathBuf,
    },
    /// Save the song (to `path`, or to its current file).
    Save {
        /// New file name, if any.
        path: Option<PathBuf>,
    },
    /// Start playback.
    Play,
    /// Stop playback.
    Stop,
    /// Move the playhead to a bar (1 = song start; fractions allowed).
    Locate {
        /// Bar number.
        bar: f64,
    },
    /// Click at a position.
    Click {
        /// X in screenshot pixels.
        x: f32,
        /// Y in screenshot pixels.
        y: f32,
        /// "left" (default), "right" or "middle".
        #[serde(default)]
        button: Option<String>,
        /// Double click.
        #[serde(default)]
        double: bool,
        /// Modifier keys held: any of "ctrl", "shift", "alt".
        #[serde(default)]
        mods: Vec<String>,
    },
    /// Press at one position, move and release at another.
    Drag {
        /// Start [x, y] in screenshot pixels.
        from: [f32; 2],
        /// End [x, y] in screenshot pixels.
        to: [f32; 2],
        /// Modifier keys held.
        #[serde(default)]
        mods: Vec<String>,
    },
    /// Move the mouse (hover).
    Hover {
        /// X in screenshot pixels.
        x: f32,
        /// Y in screenshot pixels.
        y: f32,
    },
    /// Turn the mouse wheel at a position (positive `dy` scrolls up).
    Scroll {
        /// X in screenshot pixels.
        x: f32,
        /// Y in screenshot pixels.
        y: f32,
        /// Amount in points.
        dy: f32,
        /// Modifier keys held.
        #[serde(default)]
        mods: Vec<String>,
    },
    /// Press a key (egui key names: "Space", "Enter", "A", "F5", "ArrowLeft" …).
    Key {
        /// The key.
        key: String,
        /// Modifier keys held.
        #[serde(default)]
        mods: Vec<String>,
    },
    /// Type text into the focused field.
    Text {
        /// The text.
        text: String,
    },
    /// Save a PNG of the window.
    Screenshot {
        /// The file to write.
        path: PathBuf,
    },
    /// The last lines of the log file.
    Log {
        /// How many lines (default 50).
        #[serde(default)]
        lines: Option<usize>,
    },
    /// Close Musiton without asking (unsaved changes are lost).
    Quit,
}

/// A command waiting for the app, with the way back to its client.
pub struct Request {
    /// What to do.
    pub command: Command,
    /// Where the answer goes.
    pub reply: mpsc::Sender<Value>,
}

/// The control port of a running app.
pub struct Server {
    rx: mpsc::Receiver<Request>,
}

impl Server {
    /// Opens the port and starts answering connections; `ctx` is woken for each request.
    pub fn start(ctx: egui::Context) -> anyhow::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let endpoint = Endpoint {
            port: listener.local_addr()?.port(),
            token: new_token(),
        };
        let file = endpoint_file().ok_or_else(|| anyhow::anyhow!("no data folder"))?;
        std::fs::write(&file, serde_json::to_string(&endpoint)?)?;
        tracing::info!(
            "remote control on 127.0.0.1:{} ({})",
            endpoint.port,
            file.display()
        );
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("remote-control".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    if let Err(e) = serve(stream, &endpoint.token, &tx, &ctx) {
                        tracing::warn!("remote control: {e}");
                    }
                }
            })?;
        Ok(Self { rx })
    }

    /// Requests that arrived since the last call.
    pub fn take(&self) -> Vec<Request> {
        self.rx.try_iter().collect()
    }
}

/// A random token (from the standard library's randomly keyed hasher).
fn new_token() -> String {
    let part = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

fn serve(
    stream: TcpStream,
    token: &str,
    tx: &mpsc::Sender<Request>,
    ctx: &egui::Context,
) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_REQUEST)).read_line(&mut line)?;
    let answer = match serde_json::from_str::<Value>(&line) {
        Ok(v) if v.get("token").and_then(Value::as_str) == Some(token) => {
            match serde_json::from_value::<Command>(v) {
                Ok(command) => {
                    let (reply, answer) = mpsc::channel();
                    tx.send(Request { command, reply })?;
                    ctx.request_repaint();
                    answer
                        .recv_timeout(ANSWER_TIMEOUT)
                        .unwrap_or_else(|_| json!({"ok": false, "error": "no answer from the app"}))
                }
                Err(e) => json!({"ok": false, "error": format!("bad command: {e}")}),
            }
        }
        Ok(_) => json!({"ok": false, "error": "wrong token"}),
        Err(e) => json!({"ok": false, "error": format!("not JSON: {e}")}),
    };
    let mut stream = stream;
    writeln!(stream, "{answer}")?;
    Ok(())
}

/// Client mode: sends `command` (JSON) to the running app and prints the answer.
/// Exit code 0 when the answer says ok, 1 otherwise.
pub fn client(command: &str) -> ! {
    let result = (|| -> anyhow::Result<Value> {
        let file = endpoint_file().ok_or_else(|| anyhow::anyhow!("no data folder"))?;
        let endpoint: Endpoint = serde_json::from_slice(&std::fs::read(&file).map_err(|e| {
            anyhow::anyhow!(
                "Musiton is not running with {SERVER_ARG} ({}: {e})",
                file.display()
            )
        })?)?;
        let mut request: Value = serde_json::from_str(command)?;
        request
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("the command must be a JSON object"))?
            .insert("token".into(), Value::String(endpoint.token));
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoint.port))?;
        stream.set_read_timeout(Some(ANSWER_TIMEOUT))?;
        writeln!(stream, "{request}")?;
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer)?;
        Ok(serde_json::from_str(&answer)?)
    })();
    let answer = result.unwrap_or_else(|e| json!({"ok": false, "error": e.to_string()}));
    println!("{answer}");
    let ok = answer.get("ok").and_then(Value::as_bool) == Some(true);
    std::process::exit(if ok { 0 } else { 1 })
}

/// One frame's worth of fake input, and the answer to send once it was fed.
struct Step {
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    done: Option<mpsc::Sender<Value>>,
}

/// Turns commands into input events, one step per frame.
#[derive(Default)]
pub struct Injector {
    steps: VecDeque<Step>,
}

fn modifiers(mods: &[String]) -> egui::Modifiers {
    let mut m = egui::Modifiers::default();
    for name in mods {
        match name.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => {
                m.ctrl = true;
                m.command = true;
            }
            "shift" => m.shift = true,
            "alt" => m.alt = true,
            _ => {}
        }
    }
    m
}

impl Injector {
    /// Whether input is still waiting to be fed.
    pub fn busy(&self) -> bool {
        !self.steps.is_empty()
    }

    fn push(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
        self.steps.push_back(Step {
            events,
            modifiers,
            done: None,
        });
    }

    /// Queues the input of `command` (scaled by `ppp` pixels per point); `reply` is
    /// answered after the last step. Returns `false` for commands that are not input.
    pub fn queue(&mut self, command: &Command, ppp: f32, reply: &mpsc::Sender<Value>) -> bool {
        let at = |x: f32, y: f32| egui::pos2(x / ppp, y / ppp);
        let button = |pos, button, pressed, modifiers| egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        };
        let key = |key, pressed, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        match command {
            Command::Click {
                x,
                y,
                button: which,
                double,
                mods,
            } => {
                let m = modifiers(mods);
                let b = match which.as_deref() {
                    Some("right") => egui::PointerButton::Secondary,
                    Some("middle") => egui::PointerButton::Middle,
                    _ => egui::PointerButton::Primary,
                };
                let p = at(*x, *y);
                self.push(vec![egui::Event::PointerMoved(p)], m);
                for _ in 0..if *double { 2 } else { 1 } {
                    self.push(vec![button(p, b, true, m)], m);
                    self.push(vec![button(p, b, false, m)], m);
                }
            }
            Command::Drag { from, to, mods } => {
                let m = modifiers(mods);
                let (a, b) = (at(from[0], from[1]), at(to[0], to[1]));
                self.push(vec![egui::Event::PointerMoved(a)], m);
                self.push(vec![button(a, egui::PointerButton::Primary, true, m)], m);
                for i in 1..=10 {
                    self.push(
                        vec![egui::Event::PointerMoved(a.lerp(b, i as f32 / 10.0))],
                        m,
                    );
                }
                self.push(vec![button(b, egui::PointerButton::Primary, false, m)], m);
            }
            Command::Hover { x, y } => {
                self.push(
                    vec![egui::Event::PointerMoved(at(*x, *y))],
                    egui::Modifiers::default(),
                );
            }
            Command::Scroll { x, y, dy, mods } => {
                let m = modifiers(mods);
                self.push(vec![egui::Event::PointerMoved(at(*x, *y))], m);
                self.push(
                    vec![egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, *dy),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Wheel,
                        modifiers: m,
                    }],
                    m,
                );
            }
            Command::Key { key: name, mods } => {
                let Some(k) = egui::Key::from_name(name) else {
                    let _ =
                        reply.send(json!({"ok": false, "error": format!("unknown key {name}")}));
                    return true;
                };
                let m = modifiers(mods);
                self.push(vec![key(k, true, m)], m);
                self.push(vec![key(k, false, m)], m);
            }
            Command::Text { text } => {
                self.push(
                    vec![egui::Event::Text(text.clone())],
                    egui::Modifiers::default(),
                );
            }
            _ => return false,
        }
        if let Some(last) = self.steps.back_mut() {
            last.done = Some(reply.clone());
        }
        true
    }

    /// Feeds the next step into this frame's input.
    pub fn feed(&mut self, raw: &mut egui::RawInput) {
        if let Some(step) = self.steps.pop_front() {
            raw.events
                .push(egui::Event::ModifiersChanged(step.modifiers));
            raw.events.extend(step.events);
            if self.steps.is_empty() {
                raw.events
                    .push(egui::Event::ModifiersChanged(egui::Modifiers::default()));
            }
            if let Some(done) = step.done {
                let _ = done.send(json!({"ok": true}));
            }
        }
    }
}

/// Writes `image` as a PNG file.
pub fn save_png(image: &egui::ColorImage, path: &Path) -> anyhow::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let [w, h] = image.size;
    let mut encoder = png::Encoder::new(file, w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    writer.write_image_data(&bytes)?;
    Ok(())
}

/// The last `lines` lines of the log file.
pub fn log_tail(lines: usize) -> String {
    let text = crate::settings::Settings::data_dir()
        .and_then(|d| std::fs::read_to_string(d.join("musiton.log")).ok())
        .unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse_and_clicks_become_input_steps() {
        let c: Command =
            serde_json::from_str(r#"{"cmd":"click","x":200,"y":100,"double":true}"#).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut inj = Injector::default();
        assert!(inj.queue(&c, 2.0, &tx));
        let mut raw = egui::RawInput::default();
        let mut presses = 0;
        while inj.busy() {
            assert!(rx.try_recv().is_err(), "answered before the input was fed");
            raw.events.clear();
            inj.feed(&mut raw);
            for e in &raw.events {
                if let egui::Event::PointerButton { pos, pressed, .. } = e {
                    assert_eq!(*pos, egui::pos2(100.0, 50.0));
                    presses += usize::from(*pressed);
                }
            }
        }
        assert_eq!(presses, 2);
        assert_eq!(rx.try_recv().unwrap()["ok"], true);

        let c: Command = serde_json::from_str(r#"{"cmd":"state"}"#).unwrap();
        assert!(!inj.queue(&c, 1.0, &tx));
        let c: Command = serde_json::from_str(r#"{"cmd":"key","key":"Nope"}"#).unwrap();
        assert!(inj.queue(&c, 1.0, &tx));
        assert_eq!(rx.try_recv().unwrap()["ok"], false);
        assert!(serde_json::from_str::<Command>(r#"{"cmd":"fly"}"#).is_err());
        assert_eq!(new_token().len(), 32);
        assert_ne!(new_token(), new_token());
    }
}
