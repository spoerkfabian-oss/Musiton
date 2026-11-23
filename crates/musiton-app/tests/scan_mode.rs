//! The executable's scanner mode (`musiton --scan <file>`).

use std::process::Command;

#[test]
fn scanner_mode_prints_the_plugins_of_a_file() {
    let exe = env!("CARGO_BIN_EXE_musiton");
    let out = Command::new(exe)
        .args(["--scan", "/nonexistent/X.clap"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!out.stderr.is_empty());

    let kars = std::path::Path::new("/usr/lib/clap/Kars.clap");
    if !kars.exists() {
        eprintln!("test plugin not installed; skipping");
        return;
    }
    let out = Command::new(exe).arg("--scan").arg(kars).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("\"name\":\"Kars\""), "{text}");
}

#[test]
fn check_mode_reports_a_working_plugin() {
    let kars = std::path::Path::new("/usr/lib/clap/Kars.clap");
    let Some(info) = musiton_plugin_host::scan_file(kars)
        .ok()
        .and_then(|v| v.into_iter().next())
    else {
        eprintln!("test plugin not installed; skipping");
        return;
    };
    let out = Command::new(env!("CARGO_BIN_EXE_musiton"))
        .args(["--check-plugin", "clap"])
        .arg(kars)
        .args([info.id.as_str(), "1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let lines: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
    assert!(lines.len() >= 4, "{lines:?}");
    assert!(lines.iter().all(|l| l.starts_with("ok")), "{lines:?}");
}
