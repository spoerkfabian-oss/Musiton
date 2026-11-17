//! Headless tests of the piano roll (no window, no GPU).

use egui::{Pos2, RawInput, Rect};
use musiton_core::ids::{ClipId, TrackId};
use musiton_core::{Tick, PPQ};
use musiton_project::{Clip, Edit, MidiClip, Project, Track, TrackKind};
use musiton_ui::piano_roll::PianoRoll;
use musiton_ui::UiAction;

fn project() -> Project {
    let mut p = Project::default();
    p.tracks.push(Track {
        id: TrackId(1),
        kind: TrackKind::Instrument,
        clips: vec![Clip::Midi(MidiClip {
            id: ClipId(2),
            start: Tick(0),
            length: Tick(8 * PPQ),
            notes: Vec::new(),
            cc: Vec::new(),
        })],
        ..Track::default()
    });
    p
}

fn frame(
    ctx: &egui::Context,
    roll: &mut PianoRoll,
    p: &Project,
    events: Vec<egui::Event>,
) -> Vec<UiAction> {
    let mut actions = Vec::new();
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 900.0))),
        events,
        ..RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            actions = roll.ui(ui, p);
        });
    });
    out.textures_delta.clear();
    actions
}

fn button(pos: Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// Presses at `from`, moves to `to` in steps and releases; returns the committed clip.
fn drag(
    ctx: &egui::Context,
    roll: &mut PianoRoll,
    p: &Project,
    which: egui::PointerButton,
    (from, to): (Pos2, Pos2),
) -> Option<MidiClip> {
    frame(
        ctx,
        roll,
        p,
        vec![egui::Event::PointerMoved(from), button(from, which, true)],
    );
    for k in 1..=8 {
        let pos = from + (to - from) * (k as f32 / 8.0);
        frame(ctx, roll, p, vec![egui::Event::PointerMoved(pos)]);
    }
    let mut actions = frame(ctx, roll, p, vec![button(to, which, false)]);
    actions.extend(frame(ctx, roll, p, vec![]));
    actions.into_iter().find_map(|a| match a {
        UiAction::Edit {
            edit:
                Edit::ReplaceClip {
                    clip: Clip::Midi(m),
                    ..
                },
            ..
        } => Some(m),
        _ => None,
    })
}

#[test]
fn drawing_in_the_controller_lane_adds_changes_and_right_drag_erases_them() {
    let ctx = egui::Context::default();
    let mut p = project();
    let mut roll = PianoRoll::default();
    roll.open(TrackId(1), ClipId(2));
    roll.lane = Some(1);
    frame(&ctx, &mut roll, &p, vec![]);

    // The lane is the bottom 60 px of the panel; draw upwards from left to right.
    let (low, high) = (880.0, 845.0);
    let stroke = (Pos2::new(200.0, low), Pos2::new(600.0, high));
    let clip = drag(&ctx, &mut roll, &p, egui::PointerButton::Primary, stroke)
        .expect("no edit from drawing in the controller lane");
    assert!(clip.cc.len() >= 3, "{:?}", clip.cc);
    assert!(clip.cc.iter().all(|e| e.controller == 1));
    // Sorted in time, rising in value, one change per grid step (1/16 = 240 ticks).
    for w in clip.cc.windows(2) {
        assert!(
            w[0].tick < w[1].tick && w[0].value < w[1].value,
            "{:?}",
            clip.cc
        );
        assert_eq!((w[1].tick.0 - w[0].tick.0) % 240, 0);
    }
    assert!(clip.cc.iter().all(|e| e.tick.0 % 240 == 0));

    // One undo step = the whole stroke. Apply it, then erase the middle with the right button.
    let drawn = clip.cc.len();
    p.tracks[0].clips[0] = Clip::Midi(clip);
    let erase = (Pos2::new(300.0, low), Pos2::new(500.0, low));
    let clip = drag(&ctx, &mut roll, &p, egui::PointerButton::Secondary, erase)
        .expect("no edit from erasing");
    assert!(
        clip.cc.len() < drawn && !clip.cc.is_empty(),
        "{:?}",
        clip.cc
    );

    // The velocity lane leaves the controller data alone.
    roll.lane = None;
    assert!(drag(&ctx, &mut roll, &p, egui::PointerButton::Primary, stroke).is_none());
}
