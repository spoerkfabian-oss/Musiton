//! Sound library: free sample libraries (SFZ) downloaded from their original sources into
//! `<data dir>/Library/<id>/`, in the background, with progress, resume (HTTP range
//! requests, where the server allows them) and integrity checks (every file in the archive
//! carries a CRC-32 that is checked while unpacking). Too big for the installer.

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;

/// A downloadable library.
pub struct Library {
    /// Folder name under `Library/`.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// What is in it.
    pub description: &'static str,
    /// Licence, as stated by the source.
    pub licence: &'static str,
    /// Where it comes from (shown to the user).
    pub source: &'static str,
    /// Zip archive to download.
    pub url: &'static str,
    /// Rough download size for the list (the server's size is shown once known).
    pub approx_size: &'static str,
}

/// The libraries on offer (licences checked at the source, 2026-11-26).
pub const LIBRARIES: &[Library] = &[
    Library {
        id: "vcsl",
        name: "VCSL (Versilian Community Sample Library)",
        description: "Timpani, orchestral and world percussion, gongs, cymbals, bells, harp, pianos, harpsichords, pipe organ, recorders, saxophones, harmonicas … (180+ instruments)",
        licence: "CC0 (public domain)",
        source: "github.com/sgossner/VCSL",
        url: "https://github.com/sgossner/VCSL/archive/refs/heads/sfz.zip",
        approx_size: "several GB",
    },
    Library {
        id: "vsco2",
        name: "VSCO-2 Community Edition",
        description: "Orchestra: strings, brass, woodwinds, percussion",
        licence: "CC0 (public domain)",
        source: "github.com/sgossner/VSCO-2-CE",
        url: "https://github.com/sgossner/VSCO-2-CE/archive/refs/tags/1.1.0.zip",
        approx_size: "about 3 GB",
    },
    Library {
        id: "salamander",
        name: "Salamander Grand Piano V3",
        description: "Yamaha C5 grand piano, 16 velocity layers, release and string resonance samples",
        licence: "CC BY 3.0 (credit: Alexander Holm)",
        source: "github.com/sfzinstruments/SalamanderGrandPiano",
        url: "https://github.com/sfzinstruments/SalamanderGrandPiano/archive/refs/heads/master.zip",
        approx_size: "about 1.2 GB",
    },
    Library {
        id: "virtuosity",
        name: "Virtuosity Drums",
        description: "Acoustic drum kit played with sticks, several microphone positions (Programs: basic and full kit)",
        licence: "CC0 (public domain)",
        source: "github.com/sfzinstruments/virtuosity_drums",
        url: "https://github.com/sfzinstruments/virtuosity_drums/archive/refs/heads/master.zip",
        approx_size: "size not stated",
    },
    Library {
        id: "big-rusty-drums",
        name: "Big Rusty Drums (Karoryfer)",
        description: "Big, raw acoustic drum kit with close and room microphones",
        licence: "CC0 (public domain)",
        source: "github.com/sfzinstruments/karoryfer.big-rusty-drums",
        url: "https://github.com/sfzinstruments/karoryfer.big-rusty-drums/archive/refs/heads/main.zip",
        approx_size: "size not stated",
    },
    Library {
        id: "black-and-blue-basses",
        name: "Black and Blue Basses (Karoryfer)",
        description: "Two electric bass guitars, fingered and picked, with articulation key switches",
        licence: "CC0 (public domain)",
        source: "github.com/sfzinstruments/karoryfer.black-and-blue-basses",
        url: "https://github.com/sfzinstruments/karoryfer.black-and-blue-basses/archive/refs/heads/main.zip",
        approx_size: "size not stated",
    },
    Library {
        id: "black-and-green-guitars",
        name: "Black and Green Guitars (Karoryfer)",
        description: "Two electric guitars (Gretsch and Hofner): sustain, staccato, twang, with key switches",
        licence: "CC0 (public domain)",
        source: "github.com/sfzinstruments/karoryfer.black-and-green-guitars",
        url: "https://github.com/sfzinstruments/karoryfer.black-and-green-guitars/archive/refs/heads/main.zip",
        approx_size: "size not stated",
    },
    Library {
        id: "meatbass",
        name: "Meatbass (Karoryfer)",
        description: "Double bass, arco and pizzicato",
        licence: "CC0 (public domain)",
        source: "github.com/sfzinstruments/karoryfer.meatbass",
        url: "https://github.com/sfzinstruments/karoryfer.meatbass/archive/refs/heads/master.zip",
        approx_size: "size not stated",
    },
];

