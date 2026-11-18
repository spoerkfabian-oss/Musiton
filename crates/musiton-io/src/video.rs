//! Video through an external `ffmpeg` program (an LGPL build shipped next to Musiton).
//!
//! Musiton never links FFmpeg: it starts `ffmpeg` as a child process and reads raw RGBA
//! frames from its stdout, so a crash or hang inside FFmpeg cannot take Musiton down.
//! [`VideoPlayer`] runs on its own thread and keeps one decoding process streaming while
//! the song plays, or decodes single frames while it stands still.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::OnceLock;
use std::time::Duration;

/// Errors from running FFmpeg.
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    /// No `ffmpeg` next to Musiton, on the `PATH` or in `MUSITON_FFMPEG`.
    #[error("FFmpeg was not found (ffmpeg.exe should be next to Musiton.exe)")]
    NoFfmpeg,
    /// Starting or talking to the process failed.
    #[error("could not run FFmpeg: {0}")]
    Io(#[from] std::io::Error),
    /// FFmpeg could not find a video stream in the file.
    #[error("not a video FFmpeg can read: {0}")]
    Unreadable(String),
    /// FFmpeg ran but reported an error.
    #[error("FFmpeg failed: {0}")]
    Failed(String),
}

/// Width of decoded frames at most (frames are scaled down for the preview).
pub const PREVIEW_WIDTH: usize = 640;
/// Frames per second decoded during playback.
pub const PREVIEW_FPS: f64 = 25.0;
/// A playing stream further than this from the wanted time is restarted there.
const RESYNC_SECS: f64 = 0.5;

/// What FFmpeg reports about a video file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    /// Length in seconds.
    pub duration_secs: f64,
    /// Picture width in pixels.
    pub width: usize,
    /// Picture height in pixels.
    pub height: usize,
    /// Whether the file has a sound track.
    pub has_audio: bool,
}

impl VideoInfo {
    /// Size of the preview frames: at most [`PREVIEW_WIDTH`] wide, even, same aspect.
    pub fn preview_size(&self) -> (usize, usize) {
        let even = |x: f64| ((x / 2.0).round() as usize * 2).max(2);
        let w = even(self.width.min(PREVIEW_WIDTH) as f64);
        let h = even(w as f64 * self.height as f64 / self.width.max(1) as f64);
        (w, h)
    }
}

/// One decoded picture (RGBA, row by row).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Video time of the frame in seconds.
    pub secs: f64,
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// The `ffmpeg` program: `MUSITON_FFMPEG`, else next to the running executable, else on
/// the `PATH`. Looked up once.
pub fn ffmpeg() -> Result<&'static Path, VideoError> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let name = if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            };
            let candidates = std::env::var_os("MUSITON_FFMPEG")
                .map(PathBuf::from)
                .into_iter()
                .chain(
                    std::env::current_exe()
                        .ok()
                        .and_then(|e| e.parent().map(|d| d.join(name))),
                )
                .chain(std::iter::once(PathBuf::from(name)));
            candidates.into_iter().find(|c| {
                command(c)
                    .arg("-version")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            })
        })
        .as_deref()
        .ok_or(VideoError::NoFfmpeg)
}

/// A quiet FFmpeg command (no console window on Windows, no stdin).
fn command(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    c.arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("error")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Reads length, picture size and whether there is sound.
pub fn probe(file: &Path) -> Result<VideoInfo, VideoError> {
    let out = command(ffmpeg()?)
        .arg("-loglevel")
        .arg("info")
        .arg("-i")
        .arg(file)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    // Without an output file FFmpeg exits with an error, but it has printed the info.
    let text = String::from_utf8_lossy(&out.stderr);
    parse_probe(&text).ok_or_else(|| {
        VideoError::Unreadable(text.lines().last().unwrap_or_default().trim().to_owned())
    })
}

/// Parses FFmpeg's description of an input file.
fn parse_probe(text: &str) -> Option<VideoInfo> {
    let duration_secs = text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("Duration:")?;
        let stamp = rest.split(',').next()?.trim();
        let mut parts = stamp.split(':');
        let h: f64 = parts.next()?.parse().ok()?;
        let m: f64 = parts.next()?.parse().ok()?;
        let s: f64 = parts.next()?.parse().ok()?;
        Some(h * 3600.0 + m * 60.0 + s)
    })?;
    let video = text
        .lines()
        .find(|l| l.contains("Stream #") && l.contains("Video:"))?;
    let (width, height) = video
        .split([' ', ','])
        .find_map(|tok| {
            let (w, h) = tok.split_once('x')?;
            Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?))
        })
        .filter(|(w, h)| *w > 0 && *h > 0)?;
    let has_audio = text
        .lines()
        .any(|l| l.contains("Stream #") && l.contains("Audio:"));
    Some(VideoInfo {
        duration_secs,
        width,
        height,
        has_audio,
    })
}

