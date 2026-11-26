//! Self-update: every Windows build CI publishes is a GitHub release `build-<n>` with
//! `Musiton-portable.zip` and `Musiton-setup.exe`. Musiton compares its own build number
//! with the newest release, downloads the matching package and, once Musiton has closed,
//! a small PowerShell script swaps the files (portable) or runs the setup (installed).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver};

use eframe::egui;

use crate::library::{self, Job, Status};

/// This program's CI build number (`MUSITON_BUILD` at compile time; none for local builds).
pub const BUILD: Option<&str> = option_env!("MUSITON_BUILD");

const LATEST: &str = "https://api.github.com/repos/spoerkfabian-oss/Musiton/releases/latest";
const PORTABLE_ASSET: &str = "Musiton-portable.zip";
const SETUP_ASSET: &str = "Musiton-setup.exe";

/// The newest published build.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    /// Build number (from the tag `build-<n>`).
    pub build: u64,
    /// What changed (the commit message).
    pub notes: String,
    /// Download of the portable folder.
    pub portable_url: String,
    /// Download of the installer.
    pub setup_url: String,
}

/// This program's build number, if it was built by CI.
pub fn current_build() -> Option<u64> {
    BUILD.and_then(|b| b.parse().ok())
}

/// Reads the GitHub API answer for a release.
pub fn parse_release(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let build = v["tag_name"]
        .as_str()?
        .strip_prefix("build-")?
        .parse()
        .ok()?;
    let asset = |name: &str| {
        v["assets"].as_array()?.iter().find_map(|a| {
            (a["name"].as_str() == Some(name))
                .then(|| a["browser_download_url"].as_str().map(str::to_owned))
                .flatten()
        })
    };
    Some(Release {
        build,
        notes: v["body"].as_str().unwrap_or_default().trim().to_owned(),
        portable_url: asset(PORTABLE_ASSET)?,
        setup_url: asset(SETUP_ASSET)?,
    })
}

/// How this copy of Musiton was installed.
#[derive(Debug, Clone, PartialEq)]
pub enum Install {
    /// Unpacked folder: the update replaces its files.
    Portable(PathBuf),
    /// Installed by the setup (its uninstaller sits next to the program).
    Setup,
}

/// Portable or installed, judged from the folder `exe_dir` of the running program.
pub fn install_kind(exe_dir: &Path) -> Install {
    if exe_dir.join("uninstall.exe").exists() {
        Install::Setup
    } else {
        Install::Portable(exe_dir.to_owned())
    }
}

/// Work folder for downloads (`%TEMP%/musiton-update`).
fn work_dir() -> PathBuf {
    std::env::temp_dir().join("musiton-update")
}

/// Asks GitHub for the newest release (blocking).
fn fetch_latest() -> anyhow::Result<Release> {
    let mut resp = library::agent()
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .call()?;
    let status = resp.status().as_u16();
    anyhow::ensure!(status == 200, "GitHub answered {status}");
    let body = resp.body_mut().read_to_string()?;
    parse_release(&body).ok_or_else(|| anyhow::anyhow!("no update package in the newest release"))
}

/// A downloaded update, ready to apply once Musiton has closed.
#[derive(Debug, Clone, PartialEq)]
pub enum Ready {
    /// Unpacked new portable folder.
    Portable(PathBuf),
    /// Downloaded setup program.
    Setup(PathBuf),
}

/// Downloads (and for portable copies unpacks) `release` on the calling thread.
fn prepare(release: &Release, install: &Install, job: &Job) -> anyhow::Result<Ready> {
    let dir = work_dir().join(format!("build-{}", release.build));
    std::fs::create_dir_all(&dir)?;
    let agent = library::agent();
    match install {
        Install::Portable(_) => {
            let part = dir.join(format!("{PORTABLE_ASSET}.part"));
            library::download(&agent, &release.portable_url, &part, job)?;
            job.set(Status::Unpacking(0, 0));
            let target = dir.join("files");
            if target.exists() {
                std::fs::remove_dir_all(&target)?;
            }
            library::unpack(&part, &target, job)?;
            anyhow::ensure!(
                target.join("Musiton.exe").exists(),
                "the update package has no Musiton.exe"
            );
            Ok(Ready::Portable(target))
        }
        Install::Setup => {
            let part = dir.join(format!("{SETUP_ASSET}.part"));
            library::download(&agent, &release.setup_url, &part, job)?;
            let setup = dir.join(SETUP_ASSET);
            std::fs::rename(&part, &setup)?;
            Ok(Ready::Setup(setup))
        }
    }
}