/// Where a download stands.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Fetching the archive.
    Downloading,
    /// Unpacking (files done, files in total).
    Unpacking(usize, usize),
    /// Ready to use.
    Done,
    /// Stopped by the user (the partial file is kept for resuming).
    Cancelled,
    /// Failed with this message.
    Failed(String),
}

/// A download running in the background (shared with the UI).
#[derive(Clone)]
pub struct Job {
    /// Bytes downloaded and expected (0 = unknown).
    pub done: Arc<AtomicU64>,
    /// See `done`.
    pub total: Arc<AtomicU64>,
    /// Set to stop.
    pub cancel: Arc<AtomicBool>,
    /// Current state.
    pub status: Arc<Mutex<Status>>,
}

impl Job {
    fn new() -> Self {
        Self {
            done: Arc::default(),
            total: Arc::default(),
            cancel: Arc::default(),
            status: Arc::new(Mutex::new(Status::Downloading)),
        }
    }

    /// The current state.
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .map_or(Status::Failed("lost".into()), |s| s.clone())
    }

    fn set(&self, s: Status) {
        if let Ok(mut st) = self.status.lock() {
            *st = s;
        }
    }
}

/// `<data dir>/Library`.
pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("Library")
}

/// Folder of an installed library (it exists only once unpacked completely).
pub fn installed_dir(data_dir: &Path, id: &str) -> Option<PathBuf> {
    let dir = root(data_dir).join(id);
    dir.join(".complete").exists().then_some(dir)
}

/// Starts downloading `lib` into `data_dir` on a background thread.
pub fn start(lib: &Library, data_dir: &Path, agent: ureq::Agent) -> Job {
    let job = Job::new();
    let (url, dir, j) = (lib.url.to_owned(), root(data_dir), job.clone());
    let id = lib.id.to_owned();
    std::thread::spawn(move || {
        let result = install(&agent, &url, &dir, &id, &j);
        j.set(match result {
            Ok(()) => Status::Done,
            Err(_) if j.cancel.load(Ordering::Relaxed) => Status::Cancelled,
            Err(e) => Status::Failed(e.to_string()),
        });
    });
    job
}

/// An HTTP client for downloads (system proxy settings from the environment).
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent("Musiton")
        .build()
        .into()
}

fn install(agent: &ureq::Agent, url: &str, dir: &Path, id: &str, job: &Job) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let part = dir.join(format!("{id}.zip.part"));
    download(agent, url, &part, job)?;
    job.set(Status::Unpacking(0, 0));
    let target = dir.join(id);
    if target.exists() {
        std::fs::remove_dir_all(&target)?;
    }
    unpack(&part, &target, job)?;
    std::fs::write(target.join(".complete"), url)?;
    std::fs::remove_file(&part)?;
    Ok(())
}

/// Downloads `url` to `part`, continuing a partial file when the server allows it.
fn download(agent: &ureq::Agent, url: &str, part: &Path, job: &Job) -> anyhow::Result<()> {
    let have = std::fs::metadata(part).map_or(0, |m| m.len());
    let mut req = agent.get(url);
    if have > 0 {
        req = req.header("Range", &format!("bytes={have}-"));
    }
    let mut resp = req.call()?;
    let status = resp.status().as_u16();
    let length = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let (mut file, start) = match status {
        206 => {
            let mut f = std::fs::OpenOptions::new().append(true).open(part)?;
            f.seek(std::io::SeekFrom::End(0))?;
            (f, have)
        }
        200 => (std::fs::File::create(part)?, 0),
        // The partial file is already complete.
        416 if have > 0 => {
            job.done.store(have, Ordering::Relaxed);
            job.total.store(have, Ordering::Relaxed);
            return Ok(());
        }
        s => anyhow::bail!("the server answered {s}"),
    };
    job.done.store(start, Ordering::Relaxed);
    job.total
        .store(length.map_or(0, |l| l + start), Ordering::Relaxed);
    let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        if job.cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(buf.get(..n).unwrap_or_default())?;
        job.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    file.flush()?;
    let got = job.done.load(Ordering::Relaxed);
    if length.is_some_and(|l| got != l + start) {
        anyhow::bail!("the download ended early ({got} bytes); try again to continue");
    }
    Ok(())
}

