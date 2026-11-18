//! The video window: shows the project's video in sync with the playhead, and importing
//! a video (its sound goes onto a new audio track).

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use eframe::egui;
use musiton_io::video::{self, VideoPlayer};
use musiton_project::{Edit, Project, Video};

/// File types offered when importing a video.
pub const EXTENSIONS: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "webm", "m4v", "wmv", "mpg", "mpeg",
];

/// Something the app has to do for the video window.
pub enum VideoEvent {
    /// Change the project.
    Edit {
        /// The edit.
        edit: Edit,
        /// Merge with the previous edit (dragging).
        merge: bool,
    },
    /// The sound of an imported video is ready to import as audio.
    ImportAudio(PathBuf),
    /// Show a message in the status bar.
    Status(String),
}

/// State of the video window.
#[derive(Default)]
pub struct VideoView {
    /// Whether the window is shown.
    pub open: bool,
    /// The decoder and the file it plays.
    player: Option<(String, VideoPlayer)>,
    /// A file that could not be opened (with the reason), so it is not retried every frame.
    failed: Option<(String, String)>,
    texture: Option<egui::TextureHandle>,
    /// Sound extraction running in the background.
    extracting: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    /// Last time sent to the decoder.
    last_wish: Option<(Option<f64>, bool)>,
}

/// Folder for sound tracks extracted from videos (deleted after their import).
fn extract_dir() -> PathBuf {
    std::env::temp_dir().join(format!("musiton-video-{}", std::process::id()))
}

/// Whether `file` is a sound track extracted from a video (and can go after importing).
pub fn is_extracted(file: &Path) -> bool {
    file.parent() == Some(extract_dir().as_path())
}

impl VideoView {
    /// Reads `file` and returns the project entry for it; starts extracting its sound at
    /// `sample_rate` in the background (delivered later as [`VideoEvent::ImportAudio`]).
    pub fn load(&mut self, file: &Path, sample_rate: u32) -> Result<Video, String> {
        let info = video::probe(file).map_err(|e| e.to_string())?;
        if info.has_audio {
            let dir = extract_dir();
            let stem = file
                .file_stem()
                .map_or_else(|| "video".into(), |s| s.to_string_lossy().into_owned());
            let out = dir.join(format!("{stem}.wav"));
            let src = file.to_owned();
            let (tx, rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("video-sound".into())
                .spawn(move || {
                    let result = std::fs::create_dir_all(&dir)
                        .map_err(|e| e.to_string())
                        .and_then(|()| {
                            video::extract_audio(&src, &out, sample_rate).map_err(|e| e.to_string())
                        })
                        .map(|()| out);
                    let _ = tx.send(result);
                })
                .map_err(|e| e.to_string())?;
            self.extracting = Some(rx);
        }
        self.failed = None;
        Ok(Video {
            file: file.display().to_string(),
            offset_secs: 0.0,
            duration_secs: info.duration_secs,
        })
    }

    /// Keeps the decoder in step with the project and the playhead (`song_secs`), and
    /// draws the window.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        song_secs: Option<f64>,
        playing: bool,
    ) -> Vec<VideoEvent> {
        let mut events = Vec::new();
        if let Some(rx) = &self.extracting {
            if let Ok(result) = rx.try_recv() {
                self.extracting = None;
                events.push(match result {
                    Ok(path) => VideoEvent::ImportAudio(path),
                    Err(e) => VideoEvent::Status(format!("Could not read the video's sound: {e}")),
                });
            }
        }
        self.sync_player(project.video.as_ref());

        if let (Some((_, player)), Some(v)) = (&self.player, &project.video) {
            let wish = (
                song_secs.filter(|_| self.open).map(|s| s - v.offset_secs),
                playing,
            );
            if self.last_wish != Some(wish) {
                player.show(wish.0, playing);
                self.last_wish = Some(wish);
            }
            if let Some(f) = player.latest() {
                let image = egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.rgba);
                match &mut self.texture {
                    Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        self.texture =
                            Some(ctx.load_texture("video", image, egui::TextureOptions::LINEAR));
                    }
                }
            }
            // Outside the video's time the picture is black.
            if wish.0.is_none_or(|s| !(0.0..v.duration_secs).contains(&s)) {
                self.texture = None;
            }
        }

        let mut open = self.open;
        egui::Window::new("Video")
            .open(&mut open)
            .default_size([480.0, 300.0])
            .show(ctx, |ui| self.window(ui, project, &mut events));
        self.open = open;
        events
    }

    fn sync_player(&mut self, video: Option<&Video>) {
        let file = video.map(|v| v.file.as_str());
        if self.player.as_ref().map(|(f, _)| f.as_str()) == file {
            return;
        }
        self.player = None;
        self.texture = None;
        self.last_wish = None;
        let Some(file) = file else {
            return;
        };
        if self.failed.as_ref().is_some_and(|(f, _)| f == file) {
            return;
        }
        match VideoPlayer::open(Path::new(file)) {
            Ok(p) => self.player = Some((file.to_owned(), p)),
            Err(e) => {
                tracing::warn!("opening video {file}: {e}");
                self.failed = Some((file.to_owned(), e.to_string()));
            }
        }
    }

    fn window(&mut self, ui: &mut egui::Ui, project: &Project, events: &mut Vec<VideoEvent>) {
        let Some(v) = &project.video else {
            ui.label("No video. Use File → Import video… to add one.");
            return;
        };
        ui.horizontal(|ui| {
            ui.label("Starts at");
            let mut offset = v.offset_secs;
            let r = ui.add(
                egui::DragValue::new(&mut offset)
                    .speed(0.01)
                    .max_decimals(3)
                    .suffix(" s"),
            );
            if r.changed() {
                events.push(VideoEvent::Edit {
                    edit: Edit::SetVideo(Some(Video {
                        offset_secs: offset,
                        ..v.clone()
                    })),
                    merge: !r.drag_started(),
                });
            }
            r.on_hover_text("Song time at which the video's first frame shows.");
            if ui.button("Remove video").clicked() {
                events.push(VideoEvent::Edit {
                    edit: Edit::SetVideo(None),
                    merge: false,
                });
            }
        });
        if let Some((_, reason)) = self.failed.as_ref().filter(|(f, _)| *f == v.file) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Cannot play {}: {reason}", v.file),
            );
            return;
        }
        let avail = ui.available_size();
        let (w, h) = self
            .player
            .as_ref()
            .map_or((16, 9), |(_, p)| p.info().preview_size());
        let (w, h) = (w as f32, h as f32);
        let scale = (avail.x / w).min(avail.y / h).max(0.01);
        let size = egui::vec2(w * scale, h * scale);
        match &self.texture {
            Some(t) => {
                ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(
                    t.id(),
                    size,
                )));
            }
            None => {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            }
        }
    }
}
