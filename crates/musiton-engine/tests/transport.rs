//! Transport, metronome, loop, tempo changes, graph hot-swap and GC — all offline.

mod common;

use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use common::{assert_golden, onsets, Const};
use musiton_core::{SamplePos, TempoMap, TempoPoint, Tick, TimeSig, PPQ};
use musiton_engine::graph::GraphBuilder;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};
use musiton_engine::nodes::Metronome;
use musiton_engine::rt::{no_alloc, AllocDisabler};
use musiton_engine::{create, render_offline, Engine, EngineCommand, EngineHandle};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;

fn metronome_engine(bpm: f64) -> (EngineHandle, Engine) {
    let (mut h, mut e) = create(SR, BLOCK);
    let mut b = GraphBuilder::new();
    let m = b.add(Box::new(Metronome::new()));
    b.set_output(m);
    let g = b.build(SR, BLOCK).unwrap();
    h.send(EngineCommand::SwapGraph(Box::new(g))).ok();
    // Let the graph fade-in finish while stopped (the transport doesn't move).
    render_offline(&mut e, 1_024, 2, BLOCK);
    h.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
        bpm,
    ))))
    .ok();
    h.send(EngineCommand::SetMetronome(true)).ok();
    h.send(EngineCommand::Play).ok();
    (h, e)
}

fn left(stereo: &[f32]) -> Vec<f32> {
    stereo.chunks(2).map(|f| f[0]).collect()
}

/// Largest jump between neighbouring samples (a click/glitch shows up as a big jump).
fn max_step(x: &[f32]) -> f32 {
    x.windows(2).fold(0.0, |m, w| m.max((w[1] - w[0]).abs()))
}

#[test]
fn golden_four_bars_at_120_bpm() {
    let (_h, mut e) = metronome_engine(120.0);
    let out = left(&render_offline(&mut e, 4 * 96_000, 2, BLOCK));
    let clicks = onsets(&out);
    let expected: Vec<usize> = (0..16).map(|k| k * 24_000).collect();
    assert_eq!(clicks, expected);
    assert_golden("metronome_4bars_120bpm", &out, SR as u32);
}

#[test]
fn accent_on_beat_one() {
    let (_h, mut e) = metronome_engine(120.0);
    let out = left(&render_offline(&mut e, 2 * 96_000, 2, BLOCK));
    let peak = |at: usize| {
        out[at..at + 2_400]
            .iter()
            .fold(0.0_f32, |m, s| m.max(s.abs()))
    };
    assert!(peak(0) > peak(24_000) * 1.2);
    assert!(peak(96_000) > peak(120_000) * 1.2);
    assert!((peak(24_000) - peak(48_000)).abs() < 1e-3);
}

#[test]
fn any_bpm_and_block_size_gives_exact_beat_positions() {
    for (bpm, block) in [(60.0, 64), (97.3, 100), (140.0, 1), (200.0, 512)] {
        let (_h, mut e) = metronome_engine(bpm);
        let frames = 4 * 48_000;
        let out = left(&render_offline(&mut e, frames, 2, block));
        let beat = 60.0 / bpm * 48_000.0;
        let expected: Vec<usize> = (0..)
            .map(|k| (k as f64 * beat).round() as usize)
            .take_while(|&s| s + 1 < frames)
            .collect();
        assert_eq!(onsets(&out), expected, "bpm {bpm} block {block}");
    }
}

#[test]
fn odd_time_signature() {
    let (mut h, mut e) = metronome_engine(120.0);
    h.send(EngineCommand::SetTimeSig(TimeSig::new(6, 8))).ok();
    // 6/8 at 120 quarter-BPM: eighth note = 0.25 s = 12 000 samples; bar = 6 eighths.
    let out = left(&render_offline(&mut e, 2 * 72_000, 2, BLOCK));
    let clicks = onsets(&out);
    assert_eq!(clicks.len(), 12);
    assert!(clicks.iter().enumerate().all(|(k, &s)| s == k * 12_000));
}

#[test]
fn stopped_transport_is_silent_and_does_not_move() {
    let (mut h, mut e) = metronome_engine(120.0);
    h.send(EngineCommand::Stop).ok();
    let out = render_offline(&mut e, 48_000, 2, BLOCK);
    assert!(out.iter().all(|&s| s == 0.0));
    assert_eq!(h.shared().playhead(), 0);
    assert!(!h.shared().playing());
}

#[test]
fn locate_moves_playhead() {
    let (mut h, mut e) = metronome_engine(120.0);
    h.send(EngineCommand::Locate(SamplePos(12_000))).ok();
    let out = left(&render_offline(&mut e, 48_000, 2, BLOCK));
    // Next beat after 0.25 s is at 0.5 s → 12 000 samples into the render.
    assert_eq!(onsets(&out), vec![12_000, 36_000]);
    assert_eq!(h.shared().playhead(), 60_000);
}

