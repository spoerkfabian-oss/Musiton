//! Moving any knob abruptly while audio plays must not click.
//!
//! For every continuous parameter of every device: render with the parameter at its
//! minimum and at its maximum (no changes) to learn how steep the output normally gets,
//! then render while jumping between min and max every 2048 samples. The steepest step
//! may not be much larger than without the jumps.

mod common;

use common::*;
use musiton_core::ids::ParamId;
use musiton_core::params::ParamKind;
use musiton_devices::{create_effect, create_instrument, EFFECTS, INSTRUMENTS};
use musiton_engine::node::{AudioNode, Event, EventKind};

fn check(
    name: &str,
    make: &dyn Fn() -> Box<dyn AudioNode>,
    input: &[Vec<f32>; 2],
    events: &[(usize, Event)],
) {
    let params = make().params().to_vec();
    for p in params
        .iter()
        .filter(|p| matches!(p.kind, ParamKind::Linear | ParamKind::Log))
    {
        let render_at = |v: f32| {
            let mut n = make();
            n.set_param(p.id, v);
            run(n.as_mut(), input, 256, events, &mut |_, _| {})
        };
        let steady = [render_at(p.min), render_at(p.max)]
            .iter()
            .flat_map(|o| [max_step(&o[0][4_800..]), max_step(&o[1][4_800..])])
            .fold(0.0_f32, f32::max);
        let mut n = make();
        let jumping = run(n.as_mut(), input, 256, events, &mut |k, node| {
            if k >= 20 && k % 8 == 0 {
                node.set_param(
                    ParamId(p.id.0),
                    if (k / 8) % 2 == 0 { p.max } else { p.min },
                );
            }
        });
        let worst = max_step(&jumping[0][4_800..]).max(max_step(&jumping[1][4_800..]));
        assert!(
            worst <= steady * 1.5 + 0.02,
            "{name}: moving \"{}\" clicks (step {worst:.4} vs {steady:.4} when still)",
            p.name
        );
    }
}

#[test]
fn effect_knobs_are_click_free() {
    let input = sine(220.0, 0.4, 48_000);
    for t in EFFECTS {
        check(t.name, &|| create_effect(t.type_id, &[]).0, &input, &[]);
    }
}

#[test]
fn instrument_knobs_are_click_free() {
    let silence = [vec![0.0; 48_000], vec![0.0; 48_000]];
    let note = Event {
        offset: 0,
        kind: EventKind::NoteOn {
            ch: 0,
            key: 57,
            vel: 1.0,
        },
    };
    let sample = std::sync::Arc::new(musiton_core::audio::AudioData {
        sample_rate: 48_000,
        channels: vec![(0..96_000)
            .map(|i| 0.4 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin())
            .collect()],
    });
    for t in INSTRUMENTS {
        let samples = [(60u8, sample.clone())];
        check(
            t.name,
            &|| create_instrument(t.type_id, &[], &samples),
            &silence,
            &[(100, note)],
        );
    }
}
