//! Headless tests of the arrangement view (no window, no GPU).

use std::time::Instant;

use egui::{Pos2, RawInput, Rect};
use musiton_core::ids::TrackId;
use musiton_project::{Edit, Project, Track, UndoStack};
use musiton_ui::arrange::{Arrange, ArrangeInput};
use musiton_ui::UiAction;

fn project_with_tracks(n: u64) -> Project {
    let mut p = Project::default();
    for i in 0..n {
        let id = TrackId(p.alloc_id());
        p.tracks.push(Track {
            id,
            name: format!("Track {i}"),
            ..Track::default()
        });
    }
    p
}

fn input(events: Vec<egui::Event>) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 900.0))),
        events,
        ..RawInput::default()
    }
}

fn frame(
    ctx: &egui::Context,
    arrange: &mut Arrange,
    p: &mut Project,
    events: Vec<egui::Event>,
) -> Vec<UiAction> {
    let mut actions = Vec::new();
    let mut out = ctx.run_ui(input(events), |ui| {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            let peaks = Default::default();
            let targets = Default::default();
            let input = ArrangeInput {
                playhead: Some(1234.0),
                playing: true,
                peaks: &peaks,
                auto_targets: &targets,
            };
            actions = arrange.ui(ui, p, &input);
        });
    });
    out.textures_delta.clear();
    actions
}

#[test]
fn fifty_tracks_render_well_within_a_60fps_frame() {
    let ctx = egui::Context::default();
    let mut p = project_with_tracks(50);
    let mut arrange = Arrange::new();
    // Warm-up (font atlas etc.).
    for _ in 0..5 {
        frame(&ctx, &mut arrange, &mut p, vec![]);
    }
    let frames = 60;
    let start = Instant::now();
    for _ in 0..frames {
        let mut out = ctx.run_ui(input(vec![]), |ui| {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                let peaks = Default::default();
                let targets = Default::default();
                let input = ArrangeInput {
                    playhead: Some(1234.0),
                    playing: true,
                    peaks: &peaks,
                    auto_targets: &targets,
                };
                arrange.ui(ui, &mut p, &input);
            });
        });
        out.textures_delta.clear();
        let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
    }
    let per_frame = start.elapsed() / frames;
    eprintln!("arrange with 50 tracks: {per_frame:?} per frame (layout + tessellation)");
    // 60 fps = 16.7 ms for everything incl. GPU; UI-side work must stay far below.
    assert!(per_frame.as_secs_f64() < 0.008, "too slow: {per_frame:?}");
}

#[test]
fn ctrl_t_adds_track_and_delete_removes_it_with_undo() {
    let ctx = egui::Context::default();
    let mut p = Project::default();
    let mut undo = UndoStack::new();
    let mut arrange = Arrange::new();
    frame(&ctx, &mut arrange, &mut p, vec![]);
    let key = |k, m| egui::Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: m,
    };
    for a in frame(
        &ctx,
        &mut arrange,
        &mut p,
        vec![key(egui::Key::T, egui::Modifiers::COMMAND)],
    ) {
        if let UiAction::Edit { edit, merge } = a {
            undo.perform(&mut p, edit, merge).unwrap();
        }
    }
    assert_eq!(p.tracks.len(), 1);
    assert_eq!(p.tracks[0].name, "Audio 1");
    assert_eq!(arrange.selected, Some(p.tracks[0].id));

    let actions = frame(
        &ctx,
        &mut arrange,
        &mut p,
        vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
    );
    assert!(matches!(
        actions.as_slice(),
        [UiAction::Edit {
            edit: Edit::RemoveTrack { .. },
            ..
        }]
    ));
    for a in actions {
        if let UiAction::Edit { edit, merge } = a {
            undo.perform(&mut p, edit, merge).unwrap();
        }
    }
    assert!(p.tracks.is_empty());
    undo.undo(&mut p).unwrap();
    assert_eq!(p.tracks.len(), 1);
}

#[test]
fn clicking_the_ruler_locates_to_snapped_position() {
    let ctx = egui::Context::default();
    let mut p = project_with_tracks(2);
    let mut arrange = Arrange::new();
    frame(&ctx, &mut arrange, &mut p, vec![]);
    // Ruler sits just below the toolbar; click ~1.4 bars in (default zoom 120 px/bar).
    let x = 8.0 + musiton_ui::arrange::HEADER_WIDTH + 1.4 * 120.0;
    let mut found = None;
    for y in (20..80).step_by(4) {
        let pos = Pos2::new(x, y as f32);
        let press = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        let release = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &ctx,
            &mut arrange,
            &mut p,
            vec![egui::Event::PointerMoved(pos), press],
        );
        let actions = frame(&ctx, &mut arrange, &mut p, vec![release]);
        if let Some(UiAction::Locate(t)) = actions.into_iter().next() {
            found = Some(t);
            break;
        }
    }
    // Default snap is "Bar": 1.4 bars rounds to bar 2 (tick 3840).
    assert_eq!(found, Some(musiton_core::Tick(3840)));
}

#[test]
fn clicking_a_lane_in_automation_view_adds_a_point() {
    let ctx = egui::Context::default();
    let mut p = project_with_tracks(1);
    let mut arrange = Arrange::new();
    arrange.automation_view = true;
    let click = |ctx: &egui::Context, arrange: &mut Arrange, p: &mut Project, pos: Pos2| {
        let mk = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            ctx,
            arrange,
            p,
            vec![egui::Event::PointerMoved(pos), mk(true)],
        );
        frame(ctx, arrange, p, vec![mk(false)])
    };
    frame(&ctx, &mut arrange, &mut p, vec![]);
    let mut got = None;
    // The first track row starts below toolbar and ruler; probe downwards.
    for y in (60..140).step_by(6) {
        let x = 8.0 + musiton_ui::arrange::HEADER_WIDTH + 2.0 * 120.0;
        for a in click(&ctx, &mut arrange, &mut p, Pos2::new(x, y as f32)) {
            if let UiAction::Edit {
                edit: Edit::SetAutomation { lanes, .. },
                ..
            } = a
            {
                got = Some(lanes);
            }
        }
        if got.is_some() {
            break;
        }
    }
    let lanes = got.expect("no automation edit");
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].target, musiton_project::ParamTarget::Volume);
    assert_eq!(lanes[0].points.len(), 1);
    // Snap is "Bar": two bars in → tick 7680.
    assert_eq!(lanes[0].points[0].tick, musiton_core::Tick(7680));
}
