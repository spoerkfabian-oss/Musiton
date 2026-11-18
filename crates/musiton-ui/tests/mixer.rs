//! Headless tests of the mixer.

use egui::{Pos2, RawInput, Rect};
use musiton_core::ids::TrackId;
use musiton_core::Tick;
use musiton_project::{AutoMode, AutoPoint, AutomationLane, Edit, ParamTarget, Project, Track};
use musiton_ui::mixer::Mixer;
use musiton_ui::UiAction;

fn frame(
    ctx: &egui::Context,
    mixer: &mut Mixer,
    p: &Project,
    events: Vec<egui::Event>,
) -> Vec<UiAction> {
    let mut actions = Vec::new();
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0))),
        events,
        ..RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            actions = mixer.ui(ui, p, &Default::default());
        });
    });
    out.textures_delta.clear();
    actions
}

fn click(at: Pos2) -> Vec<egui::Event> {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    vec![egui::Event::PointerMoved(at), button(true), button(false)]
}

/// Owner's report: "the volume fader always jumps back to max". A Read lane owns the
/// fader; the strip offers an "Auto" switch that turns such lanes off.
#[test]
fn read_automation_locks_the_fader_and_auto_switches_it_off() {
    let mut p = Project::default();
    p.tracks.push(Track {
        id: TrackId(1),
        name: "Instrument 1".into(),
        automation: vec![AutomationLane {
            target: ParamTarget::Volume,
            points: vec![AutoPoint {
                tick: Tick(0),
                value: 1.0,
                curve: 0.0,
            }],
            mode: AutoMode::Read,
        }],
        ..Track::default()
    });
    let ctx = egui::Context::default();
    let mut mixer = Mixer::default();
    mixer.playhead = Some(0.0);
    for _ in 0..3 {
        frame(&ctx, &mut mixer, &p, vec![]);
    }
    // Find the "Auto" switch by clicking down the first strip (master comes last).
    let mut found = None;
    'search: for y in (200..760).step_by(4) {
        for x in [30, 45, 60] {
            let at = Pos2::new(x as f32, y as f32);
            for a in frame(&ctx, &mut mixer, &p, click(at)) {
                match a {
                    UiAction::Edit {
                        edit: Edit::SetAutomation { track, lanes },
                        ..
                    } => {
                        found = Some((track, lanes));
                        break 'search;
                    }
                    UiAction::Edit {
                        edit: Edit::SetStrip { .. },
                        ..
                    } => panic!("the locked fader changed the volume at {at:?}"),
                    _ => {}
                }
            }
            frame(&ctx, &mut mixer, &p, vec![]);
        }
    }
    let (track, lanes) = found.expect("no Auto switch found");
    assert_eq!(track, TrackId(1));
    assert_eq!(lanes[0].mode, AutoMode::Off);
    assert_eq!(lanes[0].points.len(), 1, "the points stay");
}
