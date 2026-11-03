//! Disk thread for recording: turns chunks from the audio thread into WAV takes.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use musiton_core::audio::AudioData;
use musiton_engine::nodes::RecChunk;

use crate::file::{decode_file, free_name};

/// How often the disk thread checks for new audio.
const POLL: Duration = Duration::from_millis(5);

/// One track being recorded.
pub struct RecordJob {
    /// Caller's key (e.g. the track id).
    pub key: u64,
    /// Chunks from the track's `Recorder` node.
    pub rx: rtrb::Consumer<RecChunk>,
    /// Folder for the WAV files (the project's `audio/`).
    pub dir: PathBuf,
    /// File name stem; takes are numbered automatically.
    pub name: String,
    /// 1 = mono (left input only), 2 = stereo.
    pub channels: u16,
    /// Sample rate of the files.
    pub sample_rate: u32,
}

/// A finished take.
#[derive(Debug)]
pub struct RecordedTake {
    /// The job's key.
    pub key: u64,
    /// File name inside the job's folder.
    pub file_name: String,
    /// Song position (samples) of the first recorded frame.
    pub start: u64,
    /// The recorded audio.
    pub audio: AudioData,
}

struct OpenTake {
    writer: hound::WavWriter<BufWriter<File>>,
    path: PathBuf,
    start: u64,
    next: u64,
}

/// A running disk thread. It ends by itself when all recorders are gone (graph replaced).
pub struct DiskWriter {
    rx: mpsc::Receiver<Result<RecordedTake, String>>,
    done: Arc<AtomicBool>,
}

impl DiskWriter {
    /// Starts writing for `jobs`.
    pub fn spawn(jobs: Vec<RecordJob>) -> Self {
        let (tx, rx) = mpsc::channel();
        let done = Arc::new(AtomicBool::new(false));
        let thread_done = done.clone();
        let spawned = std::thread::Builder::new()
            .name("musiton-disk".into())
            .spawn(move || {
                run(jobs, &tx);
                thread_done.store(true, Ordering::Relaxed);
            });
        if let Err(e) = spawned {
            tracing::error!("cannot start the disk thread: {e}");
            done.store(true, Ordering::Relaxed);
        }
        Self { rx, done }
    }

    /// Takes finished since the last call.
    pub fn poll(&self) -> Vec<Result<RecordedTake, String>> {
        self.rx.try_iter().collect()
    }

    /// `true` once the thread has written everything and stopped.
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

fn run(mut jobs: Vec<RecordJob>, tx: &mpsc::Sender<Result<RecordedTake, String>>) {
    let mut open: Vec<Option<OpenTake>> = jobs.iter().map(|_| None).collect();
    loop {
        let mut all_done = true;
        for (job, take) in jobs.iter_mut().zip(open.iter_mut()) {
            while let Ok(chunk) = job.rx.pop() {
                if let Err(e) = handle(job, take, &chunk, tx) {
                    let _ = tx.send(Err(e));
                }
            }
            if !job.rx.is_abandoned() {
                all_done = false;
            }
        }
        if all_done {
            for (job, take) in jobs.iter().zip(open.iter_mut()) {
                finish(job, take, tx);
            }
            return;
        }
        std::thread::sleep(POLL);
    }
}

fn handle(
    job: &RecordJob,
    take: &mut Option<OpenTake>,
    chunk: &RecChunk,
    tx: &mpsc::Sender<Result<RecordedTake, String>>,
) -> Result<(), String> {
    if chunk.len == 0 {
        finish(job, take, tx);
        return Ok(());
    }
    if take.as_ref().is_some_and(|t| t.next != chunk.pos) {
        finish(job, take, tx);
    }
    if take.is_none() {
        std::fs::create_dir_all(&job.dir).map_err(|e| e.to_string())?;
        let path = free_name(&job.dir, &job.name);
        let spec = hound::WavSpec {
            channels: job.channels.clamp(1, 2),
            sample_rate: job.sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let writer = hound::WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
        *take = Some(OpenTake {
            writer,
            path,
            start: chunk.pos,
            next: chunk.pos,
        });
    }
    let Some(t) = take.as_mut() else {
        return Ok(());
    };
    let frames = usize::from(chunk.len);
    for frame in chunk.data.chunks_exact(2).take(frames) {
        for s in frame.iter().take(usize::from(job.channels.clamp(1, 2))) {
            t.writer.write_sample(*s).map_err(|e| e.to_string())?;
        }
    }
    t.next += frames as u64;
    Ok(())
}

fn finish(
    job: &RecordJob,
    take: &mut Option<OpenTake>,
    tx: &mpsc::Sender<Result<RecordedTake, String>>,
) {
    let Some(t) = take.take() else {
        return;
    };
    let result = t
        .writer
        .finalize()
        .map_err(|e| e.to_string())
        .and_then(|()| decode_file(&t.path).map_err(|e| e.to_string()))
        .map(|audio| RecordedTake {
            key: job.key,
            file_name: t
                .path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            start: t.start,
            audio,
        });
    let _ = tx.send(result);
}
