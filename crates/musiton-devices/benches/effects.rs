//! CPU cost of each effect and instrument: one second of stereo audio at 48 kHz in
//! 256-sample blocks. Run with `cargo bench -p musiton-devices`.

use criterion::{criterion_group, criterion_main, Criterion};
use musiton_core::{TempoMap, TimeSig};
use musiton_devices::{create_effect, create_instrument, EFFECTS, INSTRUMENTS};
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;

fn second(node: &mut dyn AudioNode, events: &[Event]) {
    let tempo = TempoMap::default();
    let input: Vec<Vec<f32>> = (0..node.num_inputs())
        .map(|_| (0..BLOCK).map(|i| (i as f32 * 0.05).sin() * 0.5).collect())
        .collect();
    let mut out = vec![vec![0.0; BLOCK]; node.num_outputs()];
    for k in 0..(SR as usize / BLOCK) {
        let ctx = ProcessContext {
            sample_rate: SR,
            frames: BLOCK,
            playing: true,
            sample_pos: (k * BLOCK) as u64,
            tempo: &tempo,
            time_sig: TimeSig::default(),
            metronome: false,
            inputs: &[],
            recording: false,
            punch: None,
            live_midi: &[],
        };
        let ev = if k == 0 { events } else { &[] };
        node.process(&ctx, &mut AudioIo::new(&input, &mut out, BLOCK), ev);
    }
}

fn bench(c: &mut Criterion) {
    for t in EFFECTS {
        let (mut node, _) = create_effect(t.type_id, &[]);
        node.prepare(SR, BLOCK);
        c.bench_function(&format!("effect/{} 1s", t.name), |b| {
            b.iter(|| second(node.as_mut(), &[]))
        });
    }
    // 8 held notes.
    let notes: Vec<Event> = (0..8)
        .map(|k| Event {
            offset: 0,
            kind: EventKind::NoteOn {
                ch: 0,
                key: 48 + k * 3,
                vel: 0.8,
            },
        })
        .collect();
    let sample = std::sync::Arc::new(musiton_core::audio::AudioData {
        sample_rate: 48_000,
        channels: vec![(0..96_000).map(|i| (i as f32 * 0.03).sin() * 0.4).collect()],
    });
    for t in INSTRUMENTS {
        let mut node = create_instrument(t.type_id, &[], &[(60, sample.clone())]);
        node.prepare(SR, BLOCK);
        c.bench_function(&format!("instrument/{} 8 voices 1s", t.name), |b| {
            b.iter(|| second(node.as_mut(), &notes))
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
