//! Headless tests of the step sequencer.

use egui::{Pos2, RawInput, Rect};
use musiton_core::ids::{ClipId, TrackId};
use musiton_core::{Tick, PPQ};
use musiton_project::{Clip, Edit, MidiClip, Note, Project, Track, TrackKind};
use musiton_ui::step_seq::{StepInput, StepSequencer, ACCENT_VELOCITY};
use musiton_ui::UiAction;

fn project() -> Project {
    let mut p = Project::default();
    p.tracks.push(Track {
        id: TrackId(1),
        kind: TrackKind::Instrument,
        clips: vec![Clip::Midi(MidiClip {
            id: ClipId(2),
            start: Tick(0),
            length: Tick(4 * PPQ),
            notes: Vec::new(),
            cc: Vec::new(),
        })],
        ..Track::default()
    });
    p
}

fn pads() -> Vec<(u8, String)> {
    (36..52).map(|k| (k, format!("Pad {k}"))).collect()
}

fn frame(
    ctx: &egui::Context,
    seq: &mut StepSequencer,
    p: &Project,
    events: Vec<egui::Event>,
) -> Vec<UiAction> {
    let pads = pads();
    let mut actions = Vec::new();
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 700.0))),
        events,
        ..RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            let input = StepInput {
                clip: Some((TrackId(1), ClipId(2))),
                playhead: None,
                pads: Some(&pads),
            };
            actions = seq.ui(ui, p, &input);
        });
    });
    out.textures_delta.clear();
    actions
}

fn button(pos: Pos2, pressed: bool, shift: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: if shift {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        },
    }
}

fn notes_of(actions: Vec<UiAction>) -> Option<Vec<Note>> {
    actions.into_iter().find_map(|a| match a {
        UiAction::Edit {
            edit:
                Edit::ReplaceClip {
                    clip: Clip::Midi(m),
                    ..
                },
            ..
        } => Some(m.notes),
        _ => None,
    })
}

#[test]
fn clicking_and_dragging_sets_steps_and_shift_click_accents() {
    let ctx = egui::Context::default();
    let mut p = project();
    let mut seq = StepSequencer::default();
    frame(&ctx, &mut seq, &p, vec![]);

    // Find the first cell of the top row (pad 51) by probing: a click that adds a note.
    let mut origin = None;
    'probe: for y in (30..90).step_by(3) {
        for x in (100..160).step_by(4) {
            let pos = Pos2::new(x as f32, y as f32);
            frame(
                &ctx,
                &mut seq,
                &p,
                vec![egui::Event::PointerMoved(pos), button(pos, true, false)],
            );
            if let Some(n) = notes_of(frame(&ctx, &mut seq, &p, vec![button(pos, false, false)])) {
                if n.len() == 1 && n[0].pitch == 51 && n[0].start.0 == 0 {
                    origin = Some(pos);
                    break 'probe;
                }
            }
        }
    }
    let origin = origin.expect("no cell found");
    // Probing clicks may have hit the toolbar (its width depends on the fonts egui has):
    // restore the 1/16 step length.
    seq.step = PPQ / 4;

    // Drag along the top row over four cells (22 px each): four steps on, one undo step.
    let mut drag = vec![
        egui::Event::PointerMoved(origin),
        button(origin, true, false),
    ];
    frame(&ctx, &mut seq, &p, std::mem::take(&mut drag));
    for k in 1..=3 {
        let pos = origin + egui::vec2(22.0 * k as f32, 0.0);
        frame(&ctx, &mut seq, &p, vec![egui::Event::PointerMoved(pos)]);
    }
    let end = origin + egui::vec2(66.0, 0.0);
    let notes = notes_of(frame(&ctx, &mut seq, &p, vec![button(end, false, false)]))
        .expect("drag made no edit");
    let starts: Vec<i64> = notes.iter().map(|n| n.start.0).collect();
    assert_eq!(starts, [0, 240, 480, 720]);
    assert!(notes.iter().all(|n| n.pitch == 51 && n.length.0 == 240));
    if let Clip::Midi(m) = &mut p.tracks[0].clips[0] {
        m.notes = notes;
    }

    // Shift+click the second step: accent.
    let second = origin + egui::vec2(22.0, 0.0);
    let shift = egui::Event::ModifiersChanged(egui::Modifiers::SHIFT);
    frame(
        &ctx,
        &mut seq,
        &p,
        vec![shift, egui::Event::PointerMoved(second)],
    );
    frame(&ctx, &mut seq, &p, vec![button(second, true, true)]);
    let notes = notes_of(frame(&ctx, &mut seq, &p, vec![button(second, false, true)]))
        .expect("shift+click made no edit");
    let release = egui::Event::ModifiersChanged(egui::Modifiers::NONE);
    frame(&ctx, &mut seq, &p, vec![release]);
    assert_eq!(notes[1].velocity, ACCENT_VELOCITY);
    assert_ne!(notes[0].velocity, ACCENT_VELOCITY);

    // Dragging from a lit cell turns steps off.
    if let Clip::Midi(m) = &mut p.tracks[0].clips[0] {
        m.notes = notes;
    }
    frame(
        &ctx,
        &mut seq,
        &p,
        vec![
            egui::Event::PointerMoved(origin),
            button(origin, true, false),
        ],
    );
    frame(&ctx, &mut seq, &p, vec![egui::Event::PointerMoved(second)]);
    let notes = notes_of(frame(
        &ctx,
        &mut seq,
        &p,
        vec![button(second, false, false)],
    ))
    .expect("erase drag made no edit");
    let starts: Vec<i64> = notes.iter().map(|n| n.start.0).collect();
    assert_eq!(starts, [480, 720]);
}