/// Writes the sound track of `file` to `out` as a stereo 32-bit float WAV at `rate`.
pub fn extract_audio(file: &Path, out: &Path, rate: u32) -> Result<(), VideoError> {
    let result = command(ffmpeg()?)
        .arg("-i")
        .arg(file)
        .args([
            "-vn",
            "-ac",
            "2",
            "-ar",
            &rate.to_string(),
            "-c:a",
            "pcm_f32le",
            "-y",
        ])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if result.status.success() {
        Ok(())
    } else {
        Err(VideoError::Failed(
            String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        ))
    }
}

/// Starts FFmpeg decoding `file` from `secs` as raw RGBA preview frames.
fn spawn_decoder(
    file: &Path,
    size: (usize, usize),
    secs: f64,
    frames: Option<usize>,
) -> Result<Child, VideoError> {
    let mut c = command(ffmpeg()?);
    c.args(["-ss", &format!("{:.3}", secs.max(0.0))])
        .arg("-i")
        .arg(file)
        .args([
            "-an",
            "-vf",
            &format!("fps={PREVIEW_FPS},scale={}:{}", size.0, size.1),
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
        ]);
    if let Some(n) = frames {
        c.args(["-frames:v", &n.to_string()]);
    }
    Ok(c.arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?)
}

/// Decodes the picture shown at `secs`.
pub fn frame_at(file: &Path, info: &VideoInfo, secs: f64) -> Result<Frame, VideoError> {
    let size = info.preview_size();
    let mut child = spawn_decoder(file, size, secs, Some(1))?;
    let mut rgba = vec![0; size.0 * size.1 * 4];
    let read = child.stdout.take().map(|mut out| out.read_exact(&mut rgba));
    let _ = child.wait();
    match read {
        Some(Ok(())) => Ok(Frame {
            secs,
            width: size.0,
            height: size.1,
            rgba,
        }),
        _ => Err(VideoError::Failed(format!("no picture at {secs:.2} s"))),
    }
}

/// A running decoder that delivers consecutive frames.
struct Stream {
    child: Child,
    out: ChildStdout,
    /// Video time of the next frame it will deliver.
    next_secs: f64,
}