#[test]
fn loop_wraps_sample_accurately() {
    let (mut h, mut e) = metronome_engine(120.0);
    // Loop bar 1 only (4 beats = 96 000 samples).
    h.send(EngineCommand::SetLoop(Some((Tick(0), Tick(4 * PPQ)))))
        .ok();
    let out = left(&render_offline(&mut e, 3 * 96_000, 2, 1000));
    let expected: Vec<usize> = (0..12).map(|k| k * 24_000).collect();
    assert_eq!(onsets(&out), expected);
    // Every bar starts with the accent again.
    let peak = |at: usize| {
        out[at..at + 2_400]
            .iter()
            .fold(0.0_f32, |m, s| m.max(s.abs()))
    };
    assert!((peak(96_000) - peak(0)).abs() < 1e-4);
    assert!(h.shared().playhead() < 96_000);
}

#[test]
fn tempo_change_while_playing_keeps_musical_position() {
    let (mut h, mut e) = metronome_engine(120.0);
    // Play 1.25 beats at 120 BPM, then switch to 60 BPM.
    let mut out = left(&render_offline(&mut e, 30_000, 2, BLOCK));
    h.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
        60.0,
    ))))
    .ok();
    out.extend(left(&render_offline(&mut e, 150_000, 2, BLOCK)));
    // Beats at 0, 24 000; then position 1.25 beats → next beat (2) is 0.75 beat away = 36 000 samples.
    assert_eq!(onsets(&out), vec![0, 24_000, 66_000, 114_000, 162_000]);
    assert!(max_step(&out) < 0.25, "glitch: {}", max_step(&out));
}

#[test]
fn tempo_map_with_steps() {
    let (mut h, mut e) = metronome_engine(120.0);
    let map = TempoMap::from_points(vec![
        TempoPoint {
            tick: Tick(0),
            bpm: 120.0,
        },
        TempoPoint {
            tick: Tick(2 * PPQ),
            bpm: 60.0,
        },
    ]);
    h.send(EngineCommand::SetTempoMap(Box::new(map))).ok();
    let out = left(&render_offline(&mut e, 4 * 48_000, 2, BLOCK));
    assert_eq!(onsets(&out), vec![0, 24_000, 48_000, 96_000, 144_000]);
}

#[test]
fn graph_swap_is_click_free() {
    let (mut h, mut e) = create(SR, BLOCK);
    let build = |v: f32| {
        let mut b = GraphBuilder::new();
        let n = b.add(Box::new(Const {
            value: v,
            channels: 2,
        }));
        b.set_output(n);
        b.build(SR, BLOCK).unwrap()
    };
    h.send(EngineCommand::SwapGraph(Box::new(build(0.5)))).ok();
    let mut out = left(&render_offline(&mut e, 4_800, 2, BLOCK));
    for v in [-0.5, 0.25, 0.0] {
        h.send(EngineCommand::SwapGraph(Box::new(build(v)))).ok();
        out.extend(left(&render_offline(&mut e, 4_800, 2, BLOCK)));
    }
    // 10 ms fade = 480 samples; biggest change is 1.0 → step ≤ 1/480.
    assert!(
        max_step(&out) <= 1.0 / 480.0 + 1e-6,
        "step {}",
        max_step(&out)
    );
    assert_eq!(*out.last().unwrap(), 0.0);
    // The first graph also fades in from silence.
    assert!(out[0] < 0.01);
}

/// Records which thread drops it.
struct DropProbe(Arc<Mutex<Option<ThreadId>>>);

impl AudioNode for DropProbe {
    fn num_inputs(&self) -> usize {
        0
    }
    fn num_outputs(&self) -> usize {
        1
    }
    fn prepare(&mut self, _: f32, _: usize) {}
    fn process(&mut self, _: &ProcessContext, io: &mut AudioIo, _: &[Event]) {
        io.output(0).fill(0.0);
    }
    fn reset(&mut self) {}
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = Some(std::thread::current().id());
    }
}

#[test]
fn replaced_graph_is_freed_on_gc_thread_and_processing_never_allocates() {
    let (mut h, mut e) = create(SR, BLOCK);
    let dropped = Arc::new(Mutex::new(None));
    let mut b = GraphBuilder::new();
    let n = b.add(Box::new(DropProbe(dropped.clone())));
    b.set_output(n);
    h.send(EngineCommand::SwapGraph(Box::new(
        b.build(SR, BLOCK).unwrap(),
    )))
    .ok();
    let mut b = GraphBuilder::new();
    let m = b.add(Box::new(Metronome::new()));
    b.set_output(m);
    let next = b.build(SR, BLOCK).unwrap();
    let mut buf = vec![0.0_f32; 2 * BLOCK];
    no_alloc(|| e.process(&mut buf, 2));
    h.send(EngineCommand::SwapGraph(Box::new(next))).ok();
    h.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
        90.0,
    ))))
    .ok();
    h.send(EngineCommand::Play).ok();
    h.send(EngineCommand::SetMetronome(true)).ok();
    no_alloc(|| {
        for _ in 0..200 {
            e.process(&mut buf, 2);
        }
    });
    for _ in 0..100 {
        if dropped.lock().unwrap().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let id = dropped.lock().unwrap().expect("old graph was never freed");
    assert_ne!(id, std::thread::current().id());
}