/// Quotes `s` for a single-quoted PowerShell string.
fn ps_quote(s: &Path) -> String {
    format!("'{}'", s.display().to_string().replace('\'', "''"))
}

/// The PowerShell script that waits for Musiton (`pid`, program `exe`) to close, then
/// installs `ready`.
pub fn script(ready: &Ready, exe: &Path, pid: u32) -> String {
    let mut s = format!(
        "$ErrorActionPreference = 'Continue'\r\n\
         Wait-Process -Id {pid} -ErrorAction SilentlyContinue\r\n\
         Start-Sleep -Milliseconds 500\r\n"
    );
    match ready {
        Ready::Portable(src) => {
            let dest = exe.parent().unwrap_or(exe);
            // robocopy merges folders and retries files that are still locked.
            s += &format!(
                "robocopy {} {} /E /R:10 /W:1 /NFL /NDL /NJH /NJS | Out-Null\r\n\
                 Start-Process -FilePath {}\r\n",
                ps_quote(src),
                ps_quote(dest),
                ps_quote(exe)
            );
        }
        Ready::Setup(setup) => {
            s += &format!("Start-Process -FilePath {}\r\n", ps_quote(setup));
        }
    }
    s
}

/// Starts the script for `ready`; Musiton must close right after this.
pub fn launch(ready: &Ready) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let path = work_dir().join("apply-update.ps1");
    std::fs::write(&path, script(ready, &exe, std::process::id()))?;
    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-WindowStyle",
        "Hidden",
        "-File",
    ])
    .arg(&path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()?;
    Ok(())
}

/// Update state shown in the UI.
#[derive(Default)]
pub struct Updater {
    check: Option<(Receiver<Result<Release, String>>, bool)>,
    /// A newer build that the user has not dismissed.
    offer: Option<Release>,
    /// Result of a check the user started from the menu.
    message: Option<String>,
    download: Option<(Job, Receiver<Result<Ready, String>>)>,
    /// Downloaded and waiting for Musiton to close.
    pub ready: Option<Ready>,
}