impl Stream {
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the UI wants to see.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Want {
    /// The picture at this video time; `true` while the song plays.
    At(f64, bool),
    /// Nothing (before or after the video).
    Nothing,
}

/// Decodes a video on a worker thread, following the time the UI asks for.
pub struct VideoPlayer {
    want: mpsc::Sender<Want>,
    frames: mpsc::Receiver<Frame>,
    info: VideoInfo,
}

impl VideoPlayer {
    /// Opens `file` (probes it, then starts the worker).
    pub fn open(file: &Path) -> Result<Self, VideoError> {
        let info = probe(file)?;
        let (want_tx, want_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::channel();
        let file = file.to_owned();
        std::thread::Builder::new()
            .name("video".into())
            .spawn(move || worker(&file, info, &want_rx, &frame_tx))?;
        Ok(Self {
            want: want_tx,
            frames: frame_rx,
            info,
        })
    }

    /// What FFmpeg reported about the file.
    pub fn info(&self) -> VideoInfo {
        self.info
    }

    /// Asks for the picture at video time `secs` (`None`: no picture is needed).
    pub fn show(&self, secs: Option<f64>, playing: bool) {
        let want = secs
            .filter(|s| (0.0..self.info.duration_secs).contains(s))
            .map_or(Want::Nothing, |s| Want::At(s, playing));
        let _ = self.want.send(want);
    }

    /// The newest decoded frame since the last call, if any.
    pub fn latest(&self) -> Option<Frame> {
        self.frames.try_iter().last()
    }
}

fn worker(
    file: &Path,
    info: VideoInfo,
    wants: &mpsc::Receiver<Want>,
    frames: &mpsc::Sender<Frame>,
) {
    let size = info.preview_size();
    let frame_bytes = size.0 * size.1 * 4;
    let step = 1.0 / PREVIEW_FPS;
    let mut want = Want::Nothing;
    let mut stream: Option<Stream> = None;
    // Time of the still frame last sent (to decode each still only once).
    let mut still: Option<f64> = None;
    let mut buf = vec![0u8; frame_bytes];
    loop {
        // Wait briefly for news, then take only the newest wish.
        match wants.recv_timeout(Duration::from_millis(5)) {
            Ok(w) => want = wants.try_iter().last().unwrap_or(w),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        match want {
            Want::Nothing => {
                if let Some(s) = stream.take() {
                    s.stop();
                }
                still = None;
            }
            Want::At(secs, false) => {
                if let Some(s) = stream.take() {
                    s.stop();
                }
                if still.is_some_and(|t| (t - secs).abs() < step / 2.0) {
                    continue;
                }
                still = Some(secs);
                match frame_at(file, &info, secs) {
                    Ok(f) => {
                        if frames.send(f).is_err() {
                            break;
                        }
                    }
                    Err(e) => tracing::warn!("video frame: {e}"),
                }
            }
            Want::At(secs, true) => {
                still = None;
                let behind_or_jumped = stream
                    .as_ref()
                    .is_none_or(|s| (s.next_secs - secs).abs() > RESYNC_SECS);
                if behind_or_jumped {
                    if let Some(s) = stream.take() {
                        s.stop();
                    }
                    match spawn_decoder(file, size, secs, None) {
                        Ok(mut child) => {
                            if let Some(out) = child.stdout.take() {
                                stream = Some(Stream {
                                    child,
                                    out,
                                    next_secs: secs,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::warn!("video stream: {e}");
                            want = Want::Nothing;
                            continue;
                        }
                    }
                }
                // Read up to the wanted time; send the last frame read.
                let mut newest = None;
                if let Some(s) = &mut stream {
                    while s.next_secs <= secs + step / 2.0 {
                        if s.out.read_exact(&mut buf).is_err() {
                            break;
                        }
                        newest = Some(s.next_secs);
                        s.next_secs += step;
                    }
                }
                if let Some(t) = newest {
                    let f = Frame {
                        secs: t,
                        width: size.0,
                        height: size.1,
                        rgba: buf.clone(),
                    };
                    if frames.send(f).is_err() {
                        break;
                    }
                }
            }
        }
    }
    if let Some(s) = stream {
        s.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FFMPEG_INFO: &str = "\
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'clip.mp4':
  Metadata:
    major_brand     : isom
  Duration: 00:01:02.50, start: 0.000000, bitrate: 1205 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1920x1080 [SAR 1:1 DAR 16:9], 1070 kb/s, 25 fps, 25 tbr, 12800 tbn (default)
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 128 kb/s (default)
At least one output file must be specified";

    #[test]
    fn ffmpeg_info_is_parsed() {
        let info = parse_probe(FFMPEG_INFO).unwrap();
        assert_eq!(
            info,
            VideoInfo {
                duration_secs: 62.5,
                width: 1920,
                height: 1080,
                has_audio: true,
            }
        );
        assert_eq!(info.preview_size(), (640, 360));
        let silent = FFMPEG_INFO.replace("Audio:", "Data:");
        assert!(!parse_probe(&silent).unwrap().has_audio);
        assert!(parse_probe("Duration: 00:00:01.00, start").is_none());
    }

    /// Two seconds of 64×48 video: red for the first second, blue for the second.
    fn red_then_blue() -> Option<PathBuf> {
        let exe = ffmpeg().ok()?;
        let dir = std::env::temp_dir().join(format!("musiton-video-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("red_blue.mkv");
        let ok = command(exe)
            .args([
                "-f",
                "lavfi",
                "-i",
                "color=red:s=64x48:r=25:d=1",
                "-f",
                "lavfi",
                "-i",
                "color=blue:s=64x48:r=25:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                "-map",
                "[v]",
                "-map",
                "2:a",
                "-c:v",
                "ffv1",
                "-y",
            ])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(path)
    }

    fn colour(f: &Frame) -> &'static str {
        let mid = (f.height / 2 * f.width + f.width / 2) * 4;
        match (f.rgba[mid], f.rgba[mid + 2]) {
            (r, b) if r > 200 && b < 60 => "red",
            (r, b) if b > 200 && r < 60 => "blue",
            _ => "other",
        }
    }

    #[test]
    fn frames_follow_the_requested_time() {
        let Some(path) = red_then_blue() else {
            eprintln!("ffmpeg not available; skipping");
            return;
        };
        let info = probe(&path).unwrap();
        assert!((info.duration_secs - 2.0).abs() < 0.1, "{info:?}");
        assert_eq!((info.width, info.height, info.has_audio), (64, 48, true));
        assert_eq!(colour(&frame_at(&path, &info, 0.5).unwrap()), "red");
        assert_eq!(colour(&frame_at(&path, &info, 1.5).unwrap()), "blue");

        // Playing: frames arrive in order and switch colour at one second.
        let player = VideoPlayer::open(&path).unwrap();
        let mut seen = Vec::new();
        for k in 0..45 {
            player.show(Some(f64::from(k) * 0.04), true);
            std::thread::sleep(Duration::from_millis(15));
            seen.extend(player.latest());
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while seen.last().is_none_or(|f| f.secs < 1.7) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            seen.extend(player.latest());
        }
        assert!(seen.windows(2).all(|w| w[0].secs < w[1].secs));
        for f in &seen {
            let want = if f.secs < 0.98 { "red" } else { "blue" };
            if (f.secs - 1.0).abs() > 0.05 {
                assert_eq!(colour(f), want, "at {:.2} s", f.secs);
            }
        }
        assert!(seen.last().is_some_and(|f| colour(f) == "blue"));

        // Sound track.
        let wav = path.with_extension("wav");
        extract_audio(&path, &wav, 48_000).unwrap();
        let audio = crate::file::decode_file(&wav).unwrap();
        assert_eq!((audio.sample_rate, audio.channels.len()), (48_000, 2));
        assert!((audio.frames() as f64 / 48_000.0 - 2.0).abs() < 0.05);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
