//! End-to-end recording tests without hardware: a simulated input device feeds the engine,
//! `Recorder` nodes stream to the disk thread, which writes WAV takes.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use musiton_core::{TempoMap, Tick, PPQ};
use musiton_engine::graph::GraphBuilder;
use musiton_engine::nodes::{AudioInput, Mix, RecChunk, Recorder};
use musiton_engine::rt::{no_alloc, AllocDisabler};
use musiton_engine::{create, Engine, EngineCommand, EngineHandle, InputSource, PROBE_NONE};
use musiton_io::recording::{DiskWriter, RecordJob, RecordedTake};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const SR: u32 = 48_000;
const BLOCK: usize = 256;
const BAR: u64 = 96_000; // 4 beats at 120 BPM

/// Input sample for frame `n` on channel `ch` (unique, so positions can be checked).
fn input_value(n: u64, ch: usize) -> f32 {
    ((n % 100_000) as f32 / 100_000.0) * if ch == 0 { 1.0 } else { -1.0 }
}

struct Rig {
    h: EngineHandle,
    e: Engine,
    input: rtrb::Producer<f32>,
    fed: u64,
    writer: DiskWriter,
    dropped: Arc<AtomicU64>,
    dir: PathBuf,
}

fn rig(name: &str, tracks: usize) -> Rig {
    let dir = std::env::temp_dir().join(format!("musiton-rec-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut h, e) = create(SR as f32, BLOCK);
    let (input, samples) = rtrb::RingBuffer::new(SR as usize * 2);
    h.send(EngineCommand::SetInput(Some(Box::new(InputSource {
        samples,
        channels: 2,
    }))))
    .ok();
    let dropped = Arc::new(AtomicU64::new(0));
    let mut b = GraphBuilder::new();
    let out = b.add(Box::new(Mix::new(2)));
    let mut jobs = Vec::new();
    for k in 0..tracks {
        let (tx, rx) = rtrb::RingBuffer::<RecChunk>::new(1024);
        let inp = b.add(Box::new(AudioInput::new(0, Some(1))));
        let rec = b.add(Box::new(Recorder::new(tx, dropped.clone())));
        b.connect(inp, 0, rec, 0).unwrap();
        b.connect(inp, 1, rec, 1).unwrap();
        jobs.push(RecordJob {
            key: k as u64,
            rx,
            dir: dir.clone(),
            name: format!("track{k}"),
            channels: 2,
            sample_rate: SR,
        });
    }
    b.set_output(out);
    h.send(EngineCommand::SwapGraph(Box::new(
        b.build(SR as f32, BLOCK).unwrap(),
    )))
    .ok();
    h.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
        120.0,
    ))))
    .ok();
    Rig {
        h,
        e,
        input,
        fed: 0,
        writer: DiskWriter::spawn(jobs),
        dropped,
        dir,
    }
}

impl Rig {
    /// Runs `frames` frames like a device would: input callback, then output callback.
    fn run(&mut self, frames: usize, pace: bool) {
        let mut out = vec![0.0_f32; BLOCK * 2];
        for _ in 0..frames / BLOCK {
            for _ in 0..BLOCK {
                for ch in 0..2 {
                    let _ = self.input.push(input_value(self.fed, ch));
                }
                self.fed += 1;
            }
            let e = &mut self.e;
            no_alloc(|| e.process(&mut out, 2));
            if pace {
                std::thread::sleep(std::time::Duration::from_micros(300));
            }
        }
    }

    /// Stops, removes the recorders and returns all takes sorted by (key, start).
    fn finish(mut self) -> (Vec<RecordedTake>, u64) {
        self.h.send(EngineCommand::SetRecording(false)).ok();
        self.h.send(EngineCommand::Stop).ok();
        self.run(BLOCK, false);
        drop(self.e);
        let mut takes = Vec::new();
        while !self.writer.finished() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        for t in self.writer.poll() {
            takes.push(t.expect("take failed"));
        }
        takes.sort_by_key(|t| (t.key, t.file_name.clone()));
        let _ = std::fs::remove_dir_all(&self.dir);
        (takes, self.dropped.load(Ordering::Relaxed))
    }
}