impl Updater {
    /// Looks for a newer build in the background; `manual` reports "up to date" too.
    pub fn check(&mut self, manual: bool) {
        if !cfg!(windows) {
            if manual {
                self.message = Some("Updates are only published for Windows.".into());
            }
            return;
        }
        if current_build().is_none() {
            if manual {
                self.message =
                    Some("This copy was built by hand, so it cannot update itself.".into());
            }
            return;
        }
        if self.check.is_some() || self.download.is_some() {
            return;
        }
        if !manual {
            // Leftovers of an update that has been applied.
            let _ = std::fs::remove_dir_all(work_dir());
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(fetch_latest().map_err(|e| e.to_string()));
        });
        self.check = Some((rx, manual));
    }

    /// True while something runs in the background (keep repainting).
    pub fn busy(&self) -> bool {
        self.check.is_some() || self.download.is_some()
    }

    fn poll(&mut self) {
        if let Some((rx, manual)) = &self.check {
            if let Ok(result) = rx.try_recv() {
                let manual = *manual;
                self.check = None;
                match result {
                    Ok(r) if Some(r.build) > current_build() => self.offer = Some(r),
                    Ok(_) if manual => self.message = Some("Musiton is up to date.".into()),
                    Err(e) if manual => {
                        self.message = Some(format!("Could not check for updates: {e}"));
                    }
                    _ => {}
                }
            }
        }
        if let Some((_, rx)) = &self.download {
            if let Ok(result) = rx.try_recv() {
                self.download = None;
                match result {
                    Ok(ready) => self.ready = Some(ready),
                    Err(e) => self.message = Some(format!("The update failed: {e}")),
                }
            }
        }
    }

    fn start_download(&mut self, release: Release) {
        let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_owned))
        else {
            self.message = Some("The program folder is unknown.".into());
            return;
        };
        let install = install_kind(&dir);
        let job = Job::new();
        let (tx, rx) = channel();
        let j = job.clone();
        std::thread::spawn(move || {
            let _ = tx.send(prepare(&release, &install, &j).map_err(|e| e.to_string()));
        });
        self.download = Some((job, rx));
    }

    /// Draws the update window; returns true when the user wants to install now.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        self.poll();
        if let Some(msg) = self.message.clone() {
            let mut open = true;
            egui::Window::new("Updates")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| ui.label(msg));
            if !open {
                self.message = None;
            }
        }
        if let Some((job, _)) = &self.download {
            egui::Window::new("Updating Musiton")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    let (done, total) = (
                        job.done.load(Ordering::Relaxed),
                        job.total.load(Ordering::Relaxed),
                    );
                    let (frac, text) = match job.status() {
                        Status::Unpacking(i, n) => {
                            (i as f32 / n.max(1) as f32, "Unpacking…".to_owned())
                        }
                        _ => (
                            done as f32 / total.max(1) as f32,
                            format!("Downloading… {} MB", done / 1_000_000),
                        ),
                    };
                    ui.add(egui::ProgressBar::new(frac).text(text).desired_width(300.0));
                    if ui.button("Cancel").clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                });
        }
        let (mut install, mut later) = (false, false);
        if let Some(ready) = &self.ready {
            egui::Window::new("Update ready")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(match ready {
                        Ready::Portable(_) => "Musiton closes, swaps its files and starts again.",
                        Ready::Setup(_) => "Musiton closes and starts the setup.",
                    });
                    ui.horizontal(|ui| {
                        install = ui.button("Restart and update").clicked();
                        later = ui.button("Later").clicked();
                    });
                });
        }
        if later {
            self.ready = None;
        }
        let (mut start, mut later_offer) = (None, false);
        if let Some(r) = &self.offer {
            egui::Window::new("Update available")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Musiton build {} is out (you have build {}).",
                        r.build,
                        BUILD.unwrap_or("?")
                    ));
                    if !r.notes.is_empty() {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(&r.notes).weak());
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Download update").clicked() {
                            start = Some(r.clone());
                        }
                        later_offer = ui.button("Later").clicked();
                    });
                });
        }
        if later_offer {
            self.offer = None;
        }
        if let Some(r) = start {
            self.offer = None;
            self.start_download(r);
        }
        install
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = r#"{
        "tag_name": "build-42",
        "body": "fix: something\n",
        "assets": [
            {"name": "Musiton-setup.exe", "browser_download_url": "https://x/setup.exe"},
            {"name": "Musiton-portable.zip", "browser_download_url": "https://x/portable.zip"}
        ]
    }"#;

    #[test]
    fn reads_the_newest_release() {
        let r = parse_release(ANSWER).unwrap();
        assert_eq!(r.build, 42);
        assert_eq!(r.notes, "fix: something");
        assert_eq!(r.portable_url, "https://x/portable.zip");
        assert_eq!(r.setup_url, "https://x/setup.exe");
        assert!(parse_release(&ANSWER.replace("build-42", "v1.0")).is_none());
        assert!(parse_release(&ANSWER.replace("Musiton-setup.exe", "other.exe")).is_none());
    }

    #[test]
    fn tells_portable_from_installed_copies() {
        let dir = std::env::temp_dir().join(format!("musiton-kind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(install_kind(&dir), Install::Portable(dir.clone()));
        std::fs::write(dir.join("uninstall.exe"), b"").unwrap();
        assert_eq!(install_kind(&dir), Install::Setup);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_script_waits_then_swaps_files_or_runs_the_setup() {
        let dest = Path::new("Musiton (1)");
        let exe = dest.join("Musiton.exe");
        let src = Path::new("update").join("files");
        let s = script(&Ready::Portable(src.clone()), &exe, 7);
        assert!(s.starts_with("$ErrorActionPreference"));
        assert!(s.contains("Wait-Process -Id 7"));
        assert!(s.contains(&format!("robocopy '{}' 'Musiton (1)'", src.display())));
        assert!(s.contains(&format!("Start-Process -FilePath '{}'", exe.display())));
        let setup = Path::new("it's").join("Musiton-setup.exe");
        let s = script(&Ready::Setup(setup.clone()), &exe, 7);
        let quoted = setup.display().to_string().replace('\'', "''");
        assert!(s.contains(&format!("Start-Process -FilePath '{quoted}'")));
        assert!(quoted.contains("it''s"));
        assert!(!s.contains("robocopy"));
    }
}