/// Unpacks the zip `archive` into `target` (CRC-checked per file).
fn unpack(archive: &Path, target: &Path, job: &Job) -> anyhow::Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    let n = zip.len();
    for i in 0..n {
        if job.cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        job.set(Status::Unpacking(i, n));
        let mut entry = zip.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let path = target.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&path)?;
        // Reading to the end checks the entry's CRC-32 (a mismatch is an error).
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

/// The `.sfz` files of an installed library: (name relative to the library, path).
pub fn instruments(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    // File names that another .sfz includes: parts of an instrument, not instruments.
    let mut included: Vec<String> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sfz")) {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    included.extend(text.lines().filter_map(|l| {
                        let rest = l.trim_start().strip_prefix("#include")?;
                        let name = rest.trim().trim_matches('"');
                        Some(name.rsplit(['/', '\\']).next()?.to_owned())
                    }));
                }
                let rel = p.strip_prefix(dir).unwrap_or(&p);
                out.push((rel.with_extension("").to_string_lossy().into_owned(), p));
            }
        }
    }
    out.retain(|(_, p)| {
        p.file_name()
            .is_none_or(|n| !included.iter().any(|i| *i == n.to_string_lossy()))
    });
    out.sort();
    out
}

/// Options → Sound library: download, cancel, continue and delete libraries.
#[derive(Default)]
pub struct Dialog {
    /// Window shown.
    pub open: bool,
    jobs: std::collections::HashMap<&'static str, Job>,
    agent: Option<ureq::Agent>,
    /// Library whose deletion waits for a second click.
    confirm_delete: Option<&'static str>,
}

impl Dialog {
    /// Draws the window (and keeps repainting while downloads run).
    pub fn show(&mut self, ctx: &egui::Context, data_dir: Option<&Path>) {
        let running = self
            .jobs
            .values()
            .any(|j| matches!(j.status(), Status::Downloading | Status::Unpacking(..)));
        if running {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Sound library")
            .open(&mut open)
            .collapsible(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                let Some(data) = data_dir else {
                    ui.label("No data folder to put the libraries in.");
                    return;
                };
                ui.label(
                    "Free sample libraries, downloaded from their original sources. \
                     Use them with the \"Sample instrument\" (Load SFZ…).",
                );
                ui.separator();
                for lib in LIBRARIES {
                    self.library_row(ui, lib, data);
                    ui.separator();
                }
            });
        self.open = open;
    }