fn check_take(t: &RecordedTake, start: u64, frames: usize) {
    assert_eq!(t.start, start, "start of {}", t.file_name);
    assert_eq!(t.audio.frames(), frames, "length of {}", t.file_name);
    // Input and song position advance together, so frame i of the take is input frame start + i.
    for i in [0usize, 1, frames / 2, frames - 1] {
        let n = start + i as u64;
        assert_eq!(
            t.audio.channel(0)[i],
            input_value(n, 0),
            "{} frame {i}",
            t.file_name
        );
        assert_eq!(t.audio.channel(1)[i], input_value(n, 1));
    }
}

#[test]
fn straight_recording_is_sample_aligned() {
    let mut r = rig("straight", 1);
    r.h.send(EngineCommand::SetRecording(true)).ok();
    r.h.send(EngineCommand::Play).ok();
    r.run(2 * SR as usize, false);
    let (takes, dropped) = r.finish();
    assert_eq!(dropped, 0);
    assert_eq!(takes.len(), 1);
    check_take(&takes[0], 0, 2 * SR as usize);
}

#[test]
fn loop_recording_creates_one_take_per_pass() {
    let mut r = rig("loop", 1);
    r.h.send(EngineCommand::SetLoop(Some((Tick(0), Tick(4 * PPQ)))))
        .ok();
    r.h.send(EngineCommand::SetRecording(true)).ok();
    r.h.send(EngineCommand::Play).ok();
    // 2.5 bars, rounded down to whole blocks like a device would deliver them.
    let frames = (BAR * 5 / 2) as usize / BLOCK * BLOCK;
    r.run(frames, false);
    let (takes, _) = r.finish();
    assert_eq!(
        takes.len(),
        3,
        "{:?}",
        takes
            .iter()
            .map(|t| (t.start, t.audio.frames()))
            .collect::<Vec<_>>()
    );
    for t in &takes {
        assert_eq!(t.start, 0);
    }
    let mut lens: Vec<usize> = takes.iter().map(|t| t.audio.frames()).collect();
    lens.sort_unstable();
    assert_eq!(
        lens,
        vec![frames - 2 * BAR as usize, BAR as usize, BAR as usize]
    );
    // The second pass recorded the input that arrived one bar later.
    let second = takes
        .iter()
        .find(|t| t.audio.channel(0)[0] == input_value(BAR, 0))
        .expect("second pass");
    assert_eq!(second.audio.frames(), BAR as usize);
}

#[test]
fn punch_in_out_records_only_inside_the_range() {
    let mut r = rig("punch", 1);
    r.h.send(EngineCommand::SetPunch(Some((
        Tick(4 * PPQ),
        Tick(8 * PPQ),
    ))))
    .ok();
    r.h.send(EngineCommand::SetRecording(true)).ok();
    r.h.send(EngineCommand::Play).ok();
    r.run(3 * BAR as usize, false);
    let (takes, _) = r.finish();
    assert_eq!(takes.len(), 1);
    check_take(&takes[0], BAR, BAR as usize);
}

#[test]
fn two_tracks_for_a_minute_without_dropouts() {
    let mut r = rig("soak", 2);
    r.h.send(EngineCommand::SetRecording(true)).ok();
    r.h.send(EngineCommand::Play).ok();
    let frames = 60 * SR as usize;
    r.run(frames, true);
    let (takes, dropped) = r.finish();
    assert_eq!(dropped, 0, "chunks lost");
    assert_eq!(takes.len(), 2);
    for t in &takes {
        check_take(t, 0, frames);
    }
}

#[test]
fn latency_probe_measures_a_loopback() {
    let (h, mut e) = create(SR as f32, BLOCK);
    let (mut input, samples) = rtrb::RingBuffer::new(1 << 16);
    let mut h = h;
    h.send(EngineCommand::SetInput(Some(Box::new(InputSource {
        samples,
        channels: 1,
    }))))
    .ok();
    // A "cable" from output to input that delays by 700 frames.
    let delay = 700;
    let mut cable: VecDeque<f32> = std::iter::repeat_n(0.0, delay).collect();
    let mut out = vec![0.0_f32; BLOCK * 2];
    assert_eq!(h.shared().probe_result(), PROBE_NONE);
    h.shared().request_latency_probe();
    for _ in 0..40 {
        for _ in 0..BLOCK {
            let _ = input.push(cable.pop_front().unwrap_or(0.0));
        }
        e.process(&mut out, 2);
        cable.extend(out.chunks(2).map(|f| f[0]));
    }
    assert_eq!(h.shared().probe_result(), delay as i64);
}
