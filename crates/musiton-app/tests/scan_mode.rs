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