    fn library_row(&mut self, ui: &mut egui::Ui, lib: &'static Library, data: &Path) {
        ui.strong(lib.name);
        ui.label(lib.description);
        ui.weak(format!("Licence: {} · Source: {}", lib.licence, lib.source));
        let job = self.jobs.get(lib.id).cloned();
        let status = job.as_ref().map(Job::status);
        ui.horizontal(|ui| match (&job, &status) {
            (Some(j), Some(Status::Downloading)) => {
                let (done, total) = (
                    j.done.load(Ordering::Relaxed),
                    j.total.load(Ordering::Relaxed),
                );
                let mb = |b: u64| b as f64 / 1e6;
                let text = if total > 0 {
                    format!("{:.0} / {:.0} MB", mb(done), mb(total))
                } else {
                    format!("{:.0} MB", mb(done))
                };
                let frac = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                ui.add(egui::ProgressBar::new(frac).text(text).desired_width(300.0));
                if ui.button("Cancel").clicked() {
                    j.cancel.store(true, Ordering::Relaxed);
                }
            }
            (Some(j), Some(Status::Unpacking(i, n))) => {
                let frac = if *n > 0 { *i as f32 / *n as f32 } else { 0.0 };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .text(format!("Unpacking {i} / {n}"))
                        .desired_width(300.0),
                );
                if ui.button("Cancel").clicked() {
                    j.cancel.store(true, Ordering::Relaxed);
                }
            }
            _ => match installed_dir(data, lib.id) {
                Some(dir) => {
                    ui.label(format!(
                        "Installed: {} instruments",
                        instruments(&dir).len()
                    ));
                    let sure = self.confirm_delete == Some(lib.id);
                    let label = if sure { "Really delete?" } else { "Delete" };
                    if ui
                        .button(label)
                        .on_hover_text("Frees the disk space; it can be downloaded again")
                        .clicked()
                    {
                        if sure {
                            if let Err(e) = std::fs::remove_dir_all(&dir) {
                                tracing::error!("deleting {}: {e}", dir.display());
                            }
                            self.jobs.remove(lib.id);
                            self.confirm_delete = None;
                        } else {
                            self.confirm_delete = Some(lib.id);
                        }
                    }
                }
                None => {
                    match &status {
                        Some(Status::Failed(e)) => {
                            ui.colored_label(ui.visuals().error_fg_color, format!("Failed: {e}"));
                        }
                        Some(Status::Cancelled) => {
                            ui.label("Stopped.");
                        }
                        _ => {}
                    }
                    let partial = root(data).join(format!("{}.zip.part", lib.id)).exists();
                    let label = if partial {
                        "Continue download".to_owned()
                    } else {
                        format!("Download ({})", lib.approx_size)
                    };
                    if ui.button(label).clicked() {
                        let agent = self.agent.get_or_insert_with(agent).clone();
                        self.jobs.insert(lib.id, start(lib, data, agent));
                    }
                }
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// A zip with one instrument.
    fn test_zip() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("Lib-main/Piano/piano.sfz", opts).unwrap();
        w.write_all(b"#include \"parts/map.sfz\"\n").unwrap();
        // A part of the instrument (included), not listed as an instrument itself.
        w.start_file("Lib-main/Piano/parts/map.sfz", opts).unwrap();
        w.write_all(b"<region> sample=../a.wav\n").unwrap();
        w.start_file("Lib-main/Piano/a.wav", opts).unwrap();
        w.write_all(&vec![7_u8; 200_000]).unwrap();
        w.finish().unwrap();
        buf.into_inner()
    }

    /// Serves `data` on a local port, honouring `Range: bytes=N-`; returns the URL.
    fn serve(data: Vec<u8>, requests: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/lib.zip", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(requests) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut from = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(r) = line.to_lowercase().strip_prefix("range: bytes=") {
                        from = r.trim().trim_end_matches('-').parse().unwrap();
                    }
                }
                let body = &data[from..];
                let status = if from > 0 {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        url
    }

    fn local_agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .build()
            .into()
    }

    #[test]
    fn downloads_resumes_and_unpacks() {
        let data_dir = std::env::temp_dir().join(format!("musiton-lib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        let dir = root(&data_dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip = test_zip();
        // Half of it is already there from an earlier, interrupted download.
        std::fs::write(dir.join("test.zip.part"), &zip[..zip.len() / 2]).unwrap();
        let url = serve(zip.clone(), 1);
        let job = Job::new();
        install(&local_agent(), &url, &dir, "test", &job).unwrap();
        assert_eq!(job.done.load(Ordering::Relaxed), zip.len() as u64);
        let lib = installed_dir(&data_dir, "test").unwrap();
        let list = instruments(&lib);
        assert_eq!(list.len(), 1);
        assert!(list[0].0.ends_with("piano"), "{:?}", list[0].0);
        assert_eq!(
            std::fs::read(lib.join("Lib-main/Piano/a.wav"))
                .unwrap()
                .len(),
            200_000
        );
        assert!(!dir.join("test.zip.part").exists());
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn a_damaged_archive_is_reported() {
        let data_dir = std::env::temp_dir().join(format!("musiton-lib-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        let mut zip = test_zip();
        // Flip bytes inside the compressed data of the sample.
        let mid = zip.len() / 2;
        for b in zip.iter_mut().skip(mid).take(64) {
            *b ^= 0x5A;
        }
        let url = serve(zip, 1);
        let job = Job::new();
        let r = install(&local_agent(), &url, &root(&data_dir), "bad", &job);
        assert!(r.is_err());
        assert!(installed_dir(&data_dir, "bad").is_none());
        let _ = std::fs::remove_dir_all(&data_dir);
    }
}
