//! Offline tests for clip playback.

use std::sync::Arc;

use musiton_core::audio::AudioData;
use musiton_core::{TempoMap, Tick, TimeSig, PPQ};
use musiton_engine::node::{AudioIo, AudioNode, ProcessContext};
use musiton_engine::nodes::{ClipPlayer, PlayerClip};

const SR: f32 = 48_000.0;

fn ramp(n: usize) -> Arc<AudioData> {
    Arc::new(AudioData {
        sample_rate: 48_000,
        channels: vec![
            (0..n).map(|i| i as f32).collect(),
            (0..n).map(|i| -(i as f32)).collect(),
        ],
    })
}

fn clip(audio: Arc<AudioData>, start: i64, len: i64) -> PlayerClip {
    PlayerClip {
        start: Tick(start),
        end: Tick(start + len),
        source_offset: 0,
        gain: 1.0,
        fade_in: Tick(0),
        fade_out: Tick(0),
        audio,
    }
}

/// Renders `frames` frames starting at song position `pos` in blocks of `block`.
fn render(
    player: &mut ClipPlayer,
    tempo: &TempoMap,
    pos: u64,
    frames: usize,
    block: usize,
) -> (Vec<f32>, Vec<f32>) {
    player.prepare(SR, block);
    let mut l = Vec::new();
    let mut r = Vec::new();
    let mut outs = vec![vec![0.0; block]; 2];
    let mut done = 0;
    while done < frames {
        let n = block.min(frames - done);
        let ctx = ProcessContext {
            sample_rate: SR,
            frames: n,
            playing: true,
            sample_pos: pos + done as u64,
            tempo,
            time_sig: TimeSig::default(),
            metronome: false,
            inputs: &[],
            recording: false,
            punch: None,
            live_midi: &[],
        };
        let mut io = AudioIo::new(&[], &mut outs, n);
        player.process(&ctx, &mut io, &[]);
        l.extend_from_slice(&outs[0][..n]);
        r.extend_from_slice(&outs[1][..n]);
        done += n;
    }
    (l, r)
}

#[test]
fn clip_starts_on_exact_sample_and_ends_at_its_length() {
    let tempo = TempoMap::constant(120.0);
    // Beat 2 at 120 BPM = sample 24 000; clip one beat long.
    let mut p = ClipPlayer::new(vec![clip(ramp(100_000), PPQ, PPQ)]);
    let (l, r) = render(&mut p, &tempo, 0, 72_000, 256);
    assert!(l[..24_000].iter().all(|&s| s == 0.0));
    assert_eq!(l[24_000], 0.0);
    assert_eq!(l[24_001], 1.0);
    assert_eq!(r[30_000], -6_000.0);
    assert_eq!(l[47_999], 23_999.0);
    assert!(l[48_000..].iter().all(|&s| s == 0.0));
}

#[test]
fn starting_in_the_middle_and_odd_blocks_give_identical_samples() {
    let tempo = TempoMap::constant(97.0);
    let clips = || ClipPlayer::new(vec![clip(ramp(200_000), 1000, 5 * PPQ)]);
    let (a, _) = render(&mut clips(), &tempo, 0, 150_000, 512);
    let (b, _) = render(&mut clips(), &tempo, 10_000, 140_000, 37);
    assert_eq!(&a[10_000..], &b[..]);
}

#[test]
fn source_offset_gain_and_mono_files() {
    let tempo = TempoMap::constant(120.0);
    let mono = Arc::new(AudioData {
        sample_rate: 48_000,
        channels: vec![(0..1000).map(|i| i as f32).collect()],
    });
    let mut c = clip(mono, 0, PPQ);
    c.source_offset = 100;
    c.gain = 0.5;
    let mut p = ClipPlayer::new(vec![c]);
    let (l, r) = render(&mut p, &tempo, 0, 2000, 128);
    assert_eq!(l[0], 50.0);
    assert_eq!(r[10], 55.0);
    // Past the end of the file the clip is silent even though the clip is longer.
    assert!(l[900..].iter().all(|&s| s == 0.0));
}

#[test]
fn fades_are_linear_ramps() {
    let tempo = TempoMap::constant(120.0);
    let dc = Arc::new(AudioData {
        sample_rate: 48_000,
        channels: vec![vec![1.0; 96_000]],
    });
    let mut c = clip(dc, 0, 4 * PPQ);
    c.fade_in = Tick(PPQ); // 24 000 samples
    c.fade_out = Tick(PPQ / 2); // 12 000 samples
    let mut p = ClipPlayer::new(vec![c]);
    let (l, _) = render(&mut p, &tempo, 0, 96_000, 256);
    assert_eq!(l[0], 0.0);
    assert!((l[12_000] - 0.5).abs() < 1e-4);
    assert_eq!(l[30_000], 1.0);
    assert!((l[90_000] - 0.5).abs() < 1e-3);
    assert!(l[95_999] < 1e-3);
    let worst = l
        .windows(2)
        .fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(worst <= 1.0 / 12_000.0 + 1e-6);
}

#[test]
fn stopped_transport_is_silent() {
    let tempo = TempoMap::constant(120.0);
    let mut p = ClipPlayer::new(vec![clip(ramp(1000), 0, PPQ)]);
    p.prepare(SR, 64);
    let mut outs = vec![vec![9.0; 64]; 2];
    let ctx = ProcessContext {
        sample_rate: SR,
        frames: 64,
        playing: false,
        sample_pos: 0,
        tempo: &tempo,
        time_sig: TimeSig::default(),
        metronome: false,
        inputs: &[],
        recording: false,
        punch: None,
        live_midi: &[],
    };
    p.process(&ctx, &mut AudioIo::new(&[], &mut outs, 64), &[]);
    assert!(outs.iter().flatten().all(|&s| s == 0.0));
}
